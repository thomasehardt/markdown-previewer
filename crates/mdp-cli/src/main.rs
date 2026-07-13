//! The `mdp` binary.
//!
//! It parses the "subcommand-or-default-path" surface (design section 6) and
//! dispatches to a frontend. In Phase 1 the default (subcommand-free) path and
//! the `view` subcommand open the native webview preview (feature `webview`);
//! `serve` and `export` are wired into the clap surface but their frontends
//! land in later phases, so they report that plainly instead of pretending.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use mdp_core::{ColorScheme, RenderOptions};

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

    // Resolve the target file and the action from whichever surface was used.
    // Phase 1/2 preview a single file; the surface accepts several so multi-file
    // support is additive later.
    let (file, action) = match &cli.command {
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
            (files.first().cloned(), action)
        }
        Some(Command::Serve {
            files,
            port,
            no_open,
        }) => (
            files.first().cloned(),
            Action::Serve {
                port: *port,
                open: !*no_open,
            },
        ),
        Some(Command::Export { file, to }) => {
            eprintln!(
                "mdp: export to {} is not implemented yet (design Phase 6).",
                to.display()
            );
            let _ = file;
            return ExitCode::from(2);
        }
        None => (cli.files.first().cloned(), Action::Webview),
    };

    let Some(file) = file else {
        eprintln!("mdp: no input file given. Try `mdp <FILE>` or `mdp --help`.");
        return ExitCode::from(2);
    };

    let doc = match render_file(&file, theme.as_deref(), scheme) {
        Ok(doc) => doc,
        Err(code) => return code,
    };

    match action {
        Action::Webview => open_webview(&doc),
        Action::Serve { port, open } => open_serve(&doc, port, open),
    }
}

/// Read a Markdown file and render it to the self-contained document, applying
/// the CLI's theme and color-scheme options. Returns the CLI exit code to use
/// on any I/O failure.
fn render_file(
    file: &Path,
    theme: Option<&Path>,
    scheme: ColorScheme,
) -> Result<mdp_core::RenderedDoc, ExitCode> {
    let markdown = std::fs::read_to_string(file).map_err(|e| {
        eprintln!("mdp: cannot read {}: {e}", file.display());
        ExitCode::FAILURE
    })?;

    let extra_theme_css = match theme {
        Some(theme_path) => Some(std::fs::read_to_string(theme_path).map_err(|e| {
            eprintln!("mdp: cannot read theme {}: {e}", theme_path.display());
            ExitCode::FAILURE
        })?),
        None => None,
    };

    let opts = RenderOptions {
        title: file.file_name().map(|n| n.to_string_lossy().into_owned()),
        color_scheme: scheme,
        extra_theme_css,
    };

    Ok(mdp_core::render_document(&markdown, &opts))
}

/// Dispatch to the native webview frontend when the feature is compiled in.
///
/// `show` runs the event loop and only returns on an error (the process
/// otherwise exits from within the loop when the window closes). If the webview
/// cannot initialize (design risk #5: e.g. a missing Linux webkit runtime), we
/// automatically fall back to the local-server frontend when it is available.
#[cfg(feature = "webview")]
fn open_webview(doc: &mdp_core::RenderedDoc) -> ExitCode {
    match mdp_webview::show(doc) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mdp: {e}");
            #[cfg(feature = "serve")]
            {
                eprintln!("mdp: falling back to the local-server frontend.");
                open_serve(doc, 0, true)
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
fn open_webview(doc: &mdp_core::RenderedDoc) -> ExitCode {
    #[cfg(feature = "serve")]
    {
        eprintln!("mdp: built without the `webview` feature; using the local server.");
        open_serve(doc, 0, true)
    }
    #[cfg(not(feature = "serve"))]
    {
        let _ = doc;
        eprintln!("mdp: built without a display frontend (webview or serve).");
        ExitCode::from(2)
    }
}

/// Dispatch to the local-server frontend when the feature is compiled in.
#[cfg(feature = "serve")]
fn open_serve(doc: &mdp_core::RenderedDoc, port: u16, open_browser: bool) -> ExitCode {
    match mdp_serve::serve(doc, port, open_browser) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mdp: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Fallback when the binary is built without the `serve` feature.
#[cfg(not(feature = "serve"))]
fn open_serve(doc: &mdp_core::RenderedDoc, _port: u16, _open_browser: bool) -> ExitCode {
    let _ = doc;
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
        let cli = Cli::try_parse_from(["mdp", "--no-watch", "--view-source-default", "x.md"])
            .unwrap();
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
}
