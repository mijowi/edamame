//! Syntax highlighting for fenced code blocks.  See docs/dev/syntax-highlighting.md.
//!
//! The language comes **solely** from the fence's info string — no auto-detection, since
//! guessing wrong recolors a document the author never asked to have recolored.  An
//! unknown or absent language renders exactly as it did before this module existed.
//!
//! syntect is used for **parsing only**; its themes and HTML writer are excluded at the
//! Cargo level so [`Theme`](crate::config::Theme) stays the single source of color, and
//! this module's whole output vocabulary is the [`TokenClass`] variants.
//!
//! **Ranges are char indices.**  syntect reports byte offsets; every column map in this
//! crate is char-indexed, so the conversion happens once, here, and a byte offset never
//! leaves the module.  One escaping puts every token boundary after a non-ASCII character
//! in the wrong column.
//!
//! **Bounds.**  Code-block content is attacker-controlled and TextMate grammars
//! backtrack, so the caps are load-bearing — but they bound **color, never content**: over
//! a cap the block still renders every byte, just plain.
//! [`MAX_HIGHLIGHT_SOURCE_BYTES`] bounds the cold parse, [`MAX_HIGHLIGHT_LINE_CHARS`] the
//! per-keystroke one; both are needed, because incremental reuse wants an unchanged prefix
//! and a one-line block has none.
//!
//! **Parsing is synchronous; compiling is not.**  Deferring tokenization would have to
//! paint either plain text (colors flickering on every keystroke) or stale char ranges
//! (every color on the edited line shifted), so it stays on the render thread, with
//! incremental reuse ([`tokenize_incremental`]) keeping the steady state at one line.
//! Grammar *compilation* is different: ~9 ms per language, scaling with how many languages
//! a document names rather than with block size, and paid once per language — so it goes to
//! [`spawn_warm_worker`], bounded by [`MAX_HIGHLIGHT_GRAMMARS`], with [`warm_generation`]
//! telling a plain-rendered block it can now be colored.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

// ── Bounds ────────────────────────────────────────────────────────────────

/// Whole-block cap; over it no `ParseState` is constructed at all.
///
/// Bounds the **cold** parse, measured at ~2 µs/byte, so a worst case near 130 ms — paid
/// once per block and then held by [`RenderCache`](crate::markdown::RenderCache).
pub const MAX_HIGHLIGHT_SOURCE_BYTES: usize = 64 * 1024;

/// Single-line cap: a minified bundle or base64 blob is one enormous line, the shape that
/// makes a backtracking grammar pathological.  Such a line is never fed to the parser.
///
/// The only cap that can bound **per-keystroke** cost, since a one-line block offers no
/// unchanged prefix to reuse.  At the measured ~2.6 µs/char this keeps a keystroke inside
/// one frame while sitting far outside anything hand-written.
///
/// Skipping the parse also skips that line's state transition, so lines below it are
/// classified against a stale scope stack — an accepted degradation bounded to one block.
pub const MAX_HIGHLIGHT_LINE_CHARS: usize = 2_000;

/// How many *new* grammars may be queued for compilation in one burst — the third cap,
/// bounding syntect's lazy per-grammar regex compilation, which scales with how many
/// languages a document names rather than with any block's size.
///
/// The work runs on [`spawn_warm_worker`], so this bounds background CPU and queue depth
/// rather than a frame stall; it stays low anyway, since the queue owns a copy of each
/// block and a document naming every shipped language should not pin megabytes and seconds
/// of a core.  Past the budget a language renders plain — color, never content.
pub const MAX_HIGHLIGHT_GRAMMARS: usize = 24;

/// Wall-clock interval returning one slot to the burst budget.  Sized in seconds
/// deliberately: a refill fast enough to matter inside one render would defeat the burst
/// bound it exists to enforce.
const GRAMMAR_BUDGET_REFILL: Duration = Duration::from_secs(1);

/// What [`GrammarBudget::admit`] decided about one grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admission {
    /// Compiled already — tokenize on the render thread.
    Warm,
    /// Newly admitted: the caller owes the worker a request; plain until it lands.
    Queue,
    /// Already queued, or the budget is spent: render plain and send nothing.
    Wait,
}

/// The process-wide grammar-compilation budget.
///
/// Process-global rather than thread-local, unlike [`CACHE`]: syntect caches each compiled
/// regex in the shared `SyntaxSet`, so a grammar compiled on any thread is compiled for
/// all — which is what lets a worker warm one on the render thread's behalf.
///
/// The warm set is **never evicted**.  A compiled grammar is free to reuse, so evicting
/// its record would refuse a free grammar to admit an expensive one — and worse, an
/// adversarial fifty-language document could evict its way through all fifty in one render,
/// which is exactly the burst the cap prevents.  So `warm` only grows and what refills is
/// the budget for *new* compilations, splitting "already paid for?" from "affordable now?".
struct GrammarBudget {
    /// Grammars the worker finished compiling, as pointers into [`SYNTAXES`].  Never
    /// evicted.
    warm: Vec<usize>,
    /// Handed to the worker, not yet finished.  A grammar whose warm parse panicked stays
    /// here forever: plain, rather than retried into the same panic every frame.
    pending: Vec<usize>,
    /// Slots remaining for queueing a grammar that is neither warm nor
    /// pending.
    budget: usize,
    /// `None` until the first query, so the static can be `const`-initialized.
    last_refill: Option<Instant>,
    /// Set when [`Self::admit`] refused a grammar for want of budget, cleared by
    /// [`Self::take_retry`] once a slot refills.
    ///
    /// This is what makes the cap a *burst* limit in practice.  A refused block only
    /// re-asks on a re-render, which comes from [`warm_generation`] moving — and the
    /// queued burst finishes long inside one [`GRAMMAR_BUDGET_REFILL`], so the generation
    /// stops moving while refusals still stand and the budget refills into a bucket no one
    /// asks.  The flag gives `App::tick_syntax_warm` a second, edge-triggered reason to
    /// reparse.
    refused: bool,
}

impl GrammarBudget {
    /// Credit whole elapsed intervals, saturating at `cap`.  `last_refill` advances by
    /// the intervals actually consumed, not to `now`: otherwise a document re-rendering
    /// faster than `interval` would discard the fraction each time and never earn a slot.
    fn refill(&mut self, now: Instant, cap: usize, interval: Duration) {
        let Some(last) = self.last_refill else {
            self.last_refill = Some(now);
            return;
        };
        let elapsed = now.saturating_duration_since(last);
        // `checked_div`: a caller-supplied zero interval must not divide by zero on the
        // render thread.
        let Some(earned) = elapsed.as_nanos().checked_div(interval.as_nanos()) else {
            return;
        };
        if earned == 0 {
            return;
        }
        let room = cap.saturating_sub(self.budget);
        if earned >= room as u128 {
            // Saturated, so the clock restarts: banking slots across a long idle would
            // make the burst bound only as good as the time since the last code block.
            self.budget = cap;
            self.last_refill = Some(now);
        } else {
            // `earned < room <= cap`, so the cast cannot truncate.
            let earned = earned as u32;
            self.budget += earned as usize;
            self.last_refill = Some(last + interval * earned);
        }
    }

    /// Decide about one grammar, spending a slot on [`Admission::Queue`].  Split from the
    /// global so the clock is injected and tests don't mutate process state.
    fn admit(&mut self, key: usize, now: Instant, cap: usize, interval: Duration) -> Admission {
        if self.warm.contains(&key) {
            return Admission::Warm;
        }
        if self.pending.contains(&key) {
            return Admission::Wait;
        }
        self.refill(now, cap, interval);
        if self.budget == 0 {
            self.refused = true;
            return Admission::Wait;
        }
        self.budget -= 1;
        self.pending.push(key);
        Admission::Queue
    }

    /// Has a grammar been refused for want of budget, and has a slot since refilled?
    /// Consumes the answer.
    ///
    /// Edge-triggered on purpose: the caller's reparse gets exactly one chance to spend the
    /// slot, and blocks still refused on that pass set the flag again.  A level flag would
    /// instead reparse the whole document every tick for as long as one refusal stood.
    fn take_retry(&mut self, now: Instant, cap: usize, interval: Duration) -> bool {
        if !self.refused {
            return false;
        }
        self.refill(now, cap, interval);
        if self.budget == 0 {
            return false;
        }
        self.refused = false;
        true
    }

