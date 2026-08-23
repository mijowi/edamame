//! Syntax highlighting for fenced code blocks.
//!
//! The language comes **solely** from the opening fence's info string
//! (```` ```rust ````).  There is no auto-detection, and none is planned:
//! guessing wrong recolours a document the author did not ask to have
//! recoloured, and the info string is the one place they already said what
//! they meant.  A fence with no language, or one naming a grammar we do not
//! have, renders exactly as it did before this module existed.
//!
//! syntect is used for **parsing only**.  Its own `.tmTheme` themes and its
//! HTML writer are excluded at the Cargo level (see the feature list in
//! `Cargo.toml`): [`Theme`](crate::config::Theme) stays the single source of
//! colour for every render surface in this crate, and this module's whole
//! output vocabulary is the seven [`TokenClass`] variants, each of which the
//! renderer resolves to one `Theme` field.
//!
//! # Ranges are char indices, and that is this module's main job
//!
//! syntect reports **byte** offsets.  Every column map in this crate is
//! char-indexed — [`code_layout`](crate::markdown::code_layout)'s own doc
//! says "raw char column", and [`InlineColMap`](crate::markdown::InlineColMap)
//! and `line_render` agree.  The conversion happens once, here, and a byte
//! offset never leaves this module.  Letting one escape puts every token
//! boundary *after* a non-ASCII character in the wrong column — a `"héllo"`
//! literal or an emoji in a comment shifts the rest of the line — which is
//! issue #28's failure mode arriving through a new door.
//!
//! # Bounds
//!
//! Code-block content is attacker-controlled (see `docs/security.md`), and
//! TextMate grammars run on a backtracking engine, so both caps below are
//! load-bearing rather than tidiness.  Note what they bound: **colour, never
//! content**.  Over either cap the block still renders every byte it always
//! did, just without highlighting.  Mermaid's refuse-to-render posture would
//! be wrong here — a user must always be able to read their own code.
//!
//! [`MAX_HIGHLIGHT_SOURCE_BYTES`] bounds the cold parse and
//! [`MAX_HIGHLIGHT_LINE_CHARS`] the per-keystroke one; they are not
//! redundant, because incremental reuse needs an unchanged prefix and a
//! one-line block has none.
//!
//! # Parsing is synchronous; compiling is not
//!
//! Tokenizing runs **on the render thread**, and that is deliberate.
//! Highlighting changes only colour, on text already on screen, so any
//! deferral has to paint something during the gap and both choices are bad:
//! plain text means the colours drop out and return on every keystroke, and
//! stale tokens are char ranges, so an insert shifts every colour on the one
//! line being looked at.  Incremental reuse (see [`tokenize_incremental`])
//! makes the steady-state cost one line's parse, which is what buys the
//! right to stay synchronous.  [`highlight_block`] is wrapped in
//! `catch_unwind` for the same reason it has to be fast — it is the only
//! content-handling path in the crate with no worker between it and a frame.
//!
//! **Grammar compilation is the exception, and it is on a worker.**  syntect
//! compiles a grammar's regexes lazily, on first use of that language: ~9 ms
//! on average, ~18 ms for Rust's.  That cost is a function of *how many
//! languages a document names* rather than of how much text any block holds,
//! so neither size cap bounds it — a document of fifty one-line fences in
//! fifty languages sits inside both and still costs ~430 ms.  It is also the
//! one part that can be deferred without any of the flicker above, because
//! it happens **once per language** rather than once per keystroke: the
//! block renders plain for a frame or two and then colours permanently.
//! [`spawn_warm_worker`] owns it, [`MAX_HIGHLIGHT_GRAMMARS`] bounds how much
//! of it may be queued at once, and [`warm_generation`] is how a block that
//! rendered plain finds out it can now be coloured.
//!
//! The residual: warming replays the block's own lines, so it compiles
//! exactly the patterns the render thread is about to need — but syntect
//! compiles per *match pattern*, not per grammar, so once the user edits the
//! block new text can reach a pattern the warm parse missed and that one
//! regex compiles inline.  A single pattern rather than a whole grammar, so
//! it is accepted rather than chased.
//!
//! Deserializing the grammar dump is a third cost (~2 ms) and depends on no
//! document at all; the warm worker forces it before draining its queue.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

// ── Bounds ────────────────────────────────────────────────────────────────

/// Whole-block cap, matching `diagram::mermaid::MAX_MERMAID_SOURCE_BYTES`.
/// Over it no `ParseState` is constructed at all.
///
/// This bounds the **cold** parse — the one-time cost of first rendering a
/// block. Measured at roughly 2 µs/byte (a 56 KiB, 1 500-line Rust block
/// parses in ~110 ms), so the cap corresponds to a worst case near 130 ms.
/// That is a real hitch, but it is paid once per block and then held by
/// [`RenderCache`](crate::markdown::RenderCache); the per-keystroke cost is
/// bounded by [`MAX_HIGHLIGHT_LINE_CHARS`] instead.
pub const MAX_HIGHLIGHT_SOURCE_BYTES: usize = 64 * 1024;

/// Single-line cap.  A minified bundle or a pasted base64 blob is one
/// enormous line, which is exactly the shape that makes a backtracking
/// grammar pathological.  Such a line is not fed to the parser.
///
/// This is the cap that bounds **per-keystroke** cost, and it is the only
/// one that can: incremental reuse works by finding an unchanged prefix, and
/// a one-line block has none, so every keystroke in one re-parses the whole
/// line. The `throughput` test below measures that cost as linear in line
/// length — about 2.6 µs/char, with no backtracking blowup — which puts
/// 1 000 chars at ~2.6 ms, 2 000 at ~5.2 ms and 4 000 at ~10.6 ms. 2 000 is
/// chosen to keep a keystroke comfortably inside one frame while sitting far
/// outside anything hand-written.
///
/// Skipping the *parse* also skips that line's state transition, so the
/// lines below it are classified against a stale scope stack.  That is an
/// accepted degradation, and it is bounded to the one block: a few lines
/// may paint the wrong colour, but nothing hangs and nothing is hidden.
pub const MAX_HIGHLIGHT_LINE_CHARS: usize = 2_000;

