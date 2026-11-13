//! The account's TIN list as the apps and the assistant read it: each TIN's
//! state in the words of eBIRForms' one wording table
//! ([`BirApp::tin_gate`] and its statements), never a TIN, a registered
//! name or the account's email.
//!
//! The assistant (`ebirforms_queue_list` and the cards' snapshots) gets
//! [`assistant_fields`]: the state's name, its chip, its one line and its
//! step. Staff's free-text note never reaches it. The words are written
//! here by nobody: a TSX app shows `chip`, `line` and the step's `label` as
//! they come.

use bir_app_core::{BirApp, QueueGate, TinKey, TinState, TinStatement, TinStep};
use bir_submit::{TinsError, TinsRead};
use serde_json::{Map, Value, json};

/// The only place the apps' step links go.
pub(crate) const ACCOUNT_PAGE: &str = "https://account.buwiz.com/";

/// A step's button as JSON: `{kind, label}`.
fn step_json(step: Option<TinStep>) -> Value {
    step.map_or(
        Value::Null,
        |step| json!({ "kind": step.kind(), "label": step.label() }),
    )
}

/// What the assistant may read of a state: its name, chip, line and step.
/// No `next`, no staff note, no stored server text.
pub(crate) fn assistant_fields(statement: &TinStatement, out: &mut Value) {
    out["state"] = json!(statement.state.name());
    out["chip"] = json!(statement.chip);
    out["line"] = json!(statement.line);
    out["step"] = step_json(statement.step);
}

/// Why a read has no state to show, as the answer's `read`.
pub(crate) fn read_name(read: &TinsRead) -> &'static str {
    match read {
        Ok(_) => "ok",
        Err(TinsError::SignedOut) => "signed_out",
        Err(TinsError::Offline) => "offline",
        Err(TinsError::Banned) => "banned",
        Err(_) => "unavailable",
    }
}

/// One TIN's row for an app: `ticket` names it, `branch` is its 5-digit
/// code. `note` (staff's text) only when `with_note`, which the caller
/// allows for one TIN on a screen that shows it.
pub(crate) fn row(
    ticket: &str,
    branch: &str,
    tin: &str,
    read: &TinsRead,
    now: i64,
    with_note: bool,
) -> Value {
    let gate = BirApp::tin_gate(tin, std::slice::from_ref(read), now);
    let mut row = json!({
        "ticket": ticket,
        "branch": branch,
        "can_queue": gate.can_queue,
        "state": gate_state(&gate),
        "chip": null,
        "line": null,
        "next": null,
        "step": null,
    });
    if let Some(statement) = &gate.statement {
        assistant_fields(statement, &mut row);
        row["next"] = json!(statement.next);
        if with_note {
            row["note"] = json!(statement.staff_note);
        }
    }
    row
}

/// `active`, `unknown` (the list could not say), or the state's name.
fn gate_state(gate: &QueueGate) -> &'static str {
    match (&gate.statement, gate.unknown) {
        (Some(statement), _) => statement.state.name(),
        (None, Some(_)) => "unknown",
        (None, None) => TinState::Active.name(),
    }
}

/// Whether `url` is a page of the Buwiz account site, and only that: https,
/// that host, no whitespace or control character.
pub(crate) fn account_url(url: &str) -> bool {
    url.starts_with(ACCOUNT_PAGE) && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Where a step opens for the TIN `key`: "Add it" carries the TIN; Verify and
/// Fix open the row's own page, which the account list gave. A link the list
/// gave that is not on the account site is not followed: the account's TIN
/// list is opened instead.
pub(crate) fn link(step: TinStep, key: &TinKey, action_url: Option<&str>) -> String {
    step.link(key, action_url.filter(|url| account_url(url)))
}

/// The link answer for a step of the TIN `tin`, from the list `read`.
pub(crate) fn link_for(step: TinStep, tin: &str, read: &TinsRead) -> Result<String, String> {
    let key = TinKey::parse(tin).ok_or("this ticket names no TIN a link can be made for")?;
    let action_url = read
        .as_ref()
        .ok()
        .and_then(|list| list.find(&key))
        .and_then(|row| row.action_url.as_deref());
    let url = link(step, &key, action_url);
    if account_url(&url) {
        Ok(url)
    } else {
        Err("no link on the Buwiz account site could be made".into())
    }
}

/// The `{read, complete, checked_at, stale, rows}` answer around `rows`.
pub(crate) fn answer(read: &TinsRead, rows: Vec<Value>) -> Value {
    let mut out = Map::new();
    out.insert("read".into(), json!(read_name(read)));
    let list = read.as_ref().ok();
    out.insert(
        "complete".into(),
        json!(list.is_some_and(|list| list.complete)),
    );
    out.insert(
        "checked_at".into(),
        list.and_then(|list| chrono::DateTime::from_timestamp(list.fetched_at, 0))
            .map_or(Value::Null, |at| {
                json!(at.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            }),
    );
    out.insert("stale".into(), json!(list.is_some_and(|list| list.stale)));
    out.insert("rows".into(), Value::Array(rows));
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_page_of_the_account_site_is_a_link() {
        for url in [
            "https://account.buwiz.com/tins",
            "https://account.buwiz.com/tins/new?tin=000123453&branch=00000",
            "https://account.buwiz.com/tins/claims/c1/verify",
        ] {
            assert!(account_url(url), "{url}");
        }
        for url in [
            "",
            "https://account.buwiz.com",
            "http://account.buwiz.com/tins",
            "https://account.buwiz.com.evil.example/tins",
            "https://evil.example/https://account.buwiz.com/",
            "https://account.buwiz.com/tins now",
            "https://account.buwiz.com/tins\n",
            "javascript:alert(1)",
        ] {
            assert!(!account_url(url), "{url:?}");
        }
    }

    #[test]
    fn a_link_the_server_gave_off_the_site_is_not_followed() {
        let key = TinKey::parse("000123453").unwrap();
        assert_eq!(
            link(TinStep::Verify, &key, Some("https://evil.example/verify")),
            "https://account.buwiz.com/tins"
        );
        assert_eq!(
            link(
                TinStep::Fix,
                &key,
                Some("https://account.buwiz.com/tins/claims/c3/fix")
            ),
            "https://account.buwiz.com/tins/claims/c3/fix"
        );
        // "Add it" is the one link that carries the TIN, root and branch
        assert_eq!(
            link(TinStep::Add, &key, Some("https://account.buwiz.com/x")),
            "https://account.buwiz.com/tins/new?tin=000123453&branch=00000"
        );
    }
}