    /// When [`Self::take_retry`] next answers true while a refusal stands, or `None` without
    /// one.  Already past when a slot is banked — an `admit` refill can leave budget over
    /// after queueing another grammar — otherwise the next refill.
    fn retry_at(&self, interval: Duration) -> Option<Instant> {
        if !self.refused {
            return None;
        }
        let last = self.last_refill?;
        Some(if self.budget > 0 {
            last
        } else {
            last + interval
        })
    }

    /// Promote a finished grammar; only the warm worker calls it, only on a clean parse.
    fn mark_warm(&mut self, key: usize) {
        self.pending.retain(|k| *k != key);
        if !self.warm.contains(&key) {
            self.warm.push(key);
        }
    }
}

static GRAMMARS: Mutex<GrammarBudget> = Mutex::new(GrammarBudget {
    warm: Vec::new(),
    pending: Vec::new(),
    budget: MAX_HIGHLIGHT_GRAMMARS,
    last_refill: None,
    refused: false,
});

/// Bumped once per grammar that finishes warming — how a block that rendered plain ever
/// becomes colored.  [`RenderCache`](crate::markdown::RenderCache) keys on a `Block` value
/// warming does not change, so the counter rides in `RenderSettings` to invalidate it.
/// Polled by the event loop rather than sent on a channel, because `markdown` sits well
/// below `app` and must not learn about `AppEvent`; [`warm_in_flight`] says when to poll.
static WARM_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Changes exactly when a previously-plain code block could now be colored.
pub fn warm_generation() -> u64 {
    WARM_GENERATION.load(Ordering::Relaxed)
}

/// Requests sent to the warm worker and not yet finished, panicked ones included.  A count
/// rather than `GrammarBudget::pending`, which keeps a panicked grammar forever and would
/// keep the event loop polling for the session.
///
/// Incremented *before* the send, so the worker's decrement can never underflow it, and
/// decremented with `Release` *after* the generation bump: a reader that `Acquire`s zero
/// therefore also sees the bump it would otherwise miss.
static WARM_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Set when the warm worker thread exits, which strands any count it held.
static WARM_WORKER_GONE: AtomicBool = AtomicBool::new(false);

/// When the warm worker started the request it is on, `None` between requests.  syntect's
/// parse has no time limit, so this is how the event loop tells a stuck compile, which it
/// should stop polling at frame rate, from a normal ~9 ms one.
static WARM_BUSY_SINCE: Mutex<Option<Instant>> = Mutex::new(None);

/// Is a grammar still compiling, so that [`warm_generation`] may yet move?  The event loop
/// blocks indefinitely on input when nothing is due, so it needs this to know to wake.
pub fn warm_in_flight() -> bool {
    !WARM_WORKER_GONE.load(Ordering::Relaxed) && WARM_IN_FLIGHT.load(Ordering::Acquire) > 0
}

/// When the compile the warm worker is running began, or `None` while it is idle or between
/// requests.
pub fn warm_busy_since() -> Option<Instant> {
    *WARM_BUSY_SINCE.lock().ok()?
}

fn set_warm_busy_since(at: Option<Instant>) {
    if let Ok(mut busy) = WARM_BUSY_SINCE.lock() {
        *busy = at;
    }
}

/// Bumped once per retry granted by [`refused_grammar_retry_due`] — the companion to
/// [`WARM_GENERATION`], against the opposite failure.
///
/// A retry is granted precisely when *no* grammar warmed, so without a fingerprint change
/// the reparse would serve the whole document from `RenderCache`, never re-ask [`admit`],
/// and leave the just-refilled slot unspent with `refused` already consumed.
static RETRY_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Changes exactly when a block that rendered plain for want of *budget* should re-ask.
pub fn retry_epoch() -> u64 {
    RETRY_EPOCH.load(Ordering::Relaxed)
}

/// One block handed to the warm worker.
///
/// It carries the block's own lines, not a generic sample: syntect compiles per *match
/// pattern* rather than per grammar, so replaying the real block compiles exactly the
/// patterns the render thread is about to need.  The residual is that an edit can later
/// reach a pattern the warm parse missed, compiling that one regex inline — sub-millisecond
/// against the ~9 ms this removes.
struct WarmRequest {
    syntax: &'static SyntaxReference,
    lines: Vec<String>,
}

/// The warm worker's channel, spawning the thread on first use — `LazyLock` rather than a
/// startup-installed sender so turning the setting on mid-session works.
static WARM_TX: LazyLock<std::sync::mpsc::Sender<WarmRequest>> = LazyLock::new(|| {
    let (tx, rx) = std::sync::mpsc::channel::<WarmRequest>();
    std::thread::spawn(move || warm_worker(&rx));
    tx
});

/// Compile grammars off the render thread: first force [`SYNTAXES`] (deserializing the
/// grammar dump that would otherwise land inside the first `highlight_block`), then drain
/// warm requests forever.
///
/// A panicking parse leaves the grammar in `pending` and does **not** bump the generation,
/// so the block stays plain rather than being retried into the same panic.  The guard stops
/// the process panic hook from restoring the terminal for a panic caught here.
fn warm_worker(rx: &std::sync::mpsc::Receiver<WarmRequest>) {
    /// Runs on any exit, unwinding included, so a dead worker can't pin [`warm_in_flight`].
    struct Exit;
    impl Drop for Exit {
        fn drop(&mut self) {
            WARM_WORKER_GONE.store(true, Ordering::Relaxed);
        }
    }
    let _exit = Exit;

    LazyLock::force(&SYNTAXES);
    while let Ok(req) = rx.recv() {
        let key = std::ptr::from_ref(req.syntax) as usize;
        set_warm_busy_since(Some(Instant::now()));
        let ok = {
            let _expected = crate::terminal::ExpectedPanic::new();
            catch_unwind(AssertUnwindSafe(|| compile_grammar(&req))).is_ok()
        };
        set_warm_busy_since(None);
        if ok {
            if let Ok(mut grammars) = GRAMMARS.lock() {
                grammars.mark_warm(key);
            }
            WARM_GENERATION.fetch_add(1, Ordering::Relaxed);
        } else {
            tracing::warn!(syntax = %req.syntax.name, "panic while warming a grammar");
        }
        WARM_IN_FLIGHT.fetch_sub(1, Ordering::Release);
    }
}

/// Replay a block purely for its side effect on syntect's regex cache; the tokens are
/// discarded and recomputed cheaply on the render thread once the grammar is warm.
fn compile_grammar(req: &WarmRequest) {
    let mut state = ParseState::new(req.syntax);
    let mut stack = ScopeStack::new();
    let mut buf = String::new();
    for line in &req.lines {
        if line.chars().count() > MAX_HIGHLIGHT_LINE_CHARS {
            continue;
        }
        if tokenize_line(line, &mut state, &mut stack, &mut buf).is_none() {
            break;
        }
    }
}

/// Force the warm worker into existence so the grammar dump is deserialized before the
/// first render.  Call once at startup when the setting is on; redundant calls are free.
pub fn spawn_warm_worker() {
    LazyLock::force(&WARM_TX);
}

/// Mark `language`'s grammar usable immediately, so the next [`highlight_block`] compiles
/// it inline on the calling thread instead of rendering plain and waiting for the worker.
///
/// The escape hatch for tests and benches, which need determinism rather than eventual
/// consistency.  Production code must not call it: the mark is shared with the render
/// thread, and it lifts the quarantine on a grammar whose warm parse panicked.  A batch
/// render on its own thread wants [`highlight_block_unbudgeted`] instead.  Spends no
/// budget: that rations *background* compilation.
pub fn warm_inline(language: Option<&str>) -> bool {
    let Some(syntax) = lookup_syntax(language) else {
        return false;
    };
    let key = std::ptr::from_ref(syntax) as usize;
    if let Ok(mut grammars) = GRAMMARS.lock() {
        grammars.mark_warm(key);
    }
    true
}

