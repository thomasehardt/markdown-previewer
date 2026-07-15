//! Native webview frontend (wry + tao).
//!
//! Default frontend behind the `webview` feature. It displays `mdp-core`'s
//! self-contained HTML read-only in a native OS webview, served offline over a
//! custom protocol behind an instant spinner shell (design section 9 / 8.7).
//!
//! Live reload (design section 4.3): the document route re-renders from disk on
//! each request, and a background watcher flips a shared "changed" flag that the
//! reload endpoint reports. The in-page shell polls that endpoint and shows a
//! non-intrusive banner; the reader reloads on demand. The preview stays
//! read-only: sanitized HTML, no editing affordances, no navigation off origin.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use mdp_core::watch::{self, WatchHandle};
use mdp_core::{DocSource, RenderedDoc};

use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use wry::{
    http::{header::CONTENT_TYPE, Response},
    WebViewBuilder,
};

/// The custom protocol scheme that serves the preview and its assets offline.
const SCHEME: &str = "mdprev";

/// Path that serves the rendered document. The spinner navigates here.
const DOC_PATH: &str = "/doc";

/// The reload-signal endpoint the shell polls (design section 4.3).
const RELOAD_PATH: &str = "/.mdp/reload";

/// An in-app request the webview shell routes to the event loop over IPC.
enum ShowEvent {
    /// The user clicked "Export PDF" in the toolbar.
    Export(ExportRequest),
}

/// Open a native window and display `render`'s output read-only.
///
/// Re-renders on each document request (so a reload after a change shows fresh
/// content) and, when `watch_paths` is non-empty, runs a watcher that drives the
/// reload banner. `files` are the source paths (in switcher order) and `theme` is
/// the CSS theme in effect, both used by the in-app Export action to shell out to
/// `mdp export`. On success this runs the OS event loop and does not return until
/// the window closes; it returns `Err` only when the window or webview cannot be
/// created, so the CLI can fall back to another frontend.
pub fn show(
    source: Arc<dyn DocSource>,
    watch_paths: &[PathBuf],
    files: &[PathBuf],
    theme: Option<&Path>,
) -> Result<(), FrontendError> {
    // One render up front for the window title and instant spinner.
    let initial = source.render(0, &doc_href);
    let spinner_html = spinner_page(&initial.title);
    let title = window_title(&initial.title);

    let changed = Arc::new(AtomicBool::new(false));
    let _watch = start_watch(watch_paths, changed.clone());

    // Owned copies for the `'static` event-loop closures, plus a shared record of
    // which document is currently shown (for the Export action).
    let files: Vec<PathBuf> = files.to_vec();
    let theme: Option<PathBuf> = theme.map(Path::to_path_buf);
    let current_index = Arc::new(AtomicUsize::new(0));

    let event_loop = EventLoopBuilder::<ShowEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title(title)
        .with_inner_size(LogicalSize::new(1000.0, 800.0))
        .build(&event_loop)
        .map_err(|e| FrontendError::Window(e.to_string()))?;

    // The protocol handler is `Fn + 'static`; share the doc source and flag in.
    // It also records the shown document index so Export knows which file to use.
    let handler_source = source.clone();
    let handler_changed = changed.clone();
    let handler_index = current_index.clone();
    let ipc_proxy = proxy.clone();
    let builder = WebViewBuilder::new()
        .with_custom_protocol(SCHEME.to_string(), move |_id, request| {
            let uri = request.uri();
            let index = query_index(uri.query(), "n");
            if uri.path() == DOC_PATH {
                handler_index.store(index, Ordering::SeqCst);
            }
            respond(
                uri.path(),
                index,
                &spinner_html,
                handler_source.as_ref(),
                &handler_changed,
            )
        })
        .with_ipc_handler(move |req: wry::http::Request<String>| {
            // The shell posts "mdp:export <size> <scheme>" on an Export click;
            // other IPC (e.g. render-complete) is ignored here.
            if let Some(export) = parse_export_request(req.body().as_str()) {
                let _ = ipc_proxy.send_event(ShowEvent::Export(export));
            }
        })
        .with_url(format!("{SCHEME}://localhost/"));

    #[cfg(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    ))]
    let _webview = builder
        .build(&window)
        .map_err(|e| FrontendError::WebView(e.to_string()))?;
    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    )))]
    let _webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window
            .default_vbox()
            .ok_or_else(|| FrontendError::WebView("no GTK vbox on window".to_string()))?;
        builder
            .build_gtk(vbox)
            .map_err(|e| FrontendError::WebView(e.to_string()))?
    };

    // macOS: install a standard app menu so Cmd+Q (quit) and Cmd+W (close)
    // work. Without a menu, macOS has nothing to route those shortcuts to, so
    // only the red close button would close the window. Kept alive for the
    // process lifetime (the event loop below never returns).
    #[cfg(target_os = "macos")]
    let _menu = {
        use muda::{Menu, PredefinedMenuItem, Submenu};
        let menu = Menu::new();
        let app = Submenu::new("mdp", true);
        let _ = app.append(&PredefinedMenuItem::quit(None));
        let window = Submenu::new("Window", true);
        let _ = window.append(&PredefinedMenuItem::close_window(None));
        let _ = window.append(&PredefinedMenuItem::minimize(None));
        let _ = menu.append(&app);
        let _ = menu.append(&window);
        menu.init_for_nsapp();
        menu
    };

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(ShowEvent::Export(req)) => {
                handle_export(
                    &files,
                    theme.as_deref(),
                    current_index.load(Ordering::SeqCst),
                    &req,
                );
            }
            _ => {}
        }
    });
}

