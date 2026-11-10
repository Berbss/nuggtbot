#!/bin/sh
# nuggtbot headless installer for Linux servers.
#
#   curl -fsSL https://raw.githubusercontent.com/Berbss/nuggtbot/main/install.sh | sh
#
# Downloads the latest prebuilt headless binary (no GUI/GTK dependency) and
# installs it. Override the install dir with PREFIX=/path, the version with
# NUGGTBOT_VERSION=vX.Y.Z. Set NUGGTBOT_SERVICE=1 (as root) to also install a
# systemd service. Review this script before piping it to a shell.
set -eu

REPO="Berbss/nuggtbot"
PREFIX="${PREFIX:-/usr/local/bin}"
DATA_DIR="${NUGGTBOT_DATA_DIR:-/var/lib/nuggtbot}"

need() { command -v "$1" >/dev/null 2>&1 || { echo "error: '$1' is required" >&2; exit 1; }; }
need curl
need tar

arch="$(uname -m)"
case "$arch" in
  x86_64 | amd64) A="x86_64" ;;
  aarch64 | arm64) A="arm64" ;;
  *) echo "error: unsupported arch '$arch' (need x86_64 or aarch64)" >&2; exit 1 ;;
esac

ver="${NUGGTBOT_VERSION:-}"
if [ -z "$ver" ]; then
  ver="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | grep '"tag_name"' | head -1 | cut -d'"' -f4)"
fi
[ -n "$ver" ] || { echo "error: could not resolve latest version; set NUGGTBOT_VERSION=vX.Y.Z" >&2; exit 1; }

url="https://github.com/$REPO/releases/download/$ver/nuggtbot-linux-$A.tar.gz"
echo "downloading nuggtbot $ver ($A)..."
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fSL "$url" -o "$tmp/nuggtbot.tar.gz"
tar -C "$tmp" -xzf "$tmp/nuggtbot.tar.gz"

install -d "$PREFIX"
install -m 0755 "$tmp/nuggtbot" "$PREFIX/nuggtbot"
echo "installed: $PREFIX/nuggtbot"

if [ "${NUGGTBOT_SERVICE:-0}" = "1" ]; then
  [ "$(id -u)" = "0" ] || { echo "error: NUGGTBOT_SERVICE=1 needs root" >&2; exit 1; }
  install -d "$DATA_DIR"
  [ -f "$DATA_DIR/config.toml" ] || cat > "$DATA_DIR/config.toml" <<EOF
# Panel has NO auth — keep bind on localhost or a private network (Tailscale).
bind = "127.0.0.1:7777"
data_file = "data.json"
EOF
  cat > /etc/systemd/system/nuggtbot.service <<EOF
[Unit]
Description=nuggtbot
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=$DATA_DIR
ExecStart=$PREFIX/nuggtbot --headless
Environment=RUST_LOG=nuggtbot=info
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload
  systemctl enable --now nuggtbot.service
  echo "systemd service nuggtbot.service installed and started (workdir $DATA_DIR)"
else
  echo "run it:  nuggtbot --headless    (open http://127.0.0.1:7777)"
  echo "or set NUGGTBOT_SERVICE=1 (as root) to install a systemd service."
fi
