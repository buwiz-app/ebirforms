//! The mini apps' host calls (`ebirforms_app_<function>`): which `BirApp`
//! functions each app may call, and how each is held to the ticket it was
//! opened with (one taxpayer's 9-digit TIN root).
//!
//! One rule per function, read off its declaration in bir-app-core. A call
//! whose arguments name another root is refused before it runs; a list that
//! spans taxpayers is filtered after it runs. A new taxpayer
//! (`create_taxpayer`) names no saved root, so it is the one call about a
//! taxpayer that needs no ticket; its answer is the new taxpayer's ticket. A
//! function with no rule is not callable at all, so a new function stays
//! closed until it is classified here.

use bir_app_core::bir_core::forms::agent::{self, AgentFormKind};
use serde_json::Value;

/// How one function is held to a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rule {
    /// Touches no taxpayer's data (schemas, the forms index, seen notes).
    Free,
    /// Argument `n` is a TIN or a TIN root.
    Tin(usize),
    /// Arguments `a` and `b` are both TIN roots (`move_taxpayer_tin`).
    Tins(usize, usize),
    /// Argument `draft` is a return of the form named by argument `code`.
    Draft { code: usize, draft: usize },
    /// The answer is a draft reachable only by id
    /// (`screen_form_filed_original`): checked after the call.
    OpenedDraft,
    /// The answer is a list across taxpayers; rows outside the scope go.
    Rows,
    /// Argument 0 is a taxpayer not saved yet (`create_taxpayer`, from the
    /// Tax Profile app's New taxpayer form). It reaches no saved taxpayer's
    /// data, so it needs no ticket and is held to none; it is checked field by
    /// field first, and answers the new taxpayer's ticket and title, never its
    /// TIN (`crate::new_taxpayer`).
    New,
}

/// How a call touches the database: only a read runs for a resolve or routine call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    /// Reads or computes only.
    Read,
    /// Writes only this protocol's or the page's own bookkeeping (a
    /// one-time note marked seen), never a return, taxpayer or the queue.
    Bookkeeping,
    /// Changes filing data.
    Write,
    /// Puts a return in the sending queue (signed in only), then the queue
    /// runs once through the relay.
    Queue,
}

/// Every function an app may call, with its rule and access.
pub(crate) static FUNCTIONS: &[(&str, Rule, Access)] = &[
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
    // a write: a stored Draft follows the tax profile as it opens, and the open
    // saves that quietly (`open_screen_form`, bir-app-core)
    ("screen_form_open", Rule::Tin(0), Access::Write),
    ("screen_form_sample", Rule::Tin(0), Access::Read),
    ("screen_form_sample_period", Rule::Tin(0), Access::Read),
    ("screen_form_amend", Rule::Tin(0), Access::Read),
    (
        "screen_form_filed_original",
        Rule::OpenedDraft,
        Access::Read,
    ),
    (
        "screen_form_fill",
        Rule::Draft { code: 0, draft: 1 },
        Access::Read,
    ),
    (
        "screen_form_use_profile",
        Rule::Draft { code: 0, draft: 3 },
        Access::Read,
    ),
    (
        "screen_form_validate",
        Rule::Draft { code: 0, draft: 1 },
        Access::Read,
    ),
    (
        "screen_form_save",
        Rule::Draft { code: 0, draft: 1 },
        Access::Write,
    ),
    (
        "screen_form_check_notice",
        Rule::Draft { code: 0, draft: 1 },
        Access::Write,
    ),
    (
        "screen_form_queue",
        Rule::Draft { code: 0, draft: 1 },
        Access::Queue,
    ),
    (
        "screen_form_cancel_queue",
        Rule::Draft { code: 0, draft: 1 },
        Access::Write,
    ),
    ("queue_form_draft", Rule::Tin(0), Access::Queue),
    ("cancel_queued", Rule::Tin(0), Access::Write),
    ("queued_send_state", Rule::Tin(0), Access::Read),
    // puts a stuck return back to Draft (never back in the queue); a write, so
    // only a person's own press reaches it
    ("release_abandoned_claim", Rule::Tin(0), Access::Write),
    // Tax Profile: reference lists, then the taxpayer, its branches, tax
    // years, forms and templates
    ("tax_year_bounds", Rule::Free, Access::Read),
    ("list_rdos", Rule::Free, Access::Read),
    ("list_zip_codes", Rule::Free, Access::Read),
    ("get_taxpayer", Rule::Tin(0), Access::Read),
    ("create_taxpayer", Rule::New, Access::Write),
    ("update_taxpayer", Rule::Tin(0), Access::Write),
    ("set_taxpayer_archived", Rule::Tin(0), Access::Write),
    ("delete_taxpayer", Rule::Tin(0), Access::Write),
    ("move_taxpayer_tin", Rule::Tins(0, 1), Access::Write),
    ("create_branch", Rule::Tin(0), Access::Write),
    ("update_branch", Rule::Tin(0), Access::Write),
    ("move_branch_code", Rule::Tin(0), Access::Write),
    ("suggest_next_branch_code", Rule::Tin(0), Access::Read),
    ("add_tax_year", Rule::Tin(0), Access::Write),
    ("set_yearly_forms", Rule::Tin(0), Access::Write),
    ("starter_packs", Rule::Tin(0), Access::Read),
    ("create_template_from_latest", Rule::Tin(0), Access::Write),
    ("delete_form_template", Rule::Tin(0), Access::Write),
    // Calendar: a moved deadline is a rule for every taxpayer, no one
    // taxpayer's data, so the Calendar opened for one taxpayer can move and
    // restore one
    ("deadline_overrides", Rule::Free, Access::Read),
    ("deadline_override_set", Rule::Free, Access::Write),
    ("deadline_override_remove", Rule::Free, Access::Write),
];