/// A parsed in-app export request from the shell's "mdp:export <size> <scheme>"
/// IPC message. Values are validated here so only known tokens ever reach the
/// `mdp export` subprocess.
#[derive(Debug, PartialEq, Eq)]
struct ExportRequest {
    page_size: String,
    scheme: String,
}

/// Parse "mdp:export <size> <scheme>". Returns `None` for any other message or
/// an unknown page size; an unknown/absent scheme falls back to "auto".
fn parse_export_request(msg: &str) -> Option<ExportRequest> {
    let rest = msg.strip_prefix("mdp:export")?;
    let mut fields = rest.split_whitespace();
    let page_size = fields.next()?;
    if !matches!(page_size, "letter" | "a4") {
        return None;
    }
    let scheme = match fields.next() {
        Some("light") => "light",
        Some("dark") => "dark",
        _ => "auto",
    };
    Some(ExportRequest {
        page_size: page_size.to_string(),
        scheme: scheme.to_string(),
    })
}

/// Build the `mdp export` argument list for `file` -> `out` with the request's
/// page size and scheme, threading the active theme through when set.
fn export_args(
    file: &Path,
    out: &Path,
    req: &ExportRequest,
    theme: Option<&Path>,
) -> Vec<std::ffi::OsString> {
    use std::ffi::OsString;
    let mut args: Vec<OsString> = vec![
        OsString::from("export"),
        file.as_os_str().to_os_string(),
        OsString::from("--to"),
        out.as_os_str().to_os_string(),
        OsString::from("--page-size"),
        OsString::from(&req.page_size),
        OsString::from("--scheme"),
        OsString::from(&req.scheme),
    ];
    if let Some(theme) = theme {
        args.push(OsString::from("--theme"));
        args.push(theme.as_os_str().to_os_string());
    }
    args
}

/// Run an in-app export: prompt for a save location, then shell out to
/// `mdp export` (a fresh one-shot, which avoids nesting event loops). macOS only,
/// matching where PDF export runs.
#[cfg(target_os = "macos")]
fn handle_export(files: &[PathBuf], theme: Option<&Path>, index: usize, req: &ExportRequest) {
    let Some(file) = files.get(index).or_else(|| files.first()) else {
        eprintln!("mdp: nothing to export");
        return;
    };
    let default_name = file
        .file_stem()
        .map(|stem| format!("{}.pdf", stem.to_string_lossy()))
        .unwrap_or_else(|| "export.pdf".to_string());
    let Some(out) = rfd::FileDialog::new()
        .set_file_name(default_name)
        .add_filter("PDF", &["pdf"])
        .save_file()
    else {
        return; // The user cancelled the save dialog.
    };
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("mdp"));
    let args = export_args(file, &out, req, theme);
    match std::process::Command::new(exe).args(&args).status() {
        Ok(status) if status.success() => eprintln!("mdp: exported {}", out.display()),
        Ok(status) => eprintln!("mdp: export failed ({status})"),
        Err(e) => eprintln!("mdp: could not run export: {e}"),
    }
}

/// In-app export is currently macOS-only (PDF export is). Elsewhere the toolbar
/// button still appears under a native host, but reports the limitation.
#[cfg(not(target_os = "macos"))]
fn handle_export(_files: &[PathBuf], _theme: Option<&Path>, _index: usize, _req: &ExportRequest) {
    eprintln!("mdp: in-app PDF export is currently implemented only on macOS");
}

