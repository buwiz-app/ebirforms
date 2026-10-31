# eBIRForms mini app

The eBIRForms mini app for the Buwiz Bot: plugin id `app.buwiz.ebirforms`, publisher `app.buwiz`.

## Status: extracted, not yet standalone

This repo holds the mini app's code and its history (35 commits), extracted from
`hexuria/buwiz-forms` branch `rox-mini-app-round2` at `834e7203`:

| Here | Was |
|---|---|
| `src/mini_app/` | `crates/bir-shell/src/mini_app/` (the MCP server, v1 manifest and signing, tickets, cards, tools) |
| `mini-apps/` | `crates/bir-shell/mini-apps/` (13 TSX apps and cards) |
| `src/bin/bir-mini-app.rs` | `crates/bir-shell/src/bin/bir-mini-app.rs` |

It does not build here yet. It still calls into the old desktop app's internals (`bir_module`,
`screen_form_module`, `agent`, `account_gate`, `gpui_shell`).

## Next (in a PR)

- Build on the eBIRForms crates (`bir-app-core`, `bir-core`, `bir-print` from `eBIRForms/eBIRForms`, pinned by
  git rev) instead of the old desktop. `BirApp::call` serves the same host functions by name.
- Own data: its own database (`BirApp::open_at`) in its own data folder. Nothing is shared with either desktop app;
  old data can be exported in later.
- Plugin id `app.buwiz.ebirforms`, protocol 2.0 names (buwiz-bot #62), signed with key `gc-2026-01`.
