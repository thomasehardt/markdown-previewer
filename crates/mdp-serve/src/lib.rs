//! Local HTTP server frontend (tiny_http + system browser).
//!
//! Phase 1 compiling stub (design section 9). The `127.0.0.1:0` server, the
//! reload-signal endpoint, and the automatic webview-to-serve fallback arrive
//! in Phase 3.

use mdp_core::RenderedDoc;

/// Serve the already-rendered document on an ephemeral local port and return
/// the bound URL. Not yet implemented in Phase 1.
pub fn serve(_doc: &RenderedDoc, _open_browser: bool) -> Result<String, FrontendError> {
    Err(FrontendError::NotImplemented)
}

/// Errors a frontend can report back to the CLI.
#[derive(Debug)]
pub enum FrontendError {
    /// The frontend is a Phase 1 placeholder.
    NotImplemented,
}

impl std::fmt::Display for FrontendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrontendError::NotImplemented => {
                f.write_str("the serve frontend is not implemented yet")
            }
        }
    }
}

impl std::error::Error for FrontendError {}
