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
//! Scope (design section 9): CommonMark plus GFM (tables, task lists,
//! strikethrough, autolink, tagfilter, footnotes, alerts), ammonia
//! sanitization, and the token-based neutral default theme (Phase 1); plus
//! server-side syntect syntax highlighting, KaTeX math (base64-inlined fonts),
//! and lazy Mermaid diagrams (Phase 2). Math and Mermaid assets are injected
//! only when the document actually uses them, so plain documents stay lean.
//!
//! The [`watch`] submodule adds cross-platform file watching (design section
//! 4.3); it is the one stateful, I/O-touching part of this otherwise pure crate.

pub mod watch;

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::{self, Write as _};
use std::sync::OnceLock;

use comrak::adapters::SyntaxHighlighterAdapter;
use rust_embed::RustEmbed;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::html::{css_for_theme_with_class_style, ClassStyle, ClassedHTMLGenerator};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// CSS class prefix for every syntect-emitted highlight span. Prefixing keeps
/// the highlighter's generic scope atoms (`keyword`, `string`, ...) from
/// colliding with the shell's own class names, and lets the generated theme
/// CSS target exactly these spans. It must be a `&'static str` (syntect API).
const HL_PREFIX: &str = "mdp-hl-";

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

/// One entry in the multi-file navigation bar (multi-file preview).
#[derive(Debug, Clone)]
pub struct NavItem {
    /// Display label (typically the file name).
    pub label: String,
    /// Frontend-specific URL that switches to this document.
    pub href: String,
    /// Whether this is the document currently shown.
    pub current: bool,
}

/// A source of one or more documents a frontend can render on demand. The CLI
/// implements it over the file list; a frontend calls [`DocSource::render`] for
/// the requested index, passing a closure that builds that frontend's own
/// switch URLs so the file-nav bar links stay frontend-appropriate.
pub trait DocSource: Send + Sync {
    /// Number of documents available.
    fn count(&self) -> usize;
    /// Render the `index`-th document. `href(j)` yields the URL that switches to
    /// document `j` in the calling frontend.
    fn render(&self, index: usize, href: &dyn Fn(usize) -> String) -> RenderedDoc;
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
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Document title. When `None`, a neutral default is used.
    pub title: Option<String>,
    /// Color scheme for the rendered page.
    pub color_scheme: ColorScheme,
    /// Optional user theme CSS layered *after* the built-in default theme, so
    /// it need only redefine the tokens it wants to override (design section
    /// 4.6). This is the `--theme` rebrand surface; Phase 1 wires the plumbing.
    pub extra_theme_css: Option<String>,
    /// Start showing the read-only raw-source view instead of the rendered view
    /// (design section 4.2). The in-page toggle switches between them regardless.
    pub view_source_default: bool,
    /// Include interactive chrome: the view-source toggle and the reload banner.
    /// Disabled for PDF export so the captured page is just the document.
    pub show_chrome: bool,
    /// Multi-file navigation entries. With two or more (and chrome shown), a
    /// file switcher bar is rendered. Empty for a single file.
    pub file_nav: Vec<NavItem>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            title: None,
            color_scheme: ColorScheme::Auto,
            extra_theme_css: None,
            view_source_default: false,
            show_chrome: true,
            file_nav: Vec::new(),
        }
    }
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
    let title = opts
        .title
        .clone()
        .unwrap_or_else(|| DEFAULT_TITLE.to_string());
    let html = assemble_page(&clean, markdown, &title, opts);
    RenderedDoc { html, title }
}

