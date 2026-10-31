#!/usr/bin/env bash
# tools/check-all.sh — every gate a commit must pass, in order: rustfmt,
# clippy with warnings denied, the Rust tests, and Rox conformance.
#
# It prints one line per gate and a summary, keeps each gate's output in a
# temp folder, and exits non-zero when any gate fails.
#
# Env: ROX_DIR   a Rox checkout or worktree for the conformance runner
#      SKIP_ROX=1  skip Rox conformance (no Rox checkout at hand); the
#                  summary says so, and the run cannot pass
set -uo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$(mktemp -d "${TMPDIR:-/tmp}/ebirforms-gates.XXXXXX")"
cd "$REPO"

failed=0
skipped=0
gate() {
  local name="$1" log="$OUT/$2.log"
  shift 2
  if "$@" >"$log" 2>&1; then
    printf 'PASS  %-28s %s\n' "$name" "$(summary "$log")"
  else
    printf 'FAIL  %-28s see %s\n' "$name" "$log"
    failed=$((failed + 1))
  fi
}
summary() {
  grep -E 'test result|pass|conforms' "$1" | tail -1 | sed 's/^ *//' | cut -c1-90
}
tests_total() {
  cargo test --quiet 2>&1 | tee "$OUT/cargo-test-full.log" | grep -E '^test result' | awk '{p+=$4; f+=$6} END {printf "test result: %d passed; %d failed\n", p, f; exit (f > 0)}'
}

gate "cargo fmt --check" fmt cargo fmt --check
gate "cargo clippy -D warnings" clippy cargo clippy --all-targets -- -D warnings
gate "cargo test" cargo-test tests_total
if [ "${SKIP_ROX:-0}" = "1" ]; then
  printf 'SKIP  %-28s SKIP_ROX=1\n' "Rox conformance"
  skipped=1
else
  gate "Rox conformance" conformance bash tools/rox/conformance.sh
fi

echo "logs: $OUT"
if [ "$failed" -gt 0 ]; then
  echo "$failed gate(s) failed"
  exit 1
fi
if [ "$skipped" -gt 0 ]; then
  echo "skipped gate(s): not a pass"
  exit 1
fi
echo "all gates passed"
