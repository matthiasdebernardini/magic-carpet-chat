#!/usr/bin/env bash
# Wrap the built binary in a macOS .app bundle.
#
# Usage: scripts/make-app.sh [binary] [output-dir]
#   binary      defaults to target/release/magic-carpet-chat
#   output-dir  defaults to dist
#
# Writes "<output-dir>/Magic Carpet Chat.app" and prints its path, plus a
# "READ ME FIRST.txt" with the Gatekeeper steps — add that file to the zip
# root when distributing (macOS blocks unsigned apps on first open, and the
# recipient has no other way to learn the Open Anyway dance):
#
#   ditto -c -k --keepParent "<output-dir>/Magic Carpet Chat.app" app.zip
#   (cd "<output-dir>" && zip -X app.zip "READ ME FIRST.txt")
#
# The bundle is unsigned. Sign and notarize it after this script runs if a
# Developer ID is available.
set -euo pipefail

BIN=${1:-target/release/magic-carpet-chat}
OUT_DIR=${2:-dist}

APP_NAME="Magic Carpet Chat"
# Must match the directories::ProjectDirs triple in src/config.rs, or the app
# and the config directory disagree about who owns the data.
BUNDLE_ID="world.MagicCarpet.magic-carpet-chat"
EXECUTABLE=$(basename "$BIN")

if [ ! -f "$BIN" ]; then
  echo "make-app.sh: no binary at $BIN — run cargo build --release first" >&2
  exit 1
fi

# Version comes from Cargo.toml unless the caller sets one.
VERSION=${VERSION:-}
if [ -z "$VERSION" ]; then
  VERSION=$(awk -F'"' '/^version = "/ { print $2; exit }' Cargo.toml)
fi
VERSION=${VERSION#v}
# CFBundleVersion must be numeric dots; a pre-release suffix (0.5.0-rc.1)
# stays in the short version string only.
BUILD_VERSION=${VERSION%%-*}

APP="$OUT_DIR/$APP_NAME.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

install -m 755 "$BIN" "$APP/Contents/MacOS/$EXECUTABLE"
install -m 644 assets/icon.icns "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleDisplayName</key>
	<string>$APP_NAME</string>
	<key>CFBundleExecutable</key>
	<string>$EXECUTABLE</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
	<key>CFBundleIdentifier</key>
	<string>$BUNDLE_ID</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>$APP_NAME</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$VERSION</string>
	<key>CFBundleVersion</key>
	<string>$BUILD_VERSION</string>
	<key>LSMinimumSystemVersion</key>
	<string>12.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
PLIST

printf 'APPL????' > "$APP/Contents/PkgInfo"

# The first-open instructions. Ships at the zip root, because a non-technical
# recipient hits Gatekeeper before the app can show them anything.
cat > "$OUT_DIR/READ ME FIRST.txt" <<'TXT'
Magic Carpet Chat — how to open it the first time

This app is not signed with an Apple Developer ID yet, so macOS blocks the
first launch. This is expected. To open it:

1. Double-click "Magic Carpet Chat.app". macOS says it could not verify the
   app. Click "Done" — NOT "Move to Trash".
2. Open System Settings > Privacy & Security.
3. Scroll down to: "Magic Carpet Chat" was blocked to protect your Mac.
4. Click "Open Anyway", then "Open" (enter your Mac password if asked).

macOS asks this once. After that, the app opens normally.
TXT

echo "$APP"
