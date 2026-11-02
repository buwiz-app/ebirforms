#!/usr/bin/env bash
# tools/dev-install.sh — install this checkout's eBIRForms build straight into
# the running Buwiz Bot, with no GitHub release: build, sign, inspect, install.
#
#   BUWIZ_BROKER_BIN=<buwiz-broker> tools/dev-install.sh [--debug] [--broker <url>]
#       [--key-file <seed> [--key-id <id>]] [--data-dir <dir>] [--dry-run]
#
# 1. Builds ebirforms-mini-app (release; --debug for a faster build), into
#    CARGO_TARGET_DIR when it is set. A dirty tree is fine: this is for
#    iterating before a commit.
# 2. Copies the binary into a folder of its own for this build, outside the
#    repo: "$HOME/Library/Application Support/Buwiz Broker/dev-plugins/
#    app.buwiz.ebirforms/<git describe --always --dirty>-<timestamp>/", so the
#    command the Bot runs never changes under it when the next build comes.
#    After an install the last 3 build folders are kept and older ones deleted.
# 3. Writes the manifest with a dev version, <next patch>-dev.<timestamp>
#    (0.3.1 builds as 0.3.2-dev.<ts>): the Bot refuses a changed manifest
#    under an unchanged version, and treats an unchanged one as already
#    installed. The next patch keeps a dev build above the published
#    release, so the Bot never offers that release as an update over it; the
#    next real release (0.3.2) installs over it as an update. Downgrades are
#    still allowed, for going back to an older dev build. The mini app logs
#    at start that the signed manifest is not this build's (its version
#    differs): expected here.
# 4. Signs it with the publisher key gc-2026-01 from the login Keychain through
#    `$BUWIZ_BROKER_BIN sign-manifest`, as tools/release/publish.sh does: macOS
#    asks to allow it. --key-file signs with a seed file instead, for a test
#    broker that trusts that key (its key id: --key-id, default
#    ebirforms-test-01). This script never reads or prints a key itself.
# 5. Asks the broker to inspect the build (POST /v1/host/plugins/inspect),
#    prints what changes (the version, the permissions it adds), then installs
#    it (POST /v1/host/plugins/install) with that manifest's hash,
#    approve_permissions and allow_downgrade. Running this script is the
#    owner's consent to the permissions it prints: there is no other prompt.
#    --dry-run stops after the inspect.
#
# The plugin's data stays where the published install keeps it (no
# EBIRFORMS_DATA_DIR: ~/Library/Application Support/app.buwiz.ebirforms), so
# the dev build sees your real eBIRForms data. --data-dir <dir> points it at a
# scratch folder instead.
#
# Env: BUWIZ_BROKER_BIN       buwiz-broker (from buwiz-bot), for sign-manifest
#      BUWIZ_HOST_TOKEN_FILE  the broker's host token (default
#                             "$HOME/Library/Application Support/Buwiz Broker/host-token")
#      EBIRFORMS_DEV_PLUGINS  where the build folders go (default
#                             "$HOME/Library/Application Support/Buwiz Broker/dev-plugins")
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
NAME=ebirforms-mini-app
PLUGIN_ID=app.buwiz.ebirforms
MANIFEST_FILE="$PLUGIN_ID.manifest.json"
BROKER_DIR="$HOME/Library/Application Support/Buwiz Broker"
KEEP=3

PROFILE=release
BROKER=http://127.0.0.1:8787
KEY_FILE=
KEY_ID=
DATA_DIR=
DRY_RUN=0
usage() {
  echo "usage: BUWIZ_BROKER_BIN=<buwiz-broker> tools/dev-install.sh [--debug] [--broker <url>] [--key-file <seed> [--key-id <id>]] [--data-dir <dir>] [--dry-run]" >&2
  exit 2
}
while [ $# -gt 0 ]; do
  case "$1" in
    --debug) PROFILE=debug ;;
    --broker) BROKER="${2:?--broker needs a url}"; shift ;;
    --key-file) KEY_FILE="${2:?--key-file needs a path}"; shift ;;
    --key-id) KEY_ID="${2:?--key-id needs an id}"; shift ;;
    --data-dir) DATA_DIR="${2:?--data-dir needs a folder}"; shift ;;
    --dry-run) DRY_RUN=1 ;;
    -h|--help) usage ;;
    *) echo "dev-install: unknown argument $1" >&2; usage ;;
  esac
  shift
