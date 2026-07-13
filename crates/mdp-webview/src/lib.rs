//! Native webview frontend (wry + tao).
//!
//! This crate is the default frontend behind the `webview` feature. It consumes
//! the one load-bearing product of `mdp-core` (a fully self-contained HTML
//! string) and displays it read-only in a native OS webview.
//!
//! Launch sequence (design section 9, Phase 1, and the latency mitigation in
//! section 8, item 7):
//!
//! 1. A `tao` window is created and shown immediately.
//! 2. The webview's initial URL is `mdprev://localhost/`, whose very first
//!    request the custom protocol answers with a tiny inline spinner shell.
//!    This paints while the OS webview engine finishes cold-init, so the user
//!    sees instant feedback rather than a white flash.
//! 3. The spinner navigates to `mdprev://localhost/doc`, which the same
//!    protocol answers with the already-rendered `RenderedDoc.html`. Content
//!    swaps in.
//!
//! The custom protocol is the offline asset boundary: every byte the page needs
//! is served from memory with a correct MIME type and zero network access. In
//! Phase 1 the rendered document is fully self-contained (all CSS and JS are
//! inlined by `mdp-core`), so `/doc` is the only content route the page fetches;
//! the extension-based MIME routing is in place so later phases can serve
//! separate embedded assets (fonts, Mermaid, KaTeX) over the same protocol
//! without reworking this layer.
//!
//! The preview is read-only by construction: rendered, sanitized HTML in a
//! webview with no editing affordances, no form posts, and no navigation away
//! from the custom-protocol origin.

use std::borrow::Cow;

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

/// Open a native window and display the already-rendered, self-contained
/// document read-only.
///
/// On success this runs the OS event loop and does not return until the window
/// is closed (the process exits from within the loop, per `tao`'s diverging
/// `run`). It returns `Err` only when the window or webview cannot be created,
/// so the CLI can fall back to another frontend.
pub fn show(doc: &RenderedDoc) -> Result<(), FrontendError> {
    // Own the strings the protocol handler serves. The handler is `Fn + 'static`
    // and may run for every request, so it captures owned copies.
    let document_html = doc.html.clone();
    let spinner_html = spinner_page(&doc.title);

    let event_loop = EventLoop::new();

    let window = WindowBuilder::new()
        .with_title(window_title(&doc.title))
        .with_inner_size(LogicalSize::new(1000.0, 800.0))
        .build(&event_loop)
        .map_err(|e| FrontendError::Window(e.to_string()))?;

    let builder = WebViewBuilder::new()
        .with_custom_protocol(SCHEME.to_string(), move |_id, request| {
            serve(request.uri().path(), &spinner_html, &document_html)
        })
        // Load the spinner first; it hands off to the document (design 8.7).
        .with_url(format!("{SCHEME}://localhost/"));

    // wry builds against the window on macOS/Windows; on unix it attaches to
    // the window's GTK vbox. This mirrors wry's own custom-protocol example.
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

/// Answer one custom-protocol request. Pure and synchronous: given the request
/// path and the two owned bodies, return the correct response with its MIME
/// type. Split out so it is unit-testable without a live webview.
fn serve(
    path: &str,
    spinner_html: &str,
    document_html: &str,
) -> Response<Cow<'static, [u8]>> {
    match path {
        "/" | "" | "/index.html" => {
            html_response(spinner_html.to_owned().into_bytes())
        }
        DOC_PATH => html_response(document_html.to_owned().into_bytes()),
        other => not_found(other),
    }
}

/// Build a `text/html; charset=utf-8` response from owned bytes.
fn html_response(bytes: Vec<u8>) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .header(CONTENT_TYPE, mime_for("index.html"))
        .body(Cow::Owned(bytes))
        .expect("static html response is always well-formed")
}

/// A 404 for any path the preview does not serve. Kept explicit so an
/// unexpected fetch surfaces rather than silently returning the document.
fn not_found(path: &str) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(404)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Cow::Owned(
            format!("not found: {path}").into_bytes(),
        ))
        .expect("static 404 response is always well-formed")
}

/// Map a path's extension to a MIME type. Covers the asset kinds later phases
/// serve over this protocol (fonts, diagrams, styles) so the routing layer does
/// not change when those land. Falls back to a generic binary type.
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

/// The tiny inline spinner shell served on the first request. It is brand
/// neutral, respects `prefers-color-scheme` so there is no white flash in dark
/// mode, and immediately hands off to the rendered document. It carries no
/// external references, keeping the offline guarantee intact.
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
  /* Paint the spinner once, then hand off to the rendered document. */\n\
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

/// The window (not document) title. Falls back to the app name when the doc
/// title is empty so the OS titlebar is never blank.
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

/// Errors a frontend reports back to the CLI so it can fall back or exit
/// cleanly.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn body_string(resp: &Response<Cow<'static, [u8]>>) -> String {
        String::from_utf8(resp.body().to_vec()).unwrap()
    }

    #[test]
    fn root_serves_the_spinner_shell() {
        let resp = serve("/", "SPINNER-BODY", "DOC-BODY");
        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers().get(CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
        assert!(body_string(&resp).contains("SPINNER-BODY"));
    }

    #[test]
    fn doc_path_serves_the_rendered_document() {
        let resp = serve(DOC_PATH, "SPINNER-BODY", "DOC-BODY");
        assert_eq!(resp.status(), 200);
        assert!(body_string(&resp).contains("DOC-BODY"));
        assert!(!body_string(&resp).contains("SPINNER-BODY"));
    }

    #[test]
    fn unknown_path_is_a_404() {
        let resp = serve("/nope.png", "s", "d");
        assert_eq!(resp.status(), 404);
    }

    #[test]
    fn spinner_hands_off_to_the_document_and_is_offline() {
        let page = spinner_page("Readme.md");
        // Navigates to the doc route over the custom protocol only.
        assert!(page.contains("mdprev://localhost/doc"));
        // No external references: nothing but the custom scheme appears.
        assert!(!page.contains("http://"));
        assert!(!page.contains("https://"));
        // Title is escaped into the shell.
        assert!(page.contains("<title>Readme.md</title>"));
    }

    #[test]
    fn spinner_title_is_escaped() {
        let page = spinner_page("a<b>&\"'");
        assert!(page.contains("a&lt;b&gt;&amp;&quot;&#39;"));
    }

    #[test]
    fn window_title_falls_back_when_blank() {
        assert_eq!(window_title("   "), "mdp");
        assert_eq!(window_title("Design.md"), "Design.md");
    }

    #[test]
    fn mime_types_cover_the_offline_asset_kinds() {
        assert_eq!(mime_for("a.css"), "text/css; charset=utf-8");
        assert_eq!(mime_for("a.js"), "text/javascript; charset=utf-8");
        assert_eq!(mime_for("KaTeX_Main.woff2"), "font/woff2");
        assert_eq!(mime_for("d.svg"), "image/svg+xml");
        assert_eq!(mime_for("noext"), "application/octet-stream");
    }
}