/// Is this grammar ready to parse here, and if not, does the caller owe the worker a
/// request?  A poisoned lock answers [`Admission::Wait`] — plain text rather than a panic
/// on the render thread.
fn admit(syntax: &'static SyntaxReference) -> Admission {
    let key = std::ptr::from_ref(syntax) as usize;
    let Ok(mut grammars) = GRAMMARS.lock() else {
        return Admission::Wait;
    };
    grammars.admit(
        key,
        Instant::now(),
        MAX_HIGHLIGHT_GRAMMARS,
        GRAMMAR_BUDGET_REFILL,
    )
}

/// Is a re-render owed because a grammar the burst budget refused can now be queued?
/// Consumes the answer, so it is true once per refilled slot.
///
/// The companion to [`warm_generation`]: that covers grammars which *were* queued, this
/// the ones which were not.  Without it [`MAX_HIGHLIGHT_GRAMMARS`] becomes a session limit
/// for any document exceeding it in one render.  Granting a retry also bumps
/// [`RETRY_EPOCH`], which is what makes the reparse actually reach the highlighter.
///
/// A poisoned lock answers `false` — a partly-plain document beats a render-thread panic.
pub fn refused_grammar_retry_due() -> bool {
    let Ok(mut grammars) = GRAMMARS.lock() else {
        return false;
    };
    let due = grammars.take_retry(
        Instant::now(),
        MAX_HIGHLIGHT_GRAMMARS,
        GRAMMAR_BUDGET_REFILL,
    );
    if due {
        RETRY_EPOCH.fetch_add(1, Ordering::Relaxed);
    }
    due
}

/// When [`refused_grammar_retry_due`] will next answer true, or `None` while nothing is
/// refused — the event loop's wake-up for a retry, which no input or worker event would
/// otherwise bring.  May be in the past.
pub fn refused_grammar_retry_at() -> Option<Instant> {
    GRAMMARS.lock().ok()?.retry_at(GRAMMAR_BUDGET_REFILL)
}

/// How many blocks the incremental cache remembers.  Small on purpose: `RenderCache`
/// means only the *one* block being edited is normally asked about, and the rest is slack
/// for a full-document sweep (resize, theme change).
const CACHE_ENTRIES: usize = 4;

// ── Token classes ─────────────────────────────────────────────────────────

/// A highlighted token's kind — one [`Theme`](crate::config::Theme) field each.
///
/// Deliberately no `Default`/`Plain` variant: unclassified text produces no token, so
/// "nothing to say", "unknown language" and "feature off" are one thing downstream, byte
/// for byte what a code block looked like before this module existed.  `Operator` was cut
/// because it would derive from what unclassified text already paints, and `Error` because
/// coloring in-progress typing red is hostile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenClass {
    /// Control flow, declarations, storage modifiers (`fn`, `if`, `pub`).
    Keyword,
    /// String and character literals, including their delimiters.
    String,
    /// Line and block comments.
    Comment,
    /// Numeric literals and language constants (`42`, `true`, `nil`).
    Number,
    /// Type, class, struct and interface names.
    Type,
    /// Function and method names, at definition and call sites.
    Function,
    /// Markup tags, attribute names, preprocessor directives.
    Attribute,
}

/// One classified run within a line, in **char** indices into that line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub range: Range<usize>,
    pub class: TokenClass,
}

/// Classified runs for one line: ascending, non-overlapping, and **sparse** —
/// chars no rule claimed are simply absent, and the caller paints them with
/// the code surface's base style.
pub type HighlightedLine = Vec<Token>;

/// The theme style for a class, to be **patched over** `code_block_text`
/// rather than used on its own.
///
/// The mapping lives here, beside the enum it dispatches on, so a new class
/// cannot be added without the compiler pointing at the arm it still owes.
/// Each field sets only a foreground; the code surface's background comes
/// from the base being patched, which is what keeps a token readable in a
/// theme that moved that surface.
pub fn style_for(theme: &crate::config::Theme, class: TokenClass) -> ratatui::style::Style {
    match class {
        TokenClass::Keyword => theme.syntax_keyword,
        TokenClass::String => theme.syntax_string,
        TokenClass::Comment => theme.syntax_comment,
        TokenClass::Number => theme.syntax_number,
        TokenClass::Type => theme.syntax_type,
        TokenClass::Function => theme.syntax_function,
        TokenClass::Attribute => theme.syntax_attribute,
    }
}

/// Clip `tokens` to `[start, end)` and re-base them onto that window — for `code_wrap`,
/// which splits a source line across visual rows.  A token straddling the split survives
/// in both, keeping a wrapped keyword one color across the break.
pub fn slice_tokens(tokens: &[Token], start: usize, end: usize) -> Vec<Token> {
    tokens
        .iter()
        .filter_map(|t| {
            let s = t.range.start.max(start);
            let e = t.range.end.min(end);
            (s < e).then(|| Token {
                range: (s - start)..(e - start),
                class: t.class,
            })
        })
        .collect()
}

// ── Scope → class ─────────────────────────────────────────────────────────

/// Scope prefixes, most specific first, paired with the class they select.
///
/// Matching is [`Scope::is_prefix_of`], comparing whole dotted atoms — so `keyword` matches
/// `keyword.control.rust` but never a hypothetical `keywords`, which `str::starts_with`
/// would get wrong.  Not an exhaustive TextMate taxonomy and should not become one.
const SCOPE_RULES: &[(&str, TokenClass)] = &[
    ("comment", TokenClass::Comment),
    ("string", TokenClass::String),
    ("constant.numeric", TokenClass::Number),
    ("constant.character", TokenClass::Number),
    ("constant.language", TokenClass::Number),
    // All of `storage`: TextMate uses `storage.type` for the keyword that *declares*
    // something (Rust's `fn`, C's `int`), while a type *name* is `entity.name.type` or
    // `support.type` below.  Mapping it to `Type` would color `fn` as if it named one.
    ("storage", TokenClass::Keyword),
    ("keyword", TokenClass::Keyword),
    ("entity.name.function", TokenClass::Function),
    ("support.function", TokenClass::Function),
    ("entity.name.type", TokenClass::Type),
    ("entity.name.class", TokenClass::Type),
    ("entity.name.struct", TokenClass::Type),
    ("entity.name.enum", TokenClass::Type),
    ("entity.name.interface", TokenClass::Type),
    ("support.type", TokenClass::Type),
    ("support.class", TokenClass::Type),
    ("entity.name.tag", TokenClass::Attribute),
    ("entity.other.attribute-name", TokenClass::Attribute),
    ("meta.preprocessor", TokenClass::Attribute),
];

/// [`SCOPE_RULES`] with each prefix parsed once.  A malformed prefix is dropped rather
/// than panicking: a typo above should cost one class, not the process.
static RULES: LazyLock<Vec<(Scope, TokenClass)>> = LazyLock::new(|| {
    SCOPE_RULES
        .iter()
        .filter_map(|(text, class)| Scope::new(text).ok().map(|scope| (scope, *class)))
        .collect()
});

/// All grammars: syntect's bundled set plus `two-face`'s extras (TypeScript, Dockerfile,
/// Swift, Kotlin, Elixir, Zig, Nix).  `*_newlines` because [`tokenize_line`] feeds each
/// line with its terminator, which is what closes a line comment at end of line.
static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);

/// Classify a scope stack innermost-outward, taking the first matching rule.
///
/// Direction matters: a string's delimiter carries a scope no rule claims, so the walk
/// falls outward to `string.quoted.double` and the quote is colored with the literal it
/// opens.  Outermost-first would let a broad scope swallow the specific one inside it.
fn classify(stack: &ScopeStack) -> Option<TokenClass> {
    stack.as_slice().iter().rev().find_map(|scope| {
        RULES
            .iter()
            .find(|(prefix, _)| prefix.is_prefix_of(*scope))
            .map(|(_, class)| *class)
    })
}

// ── Info string → grammar ─────────────────────────────────────────────────

/// The language name from a fence info string.  Only the first token names a language;
/// authors append metadata (`rust,ignore`, `js {1,3-4}`), so the split covers `,`,
/// whitespace and `{`.  Feeds the grammar lookup only — the renderer's fence label still
/// shows the info string verbatim.
fn language_token(info: &str) -> &str {
    info.split([',', ' ', '\t', '{'])
        .next()
        .unwrap_or("")
        .trim()
}

