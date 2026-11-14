#!/usr/bin/env bash
# Builds UDM's macOS disk image, dist/UDM-<version>-macos.dmg, holding
# UDM.app for both Apple Silicon and Intel Macs:
#
#   installer/macos/package.sh               build both, then the image
#   installer/macos/package.sh --skip-build  the two release builds are current
#
# Runs on a Mac (lipo, sips, iconutil, codesign and hdiutil come with it)
# with the aarch64-apple-darwin and x86_64-apple-darwin targets installed,
# and node to check the Firefox and Safari copies of the extension. With
# Xcode it also builds the Safari extension into UDM.app
# (installer/macos/safari/build.sh); without it the app has none, unless
# UDM_SAFARI=require makes that an error. The app is signed ad hoc, not with
# a Developer ID, so the first start needs right-click > Open.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
version="$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$root/Cargo.toml" | head -n1)"
targets=(aarch64-apple-darwin x86_64-apple-darwin)

if [ "${1:-}" != "--skip-build" ]; then
  for t in "${targets[@]}"; do
    (cd "$root" && cargo build --release --locked --target "$t")
  done
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
app="$work/dmg/UDM.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$root/dist"

# One program for both kinds of Mac.
for p in udm-gui udm-daemon udm-host udm-cli udm-mcp; do
  lipo -create -output "$app/Contents/MacOS/$p" \
    "$root/target/aarch64-apple-darwin/release/$p" "$root/target/x86_64-apple-darwin/release/$p"
done

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key><string>UDM</string>
	<key>CFBundleDisplayName</key><string>UDM</string>
	<key>CFBundleIdentifier</key><string>com.udm.app</string>
	<key>CFBundleExecutable</key><string>udm-gui</string>
	<key>CFBundleIconFile</key><string>udm</string>
	<key>CFBundlePackageType</key><string>APPL</string>
	<key>CFBundleShortVersionString</key><string>$version</string>
	<key>CFBundleVersion</key><string>$version</string>
	<key>LSMinimumSystemVersion</key><string>11.0</string>
	<key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
	<key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# The icon, from the 1024-pixel master.
iconset="$work/udm.iconset"
mkdir -p "$iconset"
for s in 16 32 64 128 256 512 1024; do
  sips -z "$s" "$s" "$root/assets/icon-1024.png" --out "$iconset/icon_${s}x${s}.png" >/dev/null
done
for s in 16 32 128 256 512; do
  cp "$iconset/icon_$((s * 2))x$((s * 2)).png" "$iconset/icon_${s}x${s}@2x.png"
done
rm "$iconset/icon_64x64.png" "$iconset/icon_1024x1024.png"
iconutil -c icns -o "$app/Contents/Resources/udm.icns" "$iconset"

# The extension to load in the browser, its Firefox copy, and the README.
# They keep the folder names extension and firefox, so a browser that loaded
# one keeps finding it after an update. Refuse a stale Firefox copy.
node "$root/installer/make-firefox.js" --check >/dev/null
mkdir -p "$app/Contents/Resources/extension" "$app/Contents/Resources/firefox"
cp -R "$root/extension/chromium/." "$app/Contents/Resources/extension/"
cp -R "$root/extension/firefox/." "$app/Contents/Resources/firefox/"
cp "$root/README.md" "$root/THIRD-PARTY-NOTICES.txt" "$app/Contents/Resources/"

# The Safari extension, an app extension in Contents/PlugIns, signed there
# with its own entitlements. A build that fails leaves it out, so the disk
# image is still made, unless UDM_SAFARI=require.
if ! "$root/installer/macos/safari/build.sh" "$app" "$version"; then
  [ "${UDM_SAFARI:-}" = require ] && exit 1
  echo "warning: the Safari extension did not build; UDM.app has none" >&2
  rm -rf "$app/Contents/PlugIns"
fi

# Signed inside out without --deep, which would sign the Safari extension
# again without its entitlements: the other programs, then the app.
for p in udm-daemon udm-host udm-cli udm-mcp; do
  codesign --force --sign - "$app/Contents/MacOS/$p"
done
codesign --force --sign - "$app"
ln -s /Applications "$work/dmg/Applications"
dmg="$root/dist/UDM-${version}-macos.dmg"
rm -f "$dmg"
hdiutil create -volname "UDM $version" -srcfolder "$work/dmg" -ov -format UDZO "$dmg" >/dev/null
echo "Wrote $dmg"