/// How many *new* grammars may be queued for compilation in one burst.
///
/// The third cap, and the one the other two cannot stand in for.  They
/// bound *parsing*; this bounds the lazy work syntect does the first
/// time a grammar is used.  Compiling one grammar's regexes is ~9 ms on
/// average and ~18 ms for a large one like Rust's, and the cost is a
/// function of *how many languages a document names*, not of how big any
/// block is — so a document of 50 one-line fences in 50 different
/// languages sits comfortably inside both other caps and still costs
/// ~430 ms of compilation.  With all 213 grammars available that reaches
/// multiple seconds.
///
/// **That work no longer lands on the render thread** — see
/// [`spawn_warm_worker`] — so what this bounds is background CPU and the
/// depth of the warm queue rather than a frame stall.  It is kept at 24
/// anyway: the queue holds an owned copy of each block (bounded in turn
/// by [`MAX_HIGHLIGHT_SOURCE_BYTES`]), and a document naming every
/// language we ship should not get to pin ~13 MB and two seconds of a
/// core just by existing.  A polyglot README names a handful.
///
/// Past the budget a language renders plain, the same degradation as an
/// unknown one — colour, never content.
pub const MAX_HIGHLIGHT_GRAMMARS: usize = 24;

/// Wall-clock interval that returns one slot to the burst budget.
///
/// At ~9 ms per compile this is under a 1% duty cycle, so the amortized
/// cost is invisible while the burst stays bounded by
/// [`MAX_HIGHLIGHT_GRAMMARS`].  Sized in seconds rather than
/// milliseconds deliberately: the budget exists to stop one document from
/// queueing two dozen compiles at once, and a refill fast enough to
/// matter inside a single render would defeat it.
const GRAMMAR_BUDGET_REFILL: Duration = Duration::from_secs(1);

/// What [`GrammarBudget::admit`] decided about one grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admission {
    /// Compiled already — tokenize on the render thread, no compilation
    /// will happen.
    Warm,
    /// Newly admitted: the caller owes the warm worker a request.  This
    /// block renders plain until that lands.
    Queue,
    /// Already queued by an earlier render, or the budget is spent.
    /// Render plain and send nothing.
    Wait,
}

/// The process-wide grammar-compilation budget.
///
/// Process-global rather than thread-local, unlike [`CACHE`]: syntect
/// stores each compiled regex in a `once_cell::sync::OnceCell` inside the
/// shared `SyntaxSet`, so a grammar compiled on any thread is compiled
/// for all of them — which is the whole reason a worker thread can warm
/// one on the render thread's behalf.  A per-thread counter would let
/// each thread pay the cap over again.
///
/// # Why the warm set is never evicted
///
/// The obvious repair for a cap that locks a long session out of new
/// languages is LRU eviction, and it is exactly backwards here.  Once a
/// grammar is compiled the regexes live in the shared `SyntaxSet` for the
/// life of the process, so re-using it is *free* — evicting its record
/// would refuse a free grammar in order to admit an expensive one.
/// Worse, it would defeat the cap outright: an adversarial fifty-language
/// document would evict its way through all fifty in one render and queue
/// every compile, which is the burst the cap exists to prevent.
///
/// So `warm` only grows (bounded by the 213 grammars that exist), and
/// what refills is the budget for *new* compilations.  That splits the
/// two questions a flat counter conflates: "has this already been paid
/// for?" (permanent) and "can we afford another right now?" (a rate).
struct GrammarBudget {
    /// Grammars the worker has finished compiling, as `SyntaxReference`
    /// pointers into the one shared [`SYNTAXES`].  Never evicted.
    warm: Vec<usize>,
    /// Grammars handed to the worker but not yet finished.  A grammar
    /// whose warm parse panicked stays here forever, which is
    /// deliberate: it renders plain rather than being retried into the
    /// same panic on every frame.
    pending: Vec<usize>,
    /// Slots remaining for queueing a grammar that is neither warm nor
    /// pending.
    budget: usize,
    /// When the budget was last brought up to date.  `None` until the
    /// first query, so the static can be `const`-initialized.
    last_refill: Option<Instant>,
    /// Set when [`Self::admit`] turned a grammar away for want of
    /// budget, and cleared by [`Self::take_retry`] once a slot has
    /// refilled.
    ///
    /// This is what makes the cap a *burst* limit in practice rather
    /// than only in principle.  A refused block renders plain, and the
    /// only thing that re-asks [`Self::admit`] about it is a re-render,
    /// which for highlighting comes from [`warm_generation`] moving.
    /// The whole queued burst compiles in a few hundred milliseconds —
    /// far inside one [`GRAMMAR_BUDGET_REFILL`] — so once the last
    /// queued grammar lands the generation stops moving and nothing
    /// consults the budget again.  It would then refill into a bucket
    /// no one asks, and a document naming more languages than the cap
    /// would stay partly plain for the rest of the session.  The flag
    /// gives `App::tick_syntax_warm` a second, edge-triggered reason to
    /// reparse.
    refused: bool,
}