/// The functions the Forms app calls (read off `apps/ebirforms/forms.tsx`).
pub(crate) static FORMS: &[&str] = &[
    "screen_form_schema",
    "screen_forms_index",
    "screen_form_notes_seen",
    "list_taxpayers",
    "deadlines_between",
    "as_needed_forms",
    "list_returns",
    "list_tax_years",
    "yearly_forms",
    "list_form_templates",
    "screen_form_open",
    "screen_form_sample",
    "screen_form_sample_period",
    "screen_form_amend",
    "screen_form_filed_original",
    "screen_form_fill",
    "screen_form_use_profile",
    "screen_form_validate",
    "screen_form_save",
    "screen_form_check_notice",
    "screen_form_queue",
    "screen_form_cancel_queue",
    "queue_form_draft",
    "cancel_queued",
    "queued_send_state",
    "release_abandoned_claim",
];

/// The functions the Tax Profile app calls (`apps/ebirforms/tax-profile.tsx`).
pub(crate) static TAX_PROFILE: &[&str] = &[
    "add_tax_year",
    "as_needed_forms",
    "cancel_queued",
    "create_branch",
    "create_taxpayer",
    "create_template_from_latest",
    "deadlines_between",
    "delete_form_template",
    "delete_taxpayer",
    "get_taxpayer",
    "list_form_templates",
    "list_rdos",
    "list_returns",
    "list_tax_years",
    "list_zip_codes",
    "move_branch_code",
    "move_taxpayer_tin",
    "screen_forms_index",
    "set_taxpayer_archived",
    "set_yearly_forms",
    "starter_packs",
    "suggest_next_branch_code",
    "tax_year_bounds",
    "update_branch",
    "update_taxpayer",
    "yearly_forms",
];

/// The functions the Calendar app calls (`apps/ebirforms/calendar.tsx`).
pub(crate) static CALENDAR: &[&str] = &[
    "deadline_override_remove",
    "deadline_override_set",
    "deadline_overrides",
    "deadlines_between",
];

pub(crate) fn find(function: &str) -> Option<(&'static str, Rule, Access)> {
    FUNCTIONS
        .iter()
        .find(|(name, ..)| *name == function)
        .copied()
}

/// Whether a call with this rule names a taxpayer, so it needs a ticket.
pub(crate) fn needs_ticket(rule: Rule) -> bool {
    matches!(
        rule,
        Rule::Tin(_) | Rule::Tins(..) | Rule::Draft { .. } | Rule::OpenedDraft
    )
}

