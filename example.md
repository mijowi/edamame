# Heading 1
## Heading 2
### Heading 3
#### Heading 4
##### Heading 5
###### Heading 6

Also Heading 1
==============

Also Heading 2
--------------

**Bold text** | __Underscore bold__ | *Italic text* | _Underscore italic_ | **_Bold and italic_**

~~Strikethrough~~

==Highlight==

> Blockquote
> > You miss 100% of the shots you don't take.
> >
> > \- Wayne Gretzky
>
> \- Michael Scott

[Web link](https://google.com)

[File link](./plan.md)

![Image on disk (this is alt text)](/home/mjw/Pictures/me.jpg)


* Unordered list
* Foo
* Bar

- Another unordered list
- Foo
- Bar

+ Another unordered list with a very long item that should wrap when it exceeds the width of the terminal.
+ Foo
+ Bar

1. Ordered list
2. Second item
3. Third item


- [ ] A checklist
- [ ] Incomplete item
  - [ ] Sub-task
- [x] Completed item

Two horizontal rules

---

***

Table
| Crate | Version | Purpose |
|---|---|---|
| `ratatui` | latest (0.29+) | TUI framework |
| `crossterm` | latest | Terminal backend, raw mode, event handling |
| `pulldown-cmark` | 0.13 | CommonMark + GFM parsing with source-map offsets |
| `ropey` | 2.x (beta) or 1.6 stable | Rope data structure for the text buffer |


Another table
| abc | defghi |
:-: | -----------:
bar | baz

Table with escaped pipes
| f\|oo  |
| ------ |
| b `\|` az |
| b \| im |

Inline code `this is code`

Super long line of inline code `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`

Code block
```rust
// ── Logging setup ─────────────────────────────────────────────────────────────

/// Initialise the file-based tracing subscriber.
///
/// Returns the non-blocking writer guard; dropping it flushes and closes the
/// log file. The guard must be kept alive for the duration of the program.
fn setup_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let log_dir = Config::log_dir()?;
    if std::fs::create_dir_all(&log_dir).is_err() {
        return None;
    }

// really super long comment that should be longer than the whole width of the entire screen so that I can see how that behavior looks in a code block blah blah blah blah
    let file_appender = tracing_appender::rolling::daily(&log_dir, "debug.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .init();

    tracing::info!("edamame starting");
    Some(guard)
}
```

Escaped characters
\*not emphasized*
\<br/> not a tag
\[not a link](/foo)
\`not code`
1\. not a list
\* not a list
\# not a heading
\[foo]: /url "not a reference"
\&ouml; not a character entity

The quick brown fox jumped over the lazy dog.

This is the last line of the document.
