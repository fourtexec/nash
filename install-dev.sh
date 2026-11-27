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
if command -v curl >/dev/null 2>&1; then
    get() { curl -fsSL --proto '=https' --tlsv1.2 -- "$1"; }
    fetch_file() { curl -fsSL --proto '=https' --tlsv1.2 -o "$2" -- "$1"; }
elif command -v wget >/dev/null 2>&1; then
    get() { wget -qO- -- "$1"; }
    fetch_file() { wget -qO "$2" -- "$1"; }
else
    echo "install-dev: needs curl or wget" >&2
    exit 1
fi
# Dev builds are the releases tagged dev1, dev2, ... (pre-releases): take the
# one with the highest number.
tag=$(get "https://api.github.com/repos/${REPO}/releases?per_page=100" |
    tr ',' '\n' | sed -n 's/.*"tag_name": *"dev\([0-9][0-9]*\)".*/\1/p' | sort -n | tail -n 1)
case "$tag" in
    ''|*[!0-9]*)
        echo "install-dev: cannot find the latest dev build" >&2
        exit 1
        ;;
esac

base="https://github.com/${REPO}/releases/download/dev${tag}"
tmp_dir=$(mktemp -d "${BIN_DIR}/.nash-dev.XXXXXX")
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM
tmp="${tmp_dir}/${asset}"
sums="${tmp_dir}/SHA256SUMS"

echo "install-dev: downloading dev${tag} ($asset)"
fetch_file "${base}/${asset}" "$tmp"
fetch_file "${base}/SHA256SUMS" "$sums"

expected=$(awk -v f="$asset" '{gsub(/^\*/, "", $2); if ($2 == f) {print tolower($1); exit}}' "$sums")
if [ -z "$expected" ]; then
    echo "install-dev: checksum entry for $asset not found in SHA256SUMS" >&2
    exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp" | awk '{print tolower($1)}')
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$tmp" | awk '{print tolower($1)}')
elif command -v openssl >/dev/null 2>&1; then
    actual=$(openssl dgst -sha256 "$tmp" | awk '{print tolower($NF)}')
else
    echo "install-dev: needs sha256sum, shasum, or openssl to verify download integrity" >&2
    exit 1
fi
if [ "$actual" != "$expected" ]; then
    echo "install-dev: checksum mismatch for $asset (expected $expected, got $actual)" >&2
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