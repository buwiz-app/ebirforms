#!/usr/bin/env bash
# Ported from Books' host test (cd3d8cf), now at protocol 1.0. The Bot checkout is
# read-only: a pinned worktree, broker data, trust and file keys live per run.
# Usage: BUWIZ_BOT_DIR=/path/to/bot tools/bot/host-test.sh
# Env: EBIRFORMS_HOST_PORT (>=8810, default 8811), KEEP_TMP=1 (keeps test keys),
#      BUWIZ_BOT_COMMIT (default 224bffa5, buwiz-bot main with #102: protocol 1.0).
set -euo pipefail
export TMPDIR="${TMPDIR:-/tmp}"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
BOT_DIR="${BUWIZ_BOT_DIR:?set BUWIZ_BOT_DIR to the Bot test bench}"
BOT_DIR="$(cd "$BOT_DIR" && pwd -P)"
BOT_COMMIT="${BUWIZ_BOT_COMMIT:-224bffa539b910b76e4c02bcfd94a4a33102a672}"
PORT="${EBIRFORMS_HOST_PORT:-8811}"
KEY_ID="ebirforms-test-01"
PLUGIN_ID="app.buwiz.ebirforms"
die() { echo "host-test: $*" >&2; exit 2; }
command -v node >/dev/null || die "node is required"
command -v lsof >/dev/null || die "lsof is required"
[[ "$PORT" =~ ^[0-9]+$ ]] && [ "$PORT" -ge 8810 ] && [ "$PORT" -le 65535 ] || die "use a port from 8810 through 65535"
! lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1 || die "port $PORT is in use"
[ -d "$BOT_DIR/server/node_modules/@modelcontextprotocol/sdk" ] && [ -d "$BOT_DIR/server/node_modules/esbuild" ] || die "run npm ci in the Bot bench's server/ first"
git -C "$BOT_DIR" cat-file -e "$BOT_COMMIT^{commit}" || die "the Bot bench lacks the pinned commit"

cargo build --release --quiet -p ebirforms-mini-app --example host-seed --bin ebirforms-mini-app --manifest-path "$REPO/Cargo.toml"
BIN="$REPO/target/release/ebirforms-mini-app"
TMP="$(cd "$(mktemp -d "${TMPDIR%/}/ebirforms-host.XXXXXX")" && pwd -P)"
chmod 700 "$TMP"
case "$TMP" in "$REPO"/*) die "temp dir is inside the repo" ;; esac
WT="$TMP/bot"
cleanup() {
  git -C "$BOT_DIR" worktree remove --force "$WT" >/dev/null 2>&1 || true
  if [ "${KEEP_TMP:-0}" = 1 ]; then echo "kept $TMP (holds test keys; delete it after)" >&2; else rm -rf "$TMP"; fi
}
trap cleanup EXIT
git -C "$BOT_DIR" worktree add --detach --quiet "$WT" "$BOT_COMMIT"
ln -s "$BOT_DIR/server/node_modules" "$WT/server/node_modules"
mkdir -m 700 "$TMP/data" "$TMP/keys" "$TMP/ebirforms-data"
touch "$TMP/ebirforms-data/host-test-scratch"
export EBIRFORMS_DATA_DIR="$TMP/ebirforms-data"
export BUWIZ_NO_KEYCHAIN=1
export EBIRFORMS_ACCOUNT_URL="http://127.0.0.1:1"
unset BIR_SFTP_LIVE BIR_SFTP_PASSWORD BIR_SFTP_USERNAME BIR_SFTP_HOST
"$REPO/target/release/examples/host-seed"

node "$WT/server/sdk/mini-app/bin/buwiz-plugin-test.mjs" --keygen "$TMP/test.key" >"$TMP/keygen.txt"
PUB="$(sed -n 's/.*Public key: \([A-Za-z0-9_-]*\).*/\1/p' "$TMP/keygen.txt")"
[ -n "$PUB" ] || die "keygen printed no public key"
cat >"$TMP/trust.json" <<JSON
{
  "v": 1,
  "publishers": { "$KEY_ID": { "publisher": "app.buwiz", "alg": "ed25519", "public_key": "$PUB" } },
  "approved_plugins": [ { "id": "$PLUGIN_ID", "publisher_key_id": "$KEY_ID" } ]
}
JSON
"$BIN" manifest --key-id "$KEY_ID" >"$TMP/manifest.json"
APP_ARGS=()
for id in $(node -e 'for (const a of JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).apps) console.log(a.id)' "$TMP/manifest.json"); do
  "$BIN" app-source "$id" >"$TMP/app-$id.tsx"
  APP_ARGS+=(--app "$id=$TMP/app-$id.tsx")
done
BUWIZ_TRUST_FILE="$TMP/trust.json" node "$WT/server/src/v1/sign-manifest.mjs" "$TMP/manifest.json" \
  --key-file "$TMP/test.key" "${APP_ARGS[@]}" --out "$TMP/signed.json"

# The JS runner owns the real app-server child, including shutdown. Its IPC
# channel exercises the broker's assistant surface without adding HTTP routes.
PORT="$PORT" GROK_DATA_DIR="$TMP/data" BUWIZ_TRUST_FILE="$TMP/trust.json" \
  BUWIZ_KEYSTORE="file:$TMP/keys" MOCK=1 \
  node --disable-warning=ExperimentalWarning "$REPO/tests/host/ebirforms-host-test.mjs" "$WT" "$TMP" "$BIN" "$PORT"
[ -s "$TMP/keys/$PLUGIN_ID.key" ] || die "the file keystore holds no plugin key"
echo "PASS  plugin key stored in the scratch file keystore (no Keychain)"