/// Start watching `paths` (if any), flipping `changed` on each debounced change.
/// A watch-setup failure is non-fatal: the preview shows without live reload.
fn start_watch(paths: &[PathBuf], changed: Arc<AtomicBool>) -> Option<WatchHandle> {
    if paths.is_empty() {
        return None;
    }
    match watch::watch_files(paths, move |_change| changed.store(true, Ordering::SeqCst)) {
        Ok(handle) => Some(handle),
        Err(e) => {
            eprintln!("mdp: file watching disabled: {e}");
            None
        }
    }
}

/// Which resource a custom-protocol path maps to. Pure, so it is unit-testable.
#[derive(Debug, PartialEq, Eq)]
enum WebRoute {
    Spinner,
    Document,
    Reload,
    NotFound,
}

fn route(path: &str) -> WebRoute {
    match path {
        "/" | "" | "/index.html" => WebRoute::Spinner,
        DOC_PATH => WebRoute::Document,
        RELOAD_PATH => WebRoute::Reload,
        _ => WebRoute::NotFound,
    }
}

/// The URL that switches the webview to document `index`.
fn doc_href(index: usize) -> String {
    format!("{SCHEME}://localhost{DOC_PATH}?n={index}")
}

/// Parse `key=N` from a query string; 0 if absent or unparsable.
fn query_index(query: Option<&str>, key: &str) -> usize {
    let Some(q) = query else {
        return 0;
    };
    for pair in q.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return v.parse().unwrap_or(0);
            }
        }
    }
    0
}

/// Answer one custom-protocol request. The document route re-renders and clears
/// the change flag; the reload route reports the flag. Testable with a plain
/// render closure and flag, no live webview needed.
fn respond(
    path: &str,
    index: usize,
    spinner_html: &str,
    source: &dyn DocSource,
    changed: &AtomicBool,
) -> Response<Cow<'static, [u8]>> {
    match route(path) {
        WebRoute::Spinner => html_response(spinner_html.to_owned().into_bytes()),
        WebRoute::Document => {
            let n = index.min(source.count().saturating_sub(1));
            let doc = source.render(n, &doc_href);
            changed.store(false, Ordering::SeqCst);
            html_response(doc.html.into_bytes())
        }
        WebRoute::Reload => json_response(reload_body(changed.load(Ordering::SeqCst)).into_bytes()),
        WebRoute::NotFound => not_found(path),
    }
}

/// The reload endpoint's JSON body for a given changed state.
fn reload_body(changed: bool) -> String {
    if changed {
        "{\"changed\":true}".to_string()
    } else {
        "{\"changed\":false}".to_string()
    }
}

/// Build a `text/html; charset=utf-8` response from owned bytes.
fn html_response(bytes: Vec<u8>) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .header(CONTENT_TYPE, mime_for("index.html"))
        .body(Cow::Owned(bytes))
        .expect("static html response is always well-formed")
}

/// Build an `application/json` response from owned bytes.
fn json_response(bytes: Vec<u8>) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .header(CONTENT_TYPE, "application/json; charset=utf-8")
        .body(Cow::Owned(bytes))
        .expect("static json response is always well-formed")
}

/// A 404 for any path the preview does not serve.
fn not_found(path: &str) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(404)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Cow::Owned(format!("not found: {path}").into_bytes()))
        .expect("static 404 response is always well-formed")
}

