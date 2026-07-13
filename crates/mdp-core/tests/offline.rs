//! Offline enforcement (design section 5): the assembled page must be fully
//! self-contained. Its shell (inlined CSS/JS/fonts and any asset references)
//! makes zero network calls: no `scheme://`, no protocol-relative `//host`,
//! no `<link>`, no CSS `url()`, and no resource-loading attributes.
//!
//! A normal Markdown document (no user-authored external URLs) must render to
//! a page with no external reference anywhere. When the *content* itself
//! carries links or remote images, those are user content, not shell asset
//! loads (inlining/blocking remote content is a later concern); the guarantee
//! we hold in Phase 1 is that the shell stays offline regardless.

use mdp_core::{render_document, RenderOptions};

/// A feature-rich document that contains no external URLs of its own.
const NO_EXTERNAL: &str = r#"# Offline Document

A paragraph with **strong** and `code`.

- one
- two

| A | B |
|---|---|
| 1 | 2 |

> [!NOTE]
> An alert.

A footnote.[^f]

[^f]: The note.

```rust
fn main() {}
```
"#;

/// A document whose content carries external URLs and a remote image.
const WITH_URLS: &str = r#"# Links

An autolink <https://example.com> and a [named link](http://example.org).

![alt](https://cdn.example.com/pic.png)
"#;

/// A document exercising both Phase 2 asset paths at once: math (KaTeX) and a
/// diagram (Mermaid). It carries no external URLs of its own.
const MATH_AND_MERMAID: &str = r#"# Math and Mermaid

Inline $\alpha + \beta = \gamma$ and a display block:

$$\frac{a}{b} = c$$

```mermaid
graph TD; A-->B;
```
"#;

/// The assembled shell: everything except user content. Both the rendered body
/// (`<main>`) and the raw-source view (`<pre id="mdp-source">`) may legitimately
/// carry user-authored URLs, so both are set aside before the offline scan.
fn shell_only(html: &str) -> String {
    let without_body = cut(html, "<main", "</main>");
    cut(&without_body, "<pre id=\"mdp-source\"", "</pre>")
}

/// Remove the first `open..close` element, inclusive of the closing marker.
fn cut(html: &str, open: &str, close: &str) -> String {
    match html.find(open) {
        Some(start) => {
            let end = html[start..]
                .find(close)
                .map(|i| start + i + close.len())
                .unwrap_or(html.len());
            format!("{}{}", &html[..start], &html[end..])
        }
        None => html.to_string(),
    }
}

/// Assert a fragment triggers no network fetch: no scheme, no protocol-relative
/// reference, and none of the resource-loading vectors.
fn assert_offline(fragment: &str, what: &str) {
    assert!(!fragment.contains("://"), "{what} contains a scheme:// reference");
    assert!(!fragment.contains("http"), "{what} contains an http reference");
    assert!(
        !fragment.contains("=\"//") && !fragment.contains("='//"),
        "{what} contains a protocol-relative reference"
    );
    assert!(!fragment.to_lowercase().contains("<link"), "{what} has a <link> element");
    assert!(!fragment.contains("url("), "{what} has a CSS url() reference");
    for attr in ["src=", "srcset=", "poster="] {
        assert!(!fragment.contains(attr), "{what} has a `{attr}` resource load");
    }
}

#[test]
fn plain_render_is_fully_self_contained() {
    let doc = render_document(NO_EXTERNAL, &RenderOptions::default());
    // The whole output, body included, must be offline for a normal document.
    assert_offline(&doc.html, "the full rendered page");
}

#[test]
fn shell_is_offline_even_when_content_has_links() {
    let doc = render_document(WITH_URLS, &RenderOptions::default());
    // User link text may legitimately appear in the body; the shell must not.
    assert_offline(&shell_only(&doc.html), "the assembled shell");
}

/// Remove the body of a marked asset block, keeping the surrounding tags. Used
/// to set aside the large vendored library blobs (KaTeX CSS/JS, Mermaid JS)
/// before scanning the rest of the page.
fn strip_block_body(html: &str, start: &str, end: &str) -> String {
    match html.find(start) {
        Some(i) => {
            let after = i + start.len();
            let rel_end = html[after..]
                .find(end)
                .expect("marked asset block was not terminated");
            format!("{}{}", &html[..after], &html[after + rel_end..])
        }
        None => html.to_string(),
    }
}

/// Fonts-render-not-boxes (design section 4.1/5): a document with math must
/// carry `@font-face` rules whose sources are base64 `data:` woff2 URIs, so the
/// glyphs are embedded and math renders as glyphs rather than tofu boxes. No
/// font may be pulled from an external URL. Skips if KaTeX is not vendored yet.
#[test]
fn math_fonts_are_embedded_as_base64_not_external() {
    let doc = render_document("Euler's identity: $e^{i\\pi} + 1 = 0$\n", &RenderOptions::default());
    let html = doc.html;
    if !html.contains(r#"data-mdp="katex""#) {
        eprintln!("skipping: KaTeX assets not vendored (run scripts/vendor-assets.sh)");
        return;
    }
    assert!(
        html.contains("@font-face"),
        "no @font-face rules; math would render as boxes"
    );
    assert!(
        html.contains("data:font/woff2;base64,"),
        "fonts are not embedded as base64 data: URIs"
    );
    assert!(
        !html.contains("src:url(http") && !html.contains("src: url(http"),
        "a font is loaded from an external URL"
    );
}

/// The offline-URL scan must stay green on a document containing BOTH math and
/// a mermaid diagram (design section 5). The vendored KaTeX/Mermaid libraries
/// legitimately embed XML-namespace identifiers (e.g. the SVG namespace URI)
/// and base64 font blobs; their offline-ness is enforced by the vendor pipeline
/// and the KaTeX self-containment/font tests. We set those trusted blobs aside,
/// then prove our own assembly, the generated highlight CSS, the mermaid init,
/// and the shell add zero external references.
#[test]
fn math_and_mermaid_page_makes_no_network_reference() {
    let doc = render_document(MATH_AND_MERMAID, &RenderOptions::default());
    let mut html = doc.html;
    if !html.contains(r#"data-mdp="katex""#) || !html.contains(r#"data-mdp="mermaid-lib""#) {
        eprintln!("skipping: KaTeX/Mermaid assets not vendored (run scripts/vendor-assets.sh)");
        return;
    }
    html = strip_block_body(&html, r#"<style data-mdp="katex">"#, "</style>");
    html = strip_block_body(&html, r#"<script data-mdp="katex-lib">"#, "</script>");
    html = strip_block_body(&html, r#"<script data-mdp="mermaid-lib">"#, "</script>");
    assert_offline(&html, "the math+mermaid page (vendored libraries aside)");
}
