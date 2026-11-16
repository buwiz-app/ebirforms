#!/usr/bin/env bash
# tools/bot/host-test.sh: the plugin against a real Buwiz broker. It starts the
# broker binary on a temporary home and data folder, installs the signed
# eBIRForms manifest under a throwaway test key, and checks the host routes
# over loopback HTTP (tests/host/ebirforms-host-test.mjs). Nothing here touches
# the Keychain, a real data folder or the release key.
#
# Usage: BUWIZ_BROKER_BIN=/path/to/buwiz-broker tools/bot/host-test.sh
# Env:   BUWIZ_BROKER_BIN (required): a buwiz-broker binary, executable
#        EBIRFORMS_HOST_PORT (>=8811, default 8811)
#        KEEP_TMP=1 keeps the temp dir (it holds a test key; delete it after)
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
PORT="${EBIRFORMS_HOST_PORT:-8811}"
MODEL_PORT=$((PORT + 1))
KEY_ID="ebirforms-test-01"
PLUGIN_ID="app.buwiz.ebirforms"
# the broker's own PATH: system tools only, so it can reach no command of the owner's
BROKER_PATH="/usr/bin:/bin:/usr/sbin:/sbin"
die() { echo "host-test: $*" >&2; exit 2; }
[ -n "${BUWIZ_BROKER_BIN:-}" ] || die "set BUWIZ_BROKER_BIN to a buwiz-broker binary"
[ -f "$BUWIZ_BROKER_BIN" ] && [ -x "$BUWIZ_BROKER_BIN" ] || die "BUWIZ_BROKER_BIN $BUWIZ_BROKER_BIN is not an executable file"
command -v node >/dev/null || die "node is required"
command -v lsof >/dev/null || die "lsof is required"
[[ "$PORT" =~ ^[0-9]+$ ]] && [ "$PORT" -ge 8811 ] && [ "$PORT" -le 65535 ] || die "use a port from 8811 through 65535"
for p in "$PORT" "$MODEL_PORT"; do
  ! lsof -nP -iTCP:"$p" -sTCP:LISTEN >/dev/null 2>&1 || die "port $p is in use (the broker takes $PORT, the scripted model $MODEL_PORT)"
done
! PATH="$BROKER_PATH" command -v secret >/dev/null 2>&1 || die "the broker's PATH ($BROKER_PATH) holds a command named secret"

cargo build --release --quiet -p ebirforms-mini-app --example host-seed --bin ebirforms-mini-app --manifest-path "$REPO/Cargo.toml"
BIN="$REPO/target/release/ebirforms-mini-app"
TMP="$(cd "$(mktemp -d "${TMPDIR:-/tmp}/ebirforms-host.XXXXXX")" && pwd -P)"
chmod 700 "$TMP"
case "$TMP" in "$REPO"/*) die "temp dir is inside the repo" ;; esac
BROKER_PID=""
cleanup() {
  if [ -n "$BROKER_PID" ] && kill -0 "$BROKER_PID" 2>/dev/null; then
    kill "$BROKER_PID" 2>/dev/null || true
    for _ in $(seq 1 50); do kill -0 "$BROKER_PID" 2>/dev/null || break; sleep 0.1; done
    kill -9 "$BROKER_PID" 2>/dev/null || true
  fi
  if [ "${KEEP_TMP:-0}" = 1 ]; then echo "kept $TMP (holds test keys; delete it after)" >&2; else rm -rf "$TMP"; fi
}
trap cleanup EXIT
mkdir -m 700 "$TMP/home" "$TMP/data" "$TMP/keys" "$TMP/assets" "$TMP/ebirforms-data"
touch "$TMP/ebirforms-data/host-test-scratch"

# the plugin's own scratch database: one synthetic taxpayer
export EBIRFORMS_DATA_DIR="$TMP/ebirforms-data"
export BUWIZ_NO_KEYCHAIN=1
export EBIRFORMS_ACCOUNT_URL="http://127.0.0.1:1"
unset BIR_SFTP_LIVE BIR_SFTP_PASSWORD BIR_SFTP_USERNAME BIR_SFTP_HOST
"$REPO/target/release/examples/host-seed"

# a throwaway ed25519 test key (0600): the seed on disk, its public key printed
PUB="$(node -e '
  const c = require("node:crypto"), fs = require("node:fs");
  const { privateKey, publicKey } = c.generateKeyPairSync("ed25519");
  const seed = privateKey.export({ format: "der", type: "pkcs8" }).subarray(-32);
  fs.writeFileSync(process.argv[1], seed.toString("base64url"), { mode: 0o600 });
  process.stdout.write(publicKey.export({ format: "der", type: "spki" }).subarray(-32).toString("base64url"));
' "$TMP/test.key")"
[ -n "$PUB" ] || die "key generation printed no public key"
cat >"$TMP/trust.json" <<JSON
{
  "v": 1,
  "publishers": { "$KEY_ID": { "publisher": "app.buwiz", "alg": "ed25519", "public_key": "$PUB" } },
  "approved_plugins": [ { "id": "$PLUGIN_ID", "publisher_key_id": "$KEY_ID" } ]
}
JSON

# the manifest under the test key id, signed with the test key by the broker
"$BIN" manifest --key-id "$KEY_ID" >"$TMP/manifest.json"
APP_ARGS=()
for id in $(node -e 'for (const a of JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).apps) console.log(a.id)' "$TMP/manifest.json"); do
  "$BIN" app-source "$id" >"$TMP/app-$id.tsx"
  APP_ARGS+=(--app "$id=$TMP/app-$id.tsx")
done
"$BUWIZ_BROKER_BIN" sign-manifest "$TMP/manifest.json" --key-file "$TMP/test.key" "${APP_ARGS[@]}" --out "$TMP/signed.json"

# the broker: its own home, data folder, keystore and trust file; no Keychain,
# no update checks; its model is the test's scripted one on loopback
env -i PATH="$BROKER_PATH" HOME="$TMP/home" TMPDIR="$TMP" \
  BUWIZ_KEYSTORE="file:$TMP/keys" BUWIZ_NO_KEYCHAIN=1 BUWIZ_TRUST_FILE="$TMP/trust.json" \
  BUWIZ_BROKER_HUB=1 BUWIZ_BROKER_V1_HOST=1 BUWIZ_UPDATE_CHECK=0 \
  ANTHROPIC_BASE_URL="http://127.0.0.1:$MODEL_PORT" OPENCODEX_URL="http://127.0.0.1:$MODEL_PORT" ANTHROPIC_API_KEY=test-key \
  "$BUWIZ_BROKER_BIN" --port "$PORT" --no-node --assets "$TMP/assets" --data "$TMP/data" \
  >"$TMP/broker.log" 2>&1 &
BROKER_PID=$!

if ! node --disable-warning=ExperimentalWarning "$REPO/tests/host/ebirforms-host-test.mjs" "$TMP" "$BIN" "$PORT" "$MODEL_PORT"; then
  echo "host-test: the broker's log ends:" >&2
  tail -n 5 "$TMP/broker.log" | cut -c1-200 >&2 || true
  exit 1
fi
[ -s "$TMP/keys/$PLUGIN_ID.key" ] || die "the file keystore holds no plugin key"
echo "PASS  plugin key stored in the scratch file keystore (no Keychain)"