done
die() { echo "dev-install: $*" >&2; exit 1; }
: "${BUWIZ_BROKER_BIN:?set BUWIZ_BROKER_BIN to a buwiz-broker build (buwiz-bot/buwiz-bot broker/)}"
[ -x "$BUWIZ_BROKER_BIN" ] || die "BUWIZ_BROKER_BIN $BUWIZ_BROKER_BIN is not executable"
[ -z "$KEY_ID" ] || [ -n "$KEY_FILE" ] || die "--key-id goes with --key-file (the Keychain key is gc-2026-01)"
command -v curl >/dev/null || die "curl is required"
command -v python3 >/dev/null || die "python3 is required"
TOKEN_FILE="${BUWIZ_HOST_TOKEN_FILE:-$BROKER_DIR/host-token}"
[ -r "$TOKEN_FILE" ] || die "no host token at $TOKEN_FILE: is the Buwiz Bot running? (BUWIZ_HOST_TOKEN_FILE names another)"
BUILDS="${EBIRFORMS_DEV_PLUGINS:-$BROKER_DIR/dev-plugins}/$PLUGIN_ID"
case "$BUILDS/" in "$REPO"/*) die "the build folders must be outside the repo" ;; esac
if [ -n "$DATA_DIR" ]; then
  mkdir -p "$DATA_DIR"
  DATA_DIR="$(cd "$DATA_DIR" && pwd -P)"
fi
if [ -n "$KEY_FILE" ]; then
  SIGN=(--key-file "$KEY_FILE")
  HOW="a key file"
  KEY_ID="${KEY_ID:-ebirforms-test-01}"
else
  SIGN=(--keychain gc-2026-01)
  HOW="approve the Keychain prompt"
  KEY_ID=gc-2026-01
fi

# 1. build
cd "$REPO"
TARGET="${CARGO_TARGET_DIR:-$REPO/target}"
BUILD_ARGS=(-p "$NAME" --bin "$NAME")
[ "$PROFILE" = release ] && BUILD_ARGS+=(--release)
echo "building $NAME ($PROFILE)" >&2
cargo build "${BUILD_ARGS[@]}" >&2
BIN_BUILT="$TARGET/$PROFILE/$NAME"
[ -x "$BIN_BUILT" ] || die "no binary at $BIN_BUILT"

# 2. a folder of its own
DESCRIBE="$(git describe --always --dirty)"
STAMP="$(date +%Y%m%d%H%M%S)"
DIR="$BUILDS/$DESCRIBE-$STAMP"
[ ! -e "$DIR" ] || die "$DIR exists already"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/ebirforms-dev.XXXXXX")"
# The build folder goes again unless the install was sent: a dry run, a
# failed step or a refused install leaves nothing behind. Once the install is
# sent it stays, answered or not: the broker may be running it.
KEEP_DIR=0
trap 'rm -rf "$WORK"; [ "$KEEP_DIR" = 1 ] || rm -rf "$DIR"' EXIT
mkdir -p "$DIR"
cp "$BIN_BUILT" "$DIR/$NAME"
BIN="$DIR/$NAME"

# 3. the manifest, with a dev version
"$BIN" manifest --key-id "$KEY_ID" >"$WORK/manifest.json"
VERSION="$(python3 - "$WORK/manifest.json" "$STAMP" <<'PY'
import json, sys
path, stamp = sys.argv[1:]
m = json.load(open(path))
base = m["version"].split("-")[0].split("+")[0]
major, minor, patch = (int(x) for x in base.split("."))
# a pre-release of the next patch: above the published <version>, so the Bot
# never offers that release as an update over a dev build
m["version"] = f"{major}.{minor}.{patch + 1}-dev.{stamp}"
json.dump(m, open(path, "w"), indent=2)
print(m["version"])
PY
)"
APPS=()
for id in $(python3 -c 'import json,sys; print(" ".join(a["id"] for a in json.load(open(sys.argv[1]))["apps"] if "native" not in a))' "$WORK/manifest.json"); do
  "$BIN" app-source "$id" >"$WORK/app-$id.tsx"
  APPS+=(--app "$id=$WORK/app-$id.tsx")
done

# 4. signed
echo "signing the manifest with $KEY_ID ($HOW)" >&2
"$BUWIZ_BROKER_BIN" sign-manifest "$WORK/manifest.json" "${SIGN[@]}" "${APPS[@]}" --out "$DIR/$MANIFEST_FILE"

# 5. inspect, then install
SPEC="$(python3 - "$BIN" "$DIR/$MANIFEST_FILE" "$DATA_DIR" <<'PY'
import json, sys
command, manifest, data = sys.argv[1:]
env = {"EBIRFORMS_MANIFEST": manifest}
if data:
    env["EBIRFORMS_DATA_DIR"] = data
print(json.dumps({"command": command, "args": ["mcp"], "env": env}))
PY
)"
# The token goes to curl through a header file (-H @file), never on a command line.
printf 'x-buwiz-host: %s\n' "$(tr -d '\r\n' <"$TOKEN_FILE")" >"$WORK/headers"
chmod 600 "$WORK/headers"
post() {
  curl -sS --max-time 120 -X POST "$BROKER$1" -H @"$WORK/headers" -H 'content-type: application/json' \
    --data-binary @- -o "$WORK/answer.json" -w '%{http_code}' || die "the broker at $BROKER did not answer"
}
STATUS="$(printf '%s' "$SPEC" | post /v1/host/plugins/inspect)"
python3 - "$WORK/answer.json" "$STATUS" "$PLUGIN_ID" >"$WORK/sha256" <<'PY'
import json, sys
path, status, plugin = sys.argv[1:]
try:
    a = json.load(open(path))
except ValueError:
    sys.exit(f"dev-install: inspect answered HTTP {status} with no JSON")
if not a.get("ok"):
    sys.exit(f"dev-install: inspect refused it (HTTP {status}): {a.get('code', '')} {a.get('message') or a.get('error') or ''}".rstrip())
m = a["manifest"]
if m.get("id") != plugin:
    sys.exit(f"dev-install: the build serves {m.get('id')}, not {plugin}")
u = a.get("update")
out = sys.stderr
if u:
    print(f"update {m['id']}: {u['from_version']} -> {u['to_version']}" + (" (a downgrade: allowed)" if u.get("downgrade") else ""), file=out)
    added = u.get("added_permissions") or []
    print("permissions it adds:" if added else "it adds no permissions", file=out)
    for p in added:
        print(f"  - {p}", file=out)
else:
    print(f"new install of {m['id']} {m['version']}", file=out)
for w in a.get("warnings") or []:
    print(f"warning: {w}", file=out)
print(a["manifest_sha256"])
PY
SHA="$(cat "$WORK/sha256")"
if [ "$DRY_RUN" = 1 ]; then
  echo "dry run: inspected $PLUGIN_ID $VERSION; nothing installed, and its build folder is removed" >&2
  exit 0
fi
BODY="$(python3 -c 'import json,sys; s=json.loads(sys.argv[1]); s.update(manifest_sha256=sys.argv[2], approve_permissions=True, allow_downgrade=True); print(json.dumps(s))' "$SPEC" "$SHA")"
KEEP_DIR=1
STATUS="$(printf '%s' "$BODY" | post /v1/host/plugins/install)"
# exit 3: refused, so nothing runs from the folder; anything else unexpected keeps it
python3 - "$WORK/answer.json" "$STATUS" "$VERSION" "$DIR" <<'PY' || { [ $? = 3 ] && KEEP_DIR=0; exit 1; }
import json, sys
path, status, version, folder = sys.argv[1:]
try:
    a = json.load(open(path))
except ValueError:
    sys.exit(f"dev-install: install answered HTTP {status} with no JSON")
if not a.get("ok"):
    reason = (a.get("details") or {}).get("reason")
    print(f"dev-install: install refused it (HTTP {status}): {a.get('code', '')} {a.get('message') or a.get('error') or ''}{f' ({reason})' if reason else ''}".rstrip(), file=sys.stderr)
    sys.exit(3)
state = "unchanged" if a.get("unchanged") else a.get("state", "")
print(f"installed {a.get('id')} {version}: {state}" + (" (updated)" if a.get("updated") else ""))
print(f"build folder: {folder}")
PY

# keep the last $KEEP builds (by their timestamp), the one just installed among them
find "$BUILDS" -mindepth 1 -maxdepth 1 -type d -exec basename {} \; | awk -F- '{print $NF "\t" $0}' | sort -r | cut -f2- | tail -n +$((KEEP + 1)) | while IFS= read -r old; do
  [ -n "$old" ] && [ "$BUILDS/$old" != "$DIR" ] && rm -rf "${BUILDS:?}/$old" && echo "removed the old build $old" >&2
done
exit 0
