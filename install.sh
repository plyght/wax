#!/usr/bin/env bash
# wax installer — downloads the latest pre-built binary from GitHub Releases.
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/semitechnological/wax/master/install.sh | bash
#   # or with a specific version:
#   WAX_VERSION=v0.13.1 bash install.sh

set -euo pipefail

REPO="semitechnological/wax"
INSTALL_DIR="${WAX_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${WAX_VERSION:-}"

RED='\033[0;31m'; GREEN='\033[0;32m'; CYAN='\033[0;36m'; BOLD='\033[1m'; NC='\033[0m'

info()  { printf "${CYAN}%s${NC}\n" "$*"; }
ok()    { printf "${GREEN}✓ %s${NC}\n" "$*"; }
die()   { printf "${RED}error: %s${NC}\n" "$*" >&2; exit 1; }

# ---- detect OS / arch -------------------------------------------------------

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Linux)  os="linux" ;;
  Darwin) os="macos" ;;
  *)      die "Unsupported OS: $OS" ;;
esac

case "$ARCH" in
  x86_64|amd64)          arch="x64"   ;;
  aarch64|arm64)         arch="arm64" ;;
  *)                     die "Unsupported architecture: $ARCH" ;;
esac

ASSET="wax-${os}-${arch}"

# ---- resolve version --------------------------------------------------------

if [ -z "$VERSION" ]; then
  info "Fetching latest release version…"
  VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\(.*\)".*/\1/')"
  [ -n "$VERSION" ] || die "Could not determine latest version from GitHub API"
fi

info "Installing wax ${VERSION} (${os}/${arch})…"

# ---- download ---------------------------------------------------------------

URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET}"
TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT

if command -v curl &>/dev/null; then
  curl -fsSL --progress-bar "$URL" -o "$TMP"
elif command -v wget &>/dev/null; then
  wget -q --show-progress "$URL" -O "$TMP"
else
  die "curl or wget is required"
fi

chmod +x "$TMP"

# ---- install ----------------------------------------------------------------

mkdir -p "$INSTALL_DIR"
mv "$TMP" "${INSTALL_DIR}/wax"

ok "wax ${VERSION} installed to ${INSTALL_DIR}/wax"

# ---- PATH hint --------------------------------------------------------------

if ! command -v wax &>/dev/null 2>&1; then
  printf "\n${BOLD}Add wax to your PATH:${NC}\n"
  case "${SHELL:-}" in
    */fish) printf '  fish_add_path %s\n' "$INSTALL_DIR" ;;
    *)      printf '  echo '\''export PATH="%s:$PATH"'\'' >> ~/.bashrc  # or ~/.zshrc\n' "$INSTALL_DIR" ;;
  esac
fi
