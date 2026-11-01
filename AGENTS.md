# AGENTS.md

Guidance for coding agents in this repo.

## What this repo is

The eBIRForms mini app for the Buwiz Bot: plugin id `app.buwiz.ebirforms`, publisher `app.buwiz`, key `gc-2026-01`, protocol 1.0 (`buwiz-sdk` `docs/protocol-1.0.md`; 2.0 and 2.1 were its pre-release numbering, which a 1.0 Bot refuses).
A Rust MCP server on stdio. It is laid out like every mini app in the `buwiz-app` org (Books is the reference):

| Path | What |
|---|---|
| `crates/ebirforms-mini-app/` | the binary: MCP server, manifest, signing, tickets, TIN scrubbing |
| `apps/ebirforms/` | the app screens, TSX until each is ported to a native nsh Rust screen crate in this repo (option C: compiled into the Bot, pinned by rev); a TSX screen is deleted once its native port passes the same tests |
| `tests/` | integration tests, wired in the crate's `Cargo.toml` with `[[test]] path = "../../tests/…"` |
| `tools/` | `check-all.sh` (every gate), `bot/conformance.sh`, `bot/host-test.sh`, `release/publish.sh` (a protocol 1.0 package release) |
| `legacy/` | the first mini app from `hexuria/buwiz-forms`, kept only as the porting reference; it does not build |

## Invariants

1. **Its own data.** The mini app keeps its own database in its own folder (`EBIRFORMS_DATA_DIR`, else the platform
   data folder `app.buwiz.ebirforms`): `ebirforms.db`, a 0600 key file `ebirforms.key`, and `logs/`. It never reads
   a keychain (`BUWIZ_NO_KEYCHAIN=1`) and never opens either eBIRForms desktop app's database. Old data comes in by
   export.
2. **No sending.** Nothing is queued, sent, filed or paid from here. Sending would need the desktop's keychain
   entries.
3. **Only `owner::prepare` is unsafe** (it sets the process environment before any thread starts). The workspace
   denies `unsafe_code`.
4. **eBIRForms' crates by commit.** `bir-app-core` is pinned by SHA in the root `Cargo.toml`. Only its `BirApp`
   facade is a supported surface; `bir-core` internals change without notice.
5. **TINs never leave as TINs.** Taxpayers reach the assistant and the apps as tickets (`tkt_…`). Never put a TIN, a
   ticket value or a key in a log, a title, a doc or a fixture.
6. **One protocol version: 1.0.** No compatibility shims for older protocol versions or names.

## Gates

`tools/check-all.sh` must pass before every commit: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test`, and `tools/bot/conformance.sh`. Conformance signs with a throwaway test key (`ebirforms-test-01`,
never `gc-2026-01`) and runs the Bot's `buwiz-plugin-test`. It needs a Buwiz Bot checkout at protocol 1.0
(`BUWIZ_BOT_DIR`) with its `server/node_modules`. Never switch branches in the Bot checkout itself.

## Do not

- Edit eBIRForms (`eBIRForms/eBIRForms`), the Bot (`buwiz-bot/buwiz-bot`) or `hexuria/buwiz-forms` from here.
- Sign with `gc-2026-01` or touch the login Keychain.
- Commit, push or deploy unless the owner asks.
