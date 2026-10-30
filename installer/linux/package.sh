#!/usr/bin/env bash
# Builds UIDM's Linux packages into dist/:
#
#   installer/linux/package.sh                  build, then every package
#   installer/linux/package.sh --skip-build deb appimage
#
# Formats: deb, rpm, appimage, tar (all when none is named). The version
# comes from Cargo.toml. Needs dpkg-deb for deb, rpmbuild for rpm, node for
# the Firefox copy of the extension, and for appimage the appimagetool
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
stage="$work/uidm"
mkdir -p "$stage"
install -m755 "$bin/uidm-gui" "$bin/uidm-daemon" "$bin/uidm-host" "$stage/"
mkdir -p "$stage/extension"
(cd "$root/extension" && tar cf - --exclude=./test --exclude=./firefox.json .) | (cd "$stage/extension" && tar xf -)
node "$root/installer/make-firefox.js" "$stage/firefox" >/dev/null
install -m644 "$root/README.md" "$stage/"
icon="$root/extension/icons/128.png"

# A system-wide tree: programs in /usr/lib/uidm, `uidm` on the PATH, and
# the menu entry and icon where desktops look for them.
system_tree() {
  local dest="$1"
  mkdir -p "$dest/usr/lib" "$dest/usr/bin" "$dest/usr/share/applications" "$dest/usr/share/icons/hicolor/128x128/apps"
  cp -a "$stage" "$dest/usr/lib/uidm"
  ln -s ../lib/uidm/uidm-gui "$dest/usr/bin/uidm"
  install -m644 "$here/uidm.desktop" "$dest/usr/share/applications/uidm.desktop"
  install -m644 "$icon" "$dest/usr/share/icons/hicolor/128x128/apps/uidm.png"
}

make_deb() {
  local tree="$work/deb"
  system_tree "$tree"
  mkdir -p "$tree/DEBIAN"
  local size
  size="$(du -sk "$tree/usr" | cut -f1)"
  cat > "$tree/DEBIAN/control" <<CONTROL
Package: uidm
Version: $version
Architecture: $debarch
Maintainer: Tajbin Khan <tajbinofficial@gmail.com>
Installed-Size: $size
Depends: libc6 (>= 2.35), libgtk-3-0, libssl3 | libssl3t64, libxkbcommon-x11-0, libgl1, libegl1
Recommends: libayatana-appindicator3-1, libnotify-bin, ffmpeg
Suggests: clamav
Section: net
Priority: optional
Homepage: https://github.com/tajbinkhan/uidm
Description: Download manager with a browser extension
 UIDM downloads files over many connections at once, resumes them,
 captures downloads and videos from the browser, and runs queues on a
 schedule.
CONTROL
  dpkg-deb --root-owner-group --build "$tree" "$dist/uidm_${version}_${debarch}.deb"
}

make_rpm() {
  local top="$work/rpm"
  mkdir -p "$top/BUILD" "$top/RPMS" "$top/SPECS" "$top/tree"
  system_tree "$top/tree"
  cat > "$top/SPECS/uidm.spec" <<SPEC
Name: uidm
Version: $version
Release: 1
Summary: Download manager with a browser extension
License: MIT
URL: https://github.com/tajbinkhan/uidm
Requires: gtk3, openssl-libs >= 3, libxkbcommon-x11, libglvnd-glx, libglvnd-egl
Recommends: libayatana-appindicator-gtk3, libnotify, ffmpeg
AutoReqProv: no

%description
UIDM downloads files over many connections at once, resumes them,
captures downloads and videos from the browser, and runs queues on a
schedule.

%install
cp -a $top/tree/. %{buildroot}/

%files
/usr/lib/uidm
/usr/bin/uidm
/usr/share/applications/uidm.desktop
/usr/share/icons/hicolor/128x128/apps/uidm.png
SPEC
  rpmbuild --define "_topdir $top" --define "_build_id_links none" -bb "$top/SPECS/uidm.spec" >/dev/null
  cp "$top"/RPMS/*/uidm-*.rpm "$dist/uidm-${version}-1.${arch}.rpm"
}

make_appimage() {
  local app="$work/UIDM.AppDir"
  mkdir -p "$app/usr/bin" "$app/usr/share/uidm"
  install -m755 "$stage/uidm-gui" "$stage/uidm-daemon" "$stage/uidm-host" "$app/usr/bin/"
  cp -a "$stage/extension" "$stage/firefox" "$stage/README.md" "$app/usr/share/uidm/"
  install -m755 "$here/AppRun" "$app/AppRun"
  install -m644 "$here/uidm.desktop" "$app/uidm.desktop"
  install -m644 "$icon" "$app/uidm.png"
  local tool="${APPIMAGETOOL:-}"
  if [ -z "$tool" ]; then
    tool="$work/appimagetool"
    curl -fsSL -o "$tool" "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${arch}.AppImage"
    chmod +x "$tool"
  fi
  # No FUSE needed to run the tool itself.
  ARCH="$arch" APPIMAGE_EXTRACT_AND_RUN=1 "$tool" --no-appstream "$app" "$dist/UIDM-${version}-${arch}.AppImage" >/dev/null
}

make_tar() {
  local name="uidm-${version}-linux-${arch}"
  local dir="$work/$name"
  cp -a "$stage" "$dir"
  install -m644 "$here/uidm.desktop" "$icon" "$dir/"
  mv "$dir/128.png" "$dir/uidm.png"
  install -m755 "$here/install.sh" "$dir/"
  tar -C "$work" -czf "$dist/$name.tar.gz" "$name"
}

for f in "${formats[@]}"; do
  echo "== $f"
  "make_$f"
done
ls -la "$dist"
