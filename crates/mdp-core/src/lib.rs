//! mdp-core: the display-agnostic heart of the Markdown previewer.
//!
//! The load-bearing contract (design section 2) is a single pure transform:
//!
//! ```
//! use mdp_core::{render_document, RenderOptions};
//! let doc = render_document("# Hello", &RenderOptions::default());
//! assert!(doc.html.contains("<h1"));
//! ```
//!
//! [`render_document`] turns Markdown into a fully self-contained HTML page
//! (all CSS and JS inlined, zero external references) that every frontend
//! consumes byte-for-byte. There is no windowing, async, or I/O here beyond
//! reading embedded assets.
//!
//! Phase 1 scope (design section 9): CommonMark plus GFM (tables, task lists,
//! strikethrough, autolink, tagfilter, footnotes, alerts), ammonia
//! sanitization, and the token-based neutral default theme. Syntax
//! highlighting, math, and Mermaid are Phase 2 and are intentionally absent;
//! code fences render as plain `<pre><code>` styled by the theme.

use std::borrow::Cow;

use rust_embed::RustEmbed;

/// Embedded shell/theme assets. In debug builds rust-embed reads these from
/// disk (fast iteration); in release they are folded into the binary
/// (design section 5). The path is relative to this crate's Cargo.toml.
#[derive(RustEmbed)]
#[folder = "assets/"]
struct Assets;

/// The one deterministic product of the core: a self-contained HTML page.
#[derive(Debug, Clone)]
pub struct RenderedDoc {
    /// The complete HTML document, with all CSS and JS inlined.
    pub html: String,
    /// The resolved document title (used for the browser/window title).
    pub title: String,
}

/// Which color scheme the rendered page should present.
///
/// [`ColorScheme::Auto`] follows the viewer's OS preference via
/// `prefers-color-scheme`; the others force a scheme by stamping
/// `data-color-scheme` on the root element, which the theme tokens honor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorScheme {
    /// Follow the OS preference (the default).
    #[default]
    Auto,
    /// Force light.
    Light,
    /// Force dark.
    Dark,
}

/// Options controlling [`render_document`]. Cheap to clone; all fields have
/// sensible defaults.
#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    /// Document title. When `None`, a neutral default is used.
    pub title: Option<String>,
    /// Color scheme for the rendered page.
    pub color_scheme: ColorScheme,
    /// Optional user theme CSS layered *after* the built-in default theme, so
    /// it need only redefine the tokens it wants to override (design section
    /// 4.6). This is the `--theme` rebrand surface; Phase 1 wires the plumbing.
    pub extra_theme_css: Option<String>,
}

/// The default document title when none is supplied.
const DEFAULT_TITLE: &str = "Markdown Preview";

/// Render Markdown into a fully self-contained HTML document.
///
/// Pipeline (design section 4.1): comrak (GFM extensions, raw HTML disabled)
/// -> ammonia sanitize (allowlist tuned to comrak's output) -> assemble a
/// self-contained page with the inlined token-based theme and app shell.
pub fn render_document(markdown: &str, opts: &RenderOptions) -> RenderedDoc {
    let dirty = markdown_to_html(markdown);
    let clean = sanitize(&dirty);
    let title = opts.title.clone().unwrap_or_else(|| DEFAULT_TITLE.to_string());
    let html = assemble_page(&clean, &title, opts);
    RenderedDoc { html, title }
}

/// Parse and render Markdown with comrak. Raw inline HTML stays disabled
/// (`render.unsafe_` off), so `<script>` and friends are escaped to text and
/// the tagfilter neutralizes the dangerous GFM tag set. Math is left as
/// `$...$` wrappers for a future KaTeX pass; comrak does not render it.
fn markdown_to_html(markdown: &str) -> String {
    let mut options = comrak::Options::default();

    // GFM + spec-required extensions (design section 4.1).
    options.extension.table = true;
    options.extension.tasklist = true;
    options.extension.strikethrough = true;
    options.extension.autolink = true;
    options.extension.tagfilter = true;
    options.extension.footnotes = true;
    options.extension.alerts = true;

    // render.unsafe_ stays false (the default): raw HTML is escaped/suppressed.

    comrak::markdown_to_html(markdown, &options)
}

/// The authoritative security step (design section 4.1/4.6). ammonia rebuilds
/// the DOM through html5ever and keeps only an explicit allowlist. It is tuned
/// to preserve exactly what comrak emits for the enabled features:
///
/// - task-list checkboxes: `input[type=checkbox][checked][disabled]`
/// - GFM alerts: `div.markdown-alert` + `p.markdown-alert-title`
/// - footnotes: `section.footnotes`, `sup.footnote-ref`, `a.footnote-backref`,
///   and the `id`/`href` fragment links that stitch refs to definitions
/// - tables with per-column alignment
/// - fenced-code language classes (`language-*`, including `language-mermaid`)
///
/// Everything else (`script`, `style`, event handlers, `javascript:` URLs,
/// `iframe`, ...) is stripped.
fn sanitize(html: &str) -> String {
    let mut builder = ammonia::Builder::default();

    builder
        // Tags beyond ammonia's defaults that comrak's feature output needs.
        .add_tags(["input", "section"])
        // class/id are safe, non-scripting attributes and are what carry the
        // feature semantics (alert type, footnote linking, code language).
        .add_generic_attributes(["class", "id"])
        // Task-list checkbox attributes.
        .add_tag_attributes("input", ["type", "checked", "disabled"])
        // Table column alignment.
        .add_tag_attributes("th", ["align"])
        .add_tag_attributes("td", ["align"])
        // Footnote anchors use in-document fragment links; let them pass.
        .url_relative(ammonia::UrlRelative::PassThrough);

    builder.clean(html).to_string()
}

