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

  /* Live theme / color-scheme / reading-width switching (design 4.6). Each
   * control maps to a root data-attribute the theme CSS keys off; the choice is
   * remembered in localStorage so it survives reloads and moving between files.
   * The attribute is removed when the value equals its default, so the base
   * (default theme, auto scheme, comfortable width) needs no attribute. */
  function setupSettings() {
    var root = document.documentElement;
    function apply(attr, value, attrDefault) {
      if (value === attrDefault) {
        root.removeAttribute(attr);
      } else {
        root.setAttribute(attr, value);
      }
    }
    function control(id, key, attr, attrDefault, initial) {
      var el = document.getElementById(id);
      if (!el) {
        return;
      }
      var saved = null;
      try {
        saved = localStorage.getItem(key);
      } catch (e) {}
      var value = saved || initial;
      el.value = value;
      apply(attr, value, attrDefault);
      el.addEventListener("change", function () {
        apply(attr, el.value, attrDefault);
        try {
          localStorage.setItem(key, el.value);
        } catch (e) {}
      });
    }
    control("mdp-theme", "mdp-theme", "data-mdp-theme", "default", "default");
    control("mdp-width", "mdp-width", "data-mdp-width", "comfortable", "comfortable");
    // Scheme's initial value is whatever the server stamped (from --scheme), or
    // auto; the attribute-default that clears it is always "auto".
    var initialScheme = root.getAttribute("data-color-scheme") || "auto";
    control("mdp-scheme", "mdp-scheme", "data-color-scheme", "auto", initialScheme);
  }

  /* Table-of-contents sidebar toggle. Flips a body class the layout keys off;
   * the open/closed state is remembered in localStorage. */
  function setupToc() {
    var btn = document.getElementById("mdp-toc-toggle");
    if (!btn) {
      return;
    }
    function apply(open) {
      document.body.classList.toggle("mdp-toc-open", open);
      btn.setAttribute("aria-pressed", open ? "true" : "false");
    }
    var open = false;
    try {
      open = localStorage.getItem("mdp-toc-open") === "1";
    } catch (e) {}
    apply(open);
    btn.addEventListener("click", function () {
      var next = !document.body.classList.contains("mdp-toc-open");
      apply(next);
      try {
        localStorage.setItem("mdp-toc-open", next ? "1" : "0");
      } catch (e) {}
    });
  }

  function setupExport() {
    // Export to PDF is host-driven: the native app captures and writes the file.
    // A plain browser (serve mode) has no host, so only reveal the controls when
    // an IPC channel exists.
    if (!window.ipc || typeof window.ipc.postMessage !== "function") {
      return;
    }
    var btn = document.getElementById("mdp-export");
    var sizeSel = document.getElementById("mdp-pagesize");
    if (!btn || !sizeSel) {
      return;
    }
    var controls = document.querySelectorAll(".mdp-export-ctl");
    for (var i = 0; i < controls.length; i++) {
      controls[i].removeAttribute("hidden");
    }
    btn.addEventListener("click", function () {
      var size = sizeSel.value || "letter";
      var schemeSel = document.getElementById("mdp-scheme");
      var scheme = (schemeSel && schemeSel.value) || "auto";
      // The host reads the current document and writes a PDF; it prompts for a
      // save location. Message shape: "mdp:export <size> <scheme>".
      window.ipc.postMessage("mdp:export " + size + " " + scheme);
    });
  }

  function ready() {
    setupViewToggle();
    setupReload();
    setupSettings();
    setupToc();
    setupExport();
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
        // PDF export only: paginate into real page boxes now that math and
        // diagrams have rendered. Preview mode leaves __mdpPaged unset.
        if (
          window.__mdpPaged &&
          window.PagedPolyfill &&
          typeof window.PagedPolyfill.preview === "function"
        ) {
          return window.PagedPolyfill.preview();
        }
        return null;
      })
      .catch(function () {})
      .then(function () {
        document.documentElement.setAttribute("data-mdp-ready", "true");
        window.dispatchEvent(new Event("mdp:render-complete"));
        // Signal a host that registered an IPC handler (e.g. PDF export waits
        // for this before capturing). Absent in the plain preview; guarded.
        // Append the rendered surface color so a PDF capture can paint page
        // margins/trailing space to match the document instead of white. After
        // pagination the surface is the paged.js page box.
        if (window.ipc && typeof window.ipc.postMessage === "function") {
          var surface =
            document.querySelector(".pagedjs_page") ||
            document.querySelector(".mdp-document") ||
            document.body;
          var bg = "";
          try {
            bg = window.getComputedStyle(surface).backgroundColor || "";
          } catch (e) {
            bg = "";
          }
          window.ipc.postMessage("render-complete " + bg);
        }
      });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", ready);
  } else {
    ready();
  }
})();