/// The TIN root a value names: a TIN string (with or without dashes, 9 to 14
/// digits) or a `{segment1, segment2, segment3}` TIN object.
pub(crate) fn root_of(value: &Value) -> Option<String> {
    let digits: String = match value {
        Value::String(text) => {
            if text
                .chars()
                .any(|c| !(c.is_ascii_digit() || c == '-' || c == ' '))
            {
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
    (digits.len() >= 9 && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits[..9].to_string())
}

/// The root a row of a cross-taxpayer list belongs to.
pub(crate) fn row_root(row: &Value) -> Option<String> {
    row.get("tin_root")
        .or_else(|| row.get("tin"))
        .and_then(root_of)
}

fn outside(function: &str, named: &str, scope: &str) -> String {
    format!(
        "`{function}` names TIN root {named}, outside this app's scope ({scope}); a mini app opened for one taxpayer cannot reach another"
    )
}

/// The root of a return draft: the form's own identity, else its `tin`.
pub(crate) fn draft_root(code: Option<&Value>, draft: Option<&Value>) -> Option<String> {
    let draft = draft?;
    let kind = code
        .and_then(Value::as_str)
        .and_then(AgentFormKind::from_code);
    if let Some(kind) = kind
        && let Ok(identity) = agent::identity(kind, draft)
        && let Some(root) = root_of(&Value::String(identity.tin))
    {
        return Some(root);
    }
    draft.get("tin").and_then(root_of)
}

/// The digits of a TIN value (a string with or without dashes, or a
/// `{segment1, segment2, segment3}` object), whatever their count: the
/// shared `TinKey` decides whether they are a TIN.
fn tin_digits(value: &Value) -> Option<String> {
    let digits: String = match value {
        Value::String(text) => text.chars().filter(char::is_ascii_digit).collect(),
        Value::Object(object) => ["segment1", "segment2", "segment3"]
            .iter()
            .map(|key| object.get(*key).and_then(Value::as_str).unwrap_or(""))
            .collect(),
        _ => return None,
    };
    (!digits.is_empty()).then_some(digits)
}

/// The TIN of the return a Queue call puts in the sending queue, as the
/// call wrote it (9, 12 or 14 digits): argument 0 of `queue_form_draft`, or
/// the draft `screen_form_queue` is given, by its own identity. `None` for
/// any other function, or a call that names no TIN (then `check_args` has
/// refused it already). Compare it only through `TinKey`.
pub(crate) fn queue_tin(function: &str, args: &[Value]) -> Option<String> {
    match function {
        "queue_form_draft" => args.first().and_then(tin_digits),
        "screen_form_queue" => {
            let draft = args.get(1)?;
            args.first()
                .and_then(Value::as_str)
                .and_then(AgentFormKind::from_code)
                .and_then(|kind| agent::identity(kind, draft).ok())
                .and_then(|identity| tin_digits(&Value::String(identity.tin)))
                .or_else(|| draft.get("tin").and_then(tin_digits))
        }
        _ => None,
    }
}

/// Refuse a call whose arguments leave `scope`. Runs before the call.
pub(crate) fn check_args(
    function: &str,
    rule: Rule,
    args: &[Value],
    scope: &str,
) -> Result<(), String> {
    let root = match rule {
        Rule::Free | Rule::Rows | Rule::OpenedDraft | Rule::New => return Ok(()),
        Rule::Tin(index) => args.get(index).and_then(root_of).ok_or_else(|| {
            format!(
                "`{function}`: argument {index} is not a TIN, so it cannot be held to scope {scope}"
            )
        })?,
        Rule::Tins(first, second) => {
            check_args(function, Rule::Tin(first), args, scope)?;
            check_args(function, Rule::Tin(second), args, scope)?;
            return Ok(());
        }
        Rule::Draft { code, draft } => {
            draft_root(args.get(code), args.get(draft)).ok_or_else(|| {
                format!(
                    "`{function}`: the draft names no TIN, so it cannot be held to scope {scope}"
                )
            })?
        }
    };
    if root == scope {
        Ok(())
    } else {
        Err(outside(function, &root, scope))
    }
}

/// Hold the answer to `scope`: filter a cross-taxpayer list, refuse an
/// opened draft of another taxpayer. Runs after the call.
pub(crate) fn filter_result(
    function: &str,
    rule: Rule,
    args: &[Value],
    result: Value,
    scope: &str,
) -> Result<Value, String> {
    match (rule, result) {
        (Rule::Rows, Value::Array(rows)) => Ok(Value::Array(
            rows.into_iter()
                .filter(|row| row_root(row).as_deref() == Some(scope))
                .collect(),
        )),
        (Rule::OpenedDraft, result) => {
            let root = draft_root(args.first(), result.get("draft"))
                .ok_or_else(|| format!("`{function}`: the return names no TIN"))?;
            if root == scope {
                Ok(result)
            } else {
                Err(outside(function, &root, scope))
            }
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
        let names: Vec<&str> = FUNCTIONS.iter().map(|(name, ..)| *name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "a function is listed twice");
        for function in FORMS.iter().chain(TAX_PROFILE).chain(CALENDAR) {
            assert!(find(function).is_some(), "{function} has no rule");
        }
    }

    #[test]
    fn each_app_lists_exactly_the_functions_its_source_calls() {
        for (functions, source) in [
            (FORMS, crate::manifest::FORMS_SOURCE),
            (TAX_PROFILE, crate::manifest::TAX_PROFILE_SOURCE),
            (CALENDAR, crate::manifest::CALENDAR_SOURCE),
        ] {
            let mut called: Vec<&str> = source
                .split('"')
                .skip(1)
                .step_by(2)
                .filter(|word| find(word).is_some())
                .collect();
            called.sort_unstable();
            called.dedup();
            let mut listed = functions.to_vec();
            listed.sort_unstable();
            for function in &listed {
                assert!(
                    source.contains(&format!("\"{function}\"")),
                    "{function} is listed but never called"
                );
            }
            for function in called {
                assert!(
                    listed.contains(&function),
                    "{function} is called but not listed"
                );
            }
        }
    }

    #[test]
    fn a_new_taxpayer_is_a_tax_profile_write_held_to_no_ticket() {
        assert_eq!(
            find("create_taxpayer"),
            Some(("create_taxpayer", Rule::New, Access::Write))
        );
        assert!(TAX_PROFILE.contains(&"create_taxpayer"));
        assert!(!FORMS.contains(&"create_taxpayer"));
        assert!(!CALENDAR.contains(&"create_taxpayer"));
        assert!(!needs_ticket(Rule::New));
        // it names no saved taxpayer, so a profile open on another may add one
        let scope = "000123453";
        let new = [json!({ "tin_root": "000987654" })];
        assert!(check_args("create_taxpayer", Rule::New, &new, scope).is_ok());
        let answer = json!({ "ticket": "tkt_0", "title": "Synthetic" });
        assert_eq!(
            filter_result("create_taxpayer", Rule::New, &new, answer.clone(), scope).unwrap(),
            answer
        );
    }

    #[test]
    fn a_queue_call_names_the_tin_of_its_return() {
        // 9, 12 and 14 digits, with or without dashes: all one key to the gate
        for tin in [
            "000123453",
            "000-123-453",
            "000123453001",
            "000-123-453-001",
            "00012345300001",
        ] {
            let args = [json!(tin), json!("0619E"), json!(2026), json!(1)];
            let named = queue_tin("queue_form_draft", &args).unwrap();
            assert!(named.chars().all(|c| c.is_ascii_digit()), "{named}");
            assert!(
                bir_app_core::TinKey::parse(&named).is_some(),
                "{tin} reads as a TIN key"
            );
        }
        // a draft of a form this build does not name is read by its `tin`
        for tin in ["000123453", "00012345300001"] {
            let args = [json!("NO-SUCH-FORM"), json!({ "tin": tin })];
            assert_eq!(queue_tin("screen_form_queue", &args).as_deref(), Some(tin));
        }
        let object = [json!({ "segment1": "000", "segment2": "123", "segment3": "453" })];
        assert_eq!(
            queue_tin("queue_form_draft", &object).as_deref(),
            Some("000123453")
        );
        // any other call, or one with no TIN, names none
        assert_eq!(queue_tin("cancel_queued", &[json!("000123453")]), None);
        assert_eq!(queue_tin("queue_form_draft", &[]), None);
        assert_eq!(queue_tin("queue_form_draft", &[json!(5)]), None);
        assert_eq!(queue_tin("screen_form_queue", &[json!("0619E")]), None);
    }

    #[test]
    fn a_tin_move_holds_both_roots() {
        let scope = "000123453";
        let rule = Rule::Tins(0, 1);
        assert!(
            check_args(
                "move_taxpayer_tin",
                rule,
                &[json!(scope), json!(scope)],
                scope
            )
            .is_ok()
        );
        assert!(
            check_args(
                "move_taxpayer_tin",
                rule,
                &[json!(scope), json!("000987654")],
                scope
            )
            .is_err()
        );
    }

    #[test]
    fn arguments_naming_another_root_are_refused() {
        let scope = "000123453";
        assert!(
            check_args(
                "list_tax_years",
                Rule::Tin(0),
                &[json!("00012345300000")],
                scope
            )
            .is_ok()
        );
        assert!(
            check_args(
                "list_tax_years",
                Rule::Tin(0),
                &[json!("000-123-453")],
                scope
            )
            .is_ok()
        );
        let refused = check_args(
            "list_tax_years",
            Rule::Tin(0),
            &[json!("00098765400000")],
            scope,
        )
        .unwrap_err();
        assert!(refused.contains("outside this app's scope"), "{refused}");
        assert!(check_args("list_tax_years", Rule::Tin(0), &[json!("not a tin")], scope).is_err());
        let draft = json!({"tin": "000123453"});
        assert!(
            check_args(
                "screen_form_save",
                Rule::Draft { code: 0, draft: 1 },
                &[json!("XXXX"), draft],
                scope
            )
            .is_ok()
        );
        let rows = json!([{"tin_root": "000123453"}, {"tin": "00098765400000"}, {"tin": "00012345300002"}]);
        let kept = filter_result("list_returns", Rule::Rows, &[], rows, scope).unwrap();
        assert_eq!(kept.as_array().unwrap().len(), 2);
    }
}
