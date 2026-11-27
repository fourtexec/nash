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
latest_base="https://github.com/${REPO}/releases/latest/download"
fallback_base="https://github.com/${REPO}/releases/download/${FALLBACK_TAG}"

tmp_dir=$(mktemp -d "${BIN_DIR}/.nash-install.XXXXXX")
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM
tmp="${tmp_dir}/${asset}"
sums="${tmp_dir}/SHA256SUMS"

fetch_file() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --proto '=https' --tlsv1.2 -o "$2" -- "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$2" -- "$1"
    else
        echo "install: needs curl or wget" >&2
        exit 1
    fi
}

verify_sha256() {
    expected=$(awk -v f="$2" '{gsub(/^\*/, "", $2); if ($2 == f) {print tolower($1); exit}}' "$3")
    if [ -z "$expected" ]; then
        echo "install: checksum entry for $2 not found in SHA256SUMS" >&2
        exit 1
    fi
    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$1" | awk '{print tolower($1)}')
    elif command -v shasum >/dev/null 2>&1; then
        actual=$(shasum -a 256 "$1" | awk '{print tolower($1)}')
    elif command -v openssl >/dev/null 2>&1; then
        actual=$(openssl dgst -sha256 "$1" | awk '{print tolower($NF)}')
    else
        echo "install: needs sha256sum, shasum, or openssl to verify download integrity" >&2
        exit 1
    fi
    if [ "$actual" != "$expected" ]; then
        echo "install: checksum mismatch for $2 (expected $expected, got $actual)" >&2
        exit 1
    fi
}

echo "install: downloading $asset"
if fetch_file "${latest_base}/${asset}" "$tmp" 2>/dev/null && fetch_file "${latest_base}/SHA256SUMS" "$sums" 2>/dev/null; then
    :
else
    fetch_file "${fallback_base}/${asset}" "$tmp"
    fetch_file "${fallback_base}/SHA256SUMS" "$sums"
fi
verify_sha256 "$tmp" "$asset" "$sums"
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