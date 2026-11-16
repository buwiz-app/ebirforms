#!/usr/bin/env bash
# tools/check-all.sh — every gate a commit must pass, in order: rustfmt,
# clippy with warnings denied, the Rust tests, the SDK links against the pin, and the SDK's conformance
# runner (from the SDK) and the Bot's real-broker host test, plus the TSX setter guard and the SDK-1
# import gate (the Bot refuses to open an app importing outside its table).
#
# It prints one line per gate and a summary, keeps each gate's output in a
# temp folder, and exits non-zero when any gate fails.
#
# Env: BUWIZ_SDK_DIR  a buwiz-sdk checkout at the pinned revision (tools/sdk-pin.sh)
#                     with `bun install` run (required): the conformance step
#      BUWIZ_BOT_DIR  a Buwiz Bot checkout holding BUWIZ_BOT_COMMIT, with
#                     server/node_modules installed: only the host test needs
#                     it (required unless SKIP_BOT=1)
#      SKIP_BOT=1     skip the host test (no Bot checkout at hand); the
#                     summary says so, and the run cannot pass
set -uo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$(mktemp -d "${TMPDIR:-/tmp}/ebirforms-gates.XXXXXX")"
cd "$REPO"

# every prerequisite up front, not after minutes of lint and tests
if [ -z "${BUWIZ_SDK_DIR:-}" ] || [ ! -d "$BUWIZ_SDK_DIR" ]; then
  echo "check-all: set BUWIZ_SDK_DIR to a buwiz-sdk checkout (the SDK conformance step needs it): git clone https://github.com/buwiz-app/buwiz-sdk; cd buwiz-sdk; bun install" >&2
  exit 2
fi
if [ "${SKIP_BOT:-0}" != "1" ] && { [ -z "${BUWIZ_BOT_DIR:-}" ] || [ ! -d "$BUWIZ_BOT_DIR" ]; }; then
  echo "check-all: set BUWIZ_BOT_DIR to a Buwiz Bot checkout (the Bot host test needs it), or SKIP_BOT=1" >&2
  exit 2
fi

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
no_detached_setters() {
  local status=0
  grep -RnE '=[[:blank:]]*\{[[:blank:]]*[A-Za-z_$][[:alnum:]_$.]*\.set[[:blank:]]*\}' apps/ || status=$?
  case "$status" in
    1) echo "no detached setters: pass" ;;
    0) echo 'use onChange={(v) => x.set(v)}, not a detached setter' >&2; return 1 ;;
    *) return "$status" ;;
  esac
}
setters_total() {
  node --test tests/detached-setters.mjs && no_detached_setters
}
sdk_links_follow_the_pin() {
  local pin stale
  pin="$(tools/sdk-pin.sh)" || return 1
  stale="$(grep -rhoE 'buwiz-sdk/blob/[^/)]+|checkout [0-9a-f]{40}' README.md AGENTS.md docs 2>/dev/null | grep -vE "blob/$pin\$|checkout $pin\$" || true)"
  [ -z "$stale" ] || { printf 'SDK links name a revision other than the pinned %s:\n%s\n' "$pin" "$stale"; return 1; }
  echo "0 findings: SDK links name $pin"
}
nsh_imports_total() {
  node --test tests/nsh-imports.mjs && node tools/check-nsh-imports.mjs
}

gate "cargo fmt --check" fmt cargo fmt --check
gate "cargo clippy -D warnings" clippy cargo clippy --all-targets -- -D warnings
gate "cargo test" cargo-test tests_total
gate "No detached setters" detached-setters setters_total
gate "SDK-1 nsh imports" nsh-imports nsh_imports_total
gate "SDK pins agree" sdk-pin tools/sdk-pin.sh
gate "SDK links follow the pin" sdk-links sdk_links_follow_the_pin
# needs BUWIZ_SDK_DIR (a buwiz-sdk checkout), not a Bot
gate "SDK conformance" conformance bash tools/conformance.sh
# needs BUWIZ_BOT_DIR (a Bot checkout at the commit the script pins)
if [ "${SKIP_BOT:-0}" = "1" ]; then
  printf 'SKIP  %-28s SKIP_BOT=1 (needs BUWIZ_BOT_DIR)\n' "Bot host test"
  skipped=1
else
  gate "Bot host test" host-test bash tools/bot/host-test.sh
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