/// Resolve a fence info string to a grammar.  Lookup is by *token* — syntect's
/// case-insensitive short-name and alias index — not by extension: an info string is a
/// language name, not a path, and the two indexes disagree (`md` vs `markdown`).
fn lookup_syntax(language: Option<&str>) -> Option<&'static SyntaxReference> {
    let token = language_token(language?);
    if token.is_empty() {
        return None;
    }
    SYNTAXES.find_syntax_by_token(token)
}

// ── Incremental cache ─────────────────────────────────────────────────────

/// One block's last highlight, so the next keystroke can reuse what the edit cannot have
/// affected.
///
/// `states` holds the parser position at the **start** of each line, with a tail entry for
/// the state *after* the last — without which a pure append could not resume.  Both halves
/// are stored: `ParseState` alone would leave every line below an edit classifying against
/// an empty `ScopeStack`.
struct CacheEntry {
    syntax: &'static SyntaxReference,
    lines: Vec<String>,
    states: Vec<(ParseState, ScopeStack)>,
    tokens: Vec<HighlightedLine>,
}

thread_local! {
    /// Most-recently-used first.  Thread-local rather than a `Mutex`: the render path is
    /// single-threaded, and per-thread state keeps parallel tests independent.
    static CACHE: RefCell<Vec<CacheEntry>> = const { RefCell::new(Vec::new()) };
}

/// Length of the common prefix of two line lists.
fn common_prefix(a: &[String], b: &[&str]) -> usize {
    a.iter()
        .zip(b)
        .take_while(|(x, y)| x.as_str() == **y)
        .count()
}

/// Length of the common suffix of two line lists, never overlapping an
/// already-counted prefix of `floor` lines.
fn common_suffix(a: &[String], b: &[&str], floor: usize) -> usize {
    let max = a.len().min(b.len()).saturating_sub(floor);
    a.iter()
        .rev()
        .zip(b.iter().rev())
        .take(max)
        .take_while(|(x, y)| x.as_str() == **y)
        .count()
}

// ── Tokenizing ────────────────────────────────────────────────────────────

/// Parse one line into classified runs in **char** indices, advancing `state` and `stack`
/// so the caller can carry them to the next line — which is what classifies a block
/// comment past its first row.
fn tokenize_line(
    line: &str,
    state: &mut ParseState,
    stack: &mut ScopeStack,
    buf: &mut String,
) -> Option<HighlightedLine> {
    // The `*_newlines` grammars expect the terminator; without it a line comment never
    // closes and leaks into the line below.
    buf.clear();
    buf.push_str(line);
    buf.push('\n');

    let ops = state.parse_line(buf, &SYNTAXES).ok()?;

    // Byte → char.  Offsets arrive ascending, so one carried cursor walks the line once in
    // total; a per-call rescan would be O(chars × tokens), which the caps do not bound.
    // All-ASCII skips the walk.  The cursor's byte half only advances by whole
    // `len_utf8()` steps, so it is always a char boundary and `line[b..]` cannot panic; a
    // backwards offset restarts the walk rather than mis-answering.
    let ascii = line.is_ascii();
    let cursor = Cell::new((0usize, 0usize));
    let to_char = |byte: usize| -> usize {
        let byte = byte.min(line.len());
        if ascii {
            return byte;
        }
        let (mut b, mut c) = cursor.get();
        if byte < b {
            (b, c) = (0, 0);
        }
        // A mid-char offset lands on the next boundary.
        let mut chars = line[b..].chars();
        while b < byte {
            let Some(ch) = chars.next() else { break };
            b += ch.len_utf8();
            c += 1;
        }
        cursor.set((b, c));
        c
    };

    let mut out: HighlightedLine = Vec::new();
    let mut run_start = 0usize;
    let push = |from: usize, to: usize, stack: &ScopeStack, out: &mut HighlightedLine| {
        if from >= to {
            return;
        }
        let Some(class) = classify(stack) else {
            return;
        };
        let (s, e) = (to_char(from), to_char(to));
        // Merge abutting same-class runs: grammars emit a scope change per delimiter, so
        // a plain string literal arrives as three ops.
        match out.last_mut() {
            Some(prev) if prev.class == class && prev.range.end == s => prev.range.end = e,
            _ if s < e => out.push(Token { range: s..e, class }),
            _ => {}
        }
    };

    for (offset, op) in ops {
        let offset = offset.min(line.len());
        push(run_start, offset, stack, &mut out);
        run_start = run_start.max(offset);
        stack.apply(&op).ok()?;
    }
    push(run_start, line.len(), stack, &mut out);

    Some(out)
}

/// Tokenize `raw_lines`, reusing whatever the cache can prove is unaffected.
///
/// Reuse is two-sided: lines before the first change carry over directly, and for lines
/// after it the parser position is compared against the cached one — once they agree over
/// unchanged text, every remaining line would parse identically, so those tokens carry
/// over too.  That convergence check re-parses exactly the lines whose color changed: one
/// for an ordinary keystroke, a cascade for a typed `"` until the state settles.
fn tokenize_incremental(
    syntax: &'static SyntaxReference,
    raw_lines: &[&str],
) -> Option<Vec<HighlightedLine>> {
    let n = raw_lines.len();
    let fresh = || (ParseState::new(syntax), ScopeStack::new());

    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let hit = cache
            .iter()
            .position(|e| std::ptr::eq(e.syntax, syntax))
            .filter(|&i| !cache[i].lines.is_empty());

        // `d` re-bases a new-line index onto the cached list, whose length differs
        // whenever the edit added or removed a line.
        let (prefix, suffix, d) = match hit {
            Some(i) => {
                let e = &cache[i];
                let p = common_prefix(&e.lines, raw_lines);
                let s = common_suffix(&e.lines, raw_lines, p);
                (p, s, e.lines.len() as isize - n as isize)
            }
            None => (0, 0, 0),
        };

        let mut tokens: Vec<HighlightedLine> = Vec::with_capacity(n);
        let mut states: Vec<(ParseState, ScopeStack)> = Vec::with_capacity(n + 1);
        let mut cur = fresh();

        if let Some(i) = hit {
            let e = &cache[i];
            tokens.extend_from_slice(&e.tokens[..prefix]);
            states.extend_from_slice(&e.states[..prefix]);
            cur = e.states[prefix].clone();
        }

        let mut buf = String::new();
        let mut line_idx = prefix;
        while line_idx < n {
            // Convergence: same parser position in an unchanged suffix means everything
            // below is already known.
            if line_idx >= n - suffix {
                if let Some(i) = hit {
                    let e = &cache[i];
                    let j = (line_idx as isize + d) as usize;
                    if j < e.lines.len() && e.states[j] == cur {
                        tokens.extend_from_slice(&e.tokens[j..]);
                        states.extend_from_slice(&e.states[j..]);
                        break;
                    }
                }
            }

            states.push(cur.clone());
            let line = raw_lines[line_idx];
            if line.chars().count() > MAX_HIGHLIGHT_LINE_CHARS {
                // Not parsed (see MAX_HIGHLIGHT_LINE_CHARS); the state carries unchanged.
                tokens.push(Vec::new());
            } else {
                let (state, stack) = &mut cur;
                tokens.push(tokenize_line(line, state, stack, &mut buf)?);
            }
            line_idx += 1;
        }
        if states.len() == n {
            states.push(cur);
        }
        debug_assert_eq!(tokens.len(), n);

        // Committing an empty block would evict a real block's entry in the same
        // language — permanently, since `hit` skips an entry with empty `lines`, so the
        // next real block inserts a second one and two `CACHE_ENTRIES` cache nothing.
        if raw_lines.is_empty() {
            return Some(tokens);
        }

        // Commit only on success, so a bail-out leaves no half-built entry to resume from.
        let entry = CacheEntry {
            syntax,
            lines: raw_lines.iter().map(|l| (*l).to_owned()).collect(),
            states,
            tokens: tokens.clone(),
        };
        match hit {
            Some(i) => {
                cache[i] = entry;
                cache[..=i].rotate_right(1);
            }
            None => {
                cache.insert(0, entry);
                cache.truncate(CACHE_ENTRIES);
            }
        }

        Some(tokens)
    })
}

