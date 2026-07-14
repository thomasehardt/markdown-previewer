#!/usr/bin/env bash
#
# vendor-assets.sh - fetch pinned, checksummed third-party front-end assets
# (KaTeX + Mermaid) into a gitignored vendor directory for rust-embed to fold
# into the binary at build time.
#
# This is the ONLY place a network fetch is allowed. The built tool makes zero
# network calls (design section 5); this script runs at development/build time.
#
# What it does:
#   1. Downloads pinned KaTeX and Mermaid files from the jsDelivr CDN.
#   2. Derives the KaTeX woff2 font list from katex.min.css and downloads each.
#   3. Verifies every downloaded file's sha256 against scripts/assets-manifest.toml.
#      On first run (no [files] entries) it generates the manifest from the
#      freshly downloaded files, then re-verifies against what it just wrote.
#   4. Produces katex.inlined.css: katex.min.css with every url(fonts/*.woff2)
#      replaced by a self-contained data:font/woff2;base64,... URI and the
#      woff/ttf fallbacks stripped, so the stylesheet has zero relative refs.
#
# The vendored files are gitignored. Only this script, the manifest, and the
# license file are committed.
#
# Usage:  scripts/vendor-assets.sh
#         scripts/vendor-assets.sh --regenerate   # rewrite the manifest hashes
#
set -euo pipefail

# --- Pinned versions (verified against upstream GitHub releases) -------------
# KaTeX: last patch of the maintained 0.16 line (design section 3 pins 0.16.x).
# Mermaid: 11.16.0 (design section 3).
KATEX_VERSION="0.16.47"
MERMAID_VERSION="11.16.0"
# paged.js: paginates the PDF-export capture into real page boxes (design 4.5).
PAGEDJS_VERSION="0.4.3"
CDN="https://cdn.jsdelivr.net/npm"

# --- Paths -------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
VENDOR_DIR="$REPO_ROOT/crates/mdp-core/assets/vendor"
KATEX_DIR="$VENDOR_DIR/katex"
KATEX_FONTS_DIR="$KATEX_DIR/fonts"
MERMAID_DIR="$VENDOR_DIR/mermaid"
PAGEDJS_DIR="$VENDOR_DIR/pagedjs"
MANIFEST="$SCRIPT_DIR/assets-manifest.toml"

REGENERATE=0
[ "${1:-}" = "--regenerate" ] && REGENERATE=1

# --- Small helpers -----------------------------------------------------------
log()  { printf '  %s\n' "$*"; }
step() { printf '\n==> %s\n' "$*"; }
die()  { printf '\nERROR: %s\n' "$*" >&2; exit 1; }

# sha256 of a file, lowercase hex only. Prefers sha256sum, falls back to shasum.
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        die "need sha256sum or shasum to compute checksums"
    fi
}

# Download a URL to a path, failing loudly on any HTTP or transport error.
fetch() {
    url="$1"; out="$2"
    mkdir -p "$(dirname "$out")"
    curl --fail --silent --show-error --location \
         --retry 3 --retry-delay 1 --connect-timeout 20 \
         -o "$out" "$url" \
        || die "download failed: $url"
    [ -s "$out" ] || die "downloaded empty file: $url"
}

# Look up a file's expected hash in the manifest. Empty output => not present.
# Manifest lines look like:  "katex/katex.min.js" = "deadbeef..."
manifest_hash() {
    key="$1"
    [ -f "$MANIFEST" ] || return 0
    grep -E "^\"${key}\"[[:space:]]*=" "$MANIFEST" 2>/dev/null \
        | sed -E 's/.*=[[:space:]]*"([0-9a-f]+)".*/\1/' \
        | head -n1
}

# True when the manifest already carries hash entries (a [files] section
# with at least one "..." = "..." line). False on first run / empty manifest.
manifest_has_files() {
    [ -f "$MANIFEST" ] || return 1
    grep -Eq '^"[^"]+"[[:space:]]*=[[:space:]]*"[0-9a-f]{64}"' "$MANIFEST"
}

