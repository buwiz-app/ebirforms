# eBIRForms mini app

The eBIRForms mini app for the Buwiz Bot: plugin id `app.buwiz.ebirforms`, publisher `app.buwiz` (key `gc-2026-01`), protocol 1.0 (`buwiz-sdk` `docs/protocol-1.0.md`; 2.0 and 2.1 were its pre-release numbering, which a 1.0 Bot refuses).
An MCP server on stdio that the Bot spawns.

## Layout

The same layout as every mini app in the `buwiz-app` org (Books is the reference):

| Path | What |
|---|---|
| `crates/ebirforms-mini-app/` | The mini app: MCP server, manifest and signing, tickets, TIN scrubbing. It began as `buwiz-mcp` (eBIRForms `feat/mcp-mode` @ `f0d5afa`) and keeps that history. |
| `apps/ebirforms/` | The app screens: `forms`, `tax-profile`, `calendar`, `deadlines` (an inline card), `register` (Buwiz Sign-in) and the answer cards (`card-dates`, `card-calc`, `card-people`: Tax profiles). |
| `tests/` | Integration tests (the Bot contract, own data). |
| `tools/` | `check-all.sh` (every gate), `bot/conformance.sh`, `bot/host-test.sh`, `release/publish.sh` (a protocol 1.0 package release: binary, signed manifest and signed `plugin.json` from `package/plugin.json`), `dev-install.sh` (a local build into the running Bot). |
| `legacy/` | The first mini app, from `hexuria/buwiz-forms` branch `rox-mini-app-round2` @ `834e7203`, with its history. It is the reference for porting the remaining 12 apps and cards; it does not build here and is deleted once ported. |

It builds on eBIRForms' domain crates (`bir-app-core`, pinned by commit in the root `Cargo.toml`). Only the
`BirApp` facade is a supported surface.

## Its own data

The mini app keeps its own taxpayer database. It shares nothing with either eBIRForms desktop app and never reads
a keychain:

- the folder: `EBIRFORMS_DATA_DIR`, else the platform data folder `app.buwiz.ebirforms`
  (`~/Library/Application Support/app.buwiz.ebirforms` on macOS);
- `ebirforms.db`, its key `ebirforms.key` (64 hex digits, 0600, made on first run), and `logs/`;
- once you sign in to Buwiz (the Sign-in app), its tokens and this computer's device key, as 0600 files.

Old desktop data comes in by export, later.

## Sending

You send a return from the Forms app, signed in to Buwiz: it is queued, sealed to Buwiz's Sender and sent through
the relay, and comes back with a signed receipt. The assistant can read the queue and each return's state, but it
cannot queue or send. Nothing is paid from here.

## Run

```
cargo build --release
target/release/ebirforms-mini-app manifest         # the unsigned manifest, for signing
tools/check-all.sh                                 # every gate
target/release/ebirforms-mini-app mcp              # what the Bot runs
```

The Bot serves only a signed manifest: `EBIRFORMS_MANIFEST`, else `app.buwiz.ebirforms.manifest.json` next to the
executable.

## Install a local build

To try a change in your running Buwiz Bot without a release (a dirty tree is fine):

```sh
BUWIZ_BROKER_BIN=/path/to/buwiz-broker tools/dev-install.sh --live            # release build
BUWIZ_BROKER_BIN=/path/to/buwiz-broker tools/dev-install.sh --live --debug    # faster build
```

It builds `ebirforms-mini-app` (into `CARGO_TARGET_DIR` when set), copies it to a folder of its own under
`~/Library/Application Support/Buwiz Broker/dev-plugins/app.buwiz.ebirforms/<git describe --dirty>-<timestamp>/`
(the last 3 are kept), signs its manifest with `gc-2026-01` (approve the Keychain prompt) under a dev version
`<version>-dev.<timestamp>`, then asks the broker on `http://127.0.0.1:8787` to inspect and install it. It prints the
version change and the permissions the build adds, and approves them: running it is your consent. A dev version is
below the release of the same number, so the first dev install over a published one is an allowed downgrade, and the
release installs over it again as an update.

