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
| `--page-size <letter\|a4>` | Paper size for PDF export. Unset follows `config.toml`, then `letter`. No effect on HTML export or the preview. |
| `--no-watch` | Disable file watching and the reload banner. |
| `--view-source-default` | Open showing the read-only raw source instead of the rendered view (toggle in-page with the button or `s`). |

Status today: the default path and `view` open the native webview; `serve` (and
`view --frontend serve`) run the local server; and the webview automatically
falls back to the server if it cannot initialize (for example, a missing Linux
webkit runtime). `export` writes a self-contained `.html`, or a `.pdf` on macOS
(Windows/Linux PDF is not yet implemented). `--help` and `--version` work.

## Detached launch (macOS)

Opening the native window does not block the shell. On macOS, `mdp` re-execs
itself as a detached child (`setsid`, a new session with no controlling
terminal, stdio redirected to `/dev/null`) and the original process returns
immediately with no output. The window therefore outlives the terminal it was
launched from. The re-exec (rather than a bare `fork`) is deliberate: AppKit and
WKWebView must not be brought up across a `fork` without an intervening `exec`.

The child is marked with the environment variable `MDP_DETACHED=1` so it does
not detach a second time. Set `MDP_DETACHED=1` yourself to keep the preview in
the foreground - useful when you want the process tied to a script's lifetime,
or to read a diagnostic if the window fails to appear:

```sh
# run the window in the foreground (blocks until it closes)
MDP_DETACHED=1 mdp notes.md
```

Only the native window detaches. `serve` and `view --frontend serve` stay in the
foreground so their URL is visible and Ctrl-C stops them.

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
renders the document in a webview (laid out at the page width), waits for the
render-complete signal (so Mermaid and KaTeX, including web fonts, have
finished), then captures silently via `WKWebView.createPDF`. That capture is one
tall page, which `mdp` then paginates into page-height pages before writing.
Text stays selectable and diagrams stay vector-sharp (nothing is re-rasterized).
Long code lines and wide tables are wrapped for export so they never clip at the
page edge. Before capture, paged.js reflows the document into real page boxes, so
each page has proper margins and no callout, table, code block, or diagram is
split across a page break. A window appears briefly during capture. Windows and
Linux each need their own native path and are not yet implemented.

A PDF is meant to be printed, so it always renders for print regardless of the
preview or `config.toml` scheme: a white page with dark text (the light scheme is
forced), a ~10pt body (the on-screen 16px base would print oversized, since the
macOS capture maps 1 CSS px to 1 pt), and ~0.5in margins. Headings, code, and
math all scale from that base, so the whole document is a normal printed density.
The in-app Export button forwards the live preview scheme, but export overrides it
to light all the same.

One caveat: for a document that contains a Mermaid diagram, the exact page count
can vary by about a page between exports. A diagram is an unbreakable block, so it
is pushed whole to the next page when it does not fit, which can leave the page
before it partly empty; and the diagram's captured height depends slightly on
render timing. Text-only documents paginate deterministically.

Choose the paper size with `--page-size letter` (default) or `--page-size a4`,
or set `page_size` in `config.toml`.

You can also export from the preview window: the toolbar has an **Export PDF**
button with a page-size picker (shown only in the native app, not in a browser).
It prompts for a save location and writes the currently-shown file, matching the
active color scheme.

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
preview shows a file switcher bar. A directory argument (`mdp docs/`) expands to
the Markdown files inside it (one level, sorted), and a glob that includes
folders expands them too. Selecting a file renders it (by index over
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
page_size = "a4"   # PDF export paper size: letter (default) or a4
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
