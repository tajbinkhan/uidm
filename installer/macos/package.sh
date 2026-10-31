#!/usr/bin/env bash
# Builds UIDM's macOS disk image, dist/UIDM-<version>-macos.dmg, holding
# UIDM.app for both Apple Silicon and Intel Macs:
#
#   installer/macos/package.sh               build both, then the image
#   installer/macos/package.sh --skip-build  the two release builds are current
#
# Runs on a Mac (lipo, sips, iconutil, codesign and hdiutil come with it)
# with the aarch64-apple-darwin and x86_64-apple-darwin targets installed,
# and node for the Firefox copy of the extension. The app is signed ad hoc,
# not with a Developer ID, so the first start needs right-click > Open.
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
app="$work/dmg/UIDM.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$root/dist"

# One program for both kinds of Mac.
for p in uidm-gui uidm-daemon uidm-host; do
  lipo -create -output "$app/Contents/MacOS/$p" \
    "$root/target/aarch64-apple-darwin/release/$p" "$root/target/x86_64-apple-darwin/release/$p"
done

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key><string>UIDM</string>
	<key>CFBundleDisplayName</key><string>UIDM</string>
	<key>CFBundleIdentifier</key><string>com.uidm.app</string>
	<key>CFBundleExecutable</key><string>uidm-gui</string>
	<key>CFBundleIconFile</key><string>uidm</string>
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
iconset="$work/uidm.iconset"
mkdir -p "$iconset"
for s in 16 32 64 128 256 512 1024; do
  sips -z "$s" "$s" "$root/assets/icon-1024.png" --out "$iconset/icon_${s}x${s}.png" >/dev/null
done
for s in 16 32 128 256 512; do
  cp "$iconset/icon_$((s * 2))x$((s * 2)).png" "$iconset/icon_${s}x${s}@2x.png"
done
rm "$iconset/icon_64x64.png" "$iconset/icon_1024x1024.png"
iconutil -c icns -o "$app/Contents/Resources/uidm.icns" "$iconset"

# The extension to load in the browser, its Firefox copy, and the README.
mkdir -p "$app/Contents/Resources/extension"
(cd "$root/extension" && tar cf - --exclude=./test --exclude=./firefox.json .) | (cd "$app/Contents/Resources/extension" && tar xf -)
node "$root/installer/make-firefox.js" "$app/Contents/Resources/firefox" >/dev/null
cp "$root/README.md" "$app/Contents/Resources/"

codesign --force --deep --sign - "$app"
ln -s /Applications "$work/dmg/Applications"
dmg="$root/dist/UIDM-${version}-macos.dmg"
rm -f "$dmg"
hdiutil create -volname "UIDM $version" -srcfolder "$work/dmg" -ov -format UDZO "$dmg" >/dev/null
echo "Wrote $dmg"
