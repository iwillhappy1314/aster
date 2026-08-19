#!/bin/bash
# build-dmg.sh - Build Aster and embed the Markdown Quick Look extension.
# Usage: ./scripts/build-dmg.sh [arm64|x86_64|universal]

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[1;34m'
NC='\033[0m'

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

APP_NAME="Aster"
VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
SIGN_IDENTITY="${ASTER_CODESIGN_IDENTITY:--}"

get_target_dir() {
    if command -v cargo >/dev/null 2>&1; then
        local target_dir
        target_dir=$(cargo metadata --format-version=1 2>/dev/null \
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

TARGET_DIR="$(get_target_dir)"

bundle_executable() {
    local bundle="$1"
    local name
    name=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$bundle/Contents/Info.plist" 2>/dev/null || true)
    if [ -z "$name" ]; then
        name="$APP_NAME"
    fi
    echo "$bundle/Contents/MacOS/$name"
}

sign_app_bundle() {
    local app="$1"
    local appex="$app/Contents/PlugIns/AsterQuickLook.appex"
    local sign_args=(--force --sign "$SIGN_IDENTITY")

    if [ "$SIGN_IDENTITY" != "-" ]; then
        sign_args+=(--options runtime --timestamp)
    fi

    if [ -d "$appex" ]; then
        codesign \
            "${sign_args[@]}" \
            --entitlements "$ROOT_DIR/macos/quicklook/AsterQuickLook.entitlements" \
            "$appex"
    fi

    codesign "${sign_args[@]}" "$app"
    codesign --verify --deep --strict --verbose=1 "$app"
}

embed_quicklook() {
    local app="$1"
    local arch="$2"
    local appex
    appex=$("$ROOT_DIR/scripts/build-quicklook.sh" "$arch" | tail -1)

    if [ ! -d "$appex" ]; then
        echo -e "${RED}Quick Look extension not found: $appex${NC}" >&2
        exit 1
    fi

    mkdir -p "$app/Contents/PlugIns"
    rm -rf "$app/Contents/PlugIns/AsterQuickLook.appex"
    cp -R "$appex" "$app/Contents/PlugIns/AsterQuickLook.appex"
    sign_app_bundle "$app"
}

create_dmg() {
    local app_dir="$1"
    local suffix="$2"
    local dmg_name="${APP_NAME}-${suffix}.dmg"
    local app_path="${app_dir}/${APP_NAME}.app"
    local dmg_temp="${TARGET_DIR}/dmg-temp-${suffix}"

    echo -e "\n${BLUE}Creating DMG: ${dmg_name}...${NC}"

    rm -rf "$dmg_temp"
    mkdir -p "$dmg_temp"
    cp -R "$app_path" "$dmg_temp/"
    ln -s /Applications "$dmg_temp/Applications"
    rm -f "$dmg_name"

    hdiutil create \
        -volname "$APP_NAME" \
        -srcfolder "$dmg_temp" \
        -ov \
        -format UDZO \
        "$dmg_name"

    rm -rf "$dmg_temp"
    echo -e "${GREEN}Created: ${dmg_name} ($(du -h "$dmg_name" | cut -f1))${NC}"
}

build_arch() {
    local arch="$1"
    local rust_target
    local suffix

    case "$arch" in
        arm64|aarch64)
            rust_target="aarch64-apple-darwin"
            suffix="arm64"
            ;;
        x86_64|intel)
            rust_target="x86_64-apple-darwin"
            suffix="x86_64"
            ;;
        *)
            echo -e "${RED}Unknown architecture: $arch${NC}" >&2
            exit 1
            ;;
    esac

    echo -e "\n${YELLOW}Building target: ${rust_target}${NC}"
    rustup target add "$rust_target" >/dev/null 2>&1 || true
    cargo build --release --target "$rust_target"
    cargo bundle --release --target "$rust_target"

    local bundle_dir
    bundle_dir=$(find "$TARGET_DIR" -path "*${rust_target}/release/bundle/osx" -type d 2>/dev/null | head -1)
    if [ -z "$bundle_dir" ] || [ ! -d "$bundle_dir/${APP_NAME}.app" ]; then
        echo -e "${RED}Could not find ${APP_NAME}.app for ${rust_target}${NC}" >&2
        exit 1
    fi

    embed_quicklook "$bundle_dir/${APP_NAME}.app" "$suffix"
    echo "$bundle_dir"
}

echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo -e "${BLUE}  Building ${APP_NAME} v${VERSION} DMG Installer${NC}"
echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo -e "${YELLOW}Target directory: ${TARGET_DIR}${NC}"

if ! command -v cargo-bundle >/dev/null 2>&1; then
    echo -e "${YELLOW}Installing cargo-bundle...${NC}"
    cargo install cargo-bundle
fi

ARCH="${1:-$(uname -m)}"

case "$ARCH" in
    arm64|aarch64|x86_64|intel)
        BUNDLE_DIR=$(build_arch "$ARCH" | tail -1)
        case "$ARCH" in
            arm64|aarch64) SUFFIX="arm64" ;;
            *) SUFFIX="x86_64" ;;
        esac
        create_dmg "$BUNDLE_DIR" "$SUFFIX"
        ;;

    universal)
        echo -e "${YELLOW}Building both architectures for a universal bundle...${NC}"
        ARM_DIR=$(build_arch arm64 | tail -1)
        X86_DIR=$(build_arch x86_64 | tail -1)
        ARM_APP="$ARM_DIR/${APP_NAME}.app"
        X86_APP="$X86_DIR/${APP_NAME}.app"

        UNIVERSAL_DIR="${TARGET_DIR}/universal"
        UNIVERSAL_APP="${UNIVERSAL_DIR}/${APP_NAME}.app"
        rm -rf "$UNIVERSAL_DIR"
        mkdir -p "$UNIVERSAL_DIR"
        cp -R "$ARM_APP" "$UNIVERSAL_APP"

        ARM_MAIN=$(bundle_executable "$ARM_APP")
        X86_MAIN=$(bundle_executable "$X86_APP")
        UNIVERSAL_MAIN=$(bundle_executable "$UNIVERSAL_APP")
        lipo -create "$ARM_MAIN" "$X86_MAIN" -output "$UNIVERSAL_MAIN"

        ARM_QL="$ARM_APP/Contents/PlugIns/AsterQuickLook.appex/Contents/MacOS/AsterQuickLook"
        X86_QL="$X86_APP/Contents/PlugIns/AsterQuickLook.appex/Contents/MacOS/AsterQuickLook"
        UNIVERSAL_QL="$UNIVERSAL_APP/Contents/PlugIns/AsterQuickLook.appex/Contents/MacOS/AsterQuickLook"
        lipo -create "$ARM_QL" "$X86_QL" -output "$UNIVERSAL_QL"

        sign_app_bundle "$UNIVERSAL_APP"
        create_dmg "$UNIVERSAL_DIR" "universal"
        ;;

    *)
        echo -e "${RED}Unknown architecture '$ARCH'${NC}" >&2
        echo "Usage: $0 [arm64|x86_64|universal]" >&2
        exit 1
        ;;
esac

echo -e "\n${GREEN}Build complete.${NC}"
