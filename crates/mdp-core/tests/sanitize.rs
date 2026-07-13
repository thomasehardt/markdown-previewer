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

#[test]
fn keeps_math_wrapper_spans() {
    // comrak math_dollars emits <span data-math-style="inline|display">; the
    // data-math-style marker is the exact hook the client KaTeX step selects
    // on, so ammonia must preserve it.
    let html = render("Inline $x^2$ and display $$y = mx + b$$.\n");
    assert!(
        html.contains(r#"<span data-math-style="inline">"#),
        "inline math wrapper/attribute was stripped"
    );
    assert!(
        html.contains(r#"<span data-math-style="display">"#),
        "display math wrapper/attribute was stripped"
    );
}

#[test]
fn keeps_syntect_highlight_spans() {
    // syntect emits class-based spans prefixed mdp-hl-*; the generic class
    // allowlist must let them survive so the inlined theme CSS can color them.
    let html = render("```rust\nfn main() {}\n```\n");
    assert!(
        html.contains(r#"class="mdp-hl-"#),
        "syntect highlight span classes were stripped"
    );
    // The <code> still carries its language class alongside the spans.
    assert!(
        html.contains(r#"class="language-rust""#),
        "language class on the code element was stripped"
    );
}

#[test]
fn keeps_mermaid_code_block_verbatim() {
    // The mermaid fence must survive sanitize as <code class="language-mermaid">
    // with its (escaped) diagram source, so the client can render it. It must
    // NOT be syntect-highlighted (no mdp-hl-* spans inside it).
    let html = render("```mermaid\ngraph TD; A-->B;\n```\n");
    assert!(
        html.contains(r#"<code class="language-mermaid">"#),
        "mermaid code block/class was stripped"
    );
    assert!(
        html.contains("graph TD; A--&gt;B;"),
        "mermaid diagram source was lost or not escaped"
    );
    // Diagram source is passed through untouched, not run through syntect.
    let mermaid_start = html.find("language-mermaid").unwrap();
    let mermaid_end = html[mermaid_start..].find("</code>").unwrap() + mermaid_start;
    assert!(
        !html[mermaid_start..mermaid_end].contains("mdp-hl-"),
        "mermaid block should not be syntax-highlighted"
    );
}

#[test]
fn math_and_mermaid_do_not_open_a_script_hole() {
    // A payload disguised inside math/mermaid must land as inert, escaped text,
    // never live markup: comrak escapes the content and ammonia keeps only
    // text. We assert on live tags, not substrings - the escaped text may still
    // contain literal "onerror=" as visible characters, which is harmless.
    let html = render(
        "$$</span><script>alert(1)</script>$$\n\n```mermaid\n</code><img src=x onerror=alert(2)>\n```\n",
    );
    // No live <script>/<img> tag was produced by the injection attempt.
    assert!(
        !html.contains("<script>alert(1)"),
        "a live script tag was injected via math"
    );
    assert!(
        !html.contains("<img src=x"),
        "a live img tag was injected via mermaid"
    );
    // The angle brackets were escaped, proving the payload is text, not markup.
    assert!(
        html.contains("&lt;script&gt;alert(1)"),
        "math payload should be escaped text"
    );
    assert!(
        html.contains("&lt;img src=x"),
        "mermaid payload should be escaped text"
    );
}
