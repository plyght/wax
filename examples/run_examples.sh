#!/usr/bin/env bash
# Run every example in this directory.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

for script in "$here"/*/run.sh; do
  echo "==> ${script#"$here"/}"
  bash "$script"
  echo
done