command -v curl   >/dev/null 2>&1 || die "curl is required"
command -v python3 >/dev/null 2>&1 || die "python3 is required (for CSS font inlining)"

# =============================================================================
step "Vendoring KaTeX $KATEX_VERSION, Mermaid $MERMAID_VERSION, paged.js $PAGEDJS_VERSION"
log  "vendor dir: $VENDOR_DIR"
rm -rf "$VENDOR_DIR"
mkdir -p "$KATEX_FONTS_DIR" "$MERMAID_DIR" "$PAGEDJS_DIR"

# --- 1. Core files -----------------------------------------------------------
step "Downloading core files"
fetch "$CDN/katex@$KATEX_VERSION/dist/katex.min.js"  "$KATEX_DIR/katex.min.js"
log "katex.min.js"
fetch "$CDN/katex@$KATEX_VERSION/dist/katex.min.css" "$KATEX_DIR/katex.min.css"
log "katex.min.css"
fetch "$CDN/mermaid@$MERMAID_VERSION/dist/mermaid.min.js" "$MERMAID_DIR/mermaid.min.js"
log "mermaid.min.js"
fetch "$CDN/pagedjs@$PAGEDJS_VERSION/dist/paged.polyfill.js" "$PAGEDJS_DIR/paged.polyfill.js"
log "paged.polyfill.js"

# --- 2. Fonts (derived from the CSS, so we fetch exactly what it references) --
step "Downloading KaTeX fonts referenced by katex.min.css"
# Extract distinct fonts/KaTeX_*.woff2 basenames from the stylesheet.
FONTS="$(grep -oE 'fonts/KaTeX_[^)]*\.woff2' "$KATEX_DIR/katex.min.css" \
         | sed 's#fonts/##' | sort -u)"
[ -n "$FONTS" ] || die "no woff2 font references found in katex.min.css"
FONT_COUNT=0
for font in $FONTS; do
    fetch "$CDN/katex@$KATEX_VERSION/dist/fonts/$font" "$KATEX_FONTS_DIR/$font"
    FONT_COUNT=$((FONT_COUNT + 1))
done
log "fetched $FONT_COUNT woff2 fonts"

# --- 3. Integrity: verify against the manifest (or generate it) --------------
# Ordered list of every fetched file, keyed by path relative to the vendor dir.
FILES="katex/katex.min.js
katex/katex.min.css
mermaid/mermaid.min.js
pagedjs/paged.polyfill.js"
for font in $FONTS; do
    FILES="$FILES
katex/fonts/$font"
done

write_manifest() {
    {
        echo "# Auto-generated by scripts/vendor-assets.sh. DO NOT edit hashes by hand."
        echo "# Pinned versions + sha256 of every vendored third-party asset. This is"
        echo "# the supply-chain integrity boundary: the script fails if a re-download"
        echo "# does not match. To intentionally bump a version, edit the versions in"
        echo "# vendor-assets.sh, then run: scripts/vendor-assets.sh --regenerate"
        echo
        echo "[versions]"
        echo "katex = \"$KATEX_VERSION\""
        echo "mermaid = \"$MERMAID_VERSION\""
        echo "pagedjs = \"$PAGEDJS_VERSION\""
        echo
        echo "[files]"
        for rel in $FILES; do
            h="$(sha256_of "$VENDOR_DIR/$rel")"
            echo "\"$rel\" = \"$h\""
        done
    } > "$MANIFEST"
}

if [ "$REGENERATE" -eq 1 ] || ! manifest_has_files; then
    if [ "$REGENERATE" -eq 1 ]; then
        step "Regenerating manifest (--regenerate)"
    else
        step "No hashes in manifest yet - generating from downloaded files"
    fi
    write_manifest
    log "wrote $MANIFEST"
    # Re-verify: read the manifest back and confirm it matches what is on disk.
    log "re-verifying freshly written manifest"
fi