/// Map a path's extension to a MIME type. Covers the asset kinds served over
/// this protocol; falls back to a generic binary type.
fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// The tiny inline spinner shell served on the first request. Brand neutral,
/// respects `prefers-color-scheme` (no white flash in dark mode), and hands off
/// to the rendered document. Carries no external references.
fn spinner_page(title: &str) -> String {
    format!(
        "<!doctype html>\n\
<html lang=\"en\">\n\
<head>\n\
<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<title>{title}</title>\n\
<style>\n\
  :root {{ color-scheme: light dark; }}\n\
  html, body {{ height: 100%; margin: 0; }}\n\
  body {{\n\
    display: flex; align-items: center; justify-content: center;\n\
    background: #ffffff; color: #6b7280;\n\
    font: 400 15px/1.5 system-ui, -apple-system, Segoe UI, Roboto, sans-serif;\n\
  }}\n\
  @media (prefers-color-scheme: dark) {{\n\
    body {{ background: #14161a; color: #9aa4b2; }}\n\
  }}\n\
  .spinner {{\n\
    width: 28px; height: 28px; border-radius: 50%;\n\
    border: 3px solid currentColor; border-top-color: transparent;\n\
    animation: spin 0.7s linear infinite;\n\
    opacity: 0.65;\n\
  }}\n\
  @keyframes spin {{ to {{ transform: rotate(360deg); }} }}\n\
  @media (prefers-reduced-motion: reduce) {{ .spinner {{ animation: none; }} }}\n\
</style>\n\
</head>\n\
<body>\n\
<div class=\"spinner\" role=\"status\" aria-label=\"Loading preview\"></div>\n\
<script>\n\
  requestAnimationFrame(function () {{\n\
    requestAnimationFrame(function () {{\n\
      window.location.replace('{scheme}://localhost{doc}');\n\
    }});\n\
  }});\n\
</script>\n\
</body>\n\
</html>\n",
        title = escape_html(title),
        scheme = SCHEME,
        doc = DOC_PATH,
    )
}

/// The window (not document) title. Falls back to the app name when blank.
fn window_title(title: &str) -> String {
    if title.trim().is_empty() {
        "mdp".to_string()
    } else {
        title.to_string()
    }
}

/// Minimal HTML/attribute escaping for text placed into the spinner markup.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&#39;")
        .replace('"', "&quot;")
}

/// Errors a frontend reports back to the CLI so it can fall back or exit.
#[derive(Debug)]
pub enum FrontendError {
    /// The native window could not be created.
    Window(String),
    /// The webview could not be created (missing engine/runtime, etc.).
    WebView(String),
}

impl std::fmt::Display for FrontendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrontendError::Window(e) => write!(f, "could not create the window: {e}"),
            FrontendError::WebView(e) => write!(f, "could not create the webview: {e}"),
        }
    }
}

impl std::error::Error for FrontendError {}

