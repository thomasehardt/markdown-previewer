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

  /* View-source toggle (design section 4.2). Swaps the rendered document and
   * the read-only raw-source <pre> entirely in-page; it never mutates or
   * refetches anything, so it behaves identically under the webview and serve
   * frontends. Remembers each view's scroll position across toggles. */
  function setupViewToggle() {
    var btn = document.getElementById("mdp-view-toggle");
    if (!btn) {
      return;
    }
    var scroll = { rendered: 0, source: 0 };
    function isSource() {
      return document.body.classList.contains("mdp-view-source-active");
    }
    function sync() {
      var src = isSource();
      btn.textContent = src ? "View rendered" : "View source";
      btn.setAttribute("aria-pressed", src ? "true" : "false");
    }
    function toggle() {
      scroll[isSource() ? "source" : "rendered"] = window.scrollY;
      document.body.classList.toggle("mdp-view-source-active");
      sync();
      window.scrollTo(0, scroll[isSource() ? "source" : "rendered"] || 0);
    }
    btn.addEventListener("click", toggle);
    document.addEventListener("keydown", function (e) {
      /* "s" toggles, unless a modifier is held (leave browser shortcuts alone). */
      if (e.key === "s" && !e.metaKey && !e.ctrlKey && !e.altKey) {
        toggle();
      }
    });
    sync();
  }

  /* Smart reload (design section 4.3). Poll the frontend's reload endpoint; when
   * it reports the file changed on disk, reveal a non-intrusive banner. We NEVER
   * swap content on our own - the reader clicks Reload (or Dismiss). Polling a
   * relative endpoint fails harmlessly for a statically opened file (no server),
   * so live reload is simply absent there. */
  function setupReload() {
    var banner = document.getElementById("mdp-reload-banner");
    if (!banner) {
      return;
    }
    // A statically opened file (a file: URL) has no server to poll, so live
    // reload is simply absent there (e.g. an exported HTML file).
    if (window.location.protocol === "file:") {
      return;
    }
    var accept = document.getElementById("mdp-reload-accept");
    var dismiss = document.getElementById("mdp-reload-dismiss");
    var dismissed = false;
    if (accept) {
      accept.addEventListener("click", function () {
        window.location.reload();
      });
    }
    if (dismiss) {
      dismiss.addEventListener("click", function () {
        banner.hidden = true;
        dismissed = true;
      });
    }
    function poll() {
      fetch("/.mdp/reload", { cache: "no-store" })
        .then(function (r) {
          return r.ok ? r.json() : null;
        })
        .then(function (data) {
          if (data && data.changed && !dismissed) {
            banner.hidden = false;
          }
        })
        .catch(function () {
          /* No server (e.g. a static file): live reload is unavailable. */
        });
    }
    setInterval(poll, 1000);
  }

  function ready() {
    setupViewToggle();
    setupReload();
    renderMath();
    renderMermaid()
      .catch(function () {})
      .then(function () {
        // Wait for web fonts (KaTeX) to finish loading before signaling done,
        // so an export/PDF capture gets real math glyphs rather than blanks.
        return document.fonts && document.fonts.ready
          ? document.fonts.ready
          : null;
      })
      .catch(function () {})
      .then(function () {
        document.documentElement.setAttribute("data-mdp-ready", "true");
        window.dispatchEvent(new Event("mdp:render-complete"));
        // Signal a host that registered an IPC handler (e.g. PDF export waits
        // for this before capturing). Absent in the plain preview; guarded.
        if (window.ipc && typeof window.ipc.postMessage === "function") {
          window.ipc.postMessage("render-complete");
        }
      });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", ready);
  } else {
    ready();
  }
})();
