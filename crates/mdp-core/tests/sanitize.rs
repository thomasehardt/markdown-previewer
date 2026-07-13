//! Sanitization corpus: the full comrak -> ammonia pipeline must strip
//! malicious HTML while every legitimate feature's markup survives
//! (design section 7). Runs against the public `render_document`.

use mdp_core::{render_document, RenderOptions};

/// One document mixing attacks and legitimate features.
const MIXED: &str = r#"# Mixed

<script>alert('xss-script')</script>

<img src=x onerror="alert('xss-onerror')">

[evil link](javascript:alert('xss-href'))

<iframe src="https://evil.example"></iframe>

A real paragraph with a footnote.[^n]

- [ ] pending
- [x] complete

> [!NOTE]
> Legitimate alert.

[^n]: The footnote body.
"#;

fn render(md: &str) -> String {
    render_document(md, &RenderOptions::default()).html
}

#[test]
fn strips_script_tag_and_its_payload() {
    let html = render(MIXED);
    // The only <script> in the page is our own app shell, tagged with a
    // data-mdp attribute; no bare markdown-injected script survives.
    assert!(
        !html.contains("<script>"),
        "a bare <script> tag survived sanitization"
    );
    assert!(
        !html.contains("xss-script"),
        "script payload text survived"
    );
}

#[test]
fn strips_javascript_url() {
    let html = render(MIXED);
    assert!(
        !html.contains("javascript:"),
        "a javascript: URL survived sanitization"
    );
    assert!(!html.contains("xss-href"), "javascript: payload survived");
}

#[test]
fn strips_event_handlers() {
    let html = render(MIXED);
    assert!(!html.contains("onerror"), "an onerror handler survived");
    assert!(!html.contains("xss-onerror"), "onerror payload survived");
}

#[test]
fn strips_iframe() {
    let html = render(MIXED);
    assert!(!html.contains("<iframe"), "an iframe survived sanitization");
}

#[test]
fn keeps_task_list_checkboxes() {
    let html = render(MIXED);
    assert!(
        html.contains(r#"<input type="checkbox" disabled="">"#),
        "unchecked task-list input was lost"
    );
    assert!(
        html.contains(r#"checked="" disabled="">"#),
        "checked task-list input was lost"
    );
}

#[test]
fn keeps_alert_markup() {
    let html = render(MIXED);
    assert!(
        html.contains(r#"class="markdown-alert markdown-alert-note""#),
        "alert div/class was stripped"
    );
    assert!(
        html.contains(r#"class="markdown-alert-title""#),
        "alert title class was stripped"
    );
}

#[test]
fn keeps_footnote_anchors_and_backrefs() {
    // comrak derives footnote ids from the label, so `[^n]` yields fn-n/fnref-n
    // (not a sequential number). The linking ids and classes must survive
    // ammonia so the anchors still stitch ref to definition.
    let html = render(MIXED);
    assert!(html.contains(r##"href="#fn-n""##), "footnote ref link lost");
    assert!(html.contains(r#"id="fnref-n""#), "footnote ref id lost");
    assert!(html.contains(r#"id="fn-n""#), "footnote definition id lost");
    assert!(
        html.contains(r#"class="footnote-backref""#),
        "footnote backref class lost"
    );
}

#[test]
fn keeps_fenced_code_language_class() {
    let html = render("```python\nprint(1)\n```\n");
    assert!(
        html.contains(r#"class="language-python""#),
        "fenced-code language class was stripped"
    );
}
