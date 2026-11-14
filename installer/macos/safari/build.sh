#!/usr/bin/env bash
# Builds UDM's Safari Web Extension into a UDM.app that package.sh has put
# together, before the app is signed:
#
#   installer/macos/safari/build.sh <UDM.app> <version>
#
# Xcode's safari-web-extension-converter makes a project from
# extension/safari (the copy installer/make-safari.js makes), the handler
# here replaces the one it writes, and xcodebuild builds the app extension
# for Apple Silicon and Intel Macs. It goes to
# UDM.app/Contents/PlugIns/UDM Extension.appex with a copy of the app's
# udm-host, and both are signed ad hoc with the sandbox Safari needs:
#
#   safari-extension.entitlements  the app extension: the sandbox, a
#       connection to the daemon on 127.0.0.1, and UDM's data folder
#       (~/Library/Application Support/UDM), which holds the daemon's token
#       and udm-host.log.
#   udm-host.entitlements          the copy of udm-host: a program a
#       sandboxed one starts must be signed sandboxed, and with inherit it
#       takes the app extension's sandbox as it is. UDM.app's own udm-host,
#       which Chrome, Edge and Firefox start, keeps none.
#
# Without Xcode (only the Command Line Tools) there is no converter: this
# says so and adds nothing, so the disk image is still made, unless
# UDM_SAFARI=require, which CI's build check sets.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
app="${1:?usage: build.sh <UDM.app> <version>}"
version="${2:?usage: build.sh <UDM.app> <version>}"

# The app extension's bundle ID, under the app's com.udm.app.
bundle_id="com.udm.app.safari-extension"

if ! xcrun --find safari-web-extension-converter >/dev/null 2>&1; then
  if [ "${UDM_SAFARI:-}" = "require" ]; then
    echo "error: Xcode's safari-web-extension-converter is missing; the Safari extension cannot be built" >&2
    exit 1
  fi
  echo "warning: Xcode's safari-web-extension-converter is missing; UDM.app gets no Safari extension" >&2
  exit 0
fi

# Refuse a stale Safari copy, as package.sh refuses a stale Firefox one.
node "$root/installer/make-safari.js" --check >/dev/null

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# The converter's options, all in one place: a macOS-only Swift project
# named UDM in $work/project, with its own copy of the extension's files,
# made without asking and without opening Xcode. The project's own app is
# built too but not used: the app extension goes into UDM.app.
converter_options=(
  --project-location "$work/project"
  --app-name "UDM"
  --bundle-identifier "com.udm.app"
  --swift
  --macos-only
  --copy-resources
  --no-open
  --no-prompt
  --force
)
xcrun safari-web-extension-converter "${converter_options[@]}" "$root/extension/safari"

handlers=0
while IFS= read -r -d '' handler; do
  cp "$here/SafariWebExtensionHandler.swift" "$handler"
  handlers=$((handlers + 1))
done < <(find "$work/project" -name SafariWebExtensionHandler.swift -print0)
if [ "$handlers" -eq 0 ]; then
  echo "error: the converter wrote no SafariWebExtensionHandler.swift to replace" >&2
  exit 1
fi

project="$(find "$work/project" -maxdepth 3 -name '*.xcodeproj' -print -quit)"
if [ -z "$project" ]; then
  echo "error: the converter made no Xcode project" >&2
  exit 1
fi
# Only the app and its extension: the converter also makes test targets
# (UDMTests, UDMUITests) whose `@testable import` does not build in Release.
targets=()
while IFS= read -r t; do
  case "$t" in
    *Tests) ;;
    *) targets+=(-target "$t") ;;
  esac
done < <(xcodebuild -list -project "$project" |
  awk '/^[[:space:]]*Targets:/ { f = 1; next } /^[[:space:]]*$/ { f = 0 } f { sub(/^[[:space:]]+/, ""); print }')
if [ "${#targets[@]}" -eq 0 ]; then
  echo "error: the converter's Xcode project lists no targets" >&2
  exit 1
fi
xcodebuild -quiet -project "$project" "${targets[@]}" -configuration Release \
  SYMROOT="$work/build" \
  ARCHS="arm64 x86_64" ONLY_ACTIVE_ARCH=NO \
  MACOSX_DEPLOYMENT_TARGET=11.0 \
  SWIFT_VERSION=5.0 \
  MARKETING_VERSION="$version" CURRENT_PROJECT_VERSION="$version" \
  CODE_SIGNING_ALLOWED=NO \
  build

built="$(find "$work/build/Release" -maxdepth 1 -name '*.appex' -print -quit)"
if [ -z "$built" ]; then
  echo "error: xcodebuild made no app extension" >&2
  exit 1
fi

mkdir -p "$app/Contents/PlugIns"
appex="$app/Contents/PlugIns/UDM Extension.appex"
rm -rf "$appex"
cp -R "$built" "$appex"
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $bundle_id" "$appex/Contents/Info.plist"
cp "$app/Contents/MacOS/udm-host" "$appex/Contents/MacOS/udm-host"

# Inside out, so each seal covers what is already signed. No --deep: it
# would sign the copy of udm-host again without its entitlements.
codesign --force --sign - --entitlements "$here/udm-host.entitlements" "$appex/Contents/MacOS/udm-host"
codesign --force --sign - --entitlements "$here/safari-extension.entitlements" "$appex"
echo "Added the Safari extension to $app"
