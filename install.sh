#!/usr/bin/env bash
# TX Open Studio — user-level install from a source checkout.
# Installs the binary, desktop entry and icons under ~/.local, and the udev
# rule (system-wide, asks for sudo) that makes the pedal's serial port
# accessible without root.
set -euo pipefail
cd "$(dirname "$0")"

BIN=target/release/tx-open-studio
if [[ ! -x $BIN ]]; then
    echo "==> Building release binary"
    cargo build --release
fi

echo "==> Installing to ~/.local"
install -Dm755 "$BIN" "$HOME/.local/bin/tx-open-studio"
install -Dm644 packaging/tx-open-studio.desktop "$HOME/.local/share/applications/tx-open-studio.desktop"
install -Dm644 assets/icon.png "$HOME/.local/share/icons/hicolor/256x256/apps/tx-open-studio.png"
install -Dm644 assets/icon.svg "$HOME/.local/share/icons/hicolor/scalable/apps/tx-open-studio.svg"
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true

RULE=/etc/udev/rules.d/70-tonex-pedal.rules
if ! cmp -s packaging/70-tonex-pedal.rules "$RULE" 2>/dev/null; then
    echo "==> Installing udev rule to $RULE (needs sudo; grants your desktop"
    echo "    session access to the pedal's serial port)"
    sudo install -Dm644 packaging/70-tonex-pedal.rules "$RULE"
    sudo udevadm control --reload-rules
    sudo udevadm trigger --subsystem-match=tty
else
    echo "==> udev rule already installed"
fi

case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) echo "NOTE: ~/.local/bin is not in your PATH; the app menu entry still works." ;;
esac

echo "Done. Run 'tx-open-studio' or launch 'TX Open Studio' from the app menu."
