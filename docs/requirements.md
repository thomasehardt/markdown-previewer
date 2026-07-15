# markdown-previewer: Requirements

This is the living requirements list for `mdp`, retro-fitted from the project's
founding conversation and kept current as phases land. It is the contract the
design (`docs/design.md`) and implementation are measured against.

Status legend: **Done** (implemented and verified) - **In progress** (being
built now) - **Planned** (designed, not yet built). The Phase column maps each
requirement to the implementation plan in `docs/design.md` section 9.

## Functional requirements

| ID | Requirement | Phase | Status |
|---|---|---|---|
| FR1 | Render Markdown: CommonMark plus GFM (tables, task lists, strikethrough, autolinks, footnotes, alerts/admonitions). | 1 | Done |
| FR2 | Render math (KaTeX): inline `$...$` and display `$$...$$`. | 2 | Done |
| FR3 | Render diagrams (Mermaid) from fenced `mermaid` code blocks. | 2 | Done |
| FR4 | Syntax-highlight fenced code blocks. | 2 | Done |
| FR5 | Launch from the CLI against one or more files, opening straight into a rendered preview. | 1 | Done |
| FR6 | The preview is read-only (no editing affordances). | 1 | Done |
| FR7 | A read-only "view source" toggle showing the raw Markdown. | 4 | Done (button or press `s`) |
| FR8 | Export to a single self-contained HTML file (offline, diagrams and math preserved). | 6 | Done (renders on open; static-SVG freezing is a later option) |
| FR9 | Export to PDF (offline, diagrams and math preserved, paginated with clean breaks). | 7 | Done (macOS): paged.js reflows the capture into real page boxes (proper `@page` margins, no element split across a boundary), which `mdp-core::pdf` then slices into pages. Always renders for print - forced light scheme, white page, ~10pt body (the capture maps 1 CSS px to 1 pt, so the 16px screen base is scaled down), ~0.5in margins - regardless of the preview/config scheme. Verified end-to-end with math + Mermaid. Caveat: a Mermaid diagram is an unbreakable block, so its page can be preceded by a partly-empty one and the count can vary ~1 page across exports; text-only docs are deterministic. Windows/Linux planned. |
| FR10 | Watch tracked file(s); on change, show a non-intrusive reload prompt. Never auto-swap the reader's view. | 5 | Done (parent-dir watch, ~300ms debounce, banner; `--no-watch`) |
| FR11 | Instant launch feedback: a window/tab with a loading state appears immediately, then content swaps in. | 1 | Done (spinner shell) |
| FR12 | Callable by other tools (including Claude Code) to display Markdown headlessly (serve on an ephemeral port; print the URL). | 3 | Done |
| FR13 | Multiple display frontends over one shared core: native webview (default), local server + browser, headless export. | 1/3/6 | webview + serve Done (with webview->serve fallback); export Planned |
| FR14 | Customizable branding via a CSS token override file (`--theme <path>`), with a config file able to set a standing default theme. | 2 | Done (`--theme` + `config.toml`, flag > config > default) |
| FR15 | Color scheme control: auto (follow OS), light, dark. | 1 | Done (plumbed; `--scheme`) |
| FR16 | Accept multiple input files. | 1/8 | Done (file switcher bar; index routing; all watched) |
| FR17 | In-preview switcher for theme, color scheme, and reading width, remembered across sessions. | 8 | Done (default/sepia/slate themes; localStorage) |
| FR18 | Table-of-contents sidebar generated from headings, toggleable. | 8 | Done (Contents toggle; shown only when headings exist; remembered) |
| FR19 | Selectable PDF page size (US Letter or A4). | post-8 | Done: `--page-size letter\|a4` flag and `page_size` config key (flag > config > letter). Verified A4/Letter via `pdfinfo`. |
| FR20 | Export to PDF from the preview UI (not just the CLI). | post-8 | Done (macOS): a toolbar Export button with a page-size picker prompts for a save location and shells out to `mdp export`. Shown only under a native host (hidden in serve/browser). Click path pending manual confirmation. |
| FR21 | Launching the native preview returns to the shell immediately, prints nothing, and the window survives the terminal closing. | post-8 | Done (macOS): `mdp` re-execs a detached child (`setsid`, stdio to `/dev/null`) and the parent exits at once; the child carries `MDP_DETACHED=1` and does not re-detach (set it to force foreground). Re-exec, not bare `fork`, so AppKit/WKWebView init after an `exec`. Verified: parent returns in ~0.4s with no output; child has no controlling tty (`ps` TTY `??`), reparented to launchd. `serve` intentionally stays in the foreground. |

