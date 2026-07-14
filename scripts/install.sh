#!/usr/bin/env bash
# Install the `mdp` binary to ~/.cargo/bin.
#
# Vendors the offline front-end assets first (so math and diagrams are embedded
# in the binary), then runs `cargo install` from the workspace. Requires a Rust
# toolchain (rustup); the repo pins the version via rust-toolchain.toml.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"

echo "==> Vendoring offline assets (KaTeX, Mermaid)"
"$repo_root/scripts/vendor-assets.sh"

echo "==> Installing the mdp binary to ~/.cargo/bin"
cargo install --path "$repo_root/crates/mdp-cli" --locked

echo
echo "Installed. Make sure ~/.cargo/bin is on your PATH, then try:"
echo "    mdp <file.md>"
