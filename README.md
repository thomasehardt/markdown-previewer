# markdown-previewer (`mdp`)

An elegant, fast-launching Markdown previewer written in Rust. Point it at one
or more Markdown files and it opens straight into a rendered, read-only
preview. It is built both for a human at a terminal and to be called by other
tools that need to display Markdown.

The full technical design lives in [`docs/design.md`](docs/design.md); that
document is the source of truth. This README covers what exists today and how
to build and run it.

## Status

Phase 1 of the [phased plan](docs/design.md) is implemented: the
display-agnostic core renders CommonMark plus GFM (tables, task lists,
strikethrough, autolinks, tag filtering, footnotes, and alerts) into a fully
self-contained HTML page, sanitized and styled by a neutral, token-based theme.
The default `mdp <FILE>` path opens that page read-only in a native window
(wry + tao): the window shows an instant spinner shell, then swaps in the
rendered content once the webview serves it over an offline custom protocol.

Not yet implemented (later phases): syntax highlighting, math (KaTeX),
diagrams (Mermaid), the local-server frontend (`serve`), file watching, and
HTML/PDF export (`export`). Fenced code renders as plain, theme-styled
`<pre><code>` for now.

## Architecture

A display-agnostic **core** does all the load-bearing work; thin **frontends**
adapt its one deterministic product, a fully self-contained HTML string.

| Crate | Kind | Responsibility |
|---|---|---|
| `mdp-core` | lib | Markdown to self-contained HTML: parse (comrak), sanitize (ammonia), inline the token-based theme and app shell. The coverage target. |
| `mdp-cli` | bin (`mdp`) | clap argument parsing; dispatches to a frontend behind cargo features. The single shipped binary. |
| `mdp-webview` | lib (feature `webview`, default) | Native window frontend (wry + tao): serves the rendered page and its assets over an offline `mdprev://` custom protocol, behind an instant spinner shell. |
| `mdp-serve` | lib (feature `serve`) | Local HTTP server frontend (tiny_http + system browser). Phase 1 stub. |

The load-bearing contract is one function:

```rust
render_document(markdown: &str, opts: &RenderOptions) -> RenderedDoc
```

`RenderedDoc.html` is a complete HTML document with all CSS and JS inlined and
zero external references. Every frontend consumes that same string, so the
core-to-frontend boundary is enforced at compile time.

## Build and run

The toolchain is pinned by `rust-toolchain.toml` (Rust 1.97.0); rustup honors
it automatically inside this directory.

```sh
# build the whole workspace
cargo build

# open a rendered, read-only preview in a native window (the default path)
cargo run -p mdp-cli -- README.md

# force a color scheme, or layer a custom theme over the default
cargo run -p mdp-cli -- --scheme dark README.md
cargo run -p mdp-cli -- --theme my-theme.css README.md
```

## Test and coverage

Coverage concentrates in `mdp-core`, where the logic is a pure string-to-string
transform. Frontends are deliberately thin and excluded from the target.

```sh
# unit + integration + snapshot tests
cargo test -p mdp-core

# review and (re)create snapshots after intentional output changes
cargo insta test --accept -p mdp-core

# coverage summary for the core (also available as `cargo cov`)
cargo llvm-cov -p mdp-core --summary-only
```

The test suite covers: snapshot tests of the render pipeline over a feature
corpus (with the inlined asset blobs redacted so snapshots stay reviewable), an
offline-URL scan proving the assembled shell makes no network calls, an
adversarial sanitization corpus (script/`javascript:`/handler/iframe stripped
while task lists, alerts, footnotes, and code-language classes survive), and
`RenderOptions` behavior.

## Theming

Visual identity is a theme layer, not hardcoded style. The layout and prose CSS
reference only semantic tokens (`--accent`, `--text-body`, `--text-heading`,
`--surface-page`, `--surface-card`, `--font-body`, `--font-heading`,
`--font-mono`, `--space-*`, `--radius-*`). The tool ships exactly one built-in
theme: a neutral, elegant default using a permissive system font stack. A
`--theme <path.css>` file layers over that default, so overriding a few tokens
is a complete rebrand with no structural change. No company branding is bundled.

## Offline and licensing

Hard constraint: no runtime network calls. Every asset is inlined into the
output (in release builds it is also embedded in the binary via rust-embed; in
dev it reads from disk for fast iteration). An automated test enforces this by
scanning generated HTML for external references.

The project is MIT licensed (see [`LICENSE`](LICENSE)). Dependencies are chosen
to be license-compatible; `comrak` carries a BSD-2-Clause notice specifically.
`Cargo.lock` is committed because `mdp` ships a binary and we pin the exact
dependency graph for reproducible builds.
