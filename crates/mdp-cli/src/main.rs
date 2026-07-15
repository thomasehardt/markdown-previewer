//! The `mdp` binary.
//!
//! It parses the "subcommand-or-default-path" surface (design section 6) and
//! dispatches to a frontend. In Phase 1 the default (subcommand-free) path and
//! the `view` subcommand open the native webview preview (feature `webview`);
//! `serve` and `export` are wired into the clap surface but their frontends
//! land in later phases, so they report that plainly instead of pretending.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use mdp_core::{render_document, ColorScheme, DocSource, NavItem, RenderOptions, RenderedDoc};

mod config;
use config::Config;

#[derive(Parser)]
#[command(name = "mdp", version, about = "An elegant, fast Markdown previewer.")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Markdown file(s) to preview (the default, subcommand-free path).
    #[arg(value_name = "FILE", global = true)]
    files: Vec<PathBuf>,

    /// Theme CSS file layered over the built-in neutral default.
    #[arg(long, value_name = "PATH", global = true)]
    theme: Option<PathBuf>,

    /// Color scheme (overrides config; unset follows config then the OS).
    #[arg(long, value_enum, global = true)]
    scheme: Option<Scheme>,

    /// Page size for PDF export (overrides config; unset follows config then
    /// letter). Has no effect on HTML export or the preview.
    #[arg(long = "page-size", value_enum, global = true)]
    page_size: Option<PageSize>,

    /// Disable file watching (no reload prompts). Wired for later phases.
    #[arg(long, global = true)]
    no_watch: bool,

    /// Open showing the raw source instead of the rendered view. Wired for
    /// later phases.
    #[arg(long, global = true)]
    view_source_default: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Open a preview (native webview or local server).
    View {
        #[arg(value_name = "FILE")]
        files: Vec<PathBuf>,
        /// Which frontend to use.
        #[arg(long, value_enum, default_value_t = Frontend::Webview)]
        frontend: Frontend,
    },
    /// Serve the preview over a local HTTP server (later phase).
    Serve {
        #[arg(value_name = "FILE")]
        files: Vec<PathBuf>,
        /// Port to bind (0 picks an ephemeral port).
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Do not auto-open a browser; just print the URL.
        #[arg(long)]
        no_open: bool,
    },
    /// Export a self-contained HTML (or, later, PDF) file (later phase).
    Export {
        #[arg(value_name = "FILE")]
        file: PathBuf,
        /// Output path (`.html` today; `.pdf` in a later phase).
        #[arg(long = "to", value_name = "OUT")]
        to: PathBuf,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Scheme {
    Auto,
    Light,
    Dark,
}

/// Paper size for PDF export. Values are the standard portrait dimensions.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PageSize {
    Letter,
    A4,
}

