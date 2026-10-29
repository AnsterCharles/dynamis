#!/usr/bin/env bash
# Build the macOS app (.dmg) of Dynamis. Run this ON the Mac (Tauri can't cross-compile
# a Mac app from Windows):
#
#   bash scripts/build_mac.sh
#
# One-time setup on the Mac:
#   xcode-select --install                       # Apple's compilers
#   curl --proto '=https' -sSf https://sh.rustup.rs | sh
#   brew install node   (or the installer from nodejs.org)
#
# Output in dist/: Dynamis_<version>_<arch>.dmg (open it, drag Dynamis to Applications).
# The app is unsigned: the first time, macOS blocks it. Open System Settings -> Privacy & Security
# and click "Open Anyway" (once per Mac). Genuine Unos and CH340 clones need no driver on
# recent macOS; the app finds the board by its USB id, so port names don't matter.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
[ -d node_modules ] || npm install
npx tauri build --bundles dmg
mkdir -p "$root/dist"
cp "$root"/src-tauri/target/release/bundle/dmg/*.dmg "$root/dist/"
ls -lh "$root/dist"
