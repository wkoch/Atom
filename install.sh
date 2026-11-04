#!/bin/sh
# Instala o binário `atom` (Linux/macOS) em ~/.local/bin.
# Uso: sh install.sh            # última release
#      ATOM_VERSION=v0.9.0 sh install.sh
#      ATOM_INSTALL_DIR=/opt/bin ATOM_REPO=voce/Atom sh install.sh
# Teste local: ATOM_DIST_URL=file:///tmp/atom-vtest-linux-amd64.tar.gz sh install.sh
set -eu

repo="${ATOM_REPO:-wkoch/Atom}"
bindir="${ATOM_INSTALL_DIR:-$HOME/.local/bin}"

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
Linux) os=linux ;;
Darwin) os=macos ;;
*) echo "SO não suportado: $os" >&2; exit 1 ;;
esac
case "$arch" in
x86_64 | amd64) arch=amd64 ;;
arm64 | aarch64) arch=arm64 ;;
*) echo "arquitetura não suportada: $arch" >&2; exit 1 ;;
esac
platform="$os-$arch"

if [ -n "${ATOM_VERSION:-}" ]; then
  tag="$ATOM_VERSION"
else
  tag="$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest" | sed 's#.*/##')"
fi
[ -n "$tag" ] || { echo "não achei a tag da release" >&2; exit 1; }

url="${ATOM_DIST_URL:-https://github.com/$repo/releases/download/$tag/atom-$tag-$platform.tar.gz}"
echo "baixando $url"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM
if command -v curl >/dev/null 2>&1; then
  curl -fsSL "$url" -o "$tmp/atom.tar.gz"
elif command -v wget >/dev/null 2>&1; then
  wget -qO "$tmp/atom.tar.gz" "$url"
else
  echo "preciso de curl ou wget" >&2; exit 1
fi
tar -xzf "$tmp/atom.tar.gz" -C "$tmp"
bin="$(find "$tmp" -name atom -type f | head -n 1)"
[ -n "$bin" ] || { echo "tarball sem binário atom" >&2; exit 1; }
mkdir -p "$bindir"
install -m 755 "$bin" "$bindir/atom"
echo "atom $tag instalado em $bindir/atom"
"$bindir/atom" version
case ":$PATH:" in
*":$bindir:"*) ;;
*) echo "adicione ao PATH: export PATH=\"$bindir:\$PATH\"" ;;
esac
