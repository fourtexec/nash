#!/usr/bin/env sh
# One-line installer for nash:
#   wget -qO- https://raw.githubusercontent.com/fourtexec/nash/main/install.sh | sh
# or:
#   curl -fsSL https://raw.githubusercontent.com/fourtexec/nash/main/install.sh | sh
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
# Stable: the latest release (development builds are pre-releases and never
# the latest). If that one has no binary for this platform, the last stable
# release known to have one.
FALLBACK_TAG="v0.1.0.7"
latest="https://github.com/${REPO}/releases/latest/download/${asset}"
fallback="https://github.com/${REPO}/releases/download/${FALLBACK_TAG}/${asset}"
tmp="$BIN_DIR/.nash.$$"
echo "install: downloading $asset"
if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$latest" -o "$tmp" 2>/dev/null || curl -fsSL "$fallback" -o "$tmp"
elif command -v wget >/dev/null 2>&1; then
    wget -qO "$tmp" "$latest" || wget -qO "$tmp" "$fallback"
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

echo "install: nash installed to $BIN_DIR/nash"
echo "install: open a new terminal or run: export PATH=\"$BIN_DIR:\$PATH\""