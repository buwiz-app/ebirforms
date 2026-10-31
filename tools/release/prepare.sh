#!/usr/bin/env bash
# tools/release/prepare.sh — everything the release needs except the
# signature. It builds the release binary
# and writes, to dist/release/ (or $1):
#   manifest.json     unsigned, publisher.key_id gc-2026-01
#   app-<id>.tsx      each app's source, exactly as the binary serves it
# then prints the three owner steps: the trust entry (Rox owner), the
# signature with gc-2026-01 from the login Keychain (the owner), and the
# install in the owner's Rox. This script never reads a key or the Keychain.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="${1:-$REPO/dist/release}"
ROX="${ROX_DIR:-/Volumes/goldcoders/Projects/rox}"
# the signer the owner runs: rox-broker (hexuria/rox main) when ROX_BROKER_BIN names
# one, else Rox's older server/src/v1/sign-manifest.mjs; the same arguments either way
if [ -n "${ROX_BROKER_BIN:-}" ]; then SIGNER="$ROX_BROKER_BIN sign-manifest"; else SIGNER="node $ROX/server/src/v1/sign-manifest.mjs"; fi
cd "$REPO"
cargo build --release -p ebirforms-mini-app >/dev/null
BIN="$REPO/target/release/ebirforms-mini-app"
mkdir -p "$OUT"
"$BIN" manifest >"$OUT/manifest.json"
APPS=()
for id in $("$BIN" manifest | python3 -c 'import json,sys; print(" ".join(a["id"] for a in json.load(sys.stdin)["apps"]))'); do
  "$BIN" app-source "$id" >"$OUT/app-$id.tsx"
  APPS+=("--app" "$id=$OUT/app-$id.tsx")
done
KEY_ID="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["publisher"]["key_id"])' "$OUT/manifest.json")"
VERSION="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$OUT/manifest.json")"
cat <<STEPS
Prepared app.buwiz.ebirforms $VERSION in $OUT (publisher.key_id $KEY_ID).

1. Rox owner: add to approved_plugins in $ROX/server/src/v1/trust.json
     { "id": "app.buwiz.ebirforms", "publisher_key_id": "$KEY_ID" }

2. You: sign with the publisher key from the login Keychain
     $SIGNER $OUT/manifest.json \\
       --keychain $KEY_ID ${APPS[*]} \\
       --out $OUT/manifest.signed.json

3. You: install in your Rox (Connect apps), as a local plugin
     command  $BIN
     args     mcp
     env      EBIRFORMS_MANIFEST=$OUT/manifest.signed.json
   eBIRForms keeps its own data in ~/Library/Application Support/app.buwiz.ebirforms
   (EBIRFORMS_DATA_DIR to move it). It reads no keychain and sends nothing.
STEPS
