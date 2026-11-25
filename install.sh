#!/bin/bash
# NeoTerm 0.3 installer.
#
#   ./install.sh            user install  (~/.local, no sudo)
#   ./install.sh --system   system install (/usr/local, needs sudo)
#
# Honors PREFIX and DESTDIR for packaging:
#   PREFIX=/opt/neoterm DESTDIR=/tmp/pkg ./install.sh
set -euo pipefail

cd "$(dirname "$0")"

PREFIX="${PREFIX:-$HOME/.local}"
for arg in "$@"; do
    case "$arg" in
        --system) PREFIX="/usr/local" ;;
        -h|--help)
            echo "Usage: ./install.sh [--system]"
            echo "  default: user install into ~/.local (no sudo)"
            echo "  --system: install into /usr/local (run with sudo)"
            exit 0
            ;;
        *) echo "unknown option: $arg (try --help)" >&2; exit 1 ;;
    esac
done
DESTDIR="${DESTDIR:-}"

BIN="$DESTDIR$PREFIX/bin/neoterm"
APPS="$DESTDIR$PREFIX/share/applications"
ICONS="$DESTDIR$PREFIX/share/icons/hicolor/scalable/apps"
MAN="$DESTDIR$PREFIX/share/man/man1"
DOCS="$DESTDIR$PREFIX/share/doc/neoterm"

echo "==> building NeoTerm (release)"
cargo build --release

echo "==> installing to $PREFIX"
install -Dm755 target/release/neoterm "$BIN"
install -Dm644 assets/neoterm.desktop "$APPS/neoterm.desktop"
install -Dm644 assets/neoterm.svg "$ICONS/neoterm.svg"
install -Dm644 docs/neoterm.1 "$MAN/neoterm.1"
install -Dm644 docs/KEYBINDINGS.md "$DOCS/KEYBINDINGS.md"

if [ -z "$DESTDIR" ]; then
    command -v update-desktop-database >/dev/null && update-desktop-database "$APPS" || true
    command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -f -t "$DESTDIR$PREFIX/share/icons/hicolor" || true
    command -v mandb >/dev/null && mandb "$MAN" || true
fi

echo "==> verifying"
test -x "$BIN" && echo "  binary: $BIN"
if command -v desktop-file-validate >/dev/null; then
    desktop-file-validate "$APPS/neoterm.desktop" && echo "  desktop file: valid"
else
    echo "  desktop file: installed (desktop-file-validate not available)"
fi

echo "done. Launch 'neoterm' (ensure $PREFIX/bin is on your PATH) or find NeoTerm in your app menu."
