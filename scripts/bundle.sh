#!/bin/sh
# Build Stepan.app plus the release archives into dist/.
#
#   scripts/bundle.sh                     # this Mac's architecture
#   scripts/bundle.sh aarch64-apple-darwin
#   scripts/bundle.sh x86_64-apple-darwin
#
# dist/stepan-macos-<arch>.zip  — what the in-app updater downloads (names are fixed)
# dist/stepan-macos-<arch>.dmg  — drag-to-Applications installer
#
# The executable inside stays `iv3`: agents call `iv3 task …` from it.
set -eu
cd "$(dirname "$0")/.."

TARGET="${1:-}"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
if [ -n "$TARGET" ]; then
  cargo build --release -p iv3-app --target "$TARGET"
  BIN="target/$TARGET/release/iv3"
  case "$TARGET" in aarch64*) ARCH=arm64 ;; *) ARCH=x86_64 ;; esac
else
  cargo build --release -p iv3-app
  BIN="target/release/iv3"
  case "$(uname -m)" in arm64) ARCH=arm64 ;; *) ARCH=x86_64 ;; esac
fi

DIST=dist
APP="$DIST/Stepan.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/iv3"
cp crates/iv3-app/assets/icon/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Stepan</string>
  <key>CFBundleDisplayName</key><string>Stepan</string>
  <key>CFBundleIdentifier</key><string>com.integratorivan.stepan</string>
  <key>CFBundleExecutable</key><string>iv3</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

# Ad-hoc signature: required to run on Apple Silicon. Not notarized, so the
# first launch of a downloaded copy needs right click → Open (see README).
codesign --force --deep --sign - "$APP"

ZIP="$DIST/stepan-macos-$ARCH.zip"
DMG="$DIST/stepan-macos-$ARCH.dmg"
rm -f "$ZIP" "$DMG"
ditto -c -k --keepParent "$APP" "$ZIP"

STAGE=$(mktemp -d)
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -quiet -volname "Stepan $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
rm -rf "$STAGE"

echo "Stepan $VERSION ($ARCH):"
ls -lh "$ZIP" "$DMG"
