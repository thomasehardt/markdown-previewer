# markdown-previewer (`mdp`)

An elegant, fast-launching Markdown previewer written in Rust. Point it at one
or more Markdown files and it opens straight into a rendered, read-only
preview. It is built both for a human at a terminal and to be called by other
tools that need to display Markdown.

The full technical design lives in [`docs/design.md`](docs/design.md); that
document is the source of truth. This README covers what exists today and how
to build and run it.

## Status

All seven phases of the [phased plan](docs/design.md) are implemented. The
display-agnostic core renders CommonMark plus GFM (tables, task lists,
strikethrough, autolinks, tag filtering, footnotes, and alerts) into a fully
self-contained HTML page, sanitized and styled by a neutral, token-based theme.
Fenced code is syntax-highlighted server-side (syntect), `$...$` and `$$...$$`
math is typeset with KaTeX, and `mermaid` fenced blocks render as diagrams --
all offline, with the KaTeX and Mermaid assets injected only when a document
actually uses them. A `--theme` file and an optional `config.toml` customize the
look. The default `mdp <FILE>` path opens the page read-only in a native window
(wry + tao) behind an instant spinner shell. The local-server frontend
(`mdp serve`) serves the same page on a loopback port for headless or
browser-based use, printing the URL to stdout; the webview automatically falls
back to it if it cannot initialize. Either view offers a read-only "view source"
toggle (a button, or press `s`) that shows the raw Markdown. When the file
changes on disk, a non-intrusive banner offers to reload; it never swaps content
on its own (disable with `--no-watch`). Passing several files -- or a directory
(`mdp docs/`) -- adds a switcher bar (a tab per file) to move between them. And
`mdp export FILE --to out.html`
writes a single self-contained HTML file that opens offline in any browser,
while `--to out.pdf` captures a high-fidelity vector PDF, paginated to US Letter
or A4 (`--page-size`, macOS today). In the preview window an Export PDF button
does the same without leaving the app.

Platform status: macOS is verified end to end. Windows and Linux are designed
but not yet built or tested; on Linux the webview needs `libwebkit2gtk` at
runtime (with an automatic fallback to the local server), and native PDF export
is macOS-only for now.

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

First, vendor the offline front-end assets (KaTeX and Mermaid). This is a
one-time step (repeat only after a bump in `scripts/assets-manifest.toml`); it
fetches pinned, checksummed files into a gitignored tree. Only this build step
touches the network -- the built tool stays fully offline.

```sh
# fetch KaTeX + Mermaid into crates/mdp-core/assets/vendor/ (gitignored)
./scripts/vendor-assets.sh

# build the whole workspace
cargo build

# open a rendered, read-only preview in a native window (the default path)
cargo run -p mdp-cli -- README.md

# force a color scheme, or layer a custom theme over the default
cargo run -p mdp-cli -- --scheme dark README.md
cargo run -p mdp-cli -- --theme my-theme.css README.md
```

Without the vendor step the tool still builds and runs; math and diagrams
simply fall back to their source text until the assets are present.

## Install

To install the `mdp` command to `~/.cargo/bin` (needs a Rust toolchain via
rustup):

```sh
./scripts/install.sh
```

That vendors the offline assets and runs `cargo install --path crates/mdp-cli`.
Ensure `~/.cargo/bin` is on your PATH, then:

```sh
mdp <file.md>
```

Prebuilt cross-platform binaries and a Homebrew formula are a later step, tied
to the Windows/Linux support in the [requirements backlog](docs/requirements.md).

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

## Continuous integration

`.github/workflows/ci.yml` builds and tests on Linux, macOS, and Windows, and
runs `rustfmt`, `clippy -D warnings`, an `mdp-core` coverage floor, and a
`cargo-deny` license gate. The Linux jobs install `libwebkit2gtk` for the
webview frontend.

## Theming

Visual identity is a theme layer, not hardcoded style. The layout and prose CSS
reference only semantic tokens (`--accent`, `--text-body`, `--text-heading`,
`--surface-page`, `--surface-card`, `--font-body`, `--font-heading`,
`--font-mono`, `--space-*`, `--radius-*`). The tool ships exactly one built-in
theme: a neutral, elegant default using a permissive system font stack. A
`--theme <path.css>` file layers over that default, so overriding a few tokens
is a complete rebrand with no structural change. No company branding is bundled.
The preview also has a small settings toolbar to switch among the bundled themes
(default, sepia, slate), light/dark, and reading width live, remembering the
choice.

Persistent defaults live in an optional `config.toml` in the platform config
directory (`~/.config/mdp/config.toml` on Linux and macOS, `%APPDATA%\mdp\` on
Windows; override the location with `MDP_CONFIG_DIR`):

```toml
# paths are used as-is (no shell ~ expansion); prefer an absolute path
theme = "/home/you/.config/mdp/themes/mine.css"
scheme = "dark"
```

Resolution precedence is: CLI flag, then `config.toml`, then the built-in
default. See [`docs/power-user-guide.md`](docs/power-user-guide.md) for authoring
a theme and the full CLI reference.

## Offline and licensing

Hard constraint: no runtime network calls. Every asset is inlined into the
output (in release builds it is also embedded in the binary via rust-embed; in
dev it reads from disk for fast iteration). An automated test enforces this by
scanning generated HTML for external references.

The project is MIT licensed (see [`LICENSE`](LICENSE)). Dependencies are chosen
to be license-compatible; `comrak` carries a BSD-2-Clause notice specifically.
`Cargo.lock` is committed because `mdp` ships a binary and we pin the exact
dependency graph for reproducible builds.
