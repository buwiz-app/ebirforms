//! The Forms app's host calls (`bir_app_<function>`): which `BirApp`
//! functions it may call, and how each is held to the ticket it was opened
//! with (one taxpayer's 9-digit TIN root).
//!
//! One rule per function, read off its declaration in bir-app-core. A call
//! whose arguments name another root is refused before it runs; a list that
//! spans taxpayers is filtered after it runs. A function with no rule is not
//! callable at all, so a new function stays closed until it is classified
//! here.

use bir_app_core::bir_core::forms::agent::{self, AgentFormKind};
use serde_json::Value;

/// How one function is held to a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rule {
    /// Touches no taxpayer's data (schemas, the forms index, seen notes).
    Free,
    /// Argument `n` is a TIN or a TIN root.
    Tin(usize),
    /// Argument `draft` is a return of the form named by argument `code`.
    Draft { code: usize, draft: usize },
    /// The answer is a draft reachable only by id
    /// (`screen_form_filed_original`): checked after the call.
    OpenedDraft,
    /// The answer is a list across taxpayers; rows outside the scope go.
    Rows,
}

/// How a call touches the database, for the owner lock (`owner.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    /// Reads or computes only.
    Read,
    /// Writes only this protocol's or the page's own bookkeeping (a
    /// one-time note marked seen), never a return, taxpayer or the queue.
    Bookkeeping,
    /// Changes filing data: needs the owner lock.
    Write,
    /// Puts a return in the sending queue: needs the owner lock, then the
    /// queue runs once through the relay.
    Queue,
}

/// The functions the Forms app calls (read off `mini-apps/forms.tsx`), with
/// their rule and access.
pub(crate) static FORMS: &[(&str, Rule, Access)] = &[
    ("screen_form_schema", Rule::Free, Access::Read),
    ("screen_forms_index", Rule::Free, Access::Read),
    ("screen_form_notes_seen", Rule::Free, Access::Bookkeeping),
    ("list_taxpayers", Rule::Rows, Access::Read),
    ("deadlines_between", Rule::Rows, Access::Read),
    ("as_needed_forms", Rule::Rows, Access::Read),
    ("list_returns", Rule::Rows, Access::Read),
    ("list_tax_years", Rule::Tin(0), Access::Read),
    ("yearly_forms", Rule::Tin(0), Access::Read),
    ("list_form_templates", Rule::Tin(0), Access::Read),
    ("list_send_grants", Rule::Tin(0), Access::Read),
    ("grant_send", Rule::Tin(0), Access::Write),
    ("revoke_send", Rule::Tin(0), Access::Write),
    ("screen_form_open", Rule::Tin(0), Access::Read),
    ("screen_form_sample", Rule::Tin(0), Access::Read),
    ("screen_form_sample_period", Rule::Tin(0), Access::Read),
    ("screen_form_amend", Rule::Tin(0), Access::Read),
    ("screen_form_filed_original", Rule::OpenedDraft, Access::Read),
    ("screen_form_fill", Rule::Draft { code: 0, draft: 1 }, Access::Read),
    ("screen_form_use_profile", Rule::Draft { code: 0, draft: 3 }, Access::Read),
    ("screen_form_validate", Rule::Draft { code: 0, draft: 1 }, Access::Read),
    ("screen_form_save", Rule::Draft { code: 0, draft: 1 }, Access::Write),
    ("screen_form_check_notice", Rule::Draft { code: 0, draft: 1 }, Access::Write),
    ("screen_form_queue", Rule::Draft { code: 0, draft: 1 }, Access::Queue),
    ("screen_form_cancel_queue", Rule::Draft { code: 0, draft: 1 }, Access::Write),
    ("queue_form_draft", Rule::Tin(0), Access::Queue),
    ("cancel_queued", Rule::Tin(0), Access::Write),
];

pub(crate) fn find(function: &str) -> Option<(&'static str, Rule, Access)> {
    FORMS.iter().find(|(name, ..)| *name == function).copied()
}

/// Whether a call with this rule names a taxpayer, so it needs a ticket.
pub(crate) fn needs_ticket(rule: Rule) -> bool {
    matches!(rule, Rule::Tin(_) | Rule::Draft { .. } | Rule::OpenedDraft)
}

