#!/usr/bin/env bash
# Builds UDM's Linux packages into dist/:
#
#   installer/linux/package.sh                  build, then every package
#   installer/linux/package.sh --skip-build deb appimage
#
# Formats: deb, rpm, appimage, tar (all when none is named). The version
# comes from Cargo.toml. Needs dpkg-deb for deb, rpmbuild for rpm, node to
# check the Firefox copy of the extension, and for appimage the appimagetool
# named by $APPIMAGETOOL (downloaded when unset).
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
here="$root/installer/linux"
version="$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$root/Cargo.toml" | head -n1)"
arch="$(uname -m)"
case "$arch" in
  x86_64) debarch=amd64 ;;
  aarch64) debarch=arm64 ;;
  *) debarch="$arch" ;;
esac

build=1
formats=()
for a in "$@"; do
  case "$a" in
    --skip-build) build=0 ;;
    deb|rpm|appimage|tar) formats+=("$a") ;;
    *) echo "unknown argument: $a" >&2; exit 2 ;;
  esac
done
[ ${#formats[@]} -eq 0 ] && formats=(deb rpm appimage tar)

if [ "$build" = 1 ]; then
  (cd "$root" && cargo build --release --locked)
fi
bin="$root/target/release"
dist="$root/dist"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$dist"

# What every package holds: the programs, the extension and its Firefox
# copy, the README and the icon.
stage="$work/udm"
mkdir -p "$stage"
install -m755 "$bin/udm-gui" "$bin/udm-daemon" "$bin/udm-host" "$bin/udm-cli" "$bin/udm-mcp" "$stage/"
# Packages keep the folder names extension and firefox, so a browser that
# loaded one keeps finding it after an update. Refuse a stale Firefox copy.
node "$root/installer/make-firefox.js" --check >/dev/null
mkdir -p "$stage/extension" "$stage/firefox"
cp -R "$root/extension/chromium/." "$stage/extension/"
cp -R "$root/extension/firefox/." "$stage/firefox/"
install -m644 "$root/README.md" "$root/THIRD-PARTY-NOTICES.txt" "$stage/"
icon="$root/extension/chromium/icons/128.png"

# A system-wide tree: programs in /usr/lib/udm, `udm` (the window) and
# `udm-cli` (the command-line tool) and `udm-mcp` (the AI assistants'
# server) on the PATH, and
# the menu entry and icon where desktops look for them.
system_tree() {
  local dest="$1"
  mkdir -p "$dest/usr/lib" "$dest/usr/bin" "$dest/usr/share/applications" "$dest/usr/share/icons/hicolor/128x128/apps"
  cp -a "$stage" "$dest/usr/lib/udm"
  ln -s ../lib/udm/udm-gui "$dest/usr/bin/udm"
  ln -s ../lib/udm/udm-cli "$dest/usr/bin/udm-cli"
  ln -s ../lib/udm/udm-mcp "$dest/usr/bin/udm-mcp"
  install -m644 "$here/udm.desktop" "$dest/usr/share/applications/udm.desktop"
  install -m644 "$icon" "$dest/usr/share/icons/hicolor/128x128/apps/udm.png"
}

make_deb() {
  local tree="$work/deb"
  system_tree "$tree"
  mkdir -p "$tree/DEBIAN"
  local size
  size="$(du -sk "$tree/usr" | cut -f1)"
  cat > "$tree/DEBIAN/control" <<CONTROL
Package: udm
Version: $version
Architecture: $debarch
Maintainer: Tajbin Khan <tajbinofficial@gmail.com>
Installed-Size: $size
Depends: libc6 (>= 2.35), libgtk-3-0, libwebkit2gtk-4.1-0, libssl3 | libssl3t64
Recommends: libayatana-appindicator3-1, libnotify-bin, ffmpeg, yt-dlp
Suggests: clamav
Conflicts: pullbay, uidm
Replaces: pullbay, uidm
Section: net
Priority: optional
Homepage: https://github.com/tajbinkhan/UDM
Description: Download manager with a browser extension
 UDM (Unified Download Manager) downloads files over many connections at once, resumes them,
 captures downloads and videos from the browser, and runs queues on a
 schedule.
CONTROL
  dpkg-deb --root-owner-group --build "$tree" "$dist/udm_${version}_${debarch}.deb"
}

make_rpm() {
  local top="$work/rpm"
  mkdir -p "$top/BUILD" "$top/RPMS" "$top/SPECS" "$top/tree"
  system_tree "$top/tree"
  cat > "$top/SPECS/udm.spec" <<SPEC
Name: udm
Version: $version
Release: 1
Summary: Download manager with a browser extension
License: MIT AND CC-BY-4.0
URL: https://github.com/tajbinkhan/UDM
Requires: gtk3, webkit2gtk4.1, openssl-libs >= 3
Recommends: libayatana-appindicator-gtk3, libnotify, ffmpeg, yt-dlp
AutoReqProv: no
# UDM was called Pullbay in 0.4 and UIDM up to 0.3.3; this replaces
# those packages.
Obsoletes: pullbay
Obsoletes: uidm

%description
UDM (Unified Download Manager) downloads files over many connections at once, resumes them,
captures downloads and videos from the browser, and runs queues on a
schedule.

%install
cp -a $top/tree/. %{buildroot}/

%files
/usr/lib/udm
/usr/bin/udm
/usr/bin/udm-cli
/usr/bin/udm-mcp
/usr/share/applications/udm.desktop
/usr/share/icons/hicolor/128x128/apps/udm.png
SPEC
  rpmbuild --define "_topdir $top" --define "_build_id_links none" -bb "$top/SPECS/udm.spec" >/dev/null
  cp "$top"/RPMS/*/udm-*.rpm "$dist/udm-${version}-1.${arch}.rpm"
}

# The AppImage carries UDM's programs but not WebKitGTK, which the window
# draws with: the system needs WebKitGTK 4.1 (libwebkit2gtk-4.1), as the deb
# and rpm declare.
make_appimage() {
  local app="$work/UDM.AppDir"
  mkdir -p "$app/usr/bin" "$app/usr/share/udm"
  install -m755 "$stage/udm-gui" "$stage/udm-daemon" "$stage/udm-host" "$stage/udm-cli" "$stage/udm-mcp" "$app/usr/bin/"
  cp -a "$stage/extension" "$stage/firefox" "$stage/README.md" "$stage/THIRD-PARTY-NOTICES.txt" "$app/usr/share/udm/"
  install -m755 "$here/AppRun" "$app/AppRun"
  install -m644 "$here/udm.desktop" "$app/udm.desktop"
  install -m644 "$icon" "$app/udm.png"
  local tool="${APPIMAGETOOL:-}"
  if [ -z "$tool" ]; then
    tool="$work/appimagetool"
    curl -fsSL -o "$tool" "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${arch}.AppImage"
    chmod +x "$tool"
  fi
  # No FUSE needed to run the tool itself.
  ARCH="$arch" APPIMAGE_EXTRACT_AND_RUN=1 "$tool" --no-appstream "$app" "$dist/UDM-${version}-${arch}.AppImage" >/dev/null
}

make_tar() {
  local name="udm-${version}-linux-${arch}"
  local dir="$work/$name"
  cp -a "$stage" "$dir"
  install -m644 "$here/udm.desktop" "$icon" "$dir/"
  mv "$dir/128.png" "$dir/udm.png"
  install -m755 "$here/install.sh" "$dir/"
  tar -C "$work" -czf "$dist/$name.tar.gz" "$name"
}

for f in "${formats[@]}"; do
  echo "== $f"
  "make_$f"
done
ls -la "$dist"