`--live` confirms you mean your own Bot: the script refuses without it if a run would use the broker on 8787, the
`gc-2026-01` key or your real data. For a test Bot, give all of `--broker`, `--key-file` and `--data-dir` instead.

The build uses your real eBIRForms data (`~/Library/Application Support/app.buwiz.ebirforms`), as the published
install does. Options: `--data-dir <dir>` for a scratch data folder, `--dry-run` to stop after the inspect,
`--broker <url>`, and `--key-file <seed>` (with `--key-id`, default `ebirforms-test-01`) to sign for a test broker
that trusts that key. `BUWIZ_HOST_TOKEN_FILE` and `EBIRFORMS_DEV_PLUGINS` move the host token and the build folders.

## Import the retired BIR app's data (once)

```sh
# 1. pause eBIRForms in the Bot (its process stops and lets go of its database)
curl -sS -X POST http://127.0.0.1:8787/v1/host/plugins/app.buwiz.ebirforms/disable \
  -H "X-Buwiz-Host: $(cat "$HOME/Library/Application Support/Buwiz Broker/host-token")" \
  -H 'content-type: application/json' -d '{}'
# 2. counts only, nothing written
ebirforms-mini-app import-legacy --dry-run \
  --from "$HOME/Library/Group Containers/group.dev.goldcoders.bir/bir_data.db" \
  --key-file "$HOME/Library/Application Support/Buwiz Broker/v1/bir-db.key"
# 3. the import (same arguments, no --dry-run), then resume eBIRForms
curl -sS -X POST http://127.0.0.1:8787/v1/host/plugins/app.buwiz.ebirforms/enable \
  -H "X-Buwiz-Host: $(cat "$HOME/Library/Application Support/Buwiz Broker/host-token")" \
  -H 'content-type: application/json' -d '{}'
```

It copies the old database (and its `-wal`, `-shm`) to a temporary folder and opens only the copy, in a child
process given the old key through eBIRForms' `BUWIZ_DB_KEY_FILE`; eBIRForms migrates the copy and exports every
profile (`BirApp::export_backup`). The target (`--data-dir`, else this app's data folder) is copied to
`<data-dir>.backup-<UTC stamp>` first, then each taxpayer not there yet is restored through eBIRForms'
`restore_backup` with its yearly forms; a branch already there is skipped. It refuses while eBIRForms holds the
target open, and prints counts only. Form templates are not in eBIRForms' profile archive and do not come across.

## Checks

On Linux with Node 22, Rust (see `rust-toolchain.toml`), and `lsof`:

```sh
git clone https://github.com/buwiz-bot/buwiz-bot bot
git -C bot checkout --detach bf7cead0a53cc86408ec739a1787c9b1c23632c6   # buwiz-bot main with nsh c66f78cb (#221)
(cd bot/server && npm ci)
BUWIZ_BOT_DIR="$PWD/bot" tools/check-all.sh
```

The gates include the Rust checks, a no-detached-signal-setters check over `apps/`, the SDK-1 import gate
(`tools/check-nsh-imports.mjs`: every app imports from `nsh` only what the Bot's loader allows), Bot conformance,
and **Bot host test**. Run just the latter with `BUWIZ_BOT_DIR="$PWD/bot" tools/bot/host-test.sh`.
It uses a detached Bot worktree and a throwaway broker (port 8811, override with `EBIRFORMS_HOST_PORT`,
8810 or above), per-run trust, `ebirforms-test-01`, a file keystore, and a synthetic scratch taxpayer
database. It never uses a keychain or signs in; the account endpoint points to loopback.
Scratch data is deleted on exit; `KEEP_TMP=1` retains it, including test keys, for local debugging.

The signed `ebirforms_status` check expects refusal: status is assistant-only in the shipped manifest.
Status succeeds through the assistant surface; Forms and Deadlines each exercise a permitted signed read.
All six `ebirforms_account_*` tools (status, sign-in start and poll, sign-out, and the TIN state and link tools) are hidden from and refused to the assistant.

## Next

- Port the remaining apps and cards from `legacy/` (calendar, deadlines, register, tax profile, the cards).

## License

MIT (see `LICENSE`), as is eBIRForms, whose crates it builds on.