step "Verifying downloads against manifest"
# Check the pinned versions still agree with this script.
mf_katex="$(grep -E '^katex[[:space:]]*=' "$MANIFEST" | sed -E 's/.*"([^"]+)".*/\1/')"
mf_mermaid="$(grep -E '^mermaid[[:space:]]*=' "$MANIFEST" | sed -E 's/.*"([^"]+)".*/\1/')"
mf_pagedjs="$(grep -E '^pagedjs[[:space:]]*=' "$MANIFEST" | sed -E 's/.*"([^"]+)".*/\1/')"
[ "$mf_katex" = "$KATEX_VERSION" ] \
    || die "manifest KaTeX version ($mf_katex) != script ($KATEX_VERSION); run --regenerate"
[ "$mf_mermaid" = "$MERMAID_VERSION" ] \
    || die "manifest Mermaid version ($mf_mermaid) != script ($MERMAID_VERSION); run --regenerate"
[ "$mf_pagedjs" = "$PAGEDJS_VERSION" ] \
    || die "manifest paged.js version ($mf_pagedjs) != script ($PAGEDJS_VERSION); run --regenerate"

VERIFIED=0
for rel in $FILES; do
    want="$(manifest_hash "$rel")"
    [ -n "$want" ] || die "no manifest entry for $rel (run --regenerate)"
    got="$(sha256_of "$VENDOR_DIR/$rel")"
    if [ "$want" != "$got" ]; then
        die "checksum MISMATCH for $rel
        expected: $want
        actual:   $got"
    fi
    VERIFIED=$((VERIFIED + 1))
done
log "$VERIFIED files verified, all hashes match"

# --- 4. Precompute the self-contained KaTeX stylesheet -----------------------
# Replace url(fonts/*.woff2) with base64 data URIs and drop the woff/ttf
# fallbacks, so render_document never has to base64 fonts at runtime and the
# stylesheet carries zero relative/external references (design section 4.1/5).
step "Building self-contained katex.inlined.css"
python3 - "$KATEX_DIR/katex.min.css" "$KATEX_FONTS_DIR" "$KATEX_DIR/katex.inlined.css" <<'PY'
import base64, pathlib, re, sys

css_path, fonts_dir, out_path = sys.argv[1], sys.argv[2], sys.argv[3]
css = pathlib.Path(css_path).read_text(encoding="utf-8")
fonts = pathlib.Path(fonts_dir)

# Drop the woff and ttf fallback sources (with any leading comma). We ship
# only woff2; every target browser/webview engine supports it. The woff2
# alternation is safe because ".woff)" cannot match ".woff2)".
css = re.sub(
    r',?\s*url\(fonts/[^)]*\.(?:woff|ttf)\)\s*format\("(?:woff|truetype)"\)',
    "",
    css,
)

def inline(match):
    name = match.group(1)
    data = (fonts / name).read_bytes()
    b64 = base64.b64encode(data).decode("ascii")
    return f'url(data:font/woff2;base64,{b64})'

css, n = re.subn(r'url\(fonts/(KaTeX_[^)]*\.woff2)\)', inline, css)
if n == 0:
    sys.exit("no woff2 references found to inline")

# Self-containment guard: no relative font path may survive.
if "url(fonts/" in css:
    sys.exit("relative font reference still present after inlining")

pathlib.Path(out_path).write_text(css, encoding="utf-8")
print(f"  inlined {n} woff2 fonts")
PY

# --- 5. Report ----------------------------------------------------------------
step "Done. Vendored files:"
if command -v du >/dev/null 2>&1; then
    ( cd "$VENDOR_DIR" && find . -type f | sort | while read -r f; do
        size="$(wc -c < "$f" | tr -d ' ')"
        printf '  %10s  %s\n' "$size" "${f#./}"
      done )
fi
printf '\n  total: '
du -sh "$VENDOR_DIR" 2>/dev/null | awk '{print $1}'
log "manifest: $MANIFEST"
log "these files are gitignored; commit only the script, manifest, and licenses"
