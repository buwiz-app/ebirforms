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
   data folder `app.buwiz.ebirforms`): `ebirforms.db`, a 0600 key file `ebirforms.key`, the Buwiz sign-in's tokens
   and device key as 0600 files (`buwiz_submit_*.txt`), the boxes' sources (`fill-sources.json` and its key
   `fill-sources.key`, both 0600, and `fill-sources.lock`), and `logs/`. It never reads a keychain
   (`BUWIZ_NO_KEYCHAIN=1`) and never opens either eBIRForms desktop app's database. Old data comes in by export. The one exception is the
   one-time `import-legacy`, whose child opens a copy of the retired app's database; it never opens the original.
2. **Only the person sends, from an app.** Queueing a return is an app-only tool (`Access::Queue` in `scope.rs`):
   the agent, resolve and routine callers are refused it, and it needs a Buwiz sign-in (the Sign-in app,
   `account.rs`). The queue sends through Buwiz's relay with eBIRForms' submission client (`buwiz-ebirforms`) and
   keeps the signed receipt. Whether a return was a dry run is known only from its receipt; without one, say it is
   not known and never suggest resending. Nothing is paid from here. Tests and benches send only to a local dry-run
   server (`EBIRFORMS_ACCOUNT_URL` on loopback): never a real return, never a real taxpayer.
3. **Only `owner::prepare` is unsafe** (it sets the process environment before any thread starts). The workspace
   denies `unsafe_code`; a test that needs the environment calls `owner::prepare` as `main` does, one test per process
   (`mcp_contract` still sets two variables itself).
4. **eBIRForms' crates by commit.** `bir-app-core` is pinned by SHA in the root `Cargo.toml`. Only its `BirApp`
   facade is a supported surface; `bir-core` internals change without notice.
5. **TINs never leave as TINs.** Taxpayers reach the assistant and the apps as tickets (`tkt_…`). Never put a TIN, a
   ticket value or a key in a log, a title, a doc or a fixture. Fixtures are synthetic: never paste a real email,
   receipt or return into one, even with the TIN changed (a real MyEG receipt in a `legacy/` test forced a history
   rewrite before this repo went public).
6. **One protocol version: 1.0.** No compatibility shims for older protocol versions or names.
7. **Each filled box keeps its source.** `ebirforms_form_fill`'s `source` (and `source_ref`) is kept per box
   (`sources.rs`), shown by `ebirforms_form_fields`, saved with the return, and dropped once the box holds another
   value. Returns and values are named only by HMACs under `fill-sources.key`, so the file alone gives no TIN and no
   amount; saves hold `fill-sources.lock`, and an unreadable file is never overwritten. `user` needs `from_card: true`,
   which is the assistant's word: eBIRForms has no record of card answers to check it against, so it labels a box
   and proves nothing.
8. **Books' hand-off is a contract.** `tests/fixtures/books-handoff/` is the same files as Books'
   `tests/fixtures/handoff/` (one `books_handoff_get` payload per form, the two form tools' input schemas and the shared
   amount vectors `amounts.json`, which `shown_amount` must read as Books' `money::parse_shown` does), pinned
   by one hash in both repos (`tests/fill_sources.rs`). A change to `form_open` or `form_fill`'s schema changes it.

## Gates

`tools/check-all.sh` must pass before every commit: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test`, the SDK-1 import gate (`node tools/check-nsh-imports.mjs`: an app may import from `nsh` only the
names the Bot's loader allows, `buwiz-bot app/src/mini_v1/load.rs` `SDK1_NSH`; anything else and the Bot refuses to
open it), and `tools/bot/conformance.sh`. Conformance signs with a throwaway test key (`ebirforms-test-01`,
never `gc-2026-01`) and runs the Bot's `buwiz-plugin-test`. It needs a Buwiz Bot checkout at protocol 1.0
(`BUWIZ_BOT_DIR`) with its `server/node_modules`. Never switch branches in the Bot checkout itself.

## Do not

- Edit eBIRForms (`eBIRForms/eBIRForms`), the Bot (`buwiz-bot/buwiz-bot`) or `hexuria/buwiz-forms` from here.
- Sign with `gc-2026-01` or touch the login Keychain, unless the owner runs `tools/dev-install.sh --live` or a release
  by hand.
- Commit, push or deploy unless the owner asks.