/// Export `doc` to a PDF at `out` (design section 4.5). macOS: load the document
/// in a webview, wait for the shell's render-complete IPC signal (so Mermaid and
/// KaTeX have finished), then capture silently via `WKWebView.createPDF`. A
/// timed fallback captures even if the signal never arrives.
///
/// `createPDF` yields one page as tall as the whole document. The webview is laid
/// out at `page_width_pt` (PDF points) so the capture is that wide, then
/// `mdp_core::pdf` paginates the tall page into `page_height_pt`-tall pages before
/// writing (e.g. 612 x 792 for US Letter, 595.28 x 841.89 for A4). The window
/// briefly appears during capture. Windows/Linux native PDF are not yet done.
#[cfg(target_os = "macos")]
pub fn export_pdf(
    doc: &RenderedDoc,
    out: &Path,
    page_width_pt: f64,
    page_height_pt: f32,
) -> Result<(), FrontendError> {
    use std::cell::RefCell;
    use std::rc::Rc;

    use tao::event::Event;
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tao::platform::run_return::EventLoopExtRunReturn;
    use tao::window::WindowBuilder;
    use wry::{PageLoadEvent, WebViewBuilder, WebViewExtMacOS};

    use block2::RcBlock;
    use objc2_foundation::{MainThreadMarker, NSData, NSError};
    use objc2_web_kit::WKPDFConfiguration;

    enum PdfEvent {
        // Carries the rendered surface color (if the shell reported it) so
        // pagination can paint page trailing space to match.
        Capture(Option<[f32; 3]>),
        Done(Result<usize, String>),
    }

    let out_path = out.to_path_buf();
    let html = doc.html.clone();

    let mut event_loop = EventLoopBuilder::<PdfEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title("mdp export")
        .with_inner_size(tao::dpi::LogicalSize::new(
            page_width_pt,
            page_height_pt as f64,
        ))
        .build(&event_loop)
        .map_err(|e| FrontendError::Window(e.to_string()))?;

    let ipc_proxy = proxy.clone();
    let load_proxy = proxy.clone();
    let webview = WebViewBuilder::new()
        .with_html(html)
        .with_ipc_handler(move |req: wry::http::Request<String>| {
            // Message is "render-complete[ <css-color>]"; the color is optional.
            if let Some(rest) = req.body().as_str().strip_prefix("render-complete") {
                let bg = mdp_core::pdf::parse_css_color(rest);
                let _ = ipc_proxy.send_event(PdfEvent::Capture(bg));
            }
        })
        .with_on_page_load_handler(move |event, _url| {
            if matches!(event, PageLoadEvent::Finished) {
                // Safety net only. The shell's render-complete IPC message is the
                // real trigger; it fires after Mermaid, web fonts, and paged.js
                // pagination have all resolved (awaited promises, not timers). This
                // timeout must stay well clear of that so it never pre-empts a
                // document that is still laying out - a short timeout used to win
                // the race on slower renders and capture a half-paginated page,
                // which is what made the exported page count vary from run to run.
                // It only fires if the IPC signal never arrives (a catastrophic
                // script error before it posts).
                let p = load_proxy.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(20_000));
                    let _ = p.send_event(PdfEvent::Capture(None));
                });
            }
        })
        .build(&window)
        .map_err(|e| FrontendError::WebView(e.to_string()))?;

    let captured = AtomicBool::new(false);
    let result: Rc<RefCell<Option<Result<usize, String>>>> = Rc::new(RefCell::new(None));
    let loop_result = result.clone();

    event_loop.run_return(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(PdfEvent::Capture(background)) => {
                // Capture exactly once (both the IPC signal and the fallback fire).
                if captured.swap(true, Ordering::SeqCst) {
                    return;
                }
                let mtm = MainThreadMarker::new().expect("event loop runs on the main thread");
                let wk = webview.webview();
                // A nil-equivalent configuration captures the full page as one
                // tall page; mdp_core::pdf then paginates it below.
                let config = unsafe { WKPDFConfiguration::new(mtm) };
                let done_proxy = proxy.clone();
                let out_for_block = out_path.clone();
                let handler = RcBlock::new(move |data: *mut NSData, err: *mut NSError| {
                    let res = if !err.is_null() {
                        Err(format!(
                            "createPDF error: {}",
                            unsafe { &*err }.localizedDescription()
                        ))
                    } else if data.is_null() {
                        Err("createPDF returned neither data nor error".to_string())
                    } else {
                        let bytes = unsafe { &*data }.to_vec();
                        // Slice the single tall page into Letter-height pages before
                        // writing. If pagination somehow fails, fall back to the raw
                        // capture so the user still gets a (single-page) PDF.
                        let out_bytes =
                            match mdp_core::pdf::paginate(&bytes, page_height_pt, background) {
                                Ok(paged) => paged,
                                Err(e) => {
                                    eprintln!(
                                        "mdp: PDF pagination failed ({e}); wrote unpaginated"
                                    );
                                    bytes
                                }
                            };
                        match std::fs::write(&out_for_block, &out_bytes) {
                            Ok(()) => Ok(out_bytes.len()),
                            Err(e) => Err(format!("write failed: {e}")),
                        }
                    };
                    let _ = done_proxy.send_event(PdfEvent::Done(res));
                });
                unsafe {
                    wk.createPDFWithConfiguration_completionHandler(Some(&config), &handler);
                }
            }
            Event::UserEvent(PdfEvent::Done(res)) => {
                *loop_result.borrow_mut() = Some(res);
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });

    let outcome = result.borrow_mut().take();
    match outcome {
        Some(Ok(_)) => Ok(()),
        Some(Err(e)) => Err(FrontendError::WebView(e)),
        None => Err(FrontendError::WebView(
            "PDF capture did not complete".to_string(),
        )),
    }
}