## Non-functional requirements

| ID | Requirement | Status |
|---|---|---|
| NFR1 | Offline only at runtime: zero network calls; all rendering assets embedded in the binary. | Done for shipped scope; enforced by an offline-URL scan test |
| NFR2 | Cross-platform: macOS, Linux, Windows. | macOS verified end to end; Linux/Windows build + test in CI (GitHub Actions matrix), not yet exercised at runtime |
| NFR3 | Single binary where the OS allows (macOS, Windows). Linux carries a documented `libwebkit2gtk` runtime dependency with graceful fallback to the serve frontend. | Done (automatic webview->serve fallback wired); Linux single-binary caveat documented |
| NFR4 | Fast launch: something visibly happens immediately on start. | Done (instant spinner) |
| NFR5 | MIT licensed; every shipped dependency license-compatible (MIT/Apache-2.0/BSD/ISC/CC0/Unicode/OFL). | Done; `cargo-deny` CI gate Planned |
| NFR6 | Implemented in Rust with a pinned toolchain (`rust-toolchain.toml`) for reproducible builds. | Done (Rust 1.97.0) |
| NFR7 | Reasonable 100% test coverage on core logic (skip trivial glue like getters/setters); the coverage gate runs on `mdp-core`. | Done (~96% line/region on `mdp-core`, clean measurement; the few uncovered lines are a packaging `panic!`, release-only asset arms, and defensive fallbacks). CI floor 90. |
| NFR8 | Elegant, modern visual design. Neutral default theme; no company branding baked into the shipped tool. | Done (semantic-token theme, neutral default) |
| NFR9 | Security: untrusted Markdown HTML is sanitized (ammonia allowlist); no script execution from document content; read-only. | Done (sanitization corpus test) |
| NFR10 | No Docker. User-local, reproducible install (rustup toolchain + Cargo). | Done |
| NFR11 | Repo hygiene: no large or binary asset blobs committed to git; third-party front-end assets are fetched by a pinned, checksummed vendor script. | Done (`scripts/vendor-assets.sh`, gitignored assets) |
| NFR12 | Maintained documentation: this requirements list, general user docs, power-user docs, and an always-current README. | Ongoing |
| NFR13 | Authoring style for all project prose: straight quotes and regular hyphens only, American spelling. | Ongoing |

## Deferred tasks (backlog)

Tracked work that is deliberately not done yet:

1. **Windows and Linux runtime support.** CI now builds and tests the workspace
   on Linux and Windows, but the webview/serve frontends are not yet exercised
   at runtime there (Linux needs libwebkit2gtk; native PDF is macOS-only).
2. **Freeze diagrams to static SVG** in HTML/PDF export (needs a browser engine
   at export time) so an export does not depend on JS running on open.
3. **Test documentation code snippets** as part of the build.
4. **Distributable install**: prebuilt cross-platform binaries (e.g. via
   cargo-dist and GitHub Releases) and a Homebrew formula, so users need not
   build from source. Depends on the Windows/Linux work. `scripts/install.sh`
   (`cargo install` from source) covers local installs today.

## How this list is maintained

- Every phase updates the Status/Phase columns here as items move Planned -> In
  progress -> Done.
- New requirements get a new FR/NFR id (ids are stable and never reused).
- A requirement is only marked **Done** after it is implemented AND verified
  (build, tests, and where relevant a real end-to-end exercise), consistent with
  the project's "verify before claiming complete" rule.