impl PageSize {
    /// Portrait `(width, height)` in PDF points (72pt = 1in). Width is `f64` for
    /// the capture window size; height is `f32` for pagination.
    #[cfg_attr(not(feature = "webview"), allow(dead_code))]
    pub(crate) fn dimensions_pt(self) -> (f64, f32) {
        match self {
            // 8.5 x 11 in.
            PageSize::Letter => (612.0, 792.0),
            // 210 x 297 mm.
            PageSize::A4 => (595.28, 841.89),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Frontend {
    Webview,
    Serve,
}

/// The resolved action after parsing, carrying any per-frontend options.
enum Action {
    Webview,
    Serve { port: u16, open: bool },
}

impl From<Scheme> for ColorScheme {
    fn from(s: Scheme) -> Self {
        match s {
            Scheme::Auto => ColorScheme::Auto,
            Scheme::Light => ColorScheme::Light,
            Scheme::Dark => ColorScheme::Dark,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = Config::load();

    // Resolve options with precedence CLI flag > config.toml > built-in default.
    let theme = config::resolve_theme(cli.theme.clone(), config.theme.clone());
    let scheme: ColorScheme = config::resolve_scheme(cli.scheme, config.scheme).into();
    let page_size = config::resolve_page_size(cli.page_size, config.page_size);

    // The theme is read once (it does not change during a session) and is shared
    // by the export and preview paths.
    let extra_theme_css = match &theme {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(css) => Some(css),
            Err(e) => {
                eprintln!("mdp: cannot read theme {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };

    // Export is a headless one-shot: render and write a file, no display or watch.
    if let Some(Command::Export { file, to }) = &cli.command {
        return run_export(
            file,
            to,
            scheme,
            extra_theme_css,
            cli.view_source_default,
            page_size,
        );
    }

    // Otherwise resolve the target files and the display action. Multiple files
    // are previewed with a file switcher; a single file is shown directly.
    let (files, action) = match &cli.command {
        Some(Command::View { files, frontend }) => {
            let action = match frontend {
                Frontend::Webview => Action::Webview,
                // `view --frontend serve` uses an ephemeral port and opens the
                // browser (the `serve` subcommand is where you tune those).
                Frontend::Serve => Action::Serve {
                    port: 0,
                    open: true,
                },
            };
            (files.clone(), action)
        }
        Some(Command::Serve {
            files,
            port,
            no_open,
        }) => (
            files.clone(),
            Action::Serve {
                port: *port,
                open: !*no_open,
            },
        ),
        // Export was handled above; None (and any future command) lands here.
        _ => (cli.files.clone(), Action::Webview),
    };

    // Expand directory arguments to the Markdown files inside (one level,
    // sorted), so `mdp docs/` -- or a glob that includes folders -- opens each
    // file as a switchable tab. Plain file arguments are kept as given.
    let files = expand_inputs(&files);

    let Some(first) = files.first() else {
        eprintln!("mdp: no input file given. Try `mdp <FILE>` or `mdp --help`.");
        return ExitCode::from(2);
    };
    // Confirm the first (initially-shown) file is readable up front, so failure
    // is a clear early error rather than a blank window. Each file is re-read on
    // render, so a reload picks up on-disk edits.
    if let Err(e) = std::fs::read_to_string(first) {
        eprintln!("mdp: cannot read {}: {e}", first.display());
        return ExitCode::FAILURE;
    }

    // Watching drives the reload banner; watch every previewed file.
    let watch_paths: Vec<PathBuf> = if cli.no_watch {
        Vec::new()
    } else {
        files.clone()
    };

    let source: Arc<dyn DocSource> = Arc::new(RenderSpec {
        files: files.clone(),
        scheme,
        extra_theme_css,
        view_source: cli.view_source_default,
    });

    match action {
        // The webview frontend also gets the source paths and theme so its in-app
        // Export action can shell out to `mdp export` for the shown file.
        Action::Webview => open_webview(&source, &files, theme.as_deref(), &watch_paths),
        Action::Serve { port, open } => open_serve(&source, &watch_paths, port, open),
    }
}

/// The inputs a frontend needs to (re)render the document. Cheap to clone; each
/// `renderer()` call produces a fresh closure so a frontend can be handed one
/// and the fallback path can build another.
struct RenderSpec {
    files: Vec<PathBuf>,
    scheme: ColorScheme,
    extra_theme_css: Option<String>,
    view_source: bool,
}

impl DocSource for RenderSpec {
    fn count(&self) -> usize {
        self.files.len()
    }

    /// Render the `index`-th file from disk (read per call so a reload reflects
    /// on-disk edits; a read failure renders a small error page). When there are
    /// multiple files, a file-nav bar is built with the frontend's switch URLs.
    fn render(&self, index: usize, href: &dyn Fn(usize) -> String) -> RenderedDoc {
        let file = &self.files[index];
        let markdown = std::fs::read_to_string(file)
            .unwrap_or_else(|e| format!("# Cannot read file\n\n`{}`: {e}\n", file.display()));
        let file_nav = if self.files.len() >= 2 {
            self.files
                .iter()
                .enumerate()
                .map(|(j, f)| NavItem {
                    label: file_label(f),
                    href: href(j),
                    current: j == index,
                })
                .collect()
        } else {
            Vec::new()
        };
        let opts = RenderOptions {
            title: file.file_name().map(|n| n.to_string_lossy().into_owned()),
            color_scheme: self.scheme,
            extra_theme_css: self.extra_theme_css.clone(),
            view_source_default: self.view_source,
            show_chrome: true,
            file_nav,
            page_dimensions: None,
        };
        render_document(&markdown, &opts)
    }
}

/// A short label for a file path (the file name, or the path if it has none).
fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Expand directory arguments to the Markdown files they contain (one level,
/// sorted); plain file arguments are kept as-is. Lets `mdp docs/` and globs
/// that include folders open every file rather than erroring on a directory.
fn expand_inputs(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        if path.is_dir() {
            match std::fs::read_dir(path) {
                Ok(entries) => {
                    let mut markdown: Vec<PathBuf> = entries
                        .filter_map(|entry| entry.ok().map(|e| e.path()))
                        .filter(|p| p.is_file() && is_markdown(p))
                        .collect();
                    markdown.sort();
                    if markdown.is_empty() {
                        eprintln!("mdp: no Markdown files in {}", path.display());
                    }
                    out.extend(markdown);
                }
                Err(e) => eprintln!("mdp: cannot read directory {}: {e}", path.display()),
            }
        } else {
            out.push(path.clone());
        }
    }
    out
}

/// Whether a path has a Markdown-ish extension.
fn is_markdown(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("md" | "markdown" | "mdown" | "mkd" | "mkdn" | "mdwn")
    )
}

/// Headless export (design section 4.4/4.5). `.html` writes a self-contained
/// page (all CSS/JS/fonts inlined; math and diagrams render on open). `.pdf`
/// captures a rendered page to PDF (macOS today; see `export_pdf_file`).
fn run_export(
    file: &Path,
    out: &Path,
    scheme: ColorScheme,
    extra_theme_css: Option<String>,
    view_source: bool,
    page_size: PageSize,
) -> ExitCode {
    let ext = out
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => export_pdf_file(file, out, scheme, extra_theme_css, page_size),
        "html" | "htm" => {
            let markdown = match std::fs::read_to_string(file) {
                Ok(md) => md,
                Err(e) => {
                    eprintln!("mdp: cannot read {}: {e}", file.display());
                    return ExitCode::FAILURE;
                }
            };
            let opts = RenderOptions {
                title: file.file_name().map(|n| n.to_string_lossy().into_owned()),
                color_scheme: scheme,
                extra_theme_css,
                view_source_default: view_source,
                show_chrome: true,
                file_nav: Vec::new(),
                page_dimensions: None,
            };
            let doc = render_document(&markdown, &opts);
            match std::fs::write(out, &doc.html) {
                Ok(()) => {
                    eprintln!("mdp: wrote {}", out.display());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("mdp: cannot write {}: {e}", out.display());
                    ExitCode::FAILURE
                }
            }
        }
        other => {
            eprintln!(
                "mdp: unsupported export target .{other} ({}); use .html or .pdf.",
                out.display()
            );
            ExitCode::from(2)
        }
    }
}

/// CSS appended when rendering for PDF capture. The capture is a fixed-width
/// page, so anything that scrolls horizontally on screen (long code lines, wide
/// tables) would clip at the page edge; wrap it instead. The leading `html`
/// raises specificity so these rules win over the shell stylesheet regardless of
/// where the extra CSS is injected in the page.
#[cfg(feature = "webview")]
const PAGED_EXPORT_CSS: &str = "\
/* mdp PDF export: wrap wide content so nothing is clipped at the page edge. */\n\
html .mdp-document pre,\n\
html .mdp-document pre code { white-space: pre-wrap; overflow-wrap: anywhere; }\n\
html .mdp-document table { display: table; width: 100%; table-layout: fixed; }\n\
html .mdp-document th,\n\
html .mdp-document td { overflow-wrap: anywhere; }\n";

/// Append the paged-export CSS to any theme CSS already in effect.
#[cfg(feature = "webview")]
fn with_paged_css(theme_css: Option<String>) -> String {
    match theme_css {
        Some(mut css) => {
            css.push('\n');
            css.push_str(PAGED_EXPORT_CSS);
            css
        }
        None => PAGED_EXPORT_CSS.to_string(),
    }
}

/// Render `file` without interactive chrome and capture it to a PDF at `out`.
#[cfg(feature = "webview")]
fn export_pdf_file(
    file: &Path,
    out: &Path,
    _scheme: ColorScheme,
    extra_theme_css: Option<String>,
    page_size: PageSize,
) -> ExitCode {
    let markdown = match std::fs::read_to_string(file) {
        Ok(md) => md,
        Err(e) => {
            eprintln!("mdp: cannot read {}: {e}", file.display());
            return ExitCode::FAILURE;
        }
    };
    let (width_pt, height_pt) = page_size.dimensions_pt();
    let opts = RenderOptions {
        title: file.file_name().map(|n| n.to_string_lossy().into_owned()),
        // A PDF is for printing documentation, so it always renders light on a
        // white page regardless of the preview/config scheme (the in-app Export
        // button forwards the live scheme, which may be dark). `_scheme` is
        // ignored here on purpose.
        color_scheme: ColorScheme::Light,
        extra_theme_css: Some(with_paged_css(extra_theme_css)),
        view_source_default: false,
        show_chrome: false,
        file_nav: Vec::new(),
        // paged.js reflows to the paper box before capture (design 4.5).
        page_dimensions: Some((width_pt as f32, height_pt)),
    };
    let doc = render_document(&markdown, &opts);
    match mdp_webview::export_pdf(&doc, out, width_pt, height_pt) {
        Ok(()) => {
            eprintln!("mdp: wrote {}", out.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("mdp: PDF export failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Without the `webview` feature there is no engine to capture a PDF.
#[cfg(not(feature = "webview"))]
fn export_pdf_file(
    file: &Path,
    out: &Path,
    _scheme: ColorScheme,
    _extra_theme_css: Option<String>,
    _page_size: PageSize,
) -> ExitCode {
    let _ = (file, out);
    eprintln!("mdp: PDF export requires the `webview` feature (macOS).");
    ExitCode::from(2)
}

/// Re-launch this process fully detached from the controlling terminal, so a
/// bare `mdp <file>` returns to the shell at once, prints nothing, and leaves
/// the preview window open even after the terminal is closed.
///
/// Returns `Ok(true)` in the original (parent) process once the detached child
/// is spawned - the caller should then exit. Returns `Ok(false)` when this call
/// is already the detached child (marked by `MDP_DETACHED`), i.e. run the GUI
/// here.
///
/// We re-exec instead of a bare `fork()`: AppKit / WKWebView must not be
/// initialized across a `fork()` without an intervening `exec()`, so the child
/// starts from a clean binary image and re-runs `main`. `setsid()` in the child
/// puts it in a new session with no controlling terminal, which is what makes it
/// survive the terminal hangup; stdio is redirected to `/dev/null` so nothing
/// the window's engine might emit ever reaches the shell.
#[cfg(all(feature = "webview", target_os = "macos"))]
fn detach_from_terminal() -> std::io::Result<bool> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    // The detached child carries this marker; when we see it, do not fork again.
    const DETACH_ENV: &str = "MDP_DETACHED";
    if std::env::var_os(DETACH_ENV).is_some() {
        return Ok(false);
    }

    let exe = std::env::current_exe()?;
    // Forward the original arguments verbatim (skip argv[0]); cwd and the rest
    // of the environment are inherited by default, so relative file paths and
    // config discovery behave identically in the child.
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();

    let mut cmd = Command::new(exe);
    cmd.args(args)
        .env(DETACH_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Run in the child after fork, before exec: start a new session so the child
    // has no controlling terminal and closing the shell will not SIGHUP it.
    // SAFETY: `setsid` is async-signal-safe and touches no shared state; the
    // closure allocates nothing.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn()?;
    Ok(true)
}

/// Dispatch to the native webview frontend when the feature is compiled in.
///
/// `show` runs the event loop and only returns on an error (the process
/// otherwise exits from within the loop when the window closes). If the webview
/// cannot initialize (design risk #5: e.g. a missing Linux webkit runtime), we
/// automatically fall back to the local-server frontend when it is available.
#[cfg(feature = "webview")]
fn open_webview(
    source: &Arc<dyn DocSource>,
    files: &[PathBuf],
    theme: Option<&Path>,
    watch: &[PathBuf],
) -> ExitCode {
    // Hand the preview off to a detached copy of ourselves so the shell prompt
    // comes straight back and the window survives the terminal closing. macOS
    // only (the sole verified GUI target); on failure we just run in-place.
    #[cfg(target_os = "macos")]
    match detach_from_terminal() {
        // Parent: the detached child owns the window now; return to the shell.
        Ok(true) => return ExitCode::SUCCESS,
        // We are the detached child (or detaching is disabled): open here.
        Ok(false) => {}
        Err(e) => eprintln!("mdp: could not detach ({e}); running in the foreground."),
    }

    match mdp_webview::show(source.clone(), watch, files, theme) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mdp: {e}");
            #[cfg(feature = "serve")]
            {
                eprintln!("mdp: falling back to the local-server frontend.");
                open_serve(source, watch, 0, true)
            }
            #[cfg(not(feature = "serve"))]
            {
                ExitCode::FAILURE
            }
        }
    }
}

/// When built without the `webview` feature, prefer the server frontend if it is
/// present; otherwise there is no way to display.
#[cfg(not(feature = "webview"))]
fn open_webview(
    source: &Arc<dyn DocSource>,
    files: &[PathBuf],
    theme: Option<&Path>,
    watch: &[PathBuf],
) -> ExitCode {
    let _ = (files, theme);
    #[cfg(feature = "serve")]
    {
        eprintln!("mdp: built without the `webview` feature; using the local server.");
        open_serve(source, watch, 0, true)
    }
    #[cfg(not(feature = "serve"))]
    {
        let _ = (source, watch);
        eprintln!("mdp: built without a display frontend (webview or serve).");
        ExitCode::from(2)
    }
}

/// Dispatch to the local-server frontend when the feature is compiled in.
#[cfg(feature = "serve")]
fn open_serve(
    source: &Arc<dyn DocSource>,
    watch: &[PathBuf],
    port: u16,
    open_browser: bool,
) -> ExitCode {
    match mdp_serve::serve(source.clone(), watch, port, open_browser) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mdp: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Fallback when the binary is built without the `serve` feature.
#[cfg(not(feature = "serve"))]
fn open_serve(
    source: &Arc<dyn DocSource>,
    _watch: &[PathBuf],
    _port: u16,
    _open_browser: bool,
) -> ExitCode {
    let _ = source;
    eprintln!("mdp: built without the `serve` feature.");
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// clap's own consistency check: catches conflicting flags, bad defaults,
    /// duplicate names, etc. at test time rather than at first run.
    #[test]
    fn cli_definition_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn page_size_dimensions_are_standard() {
        assert_eq!(PageSize::Letter.dimensions_pt(), (612.0, 792.0));
        let (w, h) = PageSize::A4.dimensions_pt();
        assert!((w - 595.28).abs() < 0.01, "A4 width {w}");
        assert!((h - 841.89).abs() < 0.01, "A4 height {h}");
    }

    #[cfg(feature = "webview")]
    #[test]
    fn paged_css_appends_and_preserves_theme() {
        // With no theme, the paged CSS stands alone.
        let only = with_paged_css(None);
        assert!(only.contains("white-space: pre-wrap"));
        assert_eq!(only, PAGED_EXPORT_CSS);

        // With a theme, the theme comes first and the paged rules follow.
        let combined = with_paged_css(Some(":root{--accent:#abc}".to_string()));
        assert!(combined.starts_with(":root{--accent:#abc}"));
        assert!(combined.contains("white-space: pre-wrap"));
        // The wrap rules are `html`-qualified so they win over the shell CSS.
        assert!(combined.contains("html .mdp-document pre"));
    }

    #[test]
    fn help_renders() {
        // Rendering long help exercises the whole command tree without a TTY.
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("view"));
        assert!(help.contains("serve"));
        assert!(help.contains("export"));
        assert!(help.contains("--theme"));
        assert!(help.contains("--scheme"));
    }

    #[test]
    fn bare_file_is_the_default_path() {
        let cli = Cli::try_parse_from(["mdp", "README.md"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.files, vec![PathBuf::from("README.md")]);
        // No --scheme given: the flag is None and resolves to auto.
        assert!(cli.scheme.is_none());
        assert!(matches!(
            config::resolve_scheme(cli.scheme, None),
            Scheme::Auto
        ));
    }

    #[test]
    fn multiple_files_accepted_on_default_path() {
        let cli = Cli::try_parse_from(["mdp", "a.md", "b.md"]).unwrap();
        assert_eq!(cli.files.len(), 2);
    }

    #[test]
    fn global_flags_parse() {
        let cli =
            Cli::try_parse_from(["mdp", "--scheme", "dark", "--theme", "t.css", "x.md"]).unwrap();
        assert!(matches!(cli.scheme, Some(Scheme::Dark)));
        assert_eq!(cli.theme, Some(PathBuf::from("t.css")));
        let cli =
            Cli::try_parse_from(["mdp", "--no-watch", "--view-source-default", "x.md"]).unwrap();
        assert!(cli.no_watch);
        assert!(cli.view_source_default);
    }

    #[test]
    fn view_subcommand_defaults_to_webview_frontend() {
        let cli = Cli::try_parse_from(["mdp", "view", "x.md"]).unwrap();
        match cli.command {
            Some(Command::View { files, frontend }) => {
                assert_eq!(files, vec![PathBuf::from("x.md")]);
                assert_eq!(frontend, Frontend::Webview);
            }
            _ => panic!("expected view subcommand"),
        }
    }

    #[test]
    fn view_frontend_serve_is_selectable() {
        let cli = Cli::try_parse_from(["mdp", "view", "--frontend", "serve", "x.md"]).unwrap();
        match cli.command {
            Some(Command::View { frontend, .. }) => assert_eq!(frontend, Frontend::Serve),
            _ => panic!("expected view subcommand"),
        }
    }

    #[test]
    fn serve_subcommand_parses_port_and_no_open() {
        let cli =
            Cli::try_parse_from(["mdp", "serve", "--port", "8080", "--no-open", "x.md"]).unwrap();
        match cli.command {
            Some(Command::Serve {
                files,
                port,
                no_open,
            }) => {
                assert_eq!(files, vec![PathBuf::from("x.md")]);
                assert_eq!(port, 8080);
                assert!(no_open);
            }
            _ => panic!("expected serve subcommand"),
        }
    }

    #[test]
    fn export_requires_to() {
        let ok = Cli::try_parse_from(["mdp", "export", "in.md", "--to", "out.html"]).unwrap();
        match ok.command {
            Some(Command::Export { file, to }) => {
                assert_eq!(file, PathBuf::from("in.md"));
                assert_eq!(to, PathBuf::from("out.html"));
            }
            _ => panic!("expected export subcommand"),
        }
        // Missing --to is a parse error, not a silent default.
        assert!(Cli::try_parse_from(["mdp", "export", "in.md"]).is_err());
    }

    #[test]
    fn bad_scheme_value_is_rejected() {
        assert!(Cli::try_parse_from(["mdp", "--scheme", "sepia", "x.md"]).is_err());
    }

    #[test]
    fn export_writes_self_contained_html() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.md");
        std::fs::write(&input, "# Title\n\nHello **world**.\n").unwrap();
        let out = dir.path().join("out.html");

        let _ = run_export(
            &input,
            &out,
            ColorScheme::Auto,
            None,
            false,
            PageSize::Letter,
        );

        let html = std::fs::read_to_string(&out).expect("export wrote the file");
        assert!(html.contains("<h1"), "rendered heading present");
        assert!(html.contains("Hello"), "body content present");
        // Self-contained: the shell makes no network references for a plain doc.
        assert!(!html.contains("http://") && !html.contains("https://"));
    }

    #[test]
    fn export_rejects_unknown_format() {
        // An unsupported target is rejected without writing anything. (PDF export
        // is exercised end to end via the binary, not here: it needs a GUI event
        // loop that cannot run inside the test harness.)
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.md");
        std::fs::write(&input, "# x\n").unwrap();
        let out = dir.path().join("out.rtf");

        let _ = run_export(
            &input,
            &out,
            ColorScheme::Auto,
            None,
            false,
            PageSize::Letter,
        );
        assert!(
            !out.exists(),
            "no output should be written for an unsupported target"
        );
    }

    #[test]
    fn expand_inputs_keeps_files_and_expands_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("b.md"), "b").unwrap();
        std::fs::write(root.join("a.md"), "a").unwrap();
        std::fs::write(root.join("note.txt"), "x").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();

        // A plain file argument is kept as given.
        let file = root.join("a.md");
        assert_eq!(
            expand_inputs(std::slice::from_ref(&file)),
            vec![file.clone()]
        );

        // A directory expands to its Markdown files, sorted; the .txt file and
        // the subdirectory are excluded.
        let names: Vec<String> = expand_inputs(&[root.to_path_buf()])
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.md".to_string(), "b.md".to_string()]);
    }

    #[test]
    fn is_markdown_matches_common_extensions() {
        assert!(is_markdown(Path::new("x.md")));
        assert!(is_markdown(Path::new("X.MARKDOWN")));
        assert!(!is_markdown(Path::new("x.txt")));
        assert!(!is_markdown(Path::new("x")));
    }
}