/// PDF export is currently implemented only on macOS (design 4.5). Windows and
/// Linux each need their own native path.
#[cfg(not(target_os = "macos"))]
pub fn export_pdf(
    doc: &RenderedDoc,
    out: &Path,
    page_width_pt: f64,
    page_height_pt: f32,
) -> Result<(), FrontendError> {
    let _ = (doc, out, page_width_pt, page_height_pt);
    Err(FrontendError::WebView(
        "PDF export is currently implemented only on macOS".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mock(&'static str);
    impl DocSource for Mock {
        fn count(&self) -> usize {
            1
        }
        fn render(&self, _index: usize, _href: &dyn Fn(usize) -> String) -> RenderedDoc {
            RenderedDoc {
                html: self.0.to_string(),
                title: "T".to_string(),
            }
        }
    }

    fn body_string(resp: &Response<Cow<'static, [u8]>>) -> String {
        String::from_utf8(resp.body().to_vec()).unwrap()
    }

    #[test]
    fn parses_export_ipc_messages() {
        let r = parse_export_request("mdp:export a4 dark").unwrap();
        assert_eq!(r.page_size, "a4");
        assert_eq!(r.scheme, "dark");
        // Scheme is optional and defaults to auto.
        assert_eq!(
            parse_export_request("mdp:export letter").unwrap().scheme,
            "auto"
        );
        // An unknown scheme also falls back to auto rather than being forwarded.
        assert_eq!(
            parse_export_request("mdp:export letter neon")
                .unwrap()
                .scheme,
            "auto"
        );
        // Unknown page size, missing size, and unrelated messages are rejected.
        assert!(parse_export_request("mdp:export tabloid").is_none());
        assert!(parse_export_request("mdp:export").is_none());
        assert!(parse_export_request("render-complete rgb(0, 0, 0)").is_none());
    }

    #[test]
    fn builds_export_subprocess_args() {
        let req = ExportRequest {
            page_size: "a4".to_string(),
            scheme: "light".to_string(),
        };
        let strs: Vec<String> = export_args(
            Path::new("/src/doc.md"),
            Path::new("/out/doc.pdf"),
            &req,
            None,
        )
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
        assert_eq!(
            strs,
            vec![
                "export",
                "/src/doc.md",
                "--to",
                "/out/doc.pdf",
                "--page-size",
                "a4",
                "--scheme",
                "light"
            ]
        );

        // A theme is threaded through as `--theme <path>` when present.
        let with_theme: Vec<String> = export_args(
            Path::new("a.md"),
            Path::new("a.pdf"),
            &req,
            Some(Path::new("/themes/night.css")),
        )
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
        assert!(with_theme
            .windows(2)
            .any(|w| w[0] == "--theme" && w[1] == "/themes/night.css"));
    }

    #[test]
    fn query_index_reads_n() {
        assert_eq!(query_index(Some("n=2"), "n"), 2);
        assert_eq!(query_index(None, "n"), 0);
        assert_eq!(query_index(Some("n=bad"), "n"), 0);
    }

    #[test]
    fn routes_map_as_expected() {
        assert_eq!(route("/"), WebRoute::Spinner);
        assert_eq!(route(""), WebRoute::Spinner);
        assert_eq!(route(DOC_PATH), WebRoute::Document);
        assert_eq!(route(RELOAD_PATH), WebRoute::Reload);
        assert_eq!(route("/nope.png"), WebRoute::NotFound);
    }

    #[test]
    fn root_serves_the_spinner() {
        let changed = AtomicBool::new(false);
        let resp = respond("/", 0, "SPINNER", &Mock("DOC"), &changed);
        assert_eq!(resp.status(), 200);
        assert!(body_string(&resp).contains("SPINNER"));
    }

    #[test]
    fn doc_route_renders_fresh_and_clears_the_flag() {
        let changed = AtomicBool::new(true);
        let resp = respond(DOC_PATH, 0, "SPINNER", &Mock("DOC-BODY"), &changed);
        assert_eq!(resp.status(), 200);
        assert!(body_string(&resp).contains("DOC-BODY"));
        // Serving fresh content clears the change flag.
        assert!(!changed.load(Ordering::SeqCst));
    }

    #[test]
    fn reload_route_reports_the_flag() {
        let changed = AtomicBool::new(true);
        let resp = respond(RELOAD_PATH, 0, "SPINNER", &Mock("DOC"), &changed);
        assert!(body_string(&resp).contains("\"changed\":true"));
        // Reading the flag does not clear it (only serving the document does).
        assert!(changed.load(Ordering::SeqCst));
    }

    #[test]
    fn unknown_path_is_a_404() {
        let changed = AtomicBool::new(false);
        let resp = respond("/whatever", 0, "SPINNER", &Mock("DOC"), &changed);
        assert_eq!(resp.status(), 404);
    }

    #[test]
    fn spinner_hands_off_and_is_offline() {
        let page = spinner_page("Readme.md");
        assert!(page.contains("mdprev://localhost/doc"));
        assert!(!page.contains("http://"));
        assert!(!page.contains("https://"));
        assert!(page.contains("<title>Readme.md</title>"));
    }

    #[test]
    fn window_title_falls_back_when_blank() {
        assert_eq!(window_title("   "), "mdp");
        assert_eq!(window_title("Design.md"), "Design.md");
    }

    #[test]
    fn mime_types_cover_asset_kinds() {
        assert_eq!(mime_for("a.css"), "text/css; charset=utf-8");
        assert_eq!(mime_for("f.woff2"), "font/woff2");
        assert_eq!(mime_for("noext"), "application/octet-stream");
    }
}
