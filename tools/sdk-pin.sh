#!/usr/bin/env bash
# tools/sdk-pin.sh: print the buwiz-sdk revision this repo pins. The one place
# that reads it. Every buwiz-sdk git revision in every Cargo.toml of the
# workspace and in Cargo.lock must be the same one; otherwise it lists each
# and where it is, and fails.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"
FOUND="$(
  { git ls-files '*Cargo.toml' ':!legacy' | xargs grep -HnE 'buwiz-sdk.*rev = "[0-9a-f]{40}"' 2>/dev/null || true; } |
    sed -E 's/^([^:]*:[0-9]+):.*buwiz-sdk.*rev = "([0-9a-f]{40})".*/\2 \1/'
  { grep -nE '^source = "git\+https://github\.com/buwiz-app/buwiz-sdk[?#]' Cargo.lock 2>/dev/null || true; } |
    sed -E 's/^([0-9]+):.*rev=([0-9a-f]{40}).*/\2 Cargo.lock:\1/'
)"
[ -n "$FOUND" ] || { echo "sdk-pin: no pinned buwiz-sdk revision found in any Cargo.toml" >&2; exit 1; }
if [ "$(printf '%s\n' "$FOUND" | cut -d' ' -f1 | sort -u | wc -l | tr -d ' ')" != 1 ]; then
  echo "sdk-pin: the buwiz-sdk pins disagree:" >&2
  printf '%s\n' "$FOUND" | sed 's/^/  /' >&2
  exit 1
fi
printf '%s\n' "$FOUND" | head -1 | cut -d' ' -f1
