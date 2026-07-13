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
