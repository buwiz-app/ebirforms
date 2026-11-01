#!/usr/bin/env bash
# tools/bot/conformance.sh — run the Buwiz Bot's plugin conformance tool
# (buwiz-plugin-test) against the release build of ebirforms-mini-app.
#
# The same steps as Books' tools/bot/conformance.sh, then the runner:
#   1. build the release binary;
#   2. a fresh 0700 temp dir outside the repo;
#   3. a throwaway ed25519 test key (buwiz-plugin-test --keygen);
#   4. a per-run trust file mapping a TEST key id to that key, approving
#      app.buwiz.ebirforms under it (never gc-2026-01);
#   5. the manifest generated under the test key id and signed with the test
#      key file (--key-file, never --keychain) by `rox-broker sign-manifest`
#      when BUWIZ_BROKER_BIN names a broker build, else by the Bot's
#      server/src/v1/sign-manifest.mjs (same arguments, byte-identical output);
#   then buwiz-plugin-test --manifest <signed> --key-file <test key>, whose
#   own in-process broker uses a temp trust store of the same shape.
#
# The release manifest keeps publisher.key_id gc-2026-01; only the owner
# signs that one (the Bot's SDK README step 10). Nothing here touches the
# Keychain, the Bot's or the broker's data folder, or the Bot's trust.json.
#
# Usage: tools/bot/conformance.sh [extra buwiz-plugin-test args, e.g. --json]
# Env:   BUWIZ_BOT_DIR (default /Volumes/goldcoders/Projects/rox): a Buwiz Bot
#          checkout at protocol 2.0 (main ca7a1dd1 or later)
#        BUWIZ_BROKER_BIN (a rox-broker binary from buwiz-bot main; signs in place
#          of server/src/v1/sign-manifest.mjs)
#        EBIRFORMS_TEST_KEY_ID (default ebirforms-test-01)
#        KEEP_TMP=1 keeps the temp dir (it holds a test key; delete it after)
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BOT_DIR="${BUWIZ_BOT_DIR:-/Volumes/goldcoders/Projects/rox}"
KEY_ID="${EBIRFORMS_TEST_KEY_ID:-ebirforms-test-01}"
PLUGIN_ID="app.buwiz.ebirforms"
RUNNER="$BOT_DIR/server/sdk/mini-app/bin/buwiz-plugin-test.mjs"
if [ -n "${BUWIZ_BROKER_BIN:-}" ]; then
  [ -x "$BUWIZ_BROKER_BIN" ] || { echo "conformance: BUWIZ_BROKER_BIN $BUWIZ_BROKER_BIN is not executable" >&2; exit 2; }
  SIGN=("$BUWIZ_BROKER_BIN" sign-manifest)
  SIGNER=""
else
  SIGNER="$BOT_DIR/server/src/v1/sign-manifest.mjs"
  SIGN=(node "$SIGNER")
fi

if [ "$KEY_ID" = "gc-2026-01" ]; then
  echo "conformance: the test key id must not be gc-2026-01" >&2
  exit 2
fi
for f in "$RUNNER" ${SIGNER:+"$SIGNER"}; do
  [ -f "$f" ] || { echo "conformance: $f not found (set BUWIZ_BOT_DIR to a Bot checkout at protocol 2.0, or BUWIZ_BROKER_BIN for the signer)" >&2; exit 2; }
done
command -v node >/dev/null || { echo "conformance: node is needed to run the Bot's tools" >&2; exit 2; }
[ -d "$BOT_DIR/server/node_modules/@modelcontextprotocol/sdk" ] && [ -d "$BOT_DIR/server/node_modules/esbuild" ] || {
  echo "conformance: the Bot's server/node_modules lacks @modelcontextprotocol/sdk or esbuild; install them in the Bot checkout yourself" >&2
  exit 2
}

# 1. the release binary
cargo build --release --quiet -p ebirforms-mini-app --manifest-path "$REPO/Cargo.toml"
BIN="$REPO/target/release/ebirforms-mini-app"

# 2. a fresh temp dir outside the repo, removed on exit
TMP="$(mktemp -d "${TMPDIR:-/tmp}/ebirforms-conformance.XXXXXX")"
chmod 700 "$TMP"
case "$TMP" in "$REPO"/*) echo "conformance: temp dir is inside the repo" >&2; exit 2 ;; esac
cleanup() { if [ "${KEEP_TMP:-0}" = 1 ]; then echo "kept $TMP (holds a test key)" >&2; else rm -rf "$TMP"; fi; }
trap cleanup EXIT

# 3. a throwaway test key (0600); the runner prints only its public key
node "$RUNNER" --keygen "$TMP/test.key" >"$TMP/keygen.txt"
PUB="$(sed -n 's/.*Public key: \([A-Za-z0-9_-]*\).*/\1/p' "$TMP/keygen.txt")"
[ -n "$PUB" ] || { echo "conformance: keygen printed no public key" >&2; exit 1; }

# 4. the per-run trust file: the TEST key id only
cat >"$TMP/trust.json" <<JSON
{
  "v": 1,
  "about": "Per-run trust store for eBIRForms conformance (a throwaway test key; never gc-2026-01).",
  "publishers": { "$KEY_ID": { "publisher": "app.buwiz", "alg": "ed25519", "public_key": "$PUB" } },
  "approved_plugins": [ { "id": "$PLUGIN_ID", "publisher_key_id": "$KEY_ID" } ]
}
JSON
export BUWIZ_TRUST_FILE="$TMP/trust.json"
export BUWIZ_KEYSTORE="file:$TMP/keys"

# 5. generate under the test key id, sign with the test key file
"$BIN" manifest --key-id "$KEY_ID" >"$TMP/manifest.json"
# every app the manifest lists, as the binary serves it
APP_ARGS=()
for id in $(node -e 'for (const a of JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).apps) console.log(a.id)' "$TMP/manifest.json"); do
  "$BIN" app-source "$id" >"$TMP/app-$id.tsx"
  APP_ARGS+=(--app "$id=$TMP/app-$id.tsx")
done
"${SIGN[@]}" "$TMP/manifest.json" --key-file "$TMP/test.key" \
  "${APP_ARGS[@]}" --out "$TMP/signed.json"

# the runner: spawn the binary the way the broker does, serving the signed
# manifest through EBIRFORMS_MANIFEST; print its result verbatim
set +e
node --disable-warning=ExperimentalWarning "$RUNNER" \
  --command "$BIN" --args mcp \
  --manifest "$TMP/signed.json" --key-file "$TMP/test.key" \
  --manifest-env EBIRFORMS_MANIFEST \
  --env "EBIRFORMS_DATA_DIR=$TMP/ebirforms-data" \
  --env "EBIRFORMS_ACCOUNT_URL=http://127.0.0.1:1" "$@"
STATUS=$?
set -e
exit "$STATUS"
