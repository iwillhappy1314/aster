#!/bin/bash
# Build Aster's macOS Quick Look preview extension.
# Usage: ./scripts/build-quicklook.sh [arm64|x86_64]

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SOURCE_DIR="$ROOT_DIR/macos/quicklook"
RUST_MANIFEST="$ROOT_DIR/macos/quicklook-rust/Cargo.toml"
ARCH="${1:-$(uname -m)}"
DEPLOYMENT_TARGET="12.0"

get_target_dir() {
    if command -v cargo >/dev/null 2>&1; then
        local target_dir
        target_dir=$(cargo metadata --manifest-path "$ROOT_DIR/Cargo.toml" --format-version=1 2>/dev/null \
            | grep -o '"target_directory":"[^"]*"' \
            | head -1 \
            | cut -d'"' -f4 || true)
        if [ -n "$target_dir" ]; then
            echo "$target_dir"
            return
        fi
    fi
    echo "$ROOT_DIR/target"
}

case "$ARCH" in
    arm64|aarch64)
        ARCH_NAME="arm64"
        RUST_TARGET="aarch64-apple-darwin"
        SWIFT_TARGET="arm64-apple-macos${DEPLOYMENT_TARGET}"
        ;;
    x86_64|intel)
        ARCH_NAME="x86_64"
        RUST_TARGET="x86_64-apple-darwin"
        SWIFT_TARGET="x86_64-apple-macos${DEPLOYMENT_TARGET}"
        ;;
    *)
        echo "Unsupported Quick Look architecture: $ARCH" >&2
        exit 1
        ;;
esac

for tool in cargo rustup xcrun codesign; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "Required tool not found: $tool" >&2
        exit 1
    fi
done

TARGET_DIR="$(get_target_dir)"
BUILD_DIR="$TARGET_DIR/quicklook/$ARCH_NAME"
APPEX="$BUILD_DIR/AsterQuickLook.appex"
EXECUTABLE="$APPEX/Contents/MacOS/AsterQuickLook"
RUST_LIB="$TARGET_DIR/$RUST_TARGET/release/libaster_markdown_quicklook.a"
VERSION=$(grep '^version' "$ROOT_DIR/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')

rustup target add "$RUST_TARGET" >/dev/null 2>&1 || true

# Data-based Quick Look uses QLPreviewProvider/QLPreviewReply, available on
# macOS 12+. Keep the Rust objects and Swift extension on the same deployment
# target so the final linker does not mix incompatible minimum OS versions.
MACOSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET" \
CARGO_TARGET_DIR="$TARGET_DIR" cargo build \
    --release \
    --manifest-path "$RUST_MANIFEST" \
    --target "$RUST_TARGET"

if [ ! -f "$RUST_LIB" ]; then
    echo "Rust Quick Look library was not produced: $RUST_LIB" >&2
    exit 1
fi

rm -rf "$APPEX"
mkdir -p "$APPEX/Contents/MacOS"
cp "$SOURCE_DIR/Info.plist" "$APPEX/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$APPEX/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $VERSION" "$APPEX/Contents/Info.plist"

SDK_PATH="$(xcrun --sdk macosx --show-sdk-path)"

xcrun swiftc \
    -sdk "$SDK_PATH" \
    -target "$SWIFT_TARGET" \
    -swift-version 5 \
    -O \
    -parse-as-library \
    -application-extension \
    -module-name AsterQuickLook \
    -I "$SOURCE_DIR" \
    "$SOURCE_DIR/PreviewProvider.swift" \
    "$RUST_LIB" \
    -framework Cocoa \
    -framework QuickLookUI \
    -Xlinker -e \
    -Xlinker _NSExtensionMain \
    -o "$EXECUTABLE"

SIGN_IDENTITY="${ASTER_CODESIGN_IDENTITY:--}"
SIGN_ARGS=(--force --sign "$SIGN_IDENTITY")
if [ "$SIGN_IDENTITY" != "-" ]; then
    SIGN_ARGS+=(--options runtime --timestamp)
fi

codesign \
    "${SIGN_ARGS[@]}" \
    --entitlements "$SOURCE_DIR/AsterQuickLook.entitlements" \
    "$APPEX"

codesign --verify --strict --verbose=1 "$APPEX"

echo "$APPEX"