/// Parse and render Markdown with comrak. Raw inline HTML stays disabled
/// (`render.unsafe_` off), so `<script>` and friends are escaped to text and
/// the tagfilter neutralizes the dangerous GFM tag set.
///
/// Two Phase-2 behaviors are wired here:
///
/// - `math_dollars` turns `$x$` / `$$x$$` into
///   `<span data-math-style="inline|display">x</span>` (the `$` delimiters are
///   consumed, so client-side rendering keys off `data-math-style`, not the
///   dollar signs; see `assets/shell.js`). comrak does not render math itself.
/// - A syntect [`SyntectAdapter`] highlights fenced code server-side into
///   class-based spans, with `mermaid` fences passed through untouched for the
///   client-side diagram step.
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
    // Inline + display math passthrough for KaTeX (design section 4.1). Only
    // `math_dollars`; `math_code` (```math fences) is intentionally left off so
    // the single client selector `[data-math-style]` covers all math.
    options.extension.math_dollars = true;
    // Header ids (empty prefix) so table-of-contents links resolve to headings.
    // comrak puts id="slug" on the <hN> and adds an empty .anchor link inside.
    options.extension.header_id_prefix = Some(String::new());

    // render.unsafe_ stays false (the default): raw HTML is escaped/suppressed.

    let highlighter = highlighter();
    let adapter = SyntectAdapter { hl: highlighter };
    let mut plugins = comrak::options::Plugins::default();
    plugins.render.codefence_syntax_highlighter = Some(&adapter);

    comrak::markdown_to_html_with_plugins(markdown, &options, &plugins)
}

/// Cached, process-wide syntect syntax and theme sets. Loading the default
/// syntax/theme dumps is the expensive part of highlighting, so it happens
/// exactly once (design section 4.1: cache assembled per-process state).
struct Highlighter {
    syntaxes: SyntaxSet,
    /// Light-scheme theme; its span colors apply at top level.
    light: Theme,
    /// Dark-scheme theme; its span colors apply under the dark scope selectors.
    dark: Theme,
}

fn highlighter() -> &'static Highlighter {
    static H: OnceLock<Highlighter> = OnceLock::new();
    H.get_or_init(|| {
        let syntaxes = SyntaxSet::load_defaults_newlines();
        let themes = ThemeSet::load_defaults();
        // Two built-in themes whose backgrounds are close to the default
        // theme's --code-bg tokens, so the foreground colors read correctly
        // against the token-driven code surface without syntect owning the
        // background (which would fight the theme layer, design section 4.6).
        let light = themes.themes["InspiredGitHub"].clone();
        let dark = themes.themes["base16-ocean.dark"].clone();
        Highlighter {
            syntaxes,
            light,
            dark,
        }
    })
}

/// comrak codefence highlighter backed by pure-Rust syntect. It emits
/// class-based spans (prefixed with [`HL_PREFIX`]); the matching colors ship as
/// inlined CSS from [`syntect_theme_css`]. `mermaid` fences are passed through
/// as an escaped `<code class="language-mermaid">` body for the client-side
/// Mermaid step (design section 4.1, step 2).
struct SyntectAdapter {
    hl: &'static Highlighter,
}

impl SyntectAdapter {
    /// Highlight `code` in `lang` into class-based `<span>`s. Falls back to
    /// plaintext syntax for unknown languages, and to a plainly escaped block
    /// if the highlighter errors (so code is never dropped).
    fn highlight(&self, lang: &str, code: &str) -> String {
        let syntax = self
            .hl
            .syntaxes
            .find_syntax_by_token(lang)
            .unwrap_or_else(|| self.hl.syntaxes.find_syntax_plain_text());
        let mut generator = ClassedHTMLGenerator::new_with_class_style(
            syntax,
            &self.hl.syntaxes,
            ClassStyle::SpacedPrefixed { prefix: HL_PREFIX },
        );
        for line in LinesWithEndings::from(code) {
            if generator
                .parse_html_for_line_which_includes_newline(line)
                .is_err()
            {
                return escape_html_text(code);
            }
        }
        generator.finalize()
    }
}

impl SyntaxHighlighterAdapter for SyntectAdapter {
    fn write_highlighted(
        &self,
        output: &mut dyn fmt::Write,
        lang: Option<&str>,
        code: &str,
    ) -> fmt::Result {
        let lang = lang.unwrap_or("").trim();
        if lang.eq_ignore_ascii_case("mermaid") {
            // Pass the diagram source through verbatim (escaped) so the
            // client-side Mermaid step can read it as text.
            return output.write_str(&escape_html_text(code));
        }
        output.write_str(&self.highlight(lang, code))
    }

    fn write_pre_tag(
        &self,
        output: &mut dyn fmt::Write,
        attributes: HashMap<&'static str, Cow<'_, str>>,
    ) -> fmt::Result {
        write_open_tag(output, "pre", &attributes)
    }

