//! Local HTTP server frontend (tiny_http + system browser).
//!
//! Serves the already-rendered, self-contained document on a loopback port and
//! optionally opens the system browser. This is the robust, display-agnostic
//! path (design section 6): it needs no OS webview, so it works headless, is the
//! natural hook for a calling tool that wants a URL, and is the automatic
//! fallback when the native webview cannot initialize (design risk #5).
//!
//! Because `mdp-core` inlines every asset into the document, a single `/` route
//! is all the page fetches. A stub reload endpoint is in place for the Phase 5
//! file-watching signal.

use mdp_core::RenderedDoc;
use tiny_http::{Header, Response, Server};

/// Loopback host. This never binds a public interface.
const HOST: &str = "127.0.0.1";

/// The reload-signal endpoint. A stub in Phase 3 (always "no change"); Phase 5
/// file watching turns it into the real reload prompt hook.
const RELOAD_PATH: &str = "/.mdp/reload";

/// Serve `doc` on `port` (0 picks an ephemeral port), print the bound URL to
/// stdout, and optionally open the system browser. Runs the request loop and
/// returns only on a fatal error; otherwise the process is stopped with Ctrl-C.
pub fn serve(doc: &RenderedDoc, port: u16, open_browser: bool) -> Result<(), ServeError> {
    let (server, url) = bind(port)?;

    // The URL goes to stdout so a calling tool (e.g. Claude Code) can capture it;
    // human-facing status goes to stderr.
    println!("{url}");
    eprintln!("mdp: serving \"{}\" at {url} (Ctrl-C to stop)", doc.title);

    if open_browser {
        if let Err(e) = webbrowser::open(&url) {
            eprintln!("mdp: could not open a browser ({e}); open {url} manually.");
        }
    }

    for request in server.incoming_requests() {
        let reply = reply(request.url(), &doc.html);
        let response = Response::from_string(reply.body)
            .with_status_code(reply.status)
            .with_header(
                Header::from_bytes(b"Content-Type".as_ref(), reply.content_type.as_bytes())
                    .expect("static content-type header is always valid"),
            );
        // A dropped connection on one request must not take the server down.
        if let Err(e) = request.respond(response) {
            eprintln!("mdp: request failed: {e}");
        }
    }
    Ok(())
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

/// One resolved reply: status, content type, and body. Pure over the request
/// path and the document, so routing is unit-testable without a live socket.
struct Reply {
    status: u16,
    content_type: &'static str,
    body: String,
}

/// Route a request path to its reply. Only `/` serves the (self-contained)
/// document; the reload endpoint is a Phase 3 stub; everything else is a 404.
fn reply(raw_path: &str, doc_html: &str) -> Reply {
    // Strip any query string (e.g. a cache-buster on the reload poll).
    let path = raw_path.split('?').next().unwrap_or(raw_path);
    match path {
        "/" | "" | "/index.html" => Reply {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: doc_html.to_owned(),
        },
        RELOAD_PATH => Reply {
            status: 200,
            content_type: "application/json; charset=utf-8",
            // Phase 5 will report real changes here; for now, never reload.
            body: "{\"changed\":false}".to_owned(),
        },
        other => Reply {
            status: 404,
            content_type: "text/plain; charset=utf-8",
            body: format!("not found: {other}"),
        },
    }
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
    fn root_serves_the_document() {
        let r = reply("/", "<html>DOC</html>");
        assert_eq!(r.status, 200);
        assert_eq!(r.content_type, "text/html; charset=utf-8");
        assert!(r.body.contains("DOC"));
    }

    #[test]
    fn index_and_empty_paths_serve_the_document() {
        assert_eq!(reply("/index.html", "D").status, 200);
        assert_eq!(reply("", "D").status, 200);
    }

    #[test]
    fn query_string_is_ignored() {
        assert_eq!(reply("/?t=123", "D").status, 200);
    }

    #[test]
    fn reload_endpoint_is_a_stub() {
        let r = reply(RELOAD_PATH, "D");
        assert_eq!(r.status, 200);
        assert_eq!(r.content_type, "application/json; charset=utf-8");
        assert!(r.body.contains("\"changed\":false"));
    }

    #[test]
    fn unknown_path_is_404() {
        let r = reply("/nope", "D");
        assert_eq!(r.status, 404);
        assert!(r.body.contains("not found"));
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
}
