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

/// The assembled shell: everything except the rendered Markdown body.
fn shell_only(html: &str) -> String {
    let start = html.find("<main").expect("no <main> in output");
    let end = html.find("</main>").expect("no </main> in output") + "</main>".len();
    format!("{}{}", &html[..start], &html[end..])
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