impl GrammarBudget {
    /// Credit whole elapsed intervals to the budget, saturating at `cap`.
    ///
    /// `last_refill` advances by the intervals actually consumed rather
    /// than to `now`, so a fractional interval is not thrown away on
    /// every query — otherwise a document re-rendering faster than
    /// `interval` would never earn a slot at all.
    fn refill(&mut self, now: Instant, cap: usize, interval: Duration) {
        let Some(last) = self.last_refill else {
            self.last_refill = Some(now);
            return;
        };
        let elapsed = now.saturating_duration_since(last);
        // `checked_div` rather than `/`: a zero interval is not
        // reachable through the constant above, but a caller-supplied
        // one must not divide by zero on the render thread.
        let Some(earned) = elapsed.as_nanos().checked_div(interval.as_nanos()) else {
            return;
        };
        if earned == 0 {
            return;
        }
        let room = cap.saturating_sub(self.budget);
        if earned >= room as u128 {
            // Saturated: the unconsumed remainder is genuinely gone, so
            // the clock restarts from now.  Banking slots across a long
            // idle would make the burst bound only as good as the time
            // since the last code block.
            self.budget = cap;
            self.last_refill = Some(now);
        } else {
            // `earned < room <= cap`, so the cast cannot truncate.
            let earned = earned as u32;
            self.budget += earned as usize;
            self.last_refill = Some(last + interval * earned);
        }
    }

    /// Decide what to do about one grammar, spending a slot when the
    /// answer is [`Admission::Queue`].
    ///
    /// Split from the global so it can be tested without mutating
    /// process state a concurrently-running test may be relying on, and
    /// so the clock is injected rather than read.
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

    /// Has a grammar been refused for want of budget, and has a slot
    /// since refilled?  Consumes the answer.
    ///
    /// Edge-triggered on purpose.  Answering `true` clears `refused`,
    /// so the caller's reparse gets exactly one chance to spend the
    /// slot; whichever blocks are still refused on that pass set the
    /// flag again and earn the next one.  A document naming twice the
    /// cap therefore colours the rest of itself a language per
    /// [`GRAMMAR_BUDGET_REFILL`] instead of never — while a level flag
    /// would reparse the whole document on every tick for as long as
    /// one refusal stood.
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

