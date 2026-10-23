#!/bin/sh
# Installs Peek for the current user: the binary in ~/.local/bin, plus a
# launcher entry and icon so it shows up in the application menu.
# Run it again to update; pass --uninstall to remove.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
data=${XDG_DATA_HOME:-$HOME/.local/share}
bin=$HOME/.local/bin

if [ "${1:-}" = "--uninstall" ]; then
  rm -f "$bin/peek" "$data/applications/peek.desktop" "$data/icons/hicolor/scalable/apps/peek.svg"
  echo "Peek removed."
  exit 0
fi

install -Dm755 "$here/peek" "$bin/peek"
install -Dm644 "$here/peek.desktop" "$data/applications/peek.desktop"
install -Dm644 "$here/peek.svg" "$data/icons/hicolor/scalable/apps/peek.svg"
command -v update-desktop-database >/dev/null && update-desktop-database -q "$data/applications" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q "$data/icons/hicolor" 2>/dev/null || true

echo "Peek installed to $bin/peek."
case ":$PATH:" in *":$bin:"*) ;; *) echo "Add $bin to your PATH to run 'peek' from a terminal." ;; esac
