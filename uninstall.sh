#!/bin/bash
# NeoTerm uninstaller. Mirrors install.sh paths.
#
#   ./uninstall.sh            remove user install (~/.local)
#   ./uninstall.sh --system   remove system install (/usr/local, needs sudo)
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
for arg in "$@"; do
    case "$arg" in
        --system) PREFIX="/usr/local" ;;
        -h|--help)
            echo "Usage: ./uninstall.sh [--system]"
            exit 0
            ;;
        *) echo "unknown option: $arg (try --help)" >&2; exit 1 ;;
    esac
done
DESTDIR="${DESTDIR:-}"

rm -f "$DESTDIR$PREFIX/bin/neoterm"
rm -f "$DESTDIR$PREFIX/share/applications/neoterm.desktop"
rm -f "$DESTDIR$PREFIX/share/icons/hicolor/scalable/apps/neoterm.svg"

if [ -z "$DESTDIR" ]; then
    command -v update-desktop-database >/dev/null && update-desktop-database "$DESTDIR$PREFIX/share/applications" || true
    command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -f -t "$DESTDIR$PREFIX/share/icons/hicolor" || true
fi

echo "NeoTerm removed from $PREFIX (your ~/.config/neoterm/config.toml was left alone)."
