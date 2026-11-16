#!/usr/bin/env bash
# tools/conformance.sh: the SDK's plugin conformance runner (buwiz-plugin-test)
# against the release build of ebirforms-mini-app, under a throwaway key and a
# per-run trust file. Nothing here touches the Keychain, a broker's data or the
# release key.
#
# Steps 1-5, then the runner:
#   1. build the release binary;
#   2. a fresh 0700 temp dir outside the repo;
#   3. a throwaway ed25519 test key (buwiz-plugin-test --keygen);
#   4. a per-run trust file mapping a TEST key id to that key and approving
#      app.buwiz.ebirforms under it (never gc-2026-01);
#   5. the manifest generated under the test key id and signed with the test
#      key file by `buwiz-broker sign-manifest` when BUWIZ_BROKER_BIN names a
#      broker build, else by the SDK's sign-manifest.mjs (same arguments);
#   then buwiz-plugin-test --manifest <signed> --key-file <test key>, whose
#   own in-process broker uses a temp trust store of the same shape.
#
# The release manifest keeps publisher.key_id gc-2026-01; only the owner
# signs that one, with tools/release/publish.sh and the broker.
#
# Usage: tools/conformance.sh [runner args, e.g. --json]
# Env:   BUWIZ_SDK_DIR (required): a buwiz-sdk checkout
#          (https://github.com/buwiz-app/buwiz-sdk) at the revision this repo
#          pins (tools/sdk-pin.sh), with its dependencies installed (bun install)
#        BUWIZ_BROKER_BIN (a buwiz-broker binary; signs in place of the SDK's
#          sign-manifest.mjs)
#        EBIRFORMS_TEST_KEY_ID (default ebirforms-test-01)
#        KEEP_TMP=1 keeps the temp dir (it holds a test key; delete it after)
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
SDK="${BUWIZ_SDK_DIR:-}"
KEY_ID="${EBIRFORMS_TEST_KEY_ID:-ebirforms-test-01}"
PLUGIN_ID="app.buwiz.ebirforms"
RUNNER="$SDK/packages/conformance/bin/buwiz-plugin-test.mjs"
SIGNER="$SDK/packages/conformance/src/v1/sign-manifest.mjs"

die() { echo "conformance: $*" >&2; exit 2; }
[ -n "$SDK" ] || die "set BUWIZ_SDK_DIR to a buwiz-sdk checkout (git clone https://github.com/buwiz-app/buwiz-sdk; cd buwiz-sdk; bun install)"
if [ -n "${BUWIZ_BROKER_BIN:-}" ]; then
  [ -x "$BUWIZ_BROKER_BIN" ] || die "BUWIZ_BROKER_BIN $BUWIZ_BROKER_BIN is not executable"
  SIGN=("$BUWIZ_BROKER_BIN" sign-manifest)
else
  SIGN=(node "$SIGNER")
fi
[ "$KEY_ID" != "gc-2026-01" ] || die "the test key id must not be gc-2026-01"
[ -f "$RUNNER" ] || die "$RUNNER not found: is BUWIZ_SDK_DIR a buwiz-sdk checkout?"
# the SDK's signer is needed only when it is the one that signs
[ -n "${BUWIZ_BROKER_BIN:-}" ] || [ -f "$SIGNER" ] || die "$SIGNER not found: is BUWIZ_SDK_DIR a buwiz-sdk checkout?"
PIN="$("$REPO/tools/sdk-pin.sh")" || die "no single pinned buwiz-sdk revision (tools/sdk-pin.sh)"
# the checkout must hold exactly the pinned files (a merge of the pin has another commit id)
SDK_TREE="$(git -C "$SDK" rev-parse 'HEAD^{tree}' 2>/dev/null)" || die "$SDK is not a git checkout of buwiz-sdk"
[ "$(git -C "$SDK" rev-parse "$PIN^{tree}" 2>/dev/null)" = "$SDK_TREE" ] ||
  die "the SDK checkout is not the pinned revision $PIN: git -C \"\$BUWIZ_SDK_DIR\" fetch && git -C \"\$BUWIZ_SDK_DIR\" checkout $PIN"
[ -z "$(git -C "$SDK" status --porcelain --untracked-files=all -- packages/conformance)" ] ||
  die "the SDK checkout has changed or untracked files under packages/conformance"
command -v node >/dev/null || die "node is needed to run the SDK's tools"
MIN_NODE="$(node -e 'console.log(JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).engines.node)' "$SDK/packages/conformance/package.json" 2>/dev/null || echo "see packages/conformance/package.json")"
(cd "$SDK/packages/conformance" && node --input-type=module -e 'await import("node:sqlite"); await import("esbuild"); await import("@modelcontextprotocol/sdk/client/index.js"); await import("@modelcontextprotocol/sdk/client/stdio.js")') >/dev/null 2>&1 ||
  die "the SDK's runner cannot load node:sqlite, esbuild or @modelcontextprotocol/sdk: it needs Node $MIN_NODE (node --version), and bun install run in $SDK"

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