    fn write_code_tag(
        &self,
        output: &mut dyn fmt::Write,
        attributes: HashMap<&'static str, Cow<'_, str>>,
    ) -> fmt::Result {
        // Preserve comrak's `class="language-*"` (carries `language-mermaid`
        // and the fence language for both styling and downstream steps).
        write_open_tag(output, "code", &attributes)
    }
}

/// Write an opening tag with its attributes in a deterministic (key-sorted)
/// order so output is stable for snapshot tests. Attribute values are escaped.
fn write_open_tag(
    output: &mut dyn fmt::Write,
    tag: &str,
    attributes: &HashMap<&'static str, Cow<'_, str>>,
) -> fmt::Result {
    write!(output, "<{tag}")?;
    let mut keys: Vec<&&str> = attributes.keys().collect();
    keys.sort();
    for key in keys {
        write!(output, " {key}=\"{}\"", escape_attr(&attributes[*key]))?;
    }
    output.write_str(">")
}

/// Escape text for placement inside an element body.
fn escape_html_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Escape text for placement inside a double-quoted attribute value.
fn escape_attr(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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
/// - syntect highlight spans: `<span>` (already a default tag) carrying the
///   `mdp-hl-*` classes (the generic `class` attribute preserves them)
/// - math wrappers: `<span data-math-style="inline|display">` from comrak's
///   `math_dollars`, which KaTeX auto-render keys off client-side
///
/// Everything else (`script`, `style`, event handlers, `javascript:` URLs,
/// `iframe`, ...) is stripped. Mermaid's `<svg>` is generated client-side
/// *after* sanitize, so no inline-svg allowlist is needed; only the mermaid
/// code block must survive here, which it does as `<pre><code>` + `class`.
fn sanitize(html: &str) -> String {
    let mut builder = ammonia::Builder::default();

    builder
        // Tags beyond ammonia's defaults that comrak's feature output needs.
        .add_tags(["input", "section"])
        // class/id are safe, non-scripting attributes and are what carry the
        // feature semantics (alert type, footnote linking, code language, and
        // the syntect `mdp-hl-*` highlight classes).
        .add_generic_attributes(["class", "id"])
        // Task-list checkbox attributes.
        .add_tag_attributes("input", ["type", "checked", "disabled"])
        // comrak math_dollars wrapper: display/inline marker on the math span.
        // This is the exact hook KaTeX auto-render selects on client-side.
        .add_tag_attributes("span", ["data-math-style"])
        // Table column alignment.
        .add_tag_attributes("th", ["align"])
        .add_tag_attributes("td", ["align"])
        // Footnote anchors use in-document fragment links; let them pass.
        .url_relative(ammonia::UrlRelative::PassThrough);

    builder.clean(html).to_string()
}

/// Assemble the sanitized body into a self-contained HTML page: the inlined
/// token-based default theme, any layered user theme, the app-shell CSS/JS,
/// and - only when the document actually uses them - the syntect highlight
/// theme CSS, KaTeX (base64-inlined fonts, JS), and Mermaid (design section
/// 4.1, step 3). Injecting per-feature keeps a plain document's output lean.
///
/// The `data-mdp="..."` markers let tests redact the large asset blobs (design
/// section 7) while keeping the Markdown-derived body reviewable.
fn assemble_page(body: &str, source_md: &str, title: &str, opts: &RenderOptions) -> String {
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

    // Feature detection over the sanitized body. These substrings are produced
    // only by the pipeline (highlight prefix, comrak math marker, the mermaid
    // code class), never by arbitrary user prose, so they are safe triggers.
    let has_highlight = body.contains(HL_PREFIX);
    let has_math = body.contains("data-math-style");
    let has_mermaid = body.contains("language-mermaid");

    // Head extras: highlight CSS, then KaTeX (style + lib). KaTeX's script is
    // in <head> so its `katex` global is defined before the body-end shell
    // script that renders the math.
    let mut head_extra = String::new();
    if has_highlight {
        let _ = writeln!(
            head_extra,
            "<style data-mdp=\"syntect\">{}</style>",
            syntect_theme_css()
        );
    }
    if has_math {
        if let Some(block) = katex_assets() {
            head_extra.push_str(block);
        }
    }

    // Body-end extras: Mermaid lib + init, only when a mermaid fence exists
    // (design section 5: lazy Mermaid init). Placed before the shell script.
    let mut body_extra = String::new();
    if has_mermaid {
        if let Some(block) = mermaid_assets() {
            body_extra.push_str(block);
        }
    }

    let body_class = if opts.view_source_default {
        " class=\"mdp-view-source-active\""
    } else {
        ""
    };
    let source = escape_html_text(source_md);

    // Interactive chrome (view-source toggle + reload banner). Omitted when
    // show_chrome is off, e.g. PDF export captures just the document.
    let chrome = if opts.show_chrome {
        // A file switcher bar when previewing multiple files.
        let files_bar = build_files_bar(&opts.file_nav);
        // A Contents toggle + TOC sidebar only when the document has headings.
        let (toc_nav, contents_btn) = match build_toc(body) {
            Some(nav) => (
                nav,
                "<button type=\"button\" id=\"mdp-toc-toggle\" class=\"mdp-view-toggle\" aria-pressed=\"false\">Contents</button>",
            ),
            None => (String::new(), ""),
        };
        format!(
            "{files_bar}<div id=\"mdp-reload-banner\" class=\"mdp-reload-banner\" role=\"status\" hidden>This file changed on disk. <button type=\"button\" id=\"mdp-reload-accept\">Reload</button> <button type=\"button\" id=\"mdp-reload-dismiss\">Dismiss</button></div>\n\
{toc_nav}\
<div class=\"mdp-toolbar\" role=\"toolbar\" aria-label=\"Preview settings\">\
{contents_btn}\
<label class=\"mdp-ctl\">Theme <select id=\"mdp-theme\"><option value=\"default\">Default</option><option value=\"sepia\">Sepia</option><option value=\"slate\">Slate</option></select></label>\
<label class=\"mdp-ctl\">Scheme <select id=\"mdp-scheme\"><option value=\"auto\">Auto</option><option value=\"light\">Light</option><option value=\"dark\">Dark</option></select></label>\
<label class=\"mdp-ctl\">Width <select id=\"mdp-width\"><option value=\"comfortable\">Comfortable</option><option value=\"wide\">Wide</option><option value=\"full\">Full</option></select></label>\
<button type=\"button\" id=\"mdp-view-toggle\" class=\"mdp-view-toggle\" aria-pressed=\"false\">View source</button>\
</div>\n"
        )
    } else {
        String::new()
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
{head_extra}</head>\n\
<body{body_class}>\n\
{chrome}<main class=\"mdp-document\">\n\
{body}</main>\n\
<pre id=\"mdp-source\" class=\"mdp-source\" aria-label=\"Markdown source\"><code>{source}</code></pre>\n\
{body_extra}<script data-mdp=\"shell\">{shell_js}</script>\n\
</body>\n\
</html>\n",
        scheme_attr = scheme_attr,
        body_class = body_class,
        chrome = chrome,
        source = source,
        title = escape_title(title),
        theme_css = theme_css,
        user_theme = user_theme,
        shell_css = shell_css,
        head_extra = head_extra,
        body = body,
        body_extra = body_extra,
        shell_js = shell_js,
    )
}

/// A heading extracted from the rendered body, for the table of contents.
struct Heading {
    level: u8,
    id: String,
    text: String,
}

/// Build a table-of-contents `<nav>` from the headings in `html` (which carry
/// ids from comrak's `header_ids`). Returns `None` when there are no headings.
fn build_toc(html: &str) -> Option<String> {
    let headings = extract_headings(html);
    if headings.is_empty() {
        return None;
    }
    let mut nav = String::from(
        "<nav class=\"mdp-toc\" aria-label=\"Table of contents\"><div class=\"mdp-toc-title\">Contents</div><ul>",
    );
    for h in &headings {
        let _ = write!(
            nav,
            "<li class=\"mdp-toc-l{}\"><a href=\"#{}\">{}</a></li>",
            h.level, h.id, h.text
        );
    }
    nav.push_str("</ul></nav>");
    Some(nav)
}

/// Build the file switcher bar from `items`. Empty for fewer than two files.
fn build_files_bar(items: &[NavItem]) -> String {
    if items.len() < 2 {
        return String::new();
    }
    let mut s = String::from("<nav class=\"mdp-files\" aria-label=\"Files\"><ul>");
    for item in items {
        let cls = if item.current {
            "mdp-file mdp-file-current"
        } else {
            "mdp-file"
        };
        let _ = write!(
            s,
            "<li class=\"{cls}\"><a href=\"{}\">{}</a></li>",
            escape_attr(&item.href),
            escape_html_text(&item.label)
        );
    }
    s.push_str("</ul></nav>");
    s
}

/// Scan rendered HTML for `<h1>`..`<h6>` elements carrying an `id`, returning
/// their level, id, and plain-text label (inner tags stripped).
fn extract_headings(html: &str) -> Vec<Heading> {
    let mut out = Vec::new();
    let mut search_from = 0;
    while let Some(rel) = html[search_from..].find("<h") {
        let start = search_from + rel;
        let level_pos = start + 2;
        let level = match html.as_bytes().get(level_pos).copied() {
            Some(b) if (b'1'..=b'6').contains(&b) => b - b'0',
            _ => {
                search_from = level_pos.min(html.len());
                continue;
            }
        };
        let Some(gt_rel) = html[level_pos..].find('>') else {
            break;
        };
        let tag_end = level_pos + gt_rel;
        let open_tag = &html[start..=tag_end];
        let close = format!("</h{level}>");
        let Some(close_rel) = html[tag_end + 1..].find(&close) else {
            search_from = tag_end + 1;
            continue;
        };
        let inner_end = tag_end + 1 + close_rel;
        let inner = &html[tag_end + 1..inner_end];
        if let Some(id) = attr_value(open_tag, "id") {
            out.push(Heading {
                level,
                id,
                text: strip_tags(inner).trim().to_string(),
            });
        }
        search_from = inner_end + close.len();
    }
    out
}

/// Read the value of `name="..."` from an opening tag, if present.
fn attr_value(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Remove `<...>` tags from a fragment, keeping the (already-escaped) text.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// The inlined syntect highlight theme CSS, built once. The light theme's span
/// colors apply at top level; the dark theme's colors are re-scoped under the
/// dark-scheme selectors (auto via `prefers-color-scheme`, or forced via
/// `data-color-scheme="dark"`) so highlighting tracks the page color scheme.
fn syntect_theme_css() -> &'static str {
    static CSS: OnceLock<String> = OnceLock::new();
    CSS.get_or_init(|| {
        let hl = highlighter();
        let style = ClassStyle::SpacedPrefixed { prefix: HL_PREFIX };
        let light = css_for_theme_with_class_style(&hl.light, style).unwrap_or_default();
        let dark = css_for_theme_with_class_style(&hl.dark, style).unwrap_or_default();
        let dark_auto = prefix_selectors(&dark, ":root:not([data-color-scheme=\"light\"])");
        let dark_forced = prefix_selectors(&dark, ":root[data-color-scheme=\"dark\"]");
        format!("{light}\n@media (prefers-color-scheme: dark) {{\n{dark_auto}}}\n{dark_forced}")
    })
}

/// Re-scope a syntect-generated stylesheet under an ancestor selector by
/// prepending `ancestor` to every selector in every rule. The leading theme
/// comment is dropped. This lets the dark theme's rules win (higher
/// specificity) only when the ancestor matches, without touching the flat
/// light-theme rules.
fn prefix_selectors(css: &str, ancestor: &str) -> String {
    // Drop syntect's leading `/* ... */` comment if present.
    let body = match css.find("*/") {
        Some(i) => &css[i + 2..],
        None => css,
    };
    let mut out = String::new();
    for rule in body.split_inclusive('}') {
        let Some(brace) = rule.find('{') else {
            continue; // trailing whitespace after the last rule
        };
        let (selectors, block) = rule.split_at(brace);
        let scoped: Vec<String> = selectors
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| format!("{ancestor} {s}"))
            .collect();
        if scoped.is_empty() {
            continue;
        }
        out.push_str(&scoped.join(", "));
        out.push(' ');
        out.push_str(block);
        out.push('\n');
    }
    out
}

