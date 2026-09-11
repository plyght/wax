#!/usr/bin/env bash
# Search formulae and casks from the local Wax index.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"

if [[ -n "${WAX_BIN:-}" ]]; then
  wax="$WAX_BIN"
elif command -v wax >/dev/null 2>&1; then
  wax="$(command -v wax)"
elif [[ -x "$root/target/debug/wax" ]]; then
  wax="$root/target/debug/wax"
elif [[ -x "$root/target/release/wax" ]]; then
  wax="$root/target/release/wax"
else
  cat <<'EOF'
No wax binary found. Build one with `cargo build`, install it, or set WAX_BIN.

Expected commands:

  wax update        # fetch the formula/cask index once
  wax search nginx  # search formulae and casks

EOF
  exit 0
fi

echo "Using: $wax"
echo "--- wax search nginx ---"
"$wax" search nginx
