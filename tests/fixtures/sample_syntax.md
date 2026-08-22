# Syntax highlighting smoke test

Manual smoke fixture. Open with `cargo run -- tests/fixtures/sample_syntax.md`
and check each block below. Toggle the feature with **Syntax highlighting** in
the settings overlay to confirm every block falls back to plain code cleanly.

## Rust

```rust
use std::collections::HashMap;

/// A doc comment.
pub struct Counter {
    counts: HashMap<String, usize>,
}

impl Counter {
    pub fn new() -> Self {
        Self { counts: HashMap::new() }
    }

    fn bump(&mut self, key: &str) -> usize {
        let entry = self.counts.entry(key.to_owned()).or_insert(0);
        *entry += 1;
        *entry
    }
}

fn main() {
    let raw = r#"a "raw" string"#;   // raw strings and lifetimes
    let ch = 'x';
    let life: &'static str = "not a char literal";
    println!("{raw} {ch} {life} {}", 42.5);
}
```

## Python

```python
from dataclasses import dataclass


@dataclass
class Point:
    x: float = 0.0
    y: float = 0.0

    def scaled(self, k: float) -> "Point":
        # f-strings carry embedded expressions
        print(f"scaling {self.x} by {k!r}")
        return Point(self.x * k, self.y * k)


if __name__ == "__main__":
    print(Point(1, 2).scaled(3), True, None)
```

## TypeScript

This one only resolves because of `two-face`; syntect's bundled set has no
TypeScript grammar.

```typescript
interface User {
  id: number;
  name: string;
}

export async function fetchUser(id: number): Promise<User | null> {
  const res = await fetch(`/api/users/${id}`);
  if (!res.ok) return null;
  return (await res.json()) as User;
}
```

## Shell

```bash
#!/usr/bin/env bash
set -euo pipefail

for f in *.md; do
  echo "checking ${f}" >&2
  grep -c '```' "$f" || true
done
```

## Data formats

```json
{ "name": "edamame", "version": "0.1.2", "nested": { "ok": true, "n": null } }
```

```yaml
theme: edamame
editor:
  syntax_highlighting: true   # a comment
  tags: [one, two, three]
```

```toml
[editor]
syntax_highlighting = true
```

## Fences that must NOT be highlighted

An unknown language:

```frobnicate
fn main() { this should be plain }
```

A bare fence:

```
fn main() { this should be plain too }
```

An indented block (no fence, so no language):

    fn main() { plain as well }

## Info strings with metadata

The label row shows the whole info string; only the first token picks the
grammar, so both of these highlight as Rust.

```rust,ignore
fn ignored_by_rustdoc() {}
```

```rust {1,3-4}
fn with_line_hints() {}
```

## Edge cases

Non-ASCII inside tokens — the token boundaries must line up with the glyphs,
not drift right after the multi-byte characters:

```rust
fn main() {
    let greeting = "héllo, 世界 🎉";
    // a comment with émoji 🚀 and accents
    let n = 3.14159;
}
```

A block comment spanning several lines:

```rust
/* this comment
   continues across
   three lines */
fn after() {}
```

An unterminated string — everything below it should turn into string color
and stay readable:

```rust
fn main() {
    let s = "unterminated;
    let t = 2;
}
```

Inline `code spans` are never highlighted — only fenced blocks are.