/// The assembled KaTeX head block (inlined stylesheet with base64 fonts, plus
/// the KaTeX library), built once. Returns `None` when the vendored assets are
/// not present (a fresh checkout that has not run scripts/vendor-assets.sh), so
/// the page still assembles - math simply stays as its `$`-free source text.
fn katex_assets() -> Option<&'static str> {
    static BLOCK: OnceLock<Option<String>> = OnceLock::new();
    BLOCK
        .get_or_init(|| {
            let css = try_load_asset("vendor/katex/katex.inlined.css")?;
            let js = try_load_asset("vendor/katex/katex.min.js")?;
            Some(format!(
                "<style data-mdp=\"katex\">{css}</style>\n\
<script data-mdp=\"katex-lib\">{js}</script>\n"
            ))
        })
        .as_deref()
}

/// The assembled Mermaid body block (library plus a conservative init), built
/// once. `startOnLoad:false` defers rendering to the shell script; the strict
/// security level blocks click handlers and sanitizes HTML labels (design
/// section 4.1). Returns `None` when the vendored bundle is absent.
fn mermaid_assets() -> Option<&'static str> {
    static BLOCK: OnceLock<Option<String>> = OnceLock::new();
    BLOCK
        .get_or_init(|| {
            let js = try_load_asset("vendor/mermaid/mermaid.min.js")?;
            Some(format!(
                "<script data-mdp=\"mermaid-lib\">{js}</script>\n\
<script data-mdp=\"mermaid-init\">mermaid.initialize({{startOnLoad:false,securityLevel:\"strict\"}});</script>\n"
            ))
        })
        .as_deref()
}