    /// Promote a finished grammar.  Called only by the warm worker, and
    /// only on a parse that did not panic.
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

/// Bumped once per grammar that finishes warming.
///
/// This is how a block that rendered plain because its grammar was cold
/// ever becomes coloured.  [`RenderCache`](crate::markdown::RenderCache)
/// memoizes the plain render against a `Block` value the warming does not
/// change, so nothing would otherwise invalidate it — the counter rides
/// in `RenderSettings`, and `App::tick_timers` polls
/// [`warm_generation`] to know when a reparse is owed.  An `AtomicU64`
/// polled on the existing 60 ms loop tick, rather than a channel, because
/// `markdown` sits well below `app` and must not learn about `AppEvent`.
static WARM_GENERATION: AtomicU64 = AtomicU64::new(0);

/// How many grammars have finished warming.  Changes exactly when a
/// previously-plain code block could now be coloured.
pub fn warm_generation() -> u64 {
    WARM_GENERATION.load(Ordering::Relaxed)
}

/// Bumped once per retry granted by [`refused_grammar_retry_due`].
///
/// The companion to [`WARM_GENERATION`], and it rides in `RenderSettings`
/// for the same reason — but against the opposite failure.  A retry is
/// granted precisely when *no* grammar has warmed, so the generation has
/// not moved and every block's `Block` value is unchanged: the reparse
/// `App::tick_syntax_warm` performs would hit `RenderCache` for the whole
/// document, `render_code_block` would never run, and the refused block
/// would never re-ask [`admit`] about the slot the retry just refilled.
/// The slot goes unspent, `refused` has already been consumed by the
/// asking, and the language stays plain for the rest of the session —
/// exactly the limit the retry exists to lift.  Bumping a counter the
/// fingerprint carries is what forces that reparse to actually reach the
/// highlighter.
static RETRY_EPOCH: AtomicU64 = AtomicU64::new(0);

/// How many refused-grammar retries have been granted.  Changes exactly
/// when a block that rendered plain for want of *budget* should re-ask
/// for it.
pub fn retry_epoch() -> u64 {
    RETRY_EPOCH.load(Ordering::Relaxed)
}

/// One block handed to the warm worker.
///
/// It carries the block's own lines rather than a generic sample, and
/// that is the point: syntect compiles per *match pattern*, not per
/// grammar (`Regex` holds a `OnceCell` and there is no public API to
/// force a whole syntax), so warming with invented sample text would
/// compile whichever patterns that text happened to reach.  Replaying the
/// real block compiles exactly the patterns the render thread is about to
/// need.
///
/// The residual: once the user *edits* the block, new text can reach a
/// pattern the warm parse did not, and that one regex compiles on the
/// render thread.  It is a single pattern rather than a whole grammar —
/// sub-millisecond against the ~9 ms this removes — so it is accepted
/// rather than chased.
struct WarmRequest {
    syntax: &'static SyntaxReference,
    lines: Vec<String>,
}

/// The warm worker's channel, spawning the thread on first use.
///
/// `LazyLock` rather than a sender installed at startup so that turning
/// the setting on mid-session works: [`spawn_warm_worker`] merely forces
/// this early, and a request arriving without that call spawns the thread
/// itself.
static WARM_TX: LazyLock<std::sync::mpsc::Sender<WarmRequest>> = LazyLock::new(|| {
    let (tx, rx) = std::sync::mpsc::channel::<WarmRequest>();
    std::thread::spawn(move || warm_worker(&rx));
    tx
});

/// Compile grammars off the render thread.
///
/// Two jobs, in order.  First it forces [`SYNTAXES`], deserializing the
/// grammar dump (~2 ms) that would otherwise land synchronously inside
/// the first `highlight_block`.  Then it drains warm requests forever.
///
/// A panicking parse leaves the grammar in `pending` and does **not**
/// bump the generation: the block stays plain rather than being retried
/// into the same panic on every reparse.  The guard is what stops the
/// process panic hook from restoring the terminal for a panic caught
/// here — see `terminal::panic_guard`.
fn warm_worker(rx: &std::sync::mpsc::Receiver<WarmRequest>) {
    LazyLock::force(&SYNTAXES);
    while let Ok(req) = rx.recv() {
        let key = std::ptr::from_ref(req.syntax) as usize;
        let ok = {
            let _expected = crate::terminal::ExpectedPanic::new();
            catch_unwind(AssertUnwindSafe(|| compile_grammar(&req))).is_ok()
        };
        if !ok {
            tracing::warn!(syntax = %req.syntax.name, "panic while warming a grammar");
            continue;
        }
        if let Ok(mut grammars) = GRAMMARS.lock() {
            grammars.mark_warm(key);
        }
        WARM_GENERATION.fetch_add(1, Ordering::Relaxed);
    }
}

/// Replay a block through the parser purely for its side effect on
/// syntect's regex cache.  The tokens are discarded — the render thread
/// recomputes them, cheaply, once the grammar is warm.
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

/// Force the warm worker into existence, so the grammar dump is
/// deserialized before the first render rather than inside it.
///
/// Call once at startup, and only when the setting is on.  A redundant
/// call is free (`LazyLock`), and a request arriving without any call
/// spawns the thread on the spot — which is what makes turning the
/// setting on mid-session work.
pub fn spawn_warm_worker() {
    LazyLock::force(&WARM_TX);
}

/// Mark `language`'s grammar usable immediately, so the next
/// [`highlight_block`] tokenizes it on the calling thread — compiling it
/// inline — instead of returning plain and waiting for the warm worker.
///
/// The escape hatch for callers with **no frame budget to protect**: the
/// test suite, which needs highlighting to be deterministic rather than
/// eventually-consistent, and any future batch or non-interactive render.
/// The render thread must never call it — compiling inline is exactly the
/// ~9 ms stall [`spawn_warm_worker`] exists to remove.
///
/// Returns whether a grammar was found.  Spends no budget: the budget
/// rations *background* compilation, and a caller that has explicitly
/// asked to pay on its own thread is not the thing it is rationing.
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

/// Is this grammar ready to parse on the render thread, and if not, does
/// the caller owe the worker a request?
///
/// A poisoned lock answers [`Admission::Wait`], degrading to plain text
/// rather than propagating a panic into the render thread.
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

/// Is a re-render owed because a grammar the burst budget turned away
/// can now be queued?  Consumes the answer, so it is true once per
/// refilled slot.
///
/// The companion to [`warm_generation`], and needed for the same
/// reason: a block that rendered plain has no way of its own to find
/// out that it could now be coloured.  The counter covers the grammars
/// that *were* queued; this covers the ones that were not.  Without it
/// [`MAX_HIGHLIGHT_GRAMMARS`] is a session limit for any document that
/// exceeds it in one render — the burst compiles far faster than the
/// budget refills, so the generation stops moving while the refusals
/// still stand and nothing ever asks again.
///
/// Granting a retry also bumps [`RETRY_EPOCH`], and that is not
/// bookkeeping — it is what makes the retry reach anything.  See that
/// static: without a fingerprint change the caller's reparse serves the
/// whole document from `RenderCache` and never calls [`admit`] again.
///
/// A poisoned lock answers `false`: the cost is a document that stays
/// partly plain, against a panic on the render thread.
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

/// How many blocks the incremental cache remembers.  Small on purpose:
/// [`RenderCache`](crate::markdown::RenderCache) means the highlighter is
/// normally only asked about the *one* block being edited, so the useful
/// working set is one entry and the rest is slack for a document whose
/// blocks are being re-rendered in a sweep (a resize, a theme change).
const CACHE_ENTRIES: usize = 4;

// ── Token classes ─────────────────────────────────────────────────────────

/// A highlighted token's kind.  One [`Theme`](crate::config::Theme) field
/// each, all derived from existing `Palette` slots.
///
/// There is deliberately no `Default`/`Plain` variant.  Unclassified text
/// simply produces no token, so "this grammar had nothing to say", "this
/// language is unknown" and "the feature is switched off" are the same thing
/// downstream — plain `code_block_text`, byte for byte what a code block
/// looked like before this module existed.  That equivalence is what the
/// renderer's regression snapshots key on.
///
/// Two further classes were considered and cut.  An `Operator` class would
/// derive from `Palette::text`, which is what unclassified text already
/// paints — a field costing five edit sites to change nothing.  An `Error`
/// class (a grammar's `invalid.illegal`) fires mostly on half-written lines,
/// and colouring a user's in-progress typing red is hostile.
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

/// Clip `tokens` to the char window `[start, end)` and re-base them so they
/// index from `start`.
///
/// Needed only by `code_wrap`, which hard-splits a source line into several
/// visual rows: the tokens are computed against the whole line, so each
/// segment needs the part that overlaps it, addressed from its own column 0.
/// A token straddling the split survives in both rows, which is what keeps a
/// wrapped keyword the same colour on either side of the break.
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
/// These are TextMate/Sublime convention: dotted atoms, general to specific
/// left to right. Matching uses [`Scope::is_prefix_of`], which compares
/// whole atoms with bitwise operations — so `keyword` matches
/// `keyword.control.rust` while never matching a hypothetical `keywords`,
/// which a plain `str::starts_with` would get wrong.
///
/// This is not an exhaustive TextMate taxonomy and should not become one; it
/// only needs to cover what the bundled grammars actually emit.
const SCOPE_RULES: &[(&str, TokenClass)] = &[
    ("comment", TokenClass::Comment),
    ("string", TokenClass::String),
    ("constant.numeric", TokenClass::Number),
    ("constant.character", TokenClass::Number),
    ("constant.language", TokenClass::Number),
    // All of `storage`, not just `storage.modifier`. TextMate uses
    // `storage.type` for the keyword that *declares* something — Rust's
    // `fn` is `storage.type.function.rust`, and C's `int` and JavaScript's
    // `var` are the same shape — while an actual type *name* is
    // `entity.name.type` or `support.type` below. Mapping `storage.type`
    // to `Type` colours `fn` as if it named one.
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

/// [`SCOPE_RULES`] with each prefix parsed once.  A malformed prefix is
/// dropped rather than panicking — a typo in the table above should cost one
/// class, not the process.
static RULES: LazyLock<Vec<(Scope, TokenClass)>> = LazyLock::new(|| {
    SCOPE_RULES
        .iter()
        .filter_map(|(text, class)| Scope::new(text).ok().map(|scope| (scope, *class)))
        .collect()
});

/// All grammars: syntect's bundled set (Sublime's default packages) plus
/// `two-face`'s extras, which is where TypeScript, Dockerfile, Swift,
/// Kotlin, Elixir, Zig and Nix come from.
///
/// `*_newlines` (rather than `*_no_newlines`) because [`tokenize_line`] feeds
/// each line with its terminator attached, which is what lets a grammar close
/// a line-comment or a single-quoted string at end of line.
static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);

/// Classify a scope stack by walking it from innermost outward and taking
/// the first rule that matches.
///
/// Direction matters: a string's own delimiter carries
/// `punctuation.definition.string.begin`, which no rule claims, so the walk
/// falls outward to the enclosing `string.quoted.double` and the quote gets
/// coloured with the literal it opens. Checking outermost-first would let a
/// broad enclosing scope swallow the specific one inside it.
fn classify(stack: &ScopeStack) -> Option<TokenClass> {
    stack.as_slice().iter().rev().find_map(|scope| {
        RULES
            .iter()
            .find(|(prefix, _)| prefix.is_prefix_of(*scope))
            .map(|(_, class)| *class)
    })
}

// ── Info string → grammar ─────────────────────────────────────────────────

/// Extract the language name from a fence info string.
///
/// Only the first token names a language; authors routinely append
/// tool-specific metadata — `rust,ignore` for rustdoc, `js {1,3-4}` for the
/// line-highlight hints some static site generators read. Splitting on
/// `,`, whitespace and `{` covers the conventions in the wild.
///
/// This feeds the grammar lookup only. The ` lang ` label the renderer
/// paints on the opening fence row keeps showing the info string *verbatim*,
/// because that is what the author wrote.
fn language_token(info: &str) -> &str {
    info.split([',', ' ', '\t', '{'])
        .next()
        .unwrap_or("")
        .trim()
}

/// Resolve a fence info string to a grammar, or `None`.
///
/// Lookup is by *token* — syntect's short-name and alias index, so `rs`,
/// `rust`, `py` and `python` all resolve, and matching is case-insensitive.
/// Deliberately not `find_syntax_by_extension`: a fence info string is a
/// language name, not a path, and the two indexes disagree often enough to
/// matter (`md` is an extension, `markdown` is a token).
fn lookup_syntax(language: Option<&str>) -> Option<&'static SyntaxReference> {
    let token = language_token(language?);
    if token.is_empty() {
        return None;
    }
    SYNTAXES.find_syntax_by_token(token)
}

