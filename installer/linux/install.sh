#!/bin/sh
# Installs UIDM from this folder for the current user, without root:
#
#   ./install.sh            install into ~/.local
#   ./install.sh --remove   remove it again
#
# The programs go to ~/.local/lib/uidm, `uidm` to ~/.local/bin, and UIDM
# shows up among the applications. UIDM registers itself with the browsers
# it finds the first time it runs.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
LIB="$HOME/.local/lib/uidm"

if [ "${1:-}" = "--remove" ]; then
  rm -rf "$LIB" "$HOME/.local/bin/uidm" "$DATA/applications/uidm.desktop" "$DATA/icons/hicolor/128x128/apps/uidm.png"
  echo "Removed UIDM. Your settings and download list stay in ~/.config/uidm and $DATA/uidm."
  exit 0
fi

mkdir -p "$LIB" "$HOME/.local/bin" "$DATA/applications" "$DATA/icons/hicolor/128x128/apps"
for f in uidm-gui uidm-daemon uidm-host README.md; do
  cp -f "$HERE/$f" "$LIB/"
done
rm -rf "$LIB/extension" "$LIB/firefox"
cp -R "$HERE/extension" "$HERE/firefox" "$LIB/"
ln -sf "$LIB/uidm-gui" "$HOME/.local/bin/uidm"
cp -f "$HERE/uidm.png" "$DATA/icons/hicolor/128x128/apps/uidm.png"
sed "s|^Exec=.*|Exec=\"$LIB/uidm-gui\"|" "$HERE/uidm.desktop" > "$DATA/applications/uidm.desktop"
echo "Installed UIDM in $LIB."
echo "Load the browser extension from $LIB/extension (Chrome, Edge, Brave) or $LIB/firefox (Firefox)."