/// Load an embedded text asset. Panics only if a compiled-in asset is missing,
/// which is a build/packaging bug, not a runtime input error.
fn load_asset(name: &str) -> Cow<'static, str> {
    try_load_asset(name).unwrap_or_else(|| panic!("embedded asset missing: {name} (packaging bug)"))
}

/// Load an embedded text asset if present, returning `None` when it is absent.
/// Used for the optional vendored assets (KaTeX, Mermaid), which live in a
/// gitignored tree populated by scripts/vendor-assets.sh and may be missing on
/// a fresh checkout.
fn try_load_asset(name: &str) -> Option<Cow<'static, str>> {
    let file = Assets::get(name)?;
    Some(match file.data {
        Cow::Borrowed(bytes) => {
            Cow::Borrowed(std::str::from_utf8(bytes).expect("embedded asset is not valid UTF-8"))
        }
        Cow::Owned(bytes) => {
            Cow::Owned(String::from_utf8(bytes).expect("embedded asset is not valid UTF-8"))
        }
    })
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
        assert!(doc.html.contains("<!doctype html>\n<html lang=\"en\">\n"));
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
        assert!(dark
            .html
            .contains("<html lang=\"en\" data-color-scheme=\"dark\">"));

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

    #[test]
    fn view_source_pre_holds_escaped_raw_markdown() {
        let md = "# Hi\n\n<script>evil()</script> and `code`";
        let doc = render_document(md, &RenderOptions::default());
        assert!(doc.html.contains("<pre id=\"mdp-source\""));
        // The raw markdown appears as escaped text: tags shown, never live.
        assert!(doc.html.contains("&lt;script&gt;evil()&lt;/script&gt;"));
        assert!(doc.html.contains("# Hi"));
        // Read-only: no editing affordance anywhere in the page.
        assert!(!doc.html.contains("contenteditable"));
    }

    #[test]
    fn view_source_default_activates_source_view() {
        let on = render_document(
            "# x",
            &RenderOptions {
                view_source_default: true,
                ..Default::default()
            },
        );
        assert!(on.html.contains("<body class=\"mdp-view-source-active\">"));

        let off = render_document("# x", &RenderOptions::default());
        // Check the <body> tag itself: the class name also appears inside the
        // inlined toggle script, so a whole-page scan would be misleading.
        assert!(off.html.contains("<body>\n"));
        assert!(!off.html.contains("<body class=\"mdp-view-source-active\">"));
    }

    #[test]
    fn extract_headings_reads_level_id_and_text() {
        let hs = extract_headings(
            "<h1 id=\"a\">One</h1><p>x</p><h2 id=\"b\">Two <code>c</code></h2><hr>",
        );
        assert_eq!(hs.len(), 2);
        assert_eq!(
            (hs[0].level, hs[0].id.as_str(), hs[0].text.as_str()),
            (1, "a", "One")
        );
        assert_eq!(
            (hs[1].level, hs[1].id.as_str(), hs[1].text.as_str()),
            (2, "b", "Two c")
        );
    }

    #[test]
    fn toc_is_built_from_headings() {
        let doc = render_document(
            "# Title\n\n## Section A\n\ntext\n\n## Section B\n",
            &RenderOptions::default(),
        );
        assert!(doc.html.contains("<nav class=\"mdp-toc\""));
        assert!(doc.html.contains("Section A") && doc.html.contains("Section B"));
        assert!(doc.html.contains("id=\"mdp-toc-toggle\""));
        assert!(doc.html.contains("href=\"#"));
    }

    #[test]
    fn no_toc_without_headings() {
        let doc = render_document(
            "Just a paragraph, no headings.\n",
            &RenderOptions::default(),
        );
        assert!(!doc.html.contains("<nav class=\"mdp-toc\""));
        assert!(!doc.html.contains("id=\"mdp-toc-toggle\""));
    }

    #[test]
    fn no_toc_when_chrome_disabled() {
        let doc = render_document(
            "# H\n",
            &RenderOptions {
                show_chrome: false,
                ..Default::default()
            },
        );
        assert!(!doc.html.contains("<nav class=\"mdp-toc\""));
    }

    #[test]
    fn file_nav_bar_shows_for_multiple_files() {
        let opts = RenderOptions {
            file_nav: vec![
                NavItem {
                    label: "a.md".into(),
                    href: "/?doc=0".into(),
                    current: true,
                },
                NavItem {
                    label: "b.md".into(),
                    href: "/?doc=1".into(),
                    current: false,
                },
            ],
            ..Default::default()
        };
        let doc = render_document("# x", &opts);
        assert!(doc.html.contains("<nav class=\"mdp-files\""));
        assert!(doc.html.contains("a.md") && doc.html.contains("b.md"));
        assert!(doc.html.contains("mdp-file-current"));
        assert!(doc.html.contains("href=\"/?doc=1\""));
    }

    #[test]
    fn no_file_nav_bar_for_single_file() {
        let opts = RenderOptions {
            file_nav: vec![NavItem {
                label: "a.md".into(),
                href: "/?doc=0".into(),
                current: true,
            }],
            ..Default::default()
        };
        let doc = render_document("# x", &opts);
        assert!(!doc.html.contains("<nav class=\"mdp-files\""));
    }

    #[test]
    fn unknown_language_still_renders_the_code() {
        // An unknown fence language falls back to plaintext rather than dropping
        // the code, and keeps its language class.
        let doc = render_document(
            "```zzznotalang\nlet x = 1;\n```\n",
            &RenderOptions::default(),
        );
        assert!(doc.html.contains("let x = 1;"));
        assert!(doc.html.contains("language-zzznotalang"));
    }

    #[test]
    fn renders_all_features_and_chrome_branches_together() {
        // Exercise the whole pipeline and every chrome-assembly branch at once:
        // headings (TOC), math, mermaid, highlighted code, forced dark, a user
        // theme, view-source-default, and a multi-file nav bar.
        let md = "# Title\n\n## Section\n\nText with $a^2$ and $$b^2$$.\n\n\
| A | B |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n- [ ] todo\n\n\
> [!NOTE]\n> note\n\n```rust\nfn main() {}\n```\n\n\
```mermaid\ngraph TD; A-->B;\n```\n\nA footnote.[^f]\n\n[^f]: body\n";
        let opts = RenderOptions {
            title: Some("All".to_string()),
            color_scheme: ColorScheme::Dark,
            extra_theme_css: Some(":root{--accent:#123456}".to_string()),
            view_source_default: true,
            show_chrome: true,
            file_nav: vec![
                NavItem {
                    label: "a".into(),
                    href: "/?doc=0".into(),
                    current: true,
                },
                NavItem {
                    label: "b".into(),
                    href: "/?doc=1".into(),
                    current: false,
                },
            ],
        };
        let doc = render_document(md, &opts);
        assert!(doc.html.contains("data-math-style"));
        assert!(doc.html.contains("language-mermaid"));
        assert!(doc.html.contains("mdp-hl-"));
        assert!(doc.html.contains("<nav class=\"mdp-toc\""));
        assert!(doc.html.contains("<nav class=\"mdp-files\""));
        assert!(doc.html.contains("data-color-scheme=\"dark\""));
        assert!(doc.html.contains("--accent:#123456"));
        assert!(doc.html.contains("mdp-view-source-active"));
    }

    #[test]
    fn extract_headings_skips_headings_without_id() {
        let hs = extract_headings("<h2>No id here</h2><h3 id=\"y\">Yes</h3>");
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].id, "y");
    }

    #[test]
    fn extract_headings_tolerates_malformed_markup() {
        // A non-heading `<h` (<hr>), a bad level (<h9>), and an unclosed heading
        // must not derail extraction; only the well-formed h2 yields an entry.
        let hs = extract_headings(
            "<hr><h9 id=\"x\">nope</h9><h2 id=\"ok\">Good</h2><h3 id=\"z\">unclosed",
        );
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].id, "ok");
    }

    #[test]
    fn public_types_expose_debug_and_clone() {
        // Debug and Clone are part of the public API; exercise them.
        let opts = RenderOptions::default();
        let _ = format!("{:?}", opts.clone());
        for scheme in [ColorScheme::Auto, ColorScheme::Light, ColorScheme::Dark] {
            let _ = format!("{scheme:?}");
        }
        let item = NavItem {
            label: "a".into(),
            href: "/".into(),
            current: true,
        };
        let _ = format!("{:?}", item.clone());
        let doc = render_document("hi", &opts);
        let _ = format!("{:?}", doc.clone());
    }

    /// rust-embed reads `assets/` from disk in debug builds, so the gitignored
    /// `assets/vendor/` tree (populated by scripts/vendor-assets.sh) is visible
    /// to the same loader that serves the shell/theme files. .gitignore only
    /// affects git, not rust-embed's disk reads. This proves a vendored file is
    /// retrievable through the existing loader once vendoring has run.
    ///
    /// It skips (rather than fails) when the assets are not vendored yet, so a
    /// fresh checkout that has not run the vendor script still builds green.
    /// The vendoring itself is verified end-to-end by scripts/vendor-assets.sh.
    #[test]
    fn vendored_asset_is_retrievable_when_present() {
        let key = "vendor/mermaid/mermaid.min.js";
        match Assets::get(key) {
            None => {
                eprintln!("skipping: {key} not vendored yet (run scripts/vendor-assets.sh)");
            }
            Some(file) => {
                assert!(!file.data.is_empty(), "vendored {key} should not be empty");
                // mermaid.min.js is a multi-MB prebuilt UMD bundle.
                assert!(
                    file.data.len() > 1_000_000,
                    "vendored {key} is suspiciously small ({} bytes)",
                    file.data.len()
                );
                // It is UTF-8 JS, so the text loader path also works on it.
                let js = load_asset(key);
                assert!(
                    js.contains("mermaid"),
                    "vendored bundle should mention mermaid"
                );
            }
        }
    }

    /// The precomputed self-contained KaTeX stylesheet must carry zero relative
    /// or external font references (design section 4.1/5): every font is a
    /// base64 data: URI. Skips when not vendored yet.
    #[test]
    fn inlined_katex_css_is_self_contained_when_present() {
        let key = "vendor/katex/katex.inlined.css";
        match Assets::get(key) {
            None => {
                eprintln!("skipping: {key} not vendored yet (run scripts/vendor-assets.sh)");
            }
            Some(_) => {
                let css = load_asset(key);
                assert!(
                    !css.contains("url(fonts/"),
                    "inlined css must not reference relative font paths"
                );
                assert!(
                    !css.contains("http://") && !css.contains("https://"),
                    "inlined css must not reference external URLs"
                );
                assert!(
                    css.contains("data:font/woff2;base64,"),
                    "inlined css must embed fonts as base64 data URIs"
                );
            }
        }
    }
}
