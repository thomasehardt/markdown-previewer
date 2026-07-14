//! Local HTTP server frontend (tiny_http + system browser).
//!
//! Serves a freshly-rendered document on a loopback port and optionally opens
//! the system browser. This is the robust, display-agnostic path (design
//! section 6): no OS webview, works headless, is the natural hook for a calling
//! tool that wants a URL, and is the automatic fallback when the native webview
//! cannot initialize (design risk #5).
//!
//! Live reload (design section 4.3): the document route re-renders from disk on
//! every request, and when watch paths are given a background watcher flips a
//! "changed" flag. The page polls the reload endpoint; on a change it shows a
//! non-intrusive banner and reloads only when the reader asks.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use mdp_core::watch::{self, WatchHandle};
use mdp_core::DocSource;
use tiny_http::{Header, Response, Server};

/// Loopback host. This never binds a public interface.
const HOST: &str = "127.0.0.1";

/// The reload-signal endpoint the shell polls (design section 4.3).
const RELOAD_PATH: &str = "/.mdp/reload";

/// Serve `render`'s output on `port` (0 picks an ephemeral port), print the
/// bound URL to stdout, and optionally open the system browser. The document
/// route re-renders on each request; a non-empty `watch_paths` enables the
/// change flag that drives the reload banner. Runs the request loop and returns
/// only on a fatal error; otherwise the process stops with Ctrl-C.
pub fn serve(
    source: Arc<dyn DocSource>,
    watch_paths: &[PathBuf],
    port: u16,
    open_browser: bool,
) -> Result<(), ServeError> {
    let (server, url) = bind(port)?;
    let changed = Arc::new(AtomicBool::new(false));
    let _watch = start_watch(watch_paths, changed.clone());

    // The URL goes to stdout so a calling tool can capture it; status to stderr.
    println!("{url}");
    eprintln!("mdp: serving at {url} (Ctrl-C to stop)");
    if open_browser {
        if let Err(e) = webbrowser::open(&url) {
            eprintln!("mdp: could not open a browser ({e}); open {url} manually.");
        }
    }

    for request in server.incoming_requests() {
        let response = match route(request.url()) {
            Route::Document => {
                let index = parse_index(request.url(), "doc").min(source.count().saturating_sub(1));
                let doc = source.render(index, &serve_href);
                // Fresh content served: the reader is now up to date.
                changed.store(false, Ordering::SeqCst);
                html_response(200, "text/html; charset=utf-8", doc.html)
            }
            Route::Reload => html_response(
                200,
                "application/json; charset=utf-8",
                reload_body(changed.load(Ordering::SeqCst)),
            ),
            Route::NotFound => html_response(
                404,
                "text/plain; charset=utf-8",
                format!("not found: {}", request.url()),
            ),
        };
        // A dropped connection on one request must not take the server down.
        if let Err(e) = request.respond(response) {
            eprintln!("mdp: request failed: {e}");
        }
    }
    Ok(())
}

/// Start watching `paths` (if any), flipping `changed` on each debounced change.
/// A watch-setup failure is non-fatal: serving continues without live reload.
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

/// Bind a loopback server on `port` and return it with its
/// `http://127.0.0.1:PORT/` URL. Split from the serve loop so binding (and the
/// resolved ephemeral port) is testable without running the blocking loop.
fn bind(port: u16) -> Result<(Server, String), ServeError> {
    let server = Server::http((HOST, port)).map_err(|e| ServeError::Bind(e.to_string()))?;
    let bound_port = server
        .server_addr()
        .to_ip()
        .map(|addr| addr.port())
        .ok_or_else(|| ServeError::Bind("server bound to a non-IP address".to_string()))?;
    let url = format!("http://{HOST}:{bound_port}/");
    Ok((server, url))
}

/// Request routing. Pure over the path, so it is unit-testable.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    Document,
    Reload,
    NotFound,
}

fn route(raw_path: &str) -> Route {
    // Strip any query string (e.g. a cache-buster).
    let path = raw_path.split('?').next().unwrap_or(raw_path);
    match path {
        "/" | "" | "/index.html" => Route::Document,
        RELOAD_PATH => Route::Reload,
        _ => Route::NotFound,
    }
}

/// The URL that switches the serve frontend to document `index`.
fn serve_href(index: usize) -> String {
    format!("/?doc={index}")
}

/// Parse `key=N` from a URL's query string; 0 if absent or unparsable.
fn parse_index(url: &str, key: &str) -> usize {
    let query = match url.split_once('?') {
        Some((_, q)) => q,
        None => return 0,
    };
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return v.parse().unwrap_or(0);
            }
        }
    }
    0
}

/// The reload endpoint's JSON body for a given changed state.
fn reload_body(changed: bool) -> String {
    if changed {
        "{\"changed\":true}".to_string()
    } else {
        "{\"changed\":false}".to_string()
    }
}

/// Build a tiny_http response with a status, content type, and body.
fn html_response(status: u16, content_type: &str, body: String) -> Response<Cursor<Vec<u8>>> {
    Response::from_string(body)
        .with_status_code(status)
        .with_header(
            Header::from_bytes(b"Content-Type".as_ref(), content_type.as_bytes())
                .expect("static content-type header is always valid"),
        )
}

/// Errors the serve frontend reports to the CLI.
#[derive(Debug)]
pub enum ServeError {
    /// The loopback port could not be bound.
    Bind(String),
    /// The server loop failed fatally.
    Serve(String),
}

impl std::fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServeError::Bind(e) => write!(f, "could not start the local server: {e}"),
            ServeError::Serve(e) => write!(f, "local server error: {e}"),
        }
    }
}

impl std::error::Error for ServeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_paths_route_to_the_document() {
        assert_eq!(route("/"), Route::Document);
        assert_eq!(route(""), Route::Document);
        assert_eq!(route("/index.html"), Route::Document);
        assert_eq!(route("/?t=123"), Route::Document);
    }

    #[test]
    fn reload_path_routes_to_reload() {
        assert_eq!(route(RELOAD_PATH), Route::Reload);
        assert_eq!(route("/.mdp/reload?x=1"), Route::Reload);
    }

    #[test]
    fn unknown_path_is_not_found() {
        assert_eq!(route("/nope"), Route::NotFound);
    }

    #[test]
    fn reload_body_reflects_state() {
        assert!(reload_body(true).contains("\"changed\":true"));
        assert!(reload_body(false).contains("\"changed\":false"));
    }

    #[test]
    fn bind_picks_an_ephemeral_port_and_builds_a_loopback_url() {
        let (server, url) = bind(0).expect("bind ephemeral port");
        assert!(url.starts_with("http://127.0.0.1:"), "url was {url}");
        assert!(url.ends_with('/'));
        let port: u16 = url
            .trim_start_matches("http://127.0.0.1:")
            .trim_end_matches('/')
            .parse()
            .expect("numeric port in url");
        assert!(port > 0, "ephemeral port should be non-zero");
        drop(server);
    }

    #[test]
    fn start_watch_is_none_without_paths() {
        assert!(start_watch(&[], Arc::new(AtomicBool::new(false))).is_none());
    }

    #[test]
    fn parse_index_reads_the_query() {
        assert_eq!(parse_index("/?doc=2", "doc"), 2);
        assert_eq!(parse_index("/", "doc"), 0);
        assert_eq!(parse_index("/?doc=x", "doc"), 0);
        assert_eq!(parse_index("/?a=1&doc=3", "doc"), 3);
    }
}
