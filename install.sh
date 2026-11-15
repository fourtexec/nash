#!/usr/bin/env sh
# One-line installer for nash:
#   wget -qO- https://raw.githubusercontent.com/fourtexec/nash/main/install.sh | sh
# or:
#   curl -fsSL https://raw.githubusercontent.com/fourtexec/nash/main/install.sh | sh
set -e

REPO="fourtexec/nash"
BIN_DIR="${HOME}/.local/bin"
mkdir -p "$BIN_DIR"

os=$(uname -s 2>/dev/null | tr '[:upper:]' '[:lower:]')
arch=$(uname -m 2>/dev/null)

# Detect Termux / Android.
if [ -n "${TERMUX_VERSION:-}" ] || [ "$(uname -o 2>/dev/null)" = "Android" ]; then
    os=android
elif [ "${os#darwin}" != "$os" ]; then
    os=macos
elif [ "${os#linux}" != "$os" ]; then
    os=linux
else
    echo "install: unsupported OS: $os" >&2
    exit 1
fi

case "$arch" in
    x86_64|amd64) arch=x86_64 ;;
    aarch64|arm64) arch=aarch64 ;;
    *)
        echo "install: unsupported architecture: $arch" >&2
        exit 1
        ;;
esac

asset="nash-${os}-${arch}"
# Stable: pinned to the latest stable release tag.
STABLE_TAG="v0.1.0"
url="https://github.com/${REPO}/releases/download/${STABLE_TAG}/${asset}"
tmp="${TMPDIR:-/tmp}/nash.$$"
echo "install: downloading $asset"
if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$tmp"
elif command -v wget >/dev/null 2>&1; then
    wget -qO "$tmp" "$url"
else
    echo "install: needs curl or wget" >&2
    exit 1
fi
chmod 755 "$tmp"
mv -f "$tmp" "$BIN_DIR/nash"

# Persist PATH on a permanent basis.
for rc in "$HOME/.profile" "$HOME/.bashrc" "$HOME/.zshrc"; do
    [ -f "$rc" ] || : > "$rc"
    if ! grep -q ">>> nash >>>" "$rc" 2>/dev/null; then
        printf '\n# >>> nash >>>\nexport PATH="$HOME/.local/bin:$PATH"\n# <<< nash <<<\n' >> "$rc"
    fi
done
export PATH="$BIN_DIR:$PATH"

# Make nash the login shell when possible.
if command -v chsh >/dev/null 2>&1; then
    chsh -s "$BIN_DIR/nash" 2>/dev/null || true
fi

echo "install: nash installed to $BIN_DIR/nash"
echo "install: open a new terminal or run: export PATH=\"$BIN_DIR:\$PATH\""