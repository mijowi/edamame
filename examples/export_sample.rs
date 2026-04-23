//! Render a sample Markdown document to HTML using the Phase 16 exporter.
//!
//! Usage:
//!
//!     cargo run --example export_sample
//!         → writes `target/edamame-export-sample.html`
//!
//!     cargo run --example export_sample -- path/to/source.md
//!         → renders that file to `<source-stem>.html` next to the source
//!
//!     cargo run --example export_sample -- path/to/source.md path/to/out.html
//!         → renders to an explicit output path
//!
//! After running, point your browser at the printed path to manually
//! verify the bundled stylesheet, GFM table rendering, task lists,
//! code blocks, etc.

use std::path::PathBuf;
use std::sync::mpsc;

use edamame::export::{
    preflight, spawn_html_export, target_for_source, HtmlExportOptions, PreflightError, Stylesheet,
};

const SAMPLE_MARKDOWN: &str = r#"# edamame export sample

This document exercises every Markdown construct the bundled stylesheet
needs to look right.  If something here renders poorly, fix
`config/export/default.css` rather than the renderer.

## Inline formatting

Plain text with **bold**, *italic*, ***both***, ~~strikethrough~~,
`inline code`, and a [link to example.com](https://example.com).

> Blockquotes wrap multiple paragraphs and respect inner formatting.
>
> Including **bold inside a quote** and a second paragraph.

## Lists

Ordered:

1. First item
2. Second item
   1. Nested
   2. Also nested
3. Third item

Unordered:

- Apples
- Pears
  - Bartlett
  - Anjou
- Cherries

Task list:

- [x] Implement HTML export
- [x] Bundle a default stylesheet
- [ ] Wire into the command palette (Phase 10)
- [ ] PDF export via custom command

## Tables

| Feature        | Status   | Notes                          |
|----------------|----------|--------------------------------|
| Headings       | Stable   | h1–h6 with rule on h1/h2       |
| Tables         | Stable   | GFM alignment honoured         |
| Task lists     | Stable   | Checkbox is non-interactive    |
| Footnotes[^1]  | Stable   | Definitions appear at the foot |

[^1]: Footnote definitions render below the body in a muted block.

## Code

Inline `let x = 42;` and a fenced block:

```rust
fn main() {
    println!("Hello from edamame export!");
}
```

```python
def greet(name: str) -> None:
    print(f"hello, {name}")
```

## Horizontal rule

---

## Images

Local image references resolve against the source directory.  When
`[export.html].inline_images = true`, they're embedded as base64 data
URIs so the HTML stays self-contained.

![alt text — replace with a real path to test](missing.png)
"#;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (markdown, source_dir, target) = match args.len() {
        0 => {
            let target = workspace_target_dir().join("edamame-export-sample.html");
            (SAMPLE_MARKDOWN.to_owned(), None, target)
        }
        1 => {
            let source = PathBuf::from(&args[0]);
            let md = std::fs::read_to_string(&source).expect("read source markdown");
            let dir = source.parent().map(|p| p.to_path_buf());
            let target = target_for_source(&source, "html");
            (md, dir, target)
        }
        2 => {
            let source = PathBuf::from(&args[0]);
            let md = std::fs::read_to_string(&source).expect("read source markdown");
            let dir = source.parent().map(|p| p.to_path_buf());
            let target = PathBuf::from(&args[1]);
            (md, dir, target)
        }
        _ => {
            eprintln!("usage: export_sample [source.md] [out.html]");
            std::process::exit(2);
        }
    };

    // Demonstrate the overwrite check — always overwrite from the example
    // since manual re-runs are the whole point.
    if let Err(PreflightError::TargetExists(p)) = preflight(&target, /*overwrite=*/ true) {
        eprintln!("preflight refused {p:?}");
        std::process::exit(1);
    }

    let opts = HtmlExportOptions {
        stylesheet: Stylesheet::Builtin,
        inline_images: false,
        source_dir,
        title: Some("edamame export sample".into()),
    };

    let (tx, rx) = mpsc::channel();
    spawn_html_export(markdown, target.clone(), opts, move |outcome| {
        tx.send(outcome).expect("send outcome");
    });

    match rx.recv().expect("worker thread crashed") {
        Ok(path) => {
            let abs = std::fs::canonicalize(&path).unwrap_or(path);
            println!("Wrote {}", abs.display());
            println!("Open with:  xdg-open {}", abs.display());
        }
        Err(msg) => {
            eprintln!("Export failed: {msg}");
            std::process::exit(1);
        }
    }
}

/// Locate the workspace's `target/` directory by walking up from
/// `CARGO_MANIFEST_DIR`.  Falls back to the current working directory
/// if the env var is somehow missing (it never is when run via
/// `cargo run --example`).
fn workspace_target_dir() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().expect("cwd"));
    let target = manifest.join("target");
    std::fs::create_dir_all(&target).expect("create target dir");
    target
}
