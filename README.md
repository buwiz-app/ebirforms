# eBIRForms mini app

The eBIRForms mini app for the Buwiz Bot: plugin id `app.buwiz.ebirforms`, publisher `app.buwiz` (key `gc-2026-01`).
An MCP server on stdio that the Bot spawns.

## Layout

The same layout as every mini app in the `buwiz-app` org (Books is the reference):

| Path | What |
|---|---|
| `crates/ebirforms-mini-app/` | The mini app: MCP server, manifest and signing, tickets, TIN scrubbing. It began as `buwiz-mcp` (eBIRForms `feat/mcp-mode` @ `f0d5afa`) and keeps that history. |
| `apps/ebirforms/` | The app screens (`forms.tsx`). |
| `tests/` | Integration tests (the Bot contract, own data). |
| `tools/` | `check-all.sh` (every gate), `rox/conformance.sh`, `release/prepare.sh`. |
| `legacy/` | The first mini app, from `hexuria/buwiz-forms` branch `rox-mini-app-round2` @ `834e7203`, with its history. It is the reference for porting the remaining 12 apps and cards; it does not build here and is deleted once ported. |

It builds on eBIRForms' domain crates (`bir-app-core`, pinned by commit in the root `Cargo.toml`). Only the
`BirApp` facade is a supported surface.

## Its own data

The mini app keeps its own taxpayer database. It shares nothing with either eBIRForms desktop app and never reads
a keychain:

- the folder: `EBIRFORMS_DATA_DIR`, else the platform data folder `app.buwiz.ebirforms`
  (`~/Library/Application Support/app.buwiz.ebirforms` on macOS);
- `ebirforms.db`, its key `ebirforms.key` (64 hex digits, 0600, made on first run), and `logs/`.

Old desktop data comes in by export, later. Sending returns to BIR is off: a return can be saved, but nothing is
queued or sent (sending would need the desktop's keychain entries).

## Run

```
cargo build --release
target/release/ebirforms-mini-app manifest         # the unsigned manifest, for signing
tools/check-all.sh                                 # every gate
target/release/ebirforms-mini-app mcp              # what the Bot runs
```

The Bot serves only a signed manifest: `EBIRFORMS_MANIFEST`, else `app.buwiz.ebirforms.manifest.json` next to the
executable.

## Next

- Protocol 2.0 names (buwiz-bot #62): `buwiz.*` in place of `rox.*`, once its name table is final.
- Port the remaining apps and cards from `legacy/` (calendar, deadlines, register, tax profile, the cards).