// ── Entry point ───────────────────────────────────────────────────────────

/// Tokenize a fenced code block's body.
///
/// `raw_lines` must already be split the way `Renderer::render_code_block` splits it;
/// taking lines rather than raw content stops the tokens and the painted rows from
/// disagreeing about how many lines a block has.
///
/// Returns exactly `raw_lines.len()` entries, or an **empty vector** meaning "nothing to
/// highlight" — unknown language, over [`MAX_HIGHLIGHT_SOURCE_BYTES`], a grammar error, or
/// a panic.  Callers index with `.get(i)`, so all of those degrade identically.
pub fn highlight_block(language: Option<&str>, raw_lines: &[&str]) -> Vec<HighlightedLine> {
    // Byte cap first, so an over-cap block never spends a grammar slot.
    let Some(syntax) = capped_syntax(language, raw_lines) else {
        return Vec::new();
    };
    match admit(syntax) {
        Admission::Warm => {}
        // Cold: hand the worker this block's lines and render plain.  Deliberately not
        // parsed here — compiling is ~9 ms of render-thread work that neither size cap
        // bounds.  `App::tick_timers` reparses once the generation moves.
        Admission::Queue => {
            let request = WarmRequest {
                syntax,
                lines: raw_lines.iter().map(|l| (*l).to_owned()).collect(),
            };
            WARM_IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
            if WARM_TX.send(request).is_err() {
                WARM_IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
                // The worker is gone; the grammar stays `pending`, so this language
                // renders plain for the session rather than retrying into the failure.
                tracing::warn!("syntax-highlighting warm worker is unavailable");
            }
            return Vec::new();
        }
        Admission::Wait => return Vec::new(),
    }
    tokenize_guarded(syntax, raw_lines)
}

/// [`highlight_block`] for a batch render on its own thread (HTML export): a cold grammar
/// compiles inline on the calling thread rather than going to the warm worker, so the result
/// is complete on the first call.
///
/// Same lookup, same size caps, same panic guard — but it neither reads nor writes the
/// shared [`GRAMMARS`] budget, which belongs to the render thread.  Marking a grammar warm
/// from here would let the render thread parse it before it is compiled, and would undo
/// `warm_worker`'s quarantine of a grammar that panicked.  The render thread must never call
/// it.
pub fn highlight_block_unbudgeted(
    language: Option<&str>,
    raw_lines: &[&str],
) -> Vec<HighlightedLine> {
    match capped_syntax(language, raw_lines) {
        Some(syntax) => tokenize_guarded(syntax, raw_lines),
        None => Vec::new(),
    }
}

/// The grammar for `language`, or `None` when it is unknown or the block is over
/// [`MAX_HIGHLIGHT_SOURCE_BYTES`].
fn capped_syntax(language: Option<&str>, raw_lines: &[&str]) -> Option<&'static SyntaxReference> {
    let syntax = lookup_syntax(language)?;
    let bytes: usize = raw_lines.iter().map(|l| l.len() + 1).sum();
    (bytes <= MAX_HIGHLIGHT_SOURCE_BYTES).then_some(syntax)
}

