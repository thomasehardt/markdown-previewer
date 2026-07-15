# mdp user guide

`mdp` previews Markdown. Point it at a file and it opens a rendered, read-only
view in a native window. This guide covers everyday use. For theming, the config
file, and the full CLI, see the [power-user guide](power-user-guide.md).

## Installing it

`mdp` is a Rust program (it needs a Rust toolchain via rustup). From a checkout,
install the `mdp` command to `~/.cargo/bin`:

```sh
./scripts/install.sh
```

That fetches the offline math/diagram assets once (KaTeX and Mermaid, so the
tool works with no network afterward) and installs the binary. Make sure
`~/.cargo/bin` is on your PATH.

To build without installing:

```sh
./scripts/vendor-assets.sh
cargo build --release        # produces target/release/mdp
```

If you skip the asset step, everything else still works; math and diagrams just
show their source text until you run it.

## Previewing a file

```sh
mdp notes.md
```

A window opens with a brief loading state, then your rendered document. The
preview is read-only: it is for reading and presenting, not editing.

Your shell prompt comes back right away. `mdp` hands the window off to a
background process and returns at once, printing nothing, so you can keep using
the same terminal. The preview stays open even if you close that terminal; to
dismiss it, close the window (or press Cmd-Q). This applies to the native
window; `mdp serve` still runs in the foreground so you can see its URL and stop
it with Ctrl-C.

Pass several files and switch between them with the file bar at the top of the
preview:

```sh
mdp intro.md setup.md
```

Or point at a folder to open every Markdown file inside it, one tab each:

```sh
mdp docs/
```

## Choosing light or dark

By default `mdp` follows your operating system's light/dark setting. To force
one:

```sh
mdp --scheme dark notes.md
mdp --scheme light notes.md
```

## Using a custom look

Point `--theme` at a CSS file to restyle the preview (colors, fonts, spacing)
without changing the content:

```sh
mdp --theme my-theme.css notes.md
```

To make a theme your standing default so you do not pass the flag every time,
set it in `config.toml` (see the power-user guide).

The preview also has a small settings toolbar in the top-right corner: switch
the built-in theme (default, sepia, slate), light/dark, and reading width on the
fly, and open a Contents sidebar to jump around long documents. Your choices are
remembered the next time you open a file.

## Viewing the raw Markdown

Click **View source** (top-right), or press `s`, to switch between the rendered
view and the raw Markdown. Both are read-only. Start in source view with
`--view-source-default`.

## Live updates

While a preview is open, `mdp` watches the file. When you save changes, a small
banner appears offering to reload. It never reloads on its own, so it will not
interrupt your reading. Turn watching off with `--no-watch`.

## Saving a standalone copy

Export a document to a single HTML file you can open anywhere, offline, or send
to someone:

```sh
mdp export notes.md --to notes.html
```

The file has everything embedded (styles, fonts, math, diagrams). On macOS you
can also export a PDF:

```sh
mdp export notes.md --to notes.pdf
```

A window appears briefly while it captures. The PDF is made for printing: it is
always a white page with dark, print-sized text, even if you are previewing in a
dark or tinted theme. Pick the paper size with `--page-size letter` (default) or
`--page-size a4`. PDF export on Windows and Linux is coming in a later version.

## What renders

- CommonMark plus GitHub-flavored Markdown: tables, task lists, strikethrough,
  autolinks, footnotes, and alert/admonition callouts (`> [!NOTE]`, `> [!WARNING]`, ...).
- Math with KaTeX: inline `$E = mc^2$` and display `$$ ... $$`.
- Diagrams with Mermaid: fenced ` ```mermaid ` blocks render as diagrams.
- Syntax-highlighted code blocks.

## Opening in a browser instead

Prefer your browser, or on a machine without a desktop window? Use `serve`:

```sh
mdp serve notes.md            # start a local server and open your browser
mdp serve --no-open notes.md  # just print the URL for you to open
```

It serves only on your own machine (`127.0.0.1`) and stops with Ctrl-C.

## Troubleshooting

- **Math or diagrams show as plain text.** The offline assets are not vendored
  yet. Run `./scripts/vendor-assets.sh` and rebuild.
- **A diagram shows an error box.** Mermaid could not parse that diagram's
  source; the rest of the document still renders. Check the diagram syntax.
- **`mdp: no input file given`.** Pass a file path: `mdp <FILE>`, or run
  `mdp --help`.
