#!/bin/sh
# Installs UDM from this folder for the current user, without root:
#
#   ./install.sh            install into ~/.local
#   ./install.sh --remove   remove it again
#
# The programs go to ~/.local/lib/udm, `udm` to ~/.local/bin, and UDM
# shows up among the applications. UDM registers itself with the browsers
# it finds the first time it runs.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
LIB="$HOME/.local/lib/udm"

if [ "${1:-}" = "--remove" ]; then
  rm -rf "$LIB" "$HOME/.local/bin/udm" "$HOME/.local/bin/udm-cli" "$HOME/.local/bin/udm-mcp" "$DATA/applications/udm.desktop" "$DATA/icons/hicolor/128x128/apps/udm.png"
  echo "Removed UDM. Your settings and download list stay in ~/.config/udm and $DATA/udm."
  exit 0
fi

# UDM was called Pullbay in 0.4 and UIDM up to 0.3.3. Their programs go;
# their settings and lists move to UDM's folders when UDM first runs.
for old in pullbay uidm; do
  rm -rf "$HOME/.local/lib/$old" "$HOME/.local/bin/$old" "$DATA/applications/$old.desktop" "$DATA/icons/hicolor/128x128/apps/$old.png"
done
mkdir -p "$LIB" "$HOME/.local/bin" "$DATA/applications" "$DATA/icons/hicolor/128x128/apps"
for f in udm-gui udm-daemon udm-host udm-cli udm-mcp README.md THIRD-PARTY-NOTICES.txt; do
  cp -f "$HERE/$f" "$LIB/"
done
rm -rf "$LIB/extension" "$LIB/firefox"
cp -R "$HERE/extension" "$HERE/firefox" "$LIB/"
ln -sf "$LIB/udm-gui" "$HOME/.local/bin/udm"
ln -sf "$LIB/udm-cli" "$HOME/.local/bin/udm-cli"
ln -sf "$LIB/udm-mcp" "$HOME/.local/bin/udm-mcp"
cp -f "$HERE/udm.png" "$DATA/icons/hicolor/128x128/apps/udm.png"
sed "s|^Exec=.*|Exec=\"$LIB/udm-gui\"|" "$HERE/udm.desktop" > "$DATA/applications/udm.desktop"
echo "Installed UDM in $LIB."
echo "Load the browser extension from $LIB/extension (Chrome, Edge, Brave) or $LIB/firefox (Firefox)."
