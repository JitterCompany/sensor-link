#!/usr/bin/env bash
# Package the sensor-link-provision binary into a macOS .app bundle.
#
# Usage: macos/bundle.sh <binary> <icon.png> <out-dir> [version]
#
# Produces <out-dir>/sensor-link-provision.app with an .icns generated from
# the given PNG and an Info.plist stamped with the version. macOS only
# (needs sips + iconutil).

set -euo pipefail

BIN=${1:?binary path required}
ICON=${2:?icon png path required}
OUT=${3:?output dir required}
VERSION=${4:-0.0.0}

NAME="sensor-link-provision"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP="$OUT/$NAME.app"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BIN" "$APP/Contents/MacOS/$NAME"
chmod +x "$APP/Contents/MacOS/$NAME"

# Build the .icns from the PNG (all sizes iconutil expects).
ICONSET="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
    sips -z "$s" "$s" "$ICON" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
    d=$((s * 2))
    sips -z "$d" "$d" "$ICON" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

sed "s/@VERSION@/$VERSION/g" "$SCRIPT_DIR/Info.plist" > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist" >/dev/null

# Ad-hoc code-sign the assembled bundle. Without any signature, a quarantined
# app on Apple Silicon is reported as "damaged" with no way to allow it; an
# ad-hoc signature makes Gatekeeper show the normal "unidentified developer"
# prompt with an "Open Anyway" button instead. (Not notarised; the first
# launch still needs the user to allow it once.)
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"

echo "Built and ad-hoc signed $APP (version $VERSION)"
