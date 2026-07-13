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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use mdp_core::watch::{self, WatchHandle};
use mdp_core::RenderedDoc;

use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
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

/// Open a native window and display `render`'s output read-only.
///
/// Re-renders on each document request (so a reload after a change shows fresh
/// content) and, when `watch_paths` is non-empty, runs a watcher that drives the
/// reload banner. On success this runs the OS event loop and does not return
/// until the window closes; it returns `Err` only when the window or webview
/// cannot be created, so the CLI can fall back to another frontend.
pub fn show<F>(render: F, watch_paths: &[PathBuf]) -> Result<(), FrontendError>
where
    F: Fn() -> RenderedDoc + Send + Sync + 'static,
{
    // One render up front for the window title and instant spinner.
    let initial = render();
    let spinner_html = spinner_page(&initial.title);
    let title = window_title(&initial.title);

    let changed = Arc::new(AtomicBool::new(false));
    let _watch = start_watch(watch_paths, changed.clone());

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title(title)
        .with_inner_size(LogicalSize::new(1000.0, 800.0))
        .build(&event_loop)
        .map_err(|e| FrontendError::Window(e.to_string()))?;

    // The protocol handler is `Fn + 'static`; share the renderer and flag in.
    let render = Arc::new(render);
    let handler_render = render.clone();
    let handler_changed = changed.clone();
    let builder = WebViewBuilder::new()
        .with_custom_protocol(SCHEME.to_string(), move |_id, request| {
            respond(
                request.uri().path(),
                &spinner_html,
                handler_render.as_ref(),
                &handler_changed,
            )
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

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
        }
    });
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

/// Answer one custom-protocol request. The document route re-renders and clears
/// the change flag; the reload route reports the flag. Testable with a plain
/// render closure and flag, no live webview needed.
fn respond<F>(
    path: &str,
    spinner_html: &str,
    render: &F,
    changed: &AtomicBool,
) -> Response<Cow<'static, [u8]>>
where
    F: Fn() -> RenderedDoc,
{
    match route(path) {
        WebRoute::Spinner => html_response(spinner_html.to_owned().into_bytes()),
        WebRoute::Document => {
            let doc = render();
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
/// Known behavior (design 4.5): createPDF produces a single tall page, not
/// paginated to Letter/A4, and the window briefly appears during capture.
/// Windows/Linux native PDF are not yet implemented.
#[cfg(target_os = "macos")]
pub fn export_pdf(doc: &RenderedDoc, out: &Path) -> Result<(), FrontendError> {
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
        Capture,
        Done(Result<usize, String>),
    }

    let out_path = out.to_path_buf();
    let html = doc.html.clone();

    let mut event_loop = EventLoopBuilder::<PdfEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title("mdp export")
        .with_inner_size(tao::dpi::LogicalSize::new(900.0, 1200.0))
        .build(&event_loop)
        .map_err(|e| FrontendError::Window(e.to_string()))?;

    let ipc_proxy = proxy.clone();
    let load_proxy = proxy.clone();
    let webview = WebViewBuilder::new()
        .with_html(html)
        .with_ipc_handler(move |req: wry::http::Request<String>| {
            if req.body().as_str() == "render-complete" {
                let _ = ipc_proxy.send_event(PdfEvent::Capture);
            }
        })
        .with_on_page_load_handler(move |event, _url| {
            if matches!(event, PageLoadEvent::Finished) {
                // Fallback: capture even if the render-complete signal is missed.
                let p = load_proxy.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(3000));
                    let _ = p.send_event(PdfEvent::Capture);
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
            Event::UserEvent(PdfEvent::Capture) => {
                // Capture exactly once (both the IPC signal and the fallback fire).
                if captured.swap(true, Ordering::SeqCst) {
                    return;
                }
                let mtm = MainThreadMarker::new().expect("event loop runs on the main thread");
                let wk = webview.webview();
                // A nil-equivalent configuration captures the full page as one
                // tall page; createPDF does not paginate (design 4.5).
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
                        match std::fs::write(&out_for_block, &bytes) {
                            Ok(()) => Ok(bytes.len()),
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
pub fn export_pdf(doc: &RenderedDoc, out: &Path) -> Result<(), FrontendError> {
    let _ = (doc, out);
    Err(FrontendError::WebView(
        "PDF export is currently implemented only on macOS".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(html: &str) -> RenderedDoc {
        RenderedDoc {
            html: html.to_string(),
            title: "T".to_string(),
        }
    }

    fn body_string(resp: &Response<Cow<'static, [u8]>>) -> String {
        String::from_utf8(resp.body().to_vec()).unwrap()
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
        let resp = respond("/", "SPINNER", &|| doc("DOC"), &changed);
        assert_eq!(resp.status(), 200);
        assert!(body_string(&resp).contains("SPINNER"));
    }

    #[test]
    fn doc_route_renders_fresh_and_clears_the_flag() {
        let changed = AtomicBool::new(true);
        let resp = respond(DOC_PATH, "SPINNER", &|| doc("DOC-BODY"), &changed);
        assert_eq!(resp.status(), 200);
        assert!(body_string(&resp).contains("DOC-BODY"));
        // Serving fresh content clears the change flag.
        assert!(!changed.load(Ordering::SeqCst));
    }

    #[test]
    fn reload_route_reports_the_flag() {
        let changed = AtomicBool::new(true);
        let resp = respond(RELOAD_PATH, "SPINNER", &|| doc("DOC"), &changed);
        assert!(body_string(&resp).contains("\"changed\":true"));
        // Reading the flag does not clear it (only serving the document does).
        assert!(changed.load(Ordering::SeqCst));
    }

    #[test]
    fn unknown_path_is_a_404() {
        let changed = AtomicBool::new(false);
        let resp = respond("/whatever", "SPINNER", &|| doc("DOC"), &changed);
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