/// Tokenize with a panic guard; a grammar bug answers `[]` like any other decline.
fn tokenize_guarded(syntax: &'static SyntaxReference, raw_lines: &[&str]) -> Vec<HighlightedLine> {
    // `AssertUnwindSafe` covers the thread-local cache, which a mid-parse panic leaves
    // untouched since the entry is committed only after the walk completes.
    //
    // The guard stops the process panic hook from restoring the terminal for a panic we
    // swallow — otherwise a grammar bug leaves a live TUI painting into a terminal handed
    // back to the shell.  Scoped to the `catch_unwind` alone, so a later unrelated panic
    // that really does end the process is not silenced.
    let parsed = {
        let _expected = crate::terminal::ExpectedPanic::new();
        catch_unwind(AssertUnwindSafe(|| tokenize_incremental(syntax, raw_lines)))
    };
    parsed.ok().flatten().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Class of the token covering char `col`, if any.
    fn class_at(line: &HighlightedLine, col: usize) -> Option<TokenClass> {
        line.iter()
            .find(|t| t.range.contains(&col))
            .map(|t| t.class)
    }

    /// Text of the token covering `col`, for readable assertions.
    fn text_at(src: &str, line: &HighlightedLine, col: usize) -> String {
        line.iter()
            .find(|t| t.range.contains(&col))
            .map(|t| src.chars().take(t.range.end).skip(t.range.start).collect())
            .unwrap_or_default()
    }

    fn clear_cache() {
        CACHE.with(|c| c.borrow_mut().clear());
    }

    /// `highlight_block` with the grammar already usable.  Compilation is asynchronous in
    /// production, so a bare call would answer `[]` first and make every classification
    /// test a race; the asynchronous path has its own tests below.
    fn hl(language: &str, lines: &[&str]) -> Vec<HighlightedLine> {
        assert!(warm_inline(Some(language)), "{language} should resolve");
        highlight_block(Some(language), lines)
    }

    // ── Language resolution ───────────────────────────────────────────────

    #[test]
    fn info_string_keeps_only_the_language_token() {
        assert_eq!(language_token("rust"), "rust");
        assert_eq!(language_token("rust,ignore"), "rust");
        assert_eq!(language_token("js {1,3-4}"), "js");
        assert_eq!(language_token("python title=x"), "python");
        assert_eq!(language_token(""), "");
    }

    #[test]
    fn common_languages_and_aliases_resolve() {
        for token in [
            "rust",
            "rs",
            "python",
            "py",
            "javascript",
            "js",
            "json",
            "yaml",
        ] {
            assert!(
                lookup_syntax(Some(token)).is_some(),
                "{token} should resolve"
            );
        }
    }

    #[test]
    fn two_face_supplies_the_languages_syntect_alone_lacks() {
        // The whole reason `two-face` is a dependency.
        for token in ["typescript", "ts", "dockerfile", "swift", "kotlin"] {
            assert!(
                lookup_syntax(Some(token)).is_some(),
                "{token} should resolve"
            );
        }
    }

    #[test]
    fn language_lookup_is_case_insensitive() {
        assert!(lookup_syntax(Some("RUST")).is_some());
        assert!(lookup_syntax(Some("Python")).is_some());
    }

    #[test]
    fn unknown_and_absent_languages_resolve_to_nothing() {
        assert!(lookup_syntax(None).is_none());
        assert!(lookup_syntax(Some("")).is_none());
        assert!(lookup_syntax(Some("   ")).is_none());
        assert!(lookup_syntax(Some("not-a-real-language")).is_none());
    }

    #[test]
    fn mermaid_has_no_grammar_so_that_surface_stays_out_of_scope() {
        // Pins why `make_code_styled_body_line` was left alone: nothing highlights a
        // mermaid fence.  If this starts failing, wiring that surface is worthwhile.
        assert!(lookup_syntax(Some("mermaid")).is_none());
    }

    // ── Classification ────────────────────────────────────────────────────

    #[test]
    fn rust_keywords_and_functions_are_classified() {
        clear_cache();
        let src = "fn main() {}";
        let out = hl("rust", &[src]);
        assert_eq!(out.len(), 1);
        assert_eq!(class_at(&out[0], 0), Some(TokenClass::Keyword));
        assert_eq!(text_at(src, &out[0], 0), "fn");
        assert_eq!(class_at(&out[0], 3), Some(TokenClass::Function));
        assert_eq!(text_at(src, &out[0], 3), "main");
    }

    #[test]
    fn strings_and_comments_are_classified() {
        clear_cache();
        let src = r#"let s = "hi"; // note"#;
        let out = hl("rust", &[src]);
        let quote = src.find('"').unwrap();
        let comment = src.find("//").unwrap();
        assert_eq!(class_at(&out[0], quote), Some(TokenClass::String));
        assert_eq!(class_at(&out[0], comment), Some(TokenClass::Comment));
    }

    #[test]
    fn a_string_delimiter_takes_the_colour_of_the_literal_it_opens() {
        // Why `classify` walks innermost-outward: the quote's own scope matches no rule
        // and must fall outward to the enclosing `string.*`.
        clear_cache();
        let src = r#""hi""#;
        let out = hl("rust", &[src]);
        assert_eq!(class_at(&out[0], 0), Some(TokenClass::String));
    }

    #[test]
    fn numbers_are_classified() {
        clear_cache();
        let src = "x = 42";
        let out = hl("python", &[src]);
        assert_eq!(class_at(&out[0], 4), Some(TokenClass::Number));
    }

    #[test]
    fn a_block_comment_spans_lines() {
        // Why `ParseState` is carried rather than rebuilt per line.
        clear_cache();
        let lines = ["/* one", "still comment", "done */"];
        let out = hl("rust", &lines);
        for (i, line) in out.iter().enumerate() {
            assert_eq!(
                class_at(line, 1),
                Some(TokenClass::Comment),
                "line {i} should be inside the comment"
            );
        }
    }

    // ── Char indexing ─────────────────────────────────────────────────────

    #[test]
    fn ranges_are_char_indices_not_byte_offsets() {
        // The é is two bytes, so a byte offset would put every boundary after it one
        // column right.
        clear_cache();
        let src = r#"let s = "héllo";"#;
        let out = hl("rust", &[src]);
        let chars: Vec<char> = src.chars().collect();
        for token in &out[0] {
            assert!(
                token.range.end <= chars.len(),
                "range {:?} escapes the line's {} chars — byte offsets leaked",
                token.range,
                chars.len()
            );
        }
        let quote = chars.iter().position(|c| *c == '"').unwrap();
        assert_eq!(class_at(&out[0], quote), Some(TokenClass::String));
        assert_eq!(text_at(src, &out[0], quote), r#""héllo""#);
    }

    #[test]
    fn multibyte_content_keeps_tokens_ordered_and_disjoint() {
        clear_cache();
        let lines = [r#"// 🎉 párty"#, r#"let x = "日本語";"#];
        let out = hl("rust", &lines);
        for (i, line) in out.iter().enumerate() {
            let chars = lines[i].chars().count();
            let mut prev_end = 0;
            for token in line {
                assert!(token.range.start >= prev_end, "overlap on line {i}");
                assert!(token.range.start < token.range.end, "empty run on line {i}");
                assert!(token.range.end <= chars, "past end of line {i}");
                prev_end = token.range.end;
            }
        }
    }

    // ── Bounds ────────────────────────────────────────────────────────────

    #[test]
    fn a_block_over_the_byte_cap_is_not_highlighted() {
        clear_cache();
        let line = "fn main() {}";
        let count = MAX_HIGHLIGHT_SOURCE_BYTES / (line.len() + 1) + 2;
        let lines = vec![line; count];
        assert!(hl("rust", &lines).is_empty());
    }

    #[test]
    fn a_block_just_under_the_byte_cap_is_still_highlighted() {
        clear_cache();
        let line = "fn main() {}";
        let count = MAX_HIGHLIGHT_SOURCE_BYTES / (line.len() + 1) - 2;
        let lines = vec![line; count];
        assert!(!hl("rust", &lines).is_empty());
    }

    #[test]
    fn an_over_long_line_is_skipped_but_its_neighbours_are_not() {
        clear_cache();
        let long = "x".repeat(MAX_HIGHLIGHT_LINE_CHARS + 1);
        let lines = ["fn a() {}", long.as_str(), "fn b() {}"];
        let out = hl("rust", &lines);
        assert_eq!(out.len(), 3);
        assert_eq!(class_at(&out[0], 0), Some(TokenClass::Keyword));
        assert!(out[1].is_empty(), "the over-long line should not be parsed");
        assert_eq!(class_at(&out[2], 0), Some(TokenClass::Keyword));
    }

    /// A fresh budget, so these never touch the process-global one.
    fn budget() -> GrammarBudget {
        GrammarBudget {
            warm: Vec::new(),
            pending: Vec::new(),
            budget: MAX_HIGHLIGHT_GRAMMARS,
            last_refill: None,
            refused: false,
        }
    }

    const SEC: Duration = Duration::from_secs(1);

    /// Queue `n` distinct grammars, asserting each was admitted.
    fn fill(b: &mut GrammarBudget, keys: std::ops::Range<usize>, t: Instant) {
        for key in keys {
            assert_eq!(
                b.admit(key, t, MAX_HIGHLIGHT_GRAMMARS, SEC),
                Admission::Queue,
                "grammar {key} is inside the burst"
            );
        }
    }

    #[test]
    fn the_grammar_burst_is_capped() {
        // Through `GrammarBudget` rather than the global: filling the real one would deny
        // every later test in this process a grammar it might need.
        let mut b = budget();
        let t = Instant::now();
        fill(&mut b, 0..MAX_HIGHLIGHT_GRAMMARS, t);
        assert_eq!(
            b.admit(999, t, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Wait
        );
        assert_eq!(b.pending.len(), MAX_HIGHLIGHT_GRAMMARS);
    }

    #[test]
    fn a_queued_grammar_is_never_queued_twice() {
        // The render thread asks on every reparse, so without the `pending` check one
        // cold block would enqueue a compile per keystroke and drain the budget.
        let mut b = budget();
        let t = Instant::now();
        assert_eq!(b.admit(7, t, MAX_HIGHLIGHT_GRAMMARS, SEC), Admission::Queue);
        for _ in 0..100 {
            assert_eq!(b.admit(7, t, MAX_HIGHLIGHT_GRAMMARS, SEC), Admission::Wait);
        }
        assert_eq!(b.budget, MAX_HIGHLIGHT_GRAMMARS - 1, "one slot, not 101");
    }

    #[test]
    fn a_warm_grammar_is_free_and_never_evicted() {
        // Why `warm` only grows: reuse of a compiled grammar is free, so evicting the
        // record would refuse a free grammar to admit a paid one.
        let mut b = budget();
        let t = Instant::now();
        assert_eq!(b.admit(7, t, MAX_HIGHLIGHT_GRAMMARS, SEC), Admission::Queue);
        b.mark_warm(7);
        assert!(b.pending.is_empty(), "warming clears the pending record");

        fill(&mut b, 100..(100 + MAX_HIGHLIGHT_GRAMMARS - 1), t);
        assert_eq!(
            b.admit(999, t, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Wait
        );
        assert_eq!(b.admit(7, t, MAX_HIGHLIGHT_GRAMMARS, SEC), Admission::Warm);
        assert_eq!(
            b.admit(998, t, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Wait
        );
    }

    #[test]
    fn the_budget_refills_so_a_long_session_is_not_locked_out() {
        // The defect this fixes: a flat lifetime counter left a long session permanently
        // unable to highlight a new language short of a restart.
        let mut b = budget();
        let t = Instant::now();
        fill(&mut b, 0..MAX_HIGHLIGHT_GRAMMARS, t);
        assert_eq!(
            b.admit(999, t, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Wait
        );
        assert_eq!(
            b.admit(999, t + SEC, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Queue
        );
        assert_eq!(
            b.admit(1000, t + SEC, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Wait
        );
        assert_eq!(
            b.admit(1000, t + SEC * 2, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Queue
        );
    }

    #[test]
    fn the_budget_saturates_at_the_cap() {
        // Banking slots across an idle would make the burst bound only as good as the
        // time since the last code block.
        let mut b = budget();
        let t = Instant::now();
        fill(&mut b, 0..MAX_HIGHLIGHT_GRAMMARS, t);
        let later = t + SEC * 10_000;
        fill(&mut b, 100..(100 + MAX_HIGHLIGHT_GRAMMARS), later);
        assert_eq!(
            b.admit(999, later, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Wait
        );
    }

    #[test]
    fn a_refusal_asks_for_one_reparse_per_refilled_slot() {
        // What makes the cap a *burst* limit in practice: without it, the queued burst
        // compiles far inside one refill interval, `warm_generation` stops moving while
        // refusals still stand, and the cap becomes a session limit.
        let mut b = budget();
        let t = Instant::now();

        assert!(!b.take_retry(t + SEC * 10, MAX_HIGHLIGHT_GRAMMARS, SEC));

        fill(&mut b, 0..MAX_HIGHLIGHT_GRAMMARS, t);
        assert_eq!(
            b.admit(999, t, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Wait
        );

        assert!(!b.take_retry(t, MAX_HIGHLIGHT_GRAMMARS, SEC));

        let later = t + SEC;
        assert!(b.take_retry(later, MAX_HIGHLIGHT_GRAMMARS, SEC));
        // Consumed by the asking, or a standing refusal would reparse every tick.
        assert!(!b.take_retry(later, MAX_HIGHLIGHT_GRAMMARS, SEC));

        assert_eq!(
            b.admit(999, later, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Queue
        );
    }

    #[test]
    fn retry_at_names_the_instant_take_retry_first_answers_true() {
        // The event loop sleeps until this instant; one early or absent would leave a
        // refused language plain until the next keypress.
        let mut b = budget();
        let t = Instant::now();
        fill(&mut b, 0..MAX_HIGHLIGHT_GRAMMARS, t);
        assert_eq!(
            b.retry_at(SEC),
            None,
            "nothing refused, nothing to wake for"
        );

        b.admit(999, t, MAX_HIGHLIGHT_GRAMMARS, SEC);
        let at = b.retry_at(SEC).expect("a refusal stands");
        assert!(!b.take_retry(at - Duration::from_millis(1), MAX_HIGHLIGHT_GRAMMARS, SEC));
        assert!(b.take_retry(at, MAX_HIGHLIGHT_GRAMMARS, SEC));
        assert_eq!(b.retry_at(SEC), None, "consumed with the retry");
    }

    #[test]
    fn retry_at_is_already_due_when_a_refill_left_budget_over() {
        // A later `admit` refills two slots and spends one, leaving `refused` standing with
        // budget in hand: `take_retry` is true now, so waiting a further interval is late.
        let mut b = budget();
        let t = Instant::now();
        fill(&mut b, 0..MAX_HIGHLIGHT_GRAMMARS, t);
        b.admit(999, t, MAX_HIGHLIGHT_GRAMMARS, SEC);
        let later = t + SEC * 2;
        assert_eq!(
            b.admit(1000, later, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Queue
        );
        let at = b.retry_at(SEC).expect("a refusal stands");
        assert!(at <= later);
        assert!(b.take_retry(later, MAX_HIGHLIGHT_GRAMMARS, SEC));
    }

    #[test]
    fn a_granted_retry_moves_the_render_fingerprint() {
        // The half `take_retry` alone cannot deliver: a retry is granted precisely when
        // no grammar warmed, so without the epoch in the render fingerprint the reparse
        // serves the document from cache, `admit` is never re-asked, and the refilled slot
        // goes unspent with `refused` already consumed.
        //
        // Compared relatively — the epoch is process-global and tests run in parallel —
        // and driven through the global entry point, where the bump lives.
        {
            let mut grammars = GRAMMARS.lock().expect("budget lock");
            grammars.refused = true;
            grammars.budget = grammars.budget.max(1);
        }
        let before = retry_epoch();
        assert!(refused_grammar_retry_due(), "a refusal with a free slot");
        assert!(
            retry_epoch() > before,
            "a granted retry must invalidate the render cache, or the \
             reparse it triggers cannot reach the highlighter"
        );
    }

    #[test]
    fn a_still_refused_grammar_earns_the_next_slot_too() {
        // Two languages past the cap converge one refill at a time: the block refused on
        // the retry pass re-arms the flag on its way out.
        let mut b = budget();
        let t = Instant::now();
        fill(&mut b, 0..MAX_HIGHLIGHT_GRAMMARS, t);
        for key in [900, 901] {
            assert_eq!(
                b.admit(key, t, MAX_HIGHLIGHT_GRAMMARS, SEC),
                Admission::Wait
            );
        }

        let mut queued = Vec::new();
        for step in 1..=2 {
            let now = t + SEC * step;
            assert!(
                b.take_retry(now, MAX_HIGHLIGHT_GRAMMARS, SEC),
                "step {step}"
            );
            for key in [900, 901] {
                if b.admit(key, now, MAX_HIGHLIGHT_GRAMMARS, SEC) == Admission::Queue {
                    queued.push(key);
                }
            }
        }
        assert_eq!(queued, vec![900, 901]);
    }

    #[test]
    fn a_fractional_interval_is_not_thrown_away() {
        // Advancing `last_refill` to `now` would mean a document re-rendering faster than
        // the interval never earned a slot at all.
        let mut b = budget();
        let t = Instant::now();
        fill(&mut b, 0..MAX_HIGHLIGHT_GRAMMARS, t);
        let ms = Duration::from_millis(300);
        for step in 1..=3 {
            assert_eq!(
                b.admit(999, t + ms * step, MAX_HIGHLIGHT_GRAMMARS, SEC),
                Admission::Wait
            );
        }
        assert_eq!(
            b.admit(999, t + ms * 4, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Queue
        );
        // The remainder is kept rather than reset to the query time.
        assert_eq!(
            b.admit(
                1000,
                t + Duration::from_millis(1_800),
                MAX_HIGHLIGHT_GRAMMARS,
                SEC
            ),
            Admission::Wait
        );
        assert_eq!(
            b.admit(1000, t + SEC * 2, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Queue
        );
    }

    // ── Asynchronous warming ──────────────────────────────────────────────

    #[test]
    fn a_cold_grammar_renders_plain_and_then_colours_once_warmed() {
        // The point of moving compilation off the render thread: the first ask answers
        // plain immediately, and the color arrives on a later reparse.
        clear_cache();
        let lines = ["-- a comment", "local x = 1"];
        let before = warm_generation();

        assert!(
            highlight_block(Some("lua"), &lines).is_empty(),
            "a cold grammar must not block the render thread"
        );

        // Waits on *lua landing*, not on the counter moving: `warm` is process-global and
        // any other grammar finishing also bumps the generation, which would release this
        // thread while lua is still pending and make the classification below answer `[]`.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut out = highlight_block(Some("lua"), &lines);
        while out.is_empty() && Instant::now() < deadline {
            std::thread::yield_now();
            out = highlight_block(Some("lua"), &lines);
        }
        assert!(
            warm_generation() > before,
            "the warm worker should have compiled the grammar"
        );

        assert_eq!(out.len(), 2);
        assert_eq!(class_at(&out[0], 0), Some(TokenClass::Comment));
    }

    #[test]
    fn the_warm_generation_only_moves_when_a_grammar_lands() {
        // `RenderSettings` carries this counter, so a spurious bump invalidates the whole
        // render cache for nothing.  Asserted as "the unknown-language path never reaches
        // the queue" rather than "the counter did not move", since the counter is
        // process-global and another test's warm request could land in the window.
        assert!(lookup_syntax(Some("not-a-real-language")).is_none());
        assert!(lookup_syntax(None).is_none());
        assert!(highlight_block(Some("not-a-real-language"), &["x"]).is_empty());
        assert!(highlight_block(None, &["x"]).is_empty());

        let mut b = budget();
        let t = Instant::now();
        assert_eq!(b.admit(1, t, MAX_HIGHLIGHT_GRAMMARS, SEC), Admission::Queue);
        assert_eq!(b.budget, MAX_HIGHLIGHT_GRAMMARS - 1);
    }

    #[test]
    fn warm_inline_makes_a_grammar_usable_without_the_worker() {
        clear_cache();
        assert!(warm_inline(Some("rust")));
        assert!(!warm_inline(Some("not-a-real-language")));
        let out = highlight_block(Some("rust"), &["fn main() {}"]);
        assert_eq!(class_at(&out[0], 0), Some(TokenClass::Keyword));
    }

    /// Erlang is named by no other test, so nothing else can have warmed it in parallel.
    #[test]
    fn unbudgeted_highlighting_leaves_the_shared_budget_alone() {
        clear_cache();
        let key = std::ptr::from_ref(lookup_syntax(Some("erlang")).unwrap()) as usize;
        let out = highlight_block_unbudgeted(Some("erlang"), &["-module(m)."]);
        assert!(!out.is_empty(), "a cold grammar compiles inline");
        let grammars = GRAMMARS.lock().unwrap();
        assert!(!grammars.warm.contains(&key));
        assert!(!grammars.pending.contains(&key));
    }

    #[test]
    fn unbudgeted_highlighting_keeps_the_byte_cap() {
        let line = "x".repeat(1_000);
        let lines = vec![line.as_str(); MAX_HIGHLIGHT_SOURCE_BYTES / 1_000 + 1];
        assert!(highlight_block_unbudgeted(Some("rust"), &lines).is_empty());
    }

    #[test]
    fn a_refused_grammar_degrades_to_plain_like_an_unknown_one() {
        // The cap reuses the "no tokens" path, so an over-cap document renders exactly as
        // one naming an unshipped language.
        clear_cache();
        let mut b = budget();
        let t = Instant::now();
        fill(&mut b, 0..MAX_HIGHLIGHT_GRAMMARS, t);
        assert_eq!(
            b.admit(999, t, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Wait
        );
        assert!(highlight_block(Some("not-a-real-language"), &["x"]).is_empty());
    }

    #[test]
    fn an_unknown_language_yields_nothing_rather_than_empty_lines() {
        clear_cache();
        assert!(highlight_block(Some("frobnicate"), &["some text"]).is_empty());
        assert!(highlight_block(None, &["some text"]).is_empty());
    }

    // ── Incremental reuse ─────────────────────────────────────────────────

    /// However the cache got there, the answer must equal a cold parse of the same input.
    fn assert_matches_cold(language: &str, lines: &[&str]) {
        let warm = highlight_block(Some(language), lines);
        clear_cache();
        let cold = highlight_block(Some(language), lines);
        assert_eq!(warm, cold, "incremental result diverged from a cold parse");
    }

    #[test]
    fn editing_one_line_matches_a_cold_parse() {
        clear_cache();
        let before = ["fn a() {}", "let x = 1;", "fn b() {}"];
        hl("rust", &before);
        assert_matches_cold("rust", &["fn a() {}", "let x = 12;", "fn b() {}"]);
    }

    #[test]
    fn inserting_a_line_matches_a_cold_parse() {
        clear_cache();
        hl("rust", &["fn a() {}", "fn b() {}"]);
        assert_matches_cold("rust", &["fn a() {}", "let y = 2;", "fn b() {}"]);
    }

    #[test]
    fn deleting_a_line_matches_a_cold_parse() {
        clear_cache();
        hl("rust", &["fn a() {}", "let y = 2;", "fn b() {}"]);
        assert_matches_cold("rust", &["fn a() {}", "fn b() {}"]);
    }

    #[test]
    fn appending_to_the_end_matches_a_cold_parse() {
        // Exercises the tail entry in `states`; without it this would silently re-parse
        // from the top and still pass, so pair it with `reuse_is_actually_happening`.
        clear_cache();
        hl("rust", &["fn a() {}"]);
        assert_matches_cold("rust", &["fn a() {}", "fn b() {}"]);
    }

    #[test]
    fn opening_a_string_cascades_then_reconverges() {
        // One quote reclassifies everything below it until the grammar settles.
        clear_cache();
        hl("rust", &["let a = 1;", "let b = 2;", "let c = 3;"]);
        assert_matches_cold("rust", &[r#"let a = ";"#, "let b = 2;", "let c = 3;"]);
    }

    #[test]
    fn a_block_comment_opened_mid_block_matches_a_cold_parse() {
        clear_cache();
        hl("rust", &["let a = 1;", "let b = 2;", "let c = 3;"]);
        assert_matches_cold("rust", &["let a = 1;", "/* x", "let c = 3;"]);
    }

    #[test]
    fn reuse_is_actually_happening() {
        // Guards against the cache silently degrading into a full re-parse with every
        // correctness test above still passing.
        clear_cache();
        let before = ["fn a() {}", "fn b() {}", "let x = 1;"];
        let first = hl("rust", &before);
        let after = ["fn a() {}", "fn b() {}", "let x = 2;"];
        let second = hl("rust", &after);
        assert_eq!(first[..2], second[..2]);

        CACHE.with(|c| {
            let cache = c.borrow();
            let entry = cache.first().expect("an entry should be cached");
            assert_eq!(entry.lines, after, "cache should hold the latest content");
            assert_eq!(
                entry.states.len(),
                after.len() + 1,
                "states carries one entry per line plus the tail"
            );
        });
    }

    #[test]
    fn different_languages_do_not_evict_each_other() {
        clear_cache();
        hl("rust", &["fn a() {}"]);
        hl("python", &["def a(): pass"]);
        CACHE.with(|c| assert_eq!(c.borrow().len(), 2));
        // Re-asking for rust reuses its entry — the MRU rotation, not eviction.
        hl("rust", &["fn a() {}"]);
        CACHE.with(|c| assert_eq!(c.borrow().len(), 2));
    }

    #[test]
    fn the_cache_is_bounded() {
        clear_cache();
        for lang in ["rust", "python", "javascript", "json", "yaml", "go"] {
            highlight_block(Some(lang), &["x"]);
        }
        CACHE.with(|c| assert!(c.borrow().len() <= CACHE_ENTRIES));
    }

    // ── Throughput ────────────────────────────────────────────────────────

    /// Not a correctness test — a stopwatch for the one risk the caps do not
    /// cover: this runs synchronously on the render thread, so a pathological
    /// input inside the caps could still stall a frame.
    ///
    /// `cargo test --lib highlight::tests::throughput -- --ignored --nocapture`
    #[test]
    #[ignore = "measurement, not an assertion; run manually"]
    fn throughput() {
        use std::time::Instant;

        let cases: Vec<(&str, &str, Vec<String>)> = vec![
            (
                "minified js (one long line)",
                "javascript",
                vec!["var a=1;".repeat(400)],
            ),
            (
                "deep nesting",
                "json",
                vec![format!("{}1{}", "[".repeat(500), "]".repeat(500))],
            ),
            (
                "near-cap rust block",
                "rust",
                (0..4_000)
                    .map(|i| format!("    let x{i} = compute(\"value {i}\");"))
                    .collect(),
            ),
            (
                "unterminated string",
                "rust",
                (0..2_000).map(|i| format!("let s{i} = \"open;")).collect(),
            ),
        ];

        for (name, lang, lines) in cases {
            clear_cache();
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            let bytes: usize = refs.iter().map(|l| l.len() + 1).sum();

            let t = Instant::now();
            let out = highlight_block(Some(lang), &refs);
            let cold = t.elapsed();

            // The case that must be fast: one keystroke on the last line.
            let mut edited = lines.clone();
            if let Some(last) = edited.last_mut() {
                last.push(' ');
            }
            let edited_refs: Vec<&str> = edited.iter().map(String::as_str).collect();
            let t = Instant::now();
            highlight_block(Some(lang), &edited_refs);
            let warm = t.elapsed();

            println!(
                "{name}: {} lines / {bytes} bytes — cold {cold:?}, one-keystroke {warm:?}{}",
                refs.len(),
                if out.is_empty() {
                    " (over cap, not parsed)"
                } else {
                    ""
                },
            );
        }

        // A single long line is the case incremental reuse cannot help:
        // there is no unchanged prefix, so every keystroke pays the full
        // parse. `MAX_HIGHLIGHT_LINE_CHARS` is the only lever, so size it
        // against this sweep rather than by guess.
        println!("\n-- single-line cost by length (minified JS) --");
        for chars in [250usize, 500, 1_000, 2_000, 4_000, 8_000] {
            let line = "var a=1;".repeat(chars / 8);
            clear_cache();
            let t = Instant::now();
            hl("javascript", &[line.as_str()]);
            println!("  {chars:>5} chars: {:?}", t.elapsed());
        }

        // The "open a document containing a big code block" case: large,
        // but under the byte cap, so it really is parsed.
        let big: Vec<String> = (0..1_500)
            .map(|i| format!("    let x{i} = compute(\"value {i}\");"))
            .collect();
        let refs: Vec<&str> = big.iter().map(String::as_str).collect();
        let bytes: usize = refs.iter().map(|l| l.len() + 1).sum();
        clear_cache();
        let t = Instant::now();
        let out = hl("rust", &refs);
        println!(
            "\nlarge in-cap rust block: 1500 lines / {bytes} bytes — cold {:?} (parsed: {})",
            t.elapsed(),
            !out.is_empty()
        );
    }
}
