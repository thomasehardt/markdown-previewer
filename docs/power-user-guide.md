# mdp power-user guide

Advanced usage: the full command surface, authoring themes, the config file, and
how the offline asset vendoring works. For everyday use see the
[user guide](user-guide.md); for the full architecture see
[the design](design.md).

## Command reference

```
mdp [OPTIONS] [FILE]...                 preview (default path: native webview)
mdp view  [FILE]... [--frontend ...]    explicit preview
mdp serve [FILE]... [--port N] [--no-open]   local server (loopback + browser)
mdp export <FILE> --to <out.html|.pdf>  write self-contained HTML, or PDF (macOS)
```

Global options (apply to every path):

| Option | Meaning |
|---|---|
| `--theme <PATH>` | Layer a theme CSS file over the built-in default. |
| `--scheme <auto\|light\|dark>` | Color scheme. Unset follows `config.toml`, then the OS. |
| `--no-watch` | Disable file watching and the reload banner. |
| `--view-source-default` | Open showing the read-only raw source instead of the rendered view (toggle in-page with the button or `s`). |

Status today: the default path and `view` open the native webview; `serve` (and
`view --frontend serve`) run the local server; and the webview automatically
falls back to the server if it cannot initialize (for example, a missing Linux
webkit runtime). `export` writes a self-contained `.html`, or a `.pdf` on macOS
(Windows/Linux PDF is not yet implemented). `--help` and `--version` work.

## Headless use (calling from another tool)

`mdp serve` is the display-agnostic path: it needs no OS webview and does not
block on a window. It binds `127.0.0.1` on an ephemeral port (`--port 0`, the
default), prints the URL to **stdout** (status goes to stderr), and serves the
rendered page until interrupted.

```sh
# start a server, print the URL, do not open a browser
mdp serve --no-open --port 0 GENERATED.md
#   -> http://127.0.0.1:53017/
```

A calling tool captures stdout to get the URL, then opens or embeds it. Routes:
`/` re-renders and serves the self-contained document; `/.mdp/reload` reports
whether the file changed on disk (the in-page shell polls it to offer a reload);
anything else is a 404.

## Live reload

By default `mdp` watches the input file. It watches the file's parent directory
(so it survives editors that save by writing a temp file and renaming over the
original), coalesces the save-event storm with a short debounce, and then shows
a dismissible "this file changed on disk" banner. It never swaps content on its
own: the reader clicks Reload, which re-renders from disk. Disable with
`--no-watch`. On network filesystems where events are unreliable, watching may
miss changes; reopen the file in that case.

## Export

`mdp export <FILE> --to out.html` renders once and writes a single,
self-contained HTML file: all CSS, JS, and fonts are inlined, so it opens
offline in any browser, and math and diagrams render on open via the bundled
libraries. The exported file is not watched and has no live reload. Diagrams are
not yet pre-frozen to static SVG (that would need a browser engine at export
time).

Exporting to a `.pdf` path captures a high-fidelity vector PDF on macOS: it
renders the document in a webview, waits for the render-complete signal (so
Mermaid and KaTeX, including web fonts, have finished), then captures silently
via `WKWebView.createPDF`. A window appears briefly during capture, and the
output is a single tall page (macOS `createPDF` does not paginate to Letter/A4).
Windows and Linux each need their own native path and are not yet implemented.

## Authoring a theme

The layout and prose CSS reference only **semantic tokens**, so a theme is just
a small CSS file that redefines the tokens you care about. Everything you do not
override inherits from the neutral default.

Core tokens:

| Token | Role |
|---|---|
| `--accent` | Links and accents. |
| `--text-body`, `--text-heading` | Body and heading text. |
| `--surface-page`, `--surface-card` | Page and raised-surface backgrounds. |
| `--font-body`, `--font-heading`, `--font-mono` | Font stacks. |
| `--space-*`, `--radius-*` | Spacing and corner-radius scale. |

A minimal theme:

```css
:root {
  --accent: #2f6f4f;
  --font-body: "Iowan Old Style", Georgia, serif;
}
```

Because a theme only changes token *values*, the rendered document structure is
identical with or without it (there is a test that guarantees this). Define both
light and dark in one theme via `prefers-color-scheme`, or scope overrides under
`:root[data-color-scheme="dark"]` to match the `--scheme` flag.

## Switching in the preview

The preview's settings toolbar (top-right) switches three things live, and
remembers each in `localStorage` so it persists across reloads and files:

- **Theme** - the bundled `default`, `sepia`, or `slate` (each a full token set
  covering light and dark via CSS `light-dark()`).
- **Scheme** - `auto` (follow the OS), `light`, or `dark`.
- **Width** - `comfortable`, `wide`, or `full`.
- **Contents** - a table-of-contents sidebar built from the document's headings
  (the toggle appears only when there are headings), remembered like the rest.

Each maps to a root attribute the theme CSS keys off (`data-mdp-theme`,
`data-color-scheme`, `data-mdp-width`); the attribute is dropped at its default,
so the base needs none. A custom `--theme` file still layers over whichever
built-in theme is active, and the toolbar is interactive chrome, so it is
omitted from exports.

## Multiple files

Pass several files (`mdp a.md b.md ...`, or `mdp serve a.md b.md`) and the
preview shows a file switcher bar. Selecting a file renders it (by index over
the frontend's URL: `?doc=N` for serve, `?n=N` over the webview's custom
protocol); all passed files are watched, so a change to any of them offers a
reload. Export still takes a single file.

## config.toml

Persistent defaults live in `config.toml` in the platform config directory:

- Linux and macOS: `~/.config/mdp/config.toml` (honors `XDG_CONFIG_HOME`)
- Windows: `%APPDATA%\mdp\config.toml`
- Override the directory with the `MDP_CONFIG_DIR` environment variable.

```toml
# paths are used as-is (no shell ~ expansion); prefer an absolute path
theme = "/home/you/.config/mdp/themes/night.css"
scheme = "dark"
```

Resolution precedence is **CLI flag > config.toml > built-in default**. Unknown
keys are rejected: a typo produces a warning and the tool falls back to defaults
rather than silently ignoring it. A missing or malformed config never blocks a
preview.

This is also how you keep a private, branded theme (for example a Sherpas theme)
outside the repo: put the CSS in your own space and point `config.toml` at it.

## Offline asset vendoring

Math (KaTeX) and diagrams (Mermaid) need bundled front-end assets. To keep the
repo free of large binary blobs, they are **not** committed. Instead:

- `scripts/vendor-assets.sh` fetches pinned versions into
  `crates/mdp-core/assets/vendor/` (gitignored).
- `scripts/assets-manifest.toml` pins the versions and sha256 of every file; the
  script verifies each download against it and fails loudly on a mismatch.
- The script also precomputes `katex.inlined.css` with every font as a base64
  `data:` URI, so the rendered page has zero external font references.
- `THIRD-PARTY-LICENSES.md` carries the bundled projects' license texts.

`rust-embed` reads these from disk in debug builds and folds them into the binary
in release builds, so the shipped tool is fully offline. To update a version,
edit `scripts/assets-manifest.toml` and re-run the script.

## Build, test, coverage

```sh
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo llvm-cov -p mdp-core --summary-only     # or: cargo cov
```

Coverage concentrates in `mdp-core` (the pure render pipeline); frontends are
thin glue and excluded from the target. Cargo features select the frontend:
`webview` (default) and `serve`.
