# eBIRForms mini app

The eBIRForms mini app for the Buwiz Bot: plugin id `app.buwiz.ebirforms`, publisher `app.buwiz` (key `gc-2026-01`), protocol 1.0 (`buwiz-sdk` `docs/protocol-1.0.md`; 2.0 and 2.1 were its pre-release numbering, which a 1.0 Bot refuses).
An MCP server on stdio that the Bot spawns.

## Layout

The same layout as every mini app in the `buwiz-app` org (Books is the reference):

| Path | What |
|---|---|
| `crates/ebirforms-mini-app/` | The mini app: MCP server, manifest and signing, tickets, TIN scrubbing. It began as `buwiz-mcp` (eBIRForms `feat/mcp-mode` @ `f0d5afa`) and keeps that history. |
| `apps/ebirforms/` | The app screens: `forms`, `tax-profile`, `calendar`, `deadlines` (an inline card) and `register` (Buwiz Sign-in). |
| `tests/` | Integration tests (the Bot contract, own data). |
| `tools/` | `check-all.sh` (every gate), `bot/conformance.sh`, `bot/host-test.sh`, `release/publish.sh` (a protocol 1.0 package release: binary, signed manifest and signed `plugin.json` from `package/plugin.json`). |
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

## Checks

On Linux with Node 22, Rust (see `rust-toolchain.toml`), and `lsof`:

```sh
git clone https://github.com/buwiz-bot/buwiz-bot bot
git -C bot checkout --detach 224bffa539b910b76e4c02bcfd94a4a33102a672   # buwiz-bot main with protocol 1.0 (#102)
(cd bot/server && npm ci)
BUWIZ_BOT_DIR="$PWD/bot" tools/check-all.sh
```

The gates include the Rust checks, a no-detached-signal-setters check over `apps/`, Bot conformance,
and **Bot host test**. Run just the latter with `BUWIZ_BOT_DIR="$PWD/bot" tools/bot/host-test.sh`.
It uses a detached Bot worktree and a throwaway broker (port 8811, override with `EBIRFORMS_HOST_PORT`,
8810 or above), per-run trust, `ebirforms-test-01`, a file keystore, and a synthetic scratch taxpayer
database. It never uses a keychain or signs in; the account endpoint points to loopback.
Scratch data is deleted on exit; `KEEP_TMP=1` retains it, including test keys, for local debugging.

The signed `ebirforms_status` check expects refusal: status is assistant-only in the shipped manifest.
Status succeeds through the assistant surface; Forms and Deadlines each exercise a permitted signed read.
All four `ebirforms_account_*` tools are hidden from and refused to the assistant.

## Next

- Port the remaining apps and cards from `legacy/` (calendar, deadlines, register, tax profile, the cards).

## License

MIT (see `LICENSE`), as is eBIRForms, whose crates it builds on.
