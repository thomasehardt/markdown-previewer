/*
 * mdp app shell script (Phase 1).
 *
 * Deliberately tiny: the preview is read-only by construction (design section
 * 4.2). This only marks the document ready so a frontend can swap out its
 * spinner. The view-source toggle, KaTeX auto-render, and Mermaid init are
 * added in later phases. Block comments only, no protocol-relative slashes,
 * to keep the offline-URL scan (design section 5) clean.
 */
(function () {
  "use strict";
  document.documentElement.setAttribute("data-mdp-ready", "true");
})();
