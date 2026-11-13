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

use bir_app_core::{TinStatement, TinStep};
use serde_json::{Value, json};

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
