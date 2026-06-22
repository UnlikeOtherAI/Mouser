#!/usr/bin/env bash
# Build the mouser-ffi static libraries for iOS (device + simulator) and assemble
# them into apps/ios/Frameworks/MouserFFI.xcframework. The .a's are ~45 MB each, so
# the XCFramework is gitignored and produced here instead of committed; only the
# generated Swift binding (Sources/Generated/mouser_ffi.swift) is checked in.
#
# Run once before opening/building the Xcode project (xcodegen wires it as a
# pre-build script too, so `xcodebuild` stays self-sufficient). cargo is incremental,
# so re-runs are fast when nothing changed.
set -euo pipefail

# Repo root (this script lives at apps/ios/).
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

# Make cargo available in Xcode's reduced PATH.
# shellcheck disable=SC1090
source "$HOME/.cargo/env" 2>/dev/null || true
export PATH="$HOME/.cargo/bin:$PATH"

DEVICE_TARGET="aarch64-apple-ios"
# The simulator slice is a fat lib (Apple-Silicon arm64 + Intel x86_64) so the
# XCFramework builds on any Mac.
SIM_ARM="aarch64-apple-ios-sim"
SIM_X86="x86_64-apple-ios"

echo "build-ffi: building mouser-ffi for $DEVICE_TARGET, $SIM_ARM, $SIM_X86 (release)…"
cargo build -p mouser-ffi --release --target "$DEVICE_TARGET"
cargo build -p mouser-ffi --release --target "$SIM_ARM"
cargo build -p mouser-ffi --release --target "$SIM_X86"

# Combine the two simulator arches into one fat static lib.
SIM_UNIVERSAL="target/ios-sim-universal/release"
mkdir -p "$SIM_UNIVERSAL"
lipo -create \
    "target/$SIM_ARM/release/libmouser_ffi.a" \
    "target/$SIM_X86/release/libmouser_ffi.a" \
    -output "$SIM_UNIVERSAL/libmouser_ffi.a"

BINDINGS="crates/mouser-ffi/bindings"
HEADERS="$(mktemp -d)"
cp "$BINDINGS/mouser_ffiFFI.h" "$HEADERS/"
# A minimal modulemap (named module.modulemap as the XCFramework expects), exposing
# the `mouser_ffiFFI` module the generated Swift binding imports.
cat > "$HEADERS/module.modulemap" <<'EOF'
module mouser_ffiFFI {
    header "mouser_ffiFFI.h"
    export *
}
EOF

OUT="apps/ios/Frameworks"
mkdir -p "$OUT"
rm -rf "$OUT/MouserFFI.xcframework"
xcodebuild -create-xcframework \
    -library "target/$DEVICE_TARGET/release/libmouser_ffi.a" -headers "$HEADERS" \
    -library "$SIM_UNIVERSAL/libmouser_ffi.a" -headers "$HEADERS" \
    -output "$OUT/MouserFFI.xcframework"

echo "build-ffi: wrote $OUT/MouserFFI.xcframework"
