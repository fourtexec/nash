#!/usr/bin/env sh
# One-line installer for the development build of nash:
#   wget -qO- https://raw.githubusercontent.com/fourtexec/nash/main/install-dev.sh | sh
# or:
#   curl -fsSL https://raw.githubusercontent.com/fourtexec/nash/main/install-dev.sh | sh
set -e

REPO="fourtexec/nash"
if [ -n "${TERMUX_VERSION:-}" ] || [ "$(uname -o 2>/dev/null)" = "Android" ]; then
    BIN_DIR="${PREFIX:-/data/data/com.termux/files/usr}/bin"
else
    BIN_DIR="${HOME}/.local/bin"
fi
mkdir -p "$BIN_DIR"

os=$(uname -s 2>/dev/null | tr '[:upper:]' '[:lower:]')
arch=$(uname -m 2>/dev/null)

if [ -n "${TERMUX_VERSION:-}" ] || [ "$(uname -o 2>/dev/null)" = "Android" ]; then
    os=android
elif [ "${os#darwin}" != "$os" ]; then
    os=macos
elif [ "${os#linux}" != "$os" ]; then
    os=linux
else
    echo "install-dev: unsupported OS: $os" >&2
    exit 1
fi

case "$arch" in
    x86_64|amd64) arch=x86_64 ;;
    aarch64|arm64) arch=aarch64 ;;
    *)
        echo "install-dev: unsupported architecture: $arch" >&2
        exit 1
        ;;
esac

asset="nash-${os}-${arch}"
# Dev builds are published as the latest release.
url="https://github.com/${REPO}/releases/latest/download/${asset}"
tmp="${TMPDIR:-/tmp}/nash-dev.$$"
echo "install-dev: downloading latest dev build ($asset)"
if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$tmp"
elif command -v wget >/dev/null 2>&1; then
    wget -qO "$tmp" "$url"
else
    echo "install-dev: needs curl or wget" >&2
    exit 1
fi
chmod 755 "$tmp"
mv -f "$tmp" "$BIN_DIR/nash"

for rc in "$HOME/.profile" "$HOME/.bashrc" "$HOME/.zshrc"; do
    [ -f "$rc" ] || : > "$rc"
    if ! grep -q ">>> nash >>>" "$rc" 2>/dev/null; then
        printf '\n# >>> nash >>>\nexport PATH="$HOME/.local/bin:$PATH"\n# <<< nash <<<\n' >> "$rc"
    fi
done
export PATH="$BIN_DIR:$PATH"

if [ -n "${TERMUX_VERSION:-}" ] || [ "$(uname -o 2>/dev/null)" = "Android" ]; then
    if command -v chsh >/dev/null 2>&1; then
        chsh -s nash 2>/dev/null || true
    fi
elif command -v chsh >/dev/null 2>&1; then
    # ensure /etc/shells contains nash
    if [ -w /etc/shells ] 2>/dev/null || [ "$(id -u)" = "0" ]; then
        grep -qxF "$BIN_DIR/nash" /etc/shells 2>/dev/null || echo "$BIN_DIR/nash" >> /etc/shells
    fi
    chsh -s "$BIN_DIR/nash" 2>/dev/null || true
fi

echo "install-dev: nash dev build installed to $BIN_DIR/nash"