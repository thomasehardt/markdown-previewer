/*
 * mdp app shell script.
 *
 * The preview is read-only by construction (design section 4.2); this script
 * only renders and never mutates the source. After the DOM is ready it:
 *   1. runs KaTeX over the math wrappers comrak emitted,
 *   2. runs Mermaid over the language-mermaid code blocks,
 *   3. marks the document ready and fires a 'mdp:render-complete' event that
 *      later export phases (design section 4.5) wait on before capturing.
 *
 * KaTeX and Mermaid are present only when the document uses them; every step
 * degrades to a no-op otherwise. Block comments only and no protocol-relative
 * slashes, to keep the offline-URL scan (design section 5) clean.
 */
(function () {
  "use strict";

  /* comrak's math_dollars consumes the $ delimiters and emits
   * <span data-math-style="inline|display">TeX</span>, so we render each such
   * element directly rather than scanning text for delimiters. */
  function renderMath() {
    if (typeof katex === "undefined") {
      return;
    }
    var nodes = document.querySelectorAll("[data-math-style]");
    for (var i = 0; i < nodes.length; i++) {
      var el = nodes[i];
      var display = el.getAttribute("data-math-style") === "display";
      try {
        katex.render(el.textContent, el, {
          displayMode: display,
          throwOnError: false
        });
      } catch (e) {
        /* Leave the original source text in place on failure. */
      }
    }
  }

  /* Render every language-mermaid code block. Returns a promise that resolves
   * when Mermaid has finished so the render-complete signal is accurate. */
  function renderMermaid() {
    if (typeof mermaid === "undefined") {
      return Promise.resolve();
    }
    var blocks = document.querySelectorAll("code.language-mermaid");
    if (blocks.length === 0) {
      return Promise.resolve();
    }
    try {
      return Promise.resolve(mermaid.run({ nodes: blocks }));
    } catch (e) {
      return Promise.resolve();
    }
  }

  function ready() {
    renderMath();
    renderMermaid()
      .catch(function () {})
      .then(function () {
        document.documentElement.setAttribute("data-mdp-ready", "true");
        window.dispatchEvent(new Event("mdp:render-complete"));
      });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", ready);
  } else {
    ready();
  }
})();