/// Assemble the sanitized body into a self-contained HTML page: the inlined
/// token-based default theme, any layered user theme, the app-shell CSS, and
/// the tiny shell script. The `data-mdp="..."` markers let tests redact the
/// large asset blobs (design section 7) while keeping the body reviewable.
fn assemble_page(body: &str, title: &str, opts: &RenderOptions) -> String {
    let theme_css = load_asset("theme.css");
    let shell_css = load_asset("shell.css");
    let shell_js = load_asset("shell.js");

    let scheme_attr = match opts.color_scheme {
        ColorScheme::Auto => String::new(),
        ColorScheme::Light => " data-color-scheme=\"light\"".to_string(),
        ColorScheme::Dark => " data-color-scheme=\"dark\"".to_string(),
    };

    let user_theme = match &opts.extra_theme_css {
        Some(css) => format!("<style data-mdp=\"user-theme\">{css}</style>\n"),
        None => String::new(),
    };

    format!(
        "<!doctype html>\n\
<html lang=\"en\"{scheme_attr}>\n\
<head>\n\
<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<title>{title}</title>\n\
<style data-mdp=\"theme\">{theme_css}</style>\n\
{user_theme}<style data-mdp=\"shell\">{shell_css}</style>\n\
</head>\n\
<body>\n\
<main class=\"mdp-document\">\n\
{body}</main>\n\
<script data-mdp=\"shell\">{shell_js}</script>\n\
</body>\n\
</html>\n",
        scheme_attr = scheme_attr,
        title = escape_title(title),
        theme_css = theme_css,
        user_theme = user_theme,
        shell_css = shell_css,
        body = body,
        shell_js = shell_js,
    )
}

/// Load an embedded text asset. Panics only if a compiled-in asset is missing,
/// which is a build/packaging bug, not a runtime input error.
fn load_asset(name: &str) -> Cow<'static, str> {
    let file = Assets::get(name)
        .unwrap_or_else(|| panic!("embedded asset missing: {name} (packaging bug)"));
    match file.data {
        Cow::Borrowed(bytes) => Cow::Borrowed(
            std::str::from_utf8(bytes).expect("embedded asset is not valid UTF-8"),
        ),
        Cow::Owned(bytes) => {
            Cow::Owned(String::from_utf8(bytes).expect("embedded asset is not valid UTF-8"))
        }
    }
}

/// Minimal escaping for text placed inside `<title>`.
fn escape_title(title: &str) -> String {
    title
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_are_auto_and_untitled() {
        let opts = RenderOptions::default();
        assert_eq!(opts.color_scheme, ColorScheme::Auto);
        assert!(opts.title.is_none());
        assert!(opts.extra_theme_css.is_none());
    }

    #[test]
    fn title_defaults_when_absent() {
        let doc = render_document("# hi", &RenderOptions::default());
        assert_eq!(doc.title, DEFAULT_TITLE);
        assert!(doc.html.contains("<title>Markdown Preview</title>"));
    }

    #[test]
    fn title_option_is_used_and_escaped() {
        let opts = RenderOptions {
            title: Some("A & B <ok>".to_string()),
            ..Default::default()
        };
        let doc = render_document("hi", &opts);
        assert_eq!(doc.title, "A & B <ok>");
        assert!(doc.html.contains("<title>A &amp; B &lt;ok&gt;</title>"));
    }

    #[test]
    fn auto_scheme_stamps_no_attribute() {
        let doc = render_document("hi", &RenderOptions::default());
        // The opening <html> tag carries no scheme attribute in auto mode.
        // (The token CSS itself references data-color-scheme in its forced
        // selectors, so check the tag precisely rather than the whole page.)
        assert!(doc
            .html
            .contains("<!doctype html>\n<html lang=\"en\">\n"));
    }

    #[test]
    fn forced_schemes_stamp_the_root() {
        let dark = render_document(
            "hi",
            &RenderOptions {
                color_scheme: ColorScheme::Dark,
                ..Default::default()
            },
        );
        assert!(dark.html.contains("<html lang=\"en\" data-color-scheme=\"dark\">"));

        let light = render_document(
            "hi",
            &RenderOptions {
                color_scheme: ColorScheme::Light,
                ..Default::default()
            },
        );
        assert!(light
            .html
            .contains("<html lang=\"en\" data-color-scheme=\"light\">"));
    }

    #[test]
    fn extra_theme_css_is_layered_after_default() {
        let marker = ".sentinel{color:red}";
        let opts = RenderOptions {
            extra_theme_css: Some(marker.to_string()),
            ..Default::default()
        };
        let doc = render_document("hi", &opts);
        assert!(doc.html.contains(marker));
        // Must come after the default theme block so it overrides it.
        let default_at = doc.html.find("data-mdp=\"theme\"").unwrap();
        let user_at = doc.html.find("data-mdp=\"user-theme\"").unwrap();
        assert!(default_at < user_at, "user theme must layer after default");
    }

    #[test]
    fn no_user_theme_block_when_none() {
        let doc = render_document("hi", &RenderOptions::default());
        assert!(!doc.html.contains("data-mdp=\"user-theme\""));
    }
}