// ── Incremental cache ─────────────────────────────────────────────────────

/// One block's last highlight, kept so the next keystroke inside it can
/// reuse everything the edit could not have affected.
///
/// `states` holds the parser position at the **start** of each line and has
/// `lines.len() + 1` entries — the extra tail entry is the state *after* the
/// last line, without which a pure append (typing at the end of a block)
/// could not resume and would re-parse from the top.
///
/// Both halves of the position are stored. `ParseState` alone is not enough:
/// the scope stack is accumulated by applying each line's ops, so resuming
/// mid-block needs the `ScopeStack` that goes with it or every line below
/// the edit classifies against an empty stack.
struct CacheEntry {
    syntax: &'static SyntaxReference,
    lines: Vec<String>,
    states: Vec<(ParseState, ScopeStack)>,
    tokens: Vec<HighlightedLine>,
}

thread_local! {
    /// Most-recently-used first. Thread-local rather than a `Mutex`: the
    /// render path is single-threaded, and a per-thread cache keeps the
    /// test suite's parallel threads from contending (or from sharing state
    /// between tests, which would make them order-dependent).
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

/// Parse one line, returning its classified runs in **char** indices.
///
/// `state` and `stack` are advanced, so the caller can carry them to the
/// next line — which is what makes a block comment or a multi-line string
/// classify correctly past its first row.
fn tokenize_line(
    line: &str,
    state: &mut ParseState,
    stack: &mut ScopeStack,
    buf: &mut String,
) -> Option<HighlightedLine> {
    // syntect's `*_newlines` grammars expect the terminator; without it a
    // line comment never closes and leaks into the line below.
    buf.clear();
    buf.push_str(line);
    buf.push('\n');

    let ops = state.parse_line(buf, &SYNTAXES).ok()?;

    // Byte → char.  syntect hands its offsets over in ascending order — see
    // the walk below, where each `push` starts at the previous one's end —
    // so a single cursor carried across calls walks the line **once in
    // total**.  A per-call `char_indices().position(..)` rescan would make
    // this O(chars × tokens), which the caps do not bound: a 2 000-char line
    // is inside `MAX_HIGHLIGHT_LINE_CHARS` and can carry hundreds of tokens.
    //
    // An all-ASCII line skips the walk entirely, since there the two indices
    // coincide.  `cursor` holds `(byte, char)` and its byte half is only ever
    // advanced by whole `len_utf8()` steps, so it is always a char boundary
    // and `line[b..]` cannot panic.  An offset that went *backwards* (no
    // grammar does, but this module never trusts one) restarts the walk
    // rather than mis-answering.
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
        // Advancing past a mid-char offset lands on the next boundary, which
        // is what the previous `position(|(b, _)| b >= byte)` also did.
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
        // Merge with the previous run when it is the same class and abuts —
        // grammars emit a scope change per delimiter, so a plain string
        // literal arrives as three separate ops.
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
/// The reuse is two-sided. Lines before the first change cannot have been
/// affected by an edit below them, so their tokens carry over directly. For
/// the lines *after* the change, the parser position is compared against the
/// cached position at the matching line: once the two agree — and the text
/// from there down is unchanged — every remaining line would parse
/// identically, so the rest of the cached tokens carry over too.
///
/// That convergence check is what makes the common cases cheap in the way
/// that matters. Typing inside one line of a 300-line block reconverges
/// immediately and re-parses one line. Typing a `"` cascades until the
/// grammar's state settles, which is exactly the set of lines whose colour
/// genuinely changed.
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

        // Prefix and suffix of the cached content that survive the edit.
        // `d` re-bases a new-line index onto the cached list, which differ in
        // length whenever the edit added or removed a line.
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
            // Convergence: in the unchanged suffix, with the parser in the
            // same position the cached run had here, everything below is
            // already known.
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
                // Not parsed: see MAX_HIGHLIGHT_LINE_CHARS. The state is
                // carried unchanged, which is the stale-stack degradation
                // documented there.
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

        // An empty block has nothing worth remembering, and committing it
        // would *evict* the entry for a real block of the same language —
        // a document holding an empty ```` ```rust ```` fence beside a real
        // one would lose the real one's reuse on every sweep.  Worse, the
        // eviction is permanent rather than merely wasteful: `hit` above
        // skips an entry whose `lines` is empty, so the next real block
        // cannot even find the slot to overwrite and inserts a second one,
        // spending two of `CACHE_ENTRIES` to cache nothing.  Returning here
        // is what keeps that `hit` filter unreachable.
        if raw_lines.is_empty() {
            return Some(tokens);
        }

        // Commit only on success, so a bail-out above can never leave a
        // half-built entry behind for the next call to resume from.
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
/// `raw_lines` is the block's content already split the way
/// `Renderer::render_code_block` splits it (on `\n`, with the single empty
/// trailing entry that pulldown-cmark's trailing newline produces removed).
/// Taking the lines rather than the raw content is deliberate: the caller
/// already owns that convention, and re-deriving it here would give the
/// tokens and the painted rows two chances to disagree about how many lines
/// a block has.
///
/// Returns either exactly `raw_lines.len()` entries, or an **empty vector**
/// meaning "nothing to highlight" — an unknown or absent language, a block
/// over [`MAX_HIGHLIGHT_SOURCE_BYTES`], a grammar error, or a panic. Callers
/// index it with `.get(i)`, so all of those degrade to the same plain
/// rendering without a branch of their own.
pub fn highlight_block(language: Option<&str>, raw_lines: &[&str]) -> Vec<HighlightedLine> {
    let Some(syntax) = lookup_syntax(language) else {
        return Vec::new();
    };
    let bytes: usize = raw_lines.iter().map(|l| l.len() + 1).sum();
    if bytes > MAX_HIGHLIGHT_SOURCE_BYTES {
        return Vec::new();
    }
    // After the byte cap, so an over-cap block never spends one of the
    // grammar slots on a parse that isn't going to happen.
    match admit(syntax) {
        // Compiled already: parsing below costs parsing only.
        Admission::Warm => {}
        // Cold. Hand the worker this block's own lines and render plain
        // for now; `App::tick_timers` sees the generation move when the
        // compile lands and reparses, and this call then takes the
        // `Warm` arm. Deliberately *not* parsed here — compiling a
        // grammar is ~9 ms of synchronous render-thread work, which is
        // the one highlighting cost neither size cap can bound.
        Admission::Queue => {
            let request = WarmRequest {
                syntax,
                lines: raw_lines.iter().map(|l| (*l).to_owned()).collect(),
            };
            if WARM_TX.send(request).is_err() {
                // The worker is gone (it only ends if the channel does,
                // or it panicked outside the guarded parse). The grammar
                // stays `pending`, so this block and every other one in
                // that language render plain for the rest of the
                // session rather than retrying into the same failure.
                tracing::warn!("syntax-highlighting warm worker is unavailable");
            }
            return Vec::new();
        }
        // Queued by an earlier render, or the budget is spent.
        Admission::Wait => return Vec::new(),
    }
    // `AssertUnwindSafe` covers the thread-local cache. A panic mid-parse
    // leaves it untouched, because the new entry is only committed after the
    // walk completes.
    //
    // The guard is what stops the process panic hook from restoring the
    // terminal for a panic we are about to swallow — this runs on the
    // render thread, so without it a grammar bug left a live TUI painting
    // into a terminal that had been handed back to the shell.  Scoped to
    // the `catch_unwind` alone — this is the render thread, so a guard
    // still live afterwards would silence an unrelated panic that really
    // does end the process.  See `terminal::panic_guard`.
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

    /// `highlight_block` with the grammar already usable.
    ///
    /// Grammar compilation is asynchronous in production — a cold
    /// grammar renders plain and the warm worker compiles it — so a bare
    /// `highlight_block` would answer `[]` on its first call and make
    /// every classification test a race.  `warm_inline` opts this thread
    /// into compiling on the spot, which is what a test wants and what
    /// the render thread must never do.  The asynchronous path has its
    /// own tests below.
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
        // The whole reason `two-face` is a dependency; syntect's bundled set
        // is Sublime's defaults and has none of these.
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
        // Pins the reason `make_code_styled_body_line` was left alone: there
        // is nothing to highlight a mermaid fence with. If this ever starts
        // failing, wiring that surface becomes worthwhile.
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
        // The reason `classify` walks innermost-outward: the quote's own
        // `punctuation.definition.string.begin` matches no rule and must
        // fall outward to the enclosing `string.*`.
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
        // Cross-line parser state — the one thing a per-line design gets
        // wrong, and the reason `ParseState` is carried rather than rebuilt.
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
        // The load-bearing test for this module's whole reason to exist. In
        // `let s = "héllo";` the é is two bytes, so a byte offset would put
        // every boundary after it one column right.
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
        // Exercised through `GrammarBudget` rather than the global,
        // because filling the real one would deny every later test in
        // this process a grammar it might need — the lib tests share one.
        let mut b = budget();
        let t = Instant::now();
        fill(&mut b, 0..MAX_HIGHLIGHT_GRAMMARS, t);
        // One past the cap is refused, at the same instant.
        assert_eq!(
            b.admit(999, t, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Wait
        );
        assert_eq!(b.pending.len(), MAX_HIGHLIGHT_GRAMMARS);
    }

    #[test]
    fn a_queued_grammar_is_never_queued_twice() {
        // The render thread asks on *every* reparse of the block, so
        // without the `pending` check one cold block would enqueue a
        // fresh compile request per keystroke and drain the budget in a
        // fraction of a second.
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
        // The reason `warm` only grows: syntect keeps the compiled
        // regexes in a `once_cell::sync::OnceCell` in the shared
        // `SyntaxSet`, so re-use costs nothing and evicting the record
        // would refuse a free grammar in order to admit a paid one.
        let mut b = budget();
        let t = Instant::now();
        assert_eq!(b.admit(7, t, MAX_HIGHLIGHT_GRAMMARS, SEC), Admission::Queue);
        b.mark_warm(7);
        assert!(b.pending.is_empty(), "warming clears the pending record");

        // Exhaust the budget with other grammars...
        fill(&mut b, 100..(100 + MAX_HIGHLIGHT_GRAMMARS - 1), t);
        assert_eq!(
            b.admit(999, t, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Wait
        );
        // ...and the warm one still answers `Warm`, spending nothing.
        assert_eq!(b.admit(7, t, MAX_HIGHLIGHT_GRAMMARS, SEC), Admission::Warm);
        assert_eq!(
            b.admit(998, t, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Wait
        );
    }

    #[test]
    fn the_budget_refills_so_a_long_session_is_not_locked_out() {
        // The defect this design fixes: a flat lifetime counter left a
        // session that had visited two dozen languages permanently
        // unable to highlight a twenty-fifth, with no recovery short of
        // a restart.
        let mut b = budget();
        let t = Instant::now();
        fill(&mut b, 0..MAX_HIGHLIGHT_GRAMMARS, t);
        assert_eq!(
            b.admit(999, t, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Wait
        );
        // One interval later exactly one slot is back — not the whole cap.
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
        // A long idle must not bank slots — otherwise the burst bound is
        // only as good as the time since the last code block.
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
        // What makes the cap a *burst* limit in practice.  A refused
        // block renders plain and only a re-render re-asks `admit`, so
        // without this the cap is a session limit for any document that
        // exceeds it: the queued burst compiles in a few hundred
        // milliseconds, far inside one refill interval, so
        // `warm_generation` stops moving while the refusals still stand.
        let mut b = budget();
        let t = Instant::now();

        // Nothing refused yet, so nothing is owed however long we wait.
        assert!(!b.take_retry(t + SEC * 10, MAX_HIGHLIGHT_GRAMMARS, SEC));

        fill(&mut b, 0..MAX_HIGHLIGHT_GRAMMARS, t);
        assert_eq!(
            b.admit(999, t, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Wait
        );

        // Refused, but no slot has refilled: still nothing to do.
        assert!(!b.take_retry(t, MAX_HIGHLIGHT_GRAMMARS, SEC));

        // A slot refills, so exactly one reparse is owed...
        let later = t + SEC;
        assert!(b.take_retry(later, MAX_HIGHLIGHT_GRAMMARS, SEC));
        // ...and it is consumed by the asking, or a standing refusal
        // would reparse the whole document on every 60 ms tick.
        assert!(!b.take_retry(later, MAX_HIGHLIGHT_GRAMMARS, SEC));

        // That reparse can now spend the slot it was granted.
        assert_eq!(
            b.admit(999, later, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Queue
        );
    }

    #[test]
    fn a_granted_retry_moves_the_render_fingerprint() {
        // The half `take_retry` alone cannot deliver.  A retry is
        // granted precisely when no grammar warmed, so
        // `warm_generation` has not moved and no `Block` value has
        // changed — and `RenderCache::begin_build` only clears when
        // `RenderSettings` differ.  Without the epoch in that
        // fingerprint, `tick_syntax_warm`'s reparse serves the whole
        // document from cache, `render_code_block` never runs,
        // `admit` is never re-asked, the refilled slot goes unspent,
        // and `refused` has already been consumed by the asking — so
        // the language stays plain for the rest of the session, which
        // is exactly the session limit the retry exists to lift.
        //
        // Compared relatively, never against an absolute value: the
        // epoch is process-global and cargo runs these tests on
        // parallel threads.
        //
        // Driven through the global entry point rather than a bare
        // `GrammarBudget`, because the bump lives in the wrapper — so
        // arrange a refusal the wrapper is guaranteed to see.
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
        // Two languages past the cap converge one refill at a time
        // rather than stalling after the first — the block refused on
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
            // The reparse re-renders both blocks; the first spends the
            // slot, the second is refused again and re-arms the flag.
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
        // `last_refill` advances by the intervals consumed, not to `now`.
        // Advancing to `now` would mean a document re-rendering faster
        // than the interval never earned a slot at all.
        let mut b = budget();
        let t = Instant::now();
        fill(&mut b, 0..MAX_HIGHLIGHT_GRAMMARS, t);
        // Queried every 300 ms: no single gap is a whole second, but the
        // fourth query is 1.2 s past the last refill and earns a slot.
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
        // The 200 ms remainder was kept rather than reset to the query
        // time, so the next slot lands at t+2.0 s and not at t+2.2 s.
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
        // The whole point of moving compilation off the render thread:
        // the first ask is answered plain and *immediately* rather than
        // paying ~9 ms to compile a grammar mid-frame, and the colour
        // arrives on a later reparse.
        clear_cache();
        // A language no other test warms, so this really is a cold start.
        let lines = ["-- a comment", "local x = 1"];
        let before = warm_generation();

        // Cold: plain, and the request is queued.
        assert!(
            highlight_block(Some("lua"), &lines).is_empty(),
            "a cold grammar must not block the render thread"
        );

        // The worker compiles it and bumps the generation, which is what
        // `App::tick_syntax_warm` polls to know a reparse is owed.
        //
        // Waited on *lua landing*, not on the counter moving: `warm` is
        // process-global and `cargo test` runs this binary's tests on
        // parallel threads, so any other grammar finishing first also
        // bumps the generation.  Waiting on the counter alone therefore
        // races — it releases this thread while lua is still pending,
        // and the classification below answers `[]`.  A test whose
        // precondition is shared mutable state has to wait for the
        // thing it actually needs.
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

        // Warm: the same call now classifies, on the render thread, with
        // no compilation left to do.
        assert_eq!(out.len(), 2);
        assert_eq!(class_at(&out[0], 0), Some(TokenClass::Comment));
    }

    #[test]
    fn the_warm_generation_only_moves_when_a_grammar_lands() {
        // `RenderSettings` carries this counter, so a spurious bump
        // invalidates the whole render cache for nothing.
        //
        // Asserted as "the unknown-language path never reaches the
        // queue", not as "the counter did not move": `WARM_GENERATION`
        // is process-global and cargo runs these tests on parallel
        // threads, so a `before == after` comparison can be failed by
        // the *other* test's warm request landing in the window — a
        // flake with no defect behind it.  What this owns is that
        // `highlight_block` returns before `admit`, and it can check
        // that directly.
        assert!(lookup_syntax(Some("not-a-real-language")).is_none());
        assert!(lookup_syntax(None).is_none());
        assert!(highlight_block(Some("not-a-real-language"), &["x"]).is_empty());
        assert!(highlight_block(None, &["x"]).is_empty());

        // Nothing else can bump it: only the worker does, and only
        // after a warm request it was sent by `Admission::Queue`.
        let mut b = budget();
        let t = Instant::now();
        assert_eq!(b.admit(1, t, MAX_HIGHLIGHT_GRAMMARS, SEC), Admission::Queue);
        assert_eq!(b.budget, MAX_HIGHLIGHT_GRAMMARS - 1);
    }

    #[test]
    fn warm_inline_makes_a_grammar_usable_without_the_worker() {
        // The escape hatch the tests above rely on, and the contract the
        // renderer's own tests depend on.
        clear_cache();
        assert!(warm_inline(Some("rust")));
        assert!(!warm_inline(Some("not-a-real-language")));
        let out = highlight_block(Some("rust"), &["fn main() {}"]);
        assert_eq!(class_at(&out[0], 0), Some(TokenClass::Keyword));
    }

    #[test]
    fn a_refused_grammar_degrades_to_plain_like_an_unknown_one() {
        // The cap must reuse the "no tokens" path, so a document past it
        // renders exactly as one naming a language we do not ship.
        clear_cache();
        let mut b = budget();
        let t = Instant::now();
        fill(&mut b, 0..MAX_HIGHLIGHT_GRAMMARS, t);
        assert_eq!(
            b.admit(999, t, MAX_HIGHLIGHT_GRAMMARS, SEC),
            Admission::Wait
        );
        // `highlight_block` returns the same empty vector for both, which
        // is what `code_body_row` reads as "render this plainly".
        assert!(highlight_block(Some("not-a-real-language"), &["x"]).is_empty());
    }

    #[test]
    fn an_unknown_language_yields_nothing_rather_than_empty_lines() {
        clear_cache();
        assert!(highlight_block(Some("frobnicate"), &["some text"]).is_empty());
        assert!(highlight_block(None, &["some text"]).is_empty());
    }

    // ── Incremental reuse ─────────────────────────────────────────────────

    /// The invariant that matters: however the cache got there, the answer
    /// must equal a cold parse of the same input.
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
        // Exercises the tail entry in `states`; without it this would
        // silently re-parse from the top and still pass, so pair it with
        // `reuse_is_actually_happening` below.
        clear_cache();
        hl("rust", &["fn a() {}"]);
        assert_matches_cold("rust", &["fn a() {}", "fn b() {}"]);
    }

    #[test]
    fn opening_a_string_cascades_then_reconverges() {
        // The case a character-triggered debounce gets backwards: one quote
        // reclassifies everything below it until the grammar settles.
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
        // Guards against the cache silently degrading into a full re-parse
        // and every correctness test above still passing. After an edit to
        // the last line, the untouched prefix must be reused verbatim.
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
        // Re-asking for rust must reuse its entry rather than insert a
        // second one — the MRU rotation, not eviction.
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