/// The TIN root a value names: a TIN string (with or without dashes, 9 to 14
/// digits) or a `{segment1, segment2, segment3}` TIN object.
pub(crate) fn root_of(value: &Value) -> Option<String> {
    let digits: String = match value {
        Value::String(text) => {
            if text.chars().any(|c| !(c.is_ascii_digit() || c == '-' || c == ' ')) {
                return None;
            }
            text.chars().filter(char::is_ascii_digit).collect()
        }
        Value::Object(object) => ["segment1", "segment2", "segment3"]
            .iter()
            .map(|key| object.get(*key).and_then(Value::as_str).unwrap_or(""))
            .collect::<String>(),
        _ => return None,
    };
    (digits.len() >= 9 && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits[..9].to_string())
}

/// The root a row of a cross-taxpayer list belongs to.
pub(crate) fn row_root(row: &Value) -> Option<String> {
    row.get("tin_root").or_else(|| row.get("tin")).and_then(root_of)
}

fn outside(function: &str, named: &str, scope: &str) -> String {
    format!(
        "`{function}` names TIN root {named}, outside this app's scope ({scope}); a mini app opened for one taxpayer cannot reach another"
    )
}

/// The root of a return draft: the form's own identity, else its `tin`.
pub(crate) fn draft_root(code: Option<&Value>, draft: Option<&Value>) -> Option<String> {
    let draft = draft?;
    let kind = code.and_then(Value::as_str).and_then(AgentFormKind::from_code);
    if let Some(kind) = kind
        && let Ok(identity) = agent::identity(kind, draft)
        && let Some(root) = root_of(&Value::String(identity.tin))
    {
        return Some(root);
    }
    draft.get("tin").and_then(root_of)
}

/// Refuse a call whose arguments leave `scope`. Runs before the call.
pub(crate) fn check_args(function: &str, rule: Rule, args: &[Value], scope: &str) -> Result<(), String> {
    let root = match rule {
        Rule::Free | Rule::Rows | Rule::OpenedDraft => return Ok(()),
        Rule::Tin(index) => args.get(index).and_then(root_of).ok_or_else(|| {
            format!("`{function}`: argument {index} is not a TIN, so it cannot be held to scope {scope}")
        })?,
        Rule::Draft { code, draft } => draft_root(args.get(code), args.get(draft)).ok_or_else(|| {
            format!("`{function}`: the draft names no TIN, so it cannot be held to scope {scope}")
        })?,
    };
    if root == scope { Ok(()) } else { Err(outside(function, &root, scope)) }
}

/// Hold the answer to `scope`: filter a cross-taxpayer list, refuse an
/// opened draft of another taxpayer. Runs after the call.
pub(crate) fn filter_result(function: &str, rule: Rule, args: &[Value], result: Value, scope: &str) -> Result<Value, String> {
    match (rule, result) {
        (Rule::Rows, Value::Array(rows)) => Ok(Value::Array(
            rows.into_iter().filter(|row| row_root(row).as_deref() == Some(scope)).collect(),
        )),
        (Rule::OpenedDraft, result) => {
            let root = draft_root(args.first(), result.get("draft"))
                .ok_or_else(|| format!("`{function}`: the return names no TIN"))?;
            if root == scope { Ok(result) } else { Err(outside(function, &root, scope)) }
        }
        (_, result) => Ok(result),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn each_function_has_one_rule() {
        let names: Vec<&str> = FORMS.iter().map(|(name, ..)| *name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "a function is listed twice");
    }

    #[test]
    fn arguments_naming_another_root_are_refused() {
        let scope = "000123453";
        assert!(check_args("list_tax_years", Rule::Tin(0), &[json!("00012345300000")], scope).is_ok());
        assert!(check_args("list_tax_years", Rule::Tin(0), &[json!("000-123-453")], scope).is_ok());
        let refused = check_args("list_tax_years", Rule::Tin(0), &[json!("00098765400000")], scope).unwrap_err();
        assert!(refused.contains("outside this app's scope"), "{refused}");
        assert!(check_args("list_tax_years", Rule::Tin(0), &[json!("not a tin")], scope).is_err());
        let draft = json!({"tin": "000123453"});
        assert!(check_args("screen_form_save", Rule::Draft { code: 0, draft: 1 }, &[json!("XXXX"), draft], scope).is_ok());
        let rows = json!([{"tin_root": "000123453"}, {"tin": "00098765400000"}, {"tin": "00012345300002"}]);
        let kept = filter_result("list_returns", Rule::Rows, &[], rows, scope).unwrap();
        assert_eq!(kept.as_array().unwrap().len(), 2);
    }
}
