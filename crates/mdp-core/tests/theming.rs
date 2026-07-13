//! A `--theme` override must restyle without restructuring: it changes only
//! token values (via the injected user-theme block), never the document body.
//! This is the guarantee behind the semantic-token theming layer (design 4.6).

use mdp_core::{render_document, RenderOptions};

/// Return the substring from `<main` to `</main>` (the rendered document body).
fn body(html: &str) -> &str {
    let start = html.find("<main").expect("a <main> element");
    let end = html.find("</main>").expect("a </main> close");
    &html[start..end]
}

#[test]
fn theme_override_changes_tokens_not_structure() {
    let md = "# Title\n\nSome **text**, a [link](https://example.com), and a list:\n\n- one\n- two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";

    let base = render_document(md, &RenderOptions::default());
    let themed = render_document(
        md,
        &RenderOptions {
            extra_theme_css: Some(":root{--accent:#c0ffee;--surface-page:#010203}".to_string()),
            ..Default::default()
        },
    );

    // The rendered body is byte-for-byte identical: theming is presentation only.
    assert_eq!(
        body(&base.html),
        body(&themed.html),
        "a theme must not alter document structure"
    );

    // The themed output carries the layered user-theme block with the overrides;
    // the base output has no user-theme block at all.
    assert!(themed.html.contains("data-mdp=\"user-theme\""));
    assert!(themed.html.contains("--accent:#c0ffee"));
    assert!(!base.html.contains("data-mdp=\"user-theme\""));
}
