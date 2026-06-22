#!/usr/bin/env bash
# Build the headless `mouserd` engine and stage it as a Tauri bundle resource at
# src-tauri/binaries/mouserd, so the packaged desktop app can launch + administer the
# daemon itself (the app spawns it on startup — see src-tauri/src/lib.rs). The binary
# is gitignored; run this before `pnpm tauri build`. cargo is incremental, so re-runs
# are fast.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
# shellcheck disable=SC1090
source "$HOME/.cargo/env" 2>/dev/null || true
export PATH="$HOME/.cargo/bin:$PATH"

echo "stage-engine: building mouserd (release)…"
cargo build --release -p mouser-engine --bin mouserd

DEST="apps/desktop/src-tauri/binaries"
mkdir -p "$DEST"
cp "target/release/mouserd" "$DEST/mouserd"
echo "stage-engine: staged $DEST/mouserd"
