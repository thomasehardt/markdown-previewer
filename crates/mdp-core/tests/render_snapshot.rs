//! Snapshot tests of `render_document` over a feature-rich fixture corpus.
//!
//! The page embeds large CSS/JS blobs; insta filters redact those (design
//! section 7) so the snapshot shows the Markdown-derived body and the app
//! shell structure, staying reviewable and stable across asset bumps.

use mdp_core::{render_document, ColorScheme, RenderOptions};

/// Redact the inlined asset blobs so the snapshot reviews the body, not the
/// vendored CSS/JS. Bind these filters, then take the snapshot inside.
fn redacted<F: FnOnce()>(f: F) {
    let mut settings = insta::Settings::clone_current();
    settings.add_filter(
        r#"(?s)<style data-mdp="theme">.*?</style>"#,
        r#"<style data-mdp="theme">/* THEME_CSS redacted */</style>"#,
    );
    settings.add_filter(
        r#"(?s)<style data-mdp="shell">.*?</style>"#,
        r#"<style data-mdp="shell">/* SHELL_CSS redacted */</style>"#,
    );
    settings.add_filter(
        r#"(?s)<script data-mdp="shell">.*?</script>"#,
        r#"<script data-mdp="shell">/* SHELL_JS redacted */</script>"#,
    );
    // The raw-source <pre> just echoes the fixture input; redact it so the
    // snapshot reviews the rendered markup, not a duplicate of the source.
    settings.add_filter(
        r#"(?s)<pre id="mdp-source"[^>]*>.*?</pre>"#,
        r#"<pre id="mdp-source">/* RAW_SOURCE redacted */</pre>"#,
    );
    // Phase 2 asset blobs: the generated highlight CSS and the large vendored
    // KaTeX/Mermaid libraries. Redacted so the snapshot reviews the body's
    // highlighted/math/mermaid markup and which hooks are injected, staying
    // stable across theme and vendored-asset bumps (design section 7). The
    // small mermaid init script is left visible so its config is reviewable.
    settings.add_filter(
        r#"(?s)<style data-mdp="syntect">.*?</style>"#,
        r#"<style data-mdp="syntect">/* SYNTECT_CSS redacted */</style>"#,
    );
    settings.add_filter(
        r#"(?s)<style data-mdp="katex">.*?</style>"#,
        r#"<style data-mdp="katex">/* KATEX_CSS redacted */</style>"#,
    );
    settings.add_filter(
        r#"(?s)<script data-mdp="katex-lib">.*?</script>"#,
        r#"<script data-mdp="katex-lib">/* KATEX_JS redacted */</script>"#,
    );
    settings.add_filter(
        r#"(?s)<script data-mdp="mermaid-lib">.*?</script>"#,
        r#"<script data-mdp="mermaid-lib">/* MERMAID_JS redacted */</script>"#,
    );
    settings.bind(f);
}

#[test]
fn corpus_renders_to_expected_html() {
    let markdown = include_str!("fixtures/corpus.md");
    let opts = RenderOptions {
        title: Some("Corpus".to_string()),
        ..Default::default()
    };
    let doc = render_document(markdown, &opts);
    redacted(|| {
        insta::assert_snapshot!("corpus", doc.html);
    });
}

/// Focused review of the Phase 2 markup: a syntect-highlighted Rust block, an
/// inline and a display math wrapper, and a mermaid fence passed through for
/// the client. The redactions blank the vendored/generated asset blobs, so the
/// snapshot shows exactly the body markup those features produce.
#[test]
fn phase2_features_render_to_expected_markup() {
    let markdown = "\
Inline $a^2 + b^2 = c^2$ and a display block:\n\
\n\
$$\\sum_{i=1}^{n} i = \\frac{n(n+1)}{2}$$\n\
\n\
```rust\n\
fn add(a: i32, b: i32) -> i32 {\n\
    a + b\n\
}\n\
```\n\
\n\
```mermaid\n\
flowchart LR; X --> Y;\n\
```\n";
    let opts = RenderOptions {
        title: Some("Phase 2".to_string()),
        ..Default::default()
    };
    let doc = render_document(markdown, &opts);
    redacted(|| {
        insta::assert_snapshot!("phase2_features", doc.html);
    });
}

#[test]
fn dark_scheme_stamps_root_attribute() {
    let opts = RenderOptions {
        title: Some("Dark".to_string()),
        color_scheme: ColorScheme::Dark,
        ..Default::default()
    };
    let doc = render_document("# Dark mode\n\nBody.", &opts);
    redacted(|| {
        insta::assert_snapshot!("dark_scheme", doc.html);
    });
}
