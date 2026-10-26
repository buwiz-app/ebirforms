//! Profile scope for `bir_call`: a mini app opened for one taxpayer passes
//! that taxpayer's 9-digit TIN root, and every host call it makes is held to
//! that root.
//!
//! One rule per host function, read off its TypeScript declaration. A call
//! whose arguments name another root is refused before it runs; a list that
//! spans taxpayers is filtered after it runs. A function with no rule is
//! refused under a scope, so a new host function stays closed until it is
//! classified here (`every_host_function_has_a_rule` keeps the table whole).

use bir_core::forms::agent::{self, AgentFormKind};
use serde_json::Value;

/// How one host function is held to a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rule {
    /// Touches no taxpayer's data (catalogs, schemas, the Buwiz account).
    Free,
    /// Argument `n` is a TIN or a TIN root.
    Tin(usize),
    /// Arguments `a` and `b` are both TIN roots (`move_taxpayer_tin`).
    Tins(usize, usize),
    /// Argument `n` is an object whose `field` is the TIN or root.
    Field(usize, &'static str),
    /// Argument `draft` is a return of the form named by argument `code`.
    Draft { code: usize, draft: usize },
    /// The answer is a draft of another taxpayer's return reachable only by
    /// id (`screen_form_filed_original`): checked after the call.
    OpenedDraft,
    /// The answer is a list across taxpayers; keep rows whose `field` is in scope.
    Rows(&'static str),
    /// App-wide settings, admin, recovery and jobs: never under a scope.
    Refuse,
}

/// The rule table. Names are the `bir` host module's exports.
pub(crate) static RULES: &[(&str, Rule)] = &[
    // Catalogs, schemas and app facts.
    ("system_info", Rule::Free),
    ("forms_catalog", Rule::Free),
    ("screen_form_schema", Rule::Free),
    ("screen_forms_index", Rule::Free),
    ("screen_form_notes_seen", Rule::Free),
    ("tax_year_bounds", Rule::Free),
    ("list_rdos", Rule::Free),
    ("list_zip_codes", Rule::Free),
    ("list_deadlines", Rule::Free),
    // Deadline overrides: a moved deadline is a rule for every taxpayer
    // (maintainer decision, 2026-10-26), no one taxpayer's data, so the
    // Calendar app opened for one taxpayer can move and restore one.
    ("deadline_overrides", Rule::Free),
    ("deadline_override_set", Rule::Free),
    ("deadline_override_remove", Rule::Free),
    ("auth_state", Rule::Free),
    ("generate_totp_secret", Rule::Free),
    ("record_screen_log", Rule::Free),
    ("update_status", Rule::Free),
    // The Buwiz account (no taxpayer data).
    ("cloud_account_status", Rule::Free),
    ("cloud_account_gate", Rule::Free),
    ("cloud_account_begin_sign_in", Rule::Free),
    ("cloud_account_begin_sign_up", Rule::Free),
    ("cloud_account_finish_sign_in", Rule::Free),
    ("cloud_account_refresh", Rule::Free),
    ("cloud_account_sign_out", Rule::Free),
    ("cloud_account_session_opened", Rule::Free),
    ("cloud_account_session_closed", Rule::Free),
    // Lists across taxpayers.
    ("list_taxpayers", Rule::Rows("tin_root")),
    ("list_profiles", Rule::Rows("tin")),
    ("list_queued", Rule::Rows("tin")),
    ("deadlines_between", Rule::Rows("tin_root")),
    ("as_needed_forms", Rule::Rows("tin_root")),
    ("list_returns", Rule::Rows("tin_root")),
    // First argument is the TIN or root.
    ("get_taxpayer", Rule::Tin(0)),
    ("update_taxpayer", Rule::Tin(0)),
    ("set_taxpayer_archived", Rule::Tin(0)),
    ("delete_taxpayer", Rule::Tin(0)),
    ("taxpayer_auth", Rule::Tin(0)),
    ("set_taxpayer_pin", Rule::Tin(0)),
    ("set_taxpayer_totp", Rule::Tin(0)),
    ("verify_taxpayer_pin", Rule::Tin(0)),
    ("verify_taxpayer_totp", Rule::Tin(0)),
    ("create_branch", Rule::Tin(0)),
    ("update_branch", Rule::Tin(0)),
    ("move_branch_code", Rule::Tin(0)),
    ("suggest_next_branch_code", Rule::Tin(0)),
    ("list_tax_years", Rule::Tin(0)),
    ("add_tax_year", Rule::Tin(0)),
    ("yearly_forms", Rule::Tin(0)),
    ("set_yearly_forms", Rule::Tin(0)),
    ("starter_packs", Rule::Tin(0)),
    ("list_form_templates", Rule::Tin(0)),
    ("create_template_from_latest", Rule::Tin(0)),
    ("delete_form_template", Rule::Tin(0)),
    ("inbox_state", Rule::Tin(0)),
    ("set_inbox_app_password", Rule::Tin(0)),
    ("set_inbox_tracking", Rule::Tin(0)),
    ("disconnect_inbox_oauth", Rule::Tin(0)),
    ("connect_inbox_oauth", Rule::Tin(0)),
    ("test_inbox_connection", Rule::Tin(0)),
    ("check_inbox_now", Rule::Tin(0)),
    ("list_send_grants", Rule::Tin(0)),
    ("grant_send", Rule::Tin(0)),
    ("revoke_send", Rule::Tin(0)),
    ("profile_detail", Rule::Tin(0)),
    ("set_profile_archived", Rule::Tin(0)),
    ("delete_profile", Rule::Tin(0)),
    ("set_profile_pin", Rule::Tin(0)),
    ("set_profile_totp", Rule::Tin(0)),
    ("export_profile", Rule::Tin(0)),
    ("list_drafts", Rule::Tin(0)),
    ("get_form_draft", Rule::Tin(0)),
    ("save_form_draft", Rule::Tin(0)),
    ("queue_form_draft", Rule::Tin(0)),
    ("cancel_queued", Rule::Tin(0)),
    ("form_print_html", Rule::Tin(0)),
    ("screen_form_open", Rule::Tin(0)),
    ("screen_form_sample", Rule::Tin(0)),
    ("screen_form_amend", Rule::Tin(0)),
    ("screen_form_sample_period", Rule::Tin(0)),
    ("move_taxpayer_tin", Rule::Tins(0, 1)),
    ("create_taxpayer", Rule::Field(0, "tin_root")),
    ("save_profile", Rule::Field(0, "tin")),
    // A return travels as (form_code, draft).
    ("screen_form_fill", Rule::Draft { code: 0, draft: 1 }),
    ("screen_form_validate", Rule::Draft { code: 0, draft: 1 }),
    ("screen_form_save", Rule::Draft { code: 0, draft: 1 }),
    ("screen_form_print", Rule::Draft { code: 0, draft: 1 }),
    (
        "screen_form_print_sample",
        Rule::Draft { code: 0, draft: 1 },
    ),
    (
        "screen_form_preview_sync",
        Rule::Draft { code: 0, draft: 1 },
    ),
    ("screen_form_queue", Rule::Draft { code: 0, draft: 1 }),
    (
        "screen_form_cancel_queue",
        Rule::Draft { code: 0, draft: 1 },
    ),
    (
        "screen_form_check_notice",
        Rule::Draft { code: 0, draft: 1 },
    ),
    ("screen_form_use_profile", Rule::Draft { code: 0, draft: 3 }),
    ("screen_form_filed_original", Rule::OpenedDraft),
    // App-wide: settings, admin, recovery, updates, jobs, archives.
    ("get_setting", Rule::Refuse),
    ("set_setting", Rule::Refuse),
    ("delete_setting", Rule::Refuse),
    ("set_global_hotkey", Rule::Refuse),
    ("set_log_retention", Rule::Refuse),
    ("install_update", Rule::Refuse),
    ("dismiss_update", Rule::Refuse),
    ("check_for_updates", Rule::Refuse),
    ("set_auto_update_download", Rule::Refuse),
    ("verify_admin_pin", Rule::Refuse),
    ("verify_admin_totp", Rule::Refuse),
    ("set_admin_pin", Rule::Refuse),
    ("set_admin_totp", Rule::Refuse),
    ("setup_solo", Rule::Refuse),
    ("setup_helpdesk", Rule::Refuse),
    ("change_owner", Rule::Refuse),
    ("change_to_helpdesk", Rule::Refuse),
    ("create_recovery_key", Rule::Refuse),
    ("use_recovery_key", Rule::Refuse),
    ("mac_login", Rule::Refuse),
    ("import_archive", Rule::Refuse),
    ("list_background_jobs", Rule::Refuse),
    ("delete_background_job", Rule::Refuse),
    ("archive_background_job", Rule::Refuse),
    ("purge_archived_jobs", Rule::Refuse),
    ("job_log_tail", Rule::Refuse),
    ("agent_take", Rule::Refuse),
    ("pull_device_filings", Rule::Refuse),
    ("shell_set_chrome", Rule::Refuse),
    ("create_template_from_return", Rule::Refuse),
];

pub(crate) fn rule(function: &str) -> Option<Rule> {
    RULES
        .iter()
        .find(|(name, _)| *name == function)
        .map(|(_, rule)| *rule)
}

/// `"123-456-789"`, `"123456789"` -> the root. Anything else is refused.
pub(crate) fn parse_scope(raw: &str) -> Result<String, String> {
    let digits: String = raw.chars().filter(|c| !matches!(c, '-' | ' ')).collect();
    if digits.len() == 9 && digits.bytes().all(|b| b.is_ascii_digit()) {
        Ok(digits)
    } else {
        Err(format!("`scope` must be a 9-digit TIN root (got `{raw}`)"))
    }
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

fn outside(function: &str, named: &str, scope: &str) -> String {
    format!(
        "`{function}` names TIN root {named}, outside this app's scope ({scope}); a mini app opened for one taxpayer cannot reach another"
    )
}

fn hold(function: &str, value: Option<&Value>, what: &str, scope: &str) -> Result<(), String> {
    let root = value.and_then(root_of).ok_or_else(|| {
        format!("`{function}`: {what} is not a TIN, so it cannot be held to scope {scope}")
    })?;
    if root == scope {
        Ok(())
    } else {
        Err(outside(function, &root, scope))
    }
}

/// The root of a return draft: the form's own identity, else its `tin`.
fn draft_root(code: Option<&Value>, draft: Option<&Value>) -> Option<String> {
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

/// Refuse a call whose arguments leave `scope`. Runs before the call.
pub(crate) fn check_args(function: &str, args: &[Value], scope: &str) -> Result<(), String> {
    let rule = rule(function).ok_or_else(|| {
        format!("`{function}` is not available to a mini app opened for one taxpayer")
    })?;
    match rule {
        Rule::Free | Rule::Rows(_) | Rule::OpenedDraft => Ok(()),
        Rule::Tin(index) => hold(
            function,
            args.get(index),
            &format!("argument {index}"),
            scope,
        ),
        Rule::Tins(first, second) => {
            hold(
                function,
                args.get(first),
                &format!("argument {first}"),
                scope,
            )?;
            hold(
                function,
                args.get(second),
                &format!("argument {second}"),
                scope,
            )
        }
        Rule::Field(index, field) => hold(
            function,
            args.get(index).and_then(|value| value.get(field)),
            &format!("argument {index}.{field}"),
            scope,
        ),
        Rule::Draft { code, draft } => {
            let root = draft_root(args.get(code), args.get(draft)).ok_or_else(|| {
                format!(
                    "`{function}`: the draft names no TIN, so it cannot be held to scope {scope}"
                )
            })?;
            if root == scope {
                Ok(())
            } else {
                Err(outside(function, &root, scope))
            }
        }
        Rule::Refuse => Err(format!(
            "`{function}` is app-wide and not available to a mini app opened for one taxpayer"
        )),
    }
}

/// Hold the answer to `scope`: filter a cross-taxpayer list, refuse an opened
/// draft of another taxpayer. Runs after the call.
pub(crate) fn filter_result(
    function: &str,
    args: &[Value],
    result: Value,
    scope: &str,
) -> Result<Value, String> {
    match rule(function) {
        Some(Rule::Rows(field)) => match result {
            Value::Array(rows) => Ok(Value::Array(
                rows.into_iter()
                    .filter(|row| row.get(field).and_then(root_of).as_deref() == Some(scope))
                    .collect(),
            )),
            other => Ok(other),
        },
        Some(Rule::OpenedDraft) => {
            let root = draft_root(args.first(), result.get("draft"))
                .ok_or_else(|| format!("`{function}`: the return names no TIN"))?;
            if root == scope {
                Ok(result)
            } else {
                Err(outside(function, &root, scope))
            }
        }
        _ => Ok(result),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SCOPE: &str = "123456788";

    #[test]
    fn every_host_function_has_a_rule() {
        let module = crate::bir_module::module();
        let missing: Vec<&str> = module
            .function_names()
            .into_iter()
            .filter(|name| rule(name).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "unclassified host functions: {missing:?}"
        );
        for (name, _) in RULES {
            assert!(
                module.has(name),
                "rule for a function the module lacks: {name}"
            );
        }
    }

    #[test]
    fn scope_parses_a_root_and_refuses_anything_else() {
        assert_eq!(parse_scope("123-456-788").unwrap(), SCOPE);
        assert_eq!(parse_scope(SCOPE).unwrap(), SCOPE);
        assert!(parse_scope("12345678800000").is_err());
        assert!(parse_scope("abc").is_err());
    }

    #[test]
    fn root_of_reads_strings_and_tin_objects() {
        assert_eq!(root_of(&json!("123-456-788-00000")).as_deref(), Some(SCOPE));
        assert_eq!(
            root_of(
                &json!({"segment1": "123", "segment2": "456", "segment3": "788", "branch": "00000"})
            )
            .as_deref(),
            Some(SCOPE)
        );
        assert!(root_of(&json!("1234")).is_none());
        assert!(root_of(&json!("12345678x")).is_none());
        assert!(root_of(&json!(123456788)).is_none());
    }

    #[test]
    fn free_functions_pass_untouched() {
        check_args("forms_catalog", &[], SCOPE).unwrap();
        let rows = json!([{"code": "1601C"}]);
        assert_eq!(
            filter_result("forms_catalog", &[], rows.clone(), SCOPE).unwrap(),
            rows
        );
    }

    #[test]
    fn tin_rule_holds_the_named_argument() {
        check_args("get_taxpayer", &[json!(SCOPE)], SCOPE).unwrap();
        check_args(
            "screen_form_open",
            &[json!("12345678800000"), json!("1601C")],
            SCOPE,
        )
        .unwrap();
        let error = check_args("get_taxpayer", &[json!("999999999")], SCOPE).unwrap_err();
        assert!(error.contains("outside this app's scope"), "{error}");
        assert!(check_args("get_taxpayer", &[], SCOPE).is_err());
    }

    #[test]
    fn tins_rule_holds_both_roots() {
        check_args("move_taxpayer_tin", &[json!(SCOPE), json!(SCOPE)], SCOPE).unwrap();
        assert!(
            check_args(
                "move_taxpayer_tin",
                &[json!(SCOPE), json!("999999999")],
                SCOPE
            )
            .is_err()
        );
    }

    #[test]
    fn field_rule_reads_the_object_field() {
        check_args(
            "create_taxpayer",
            &[json!({"tin_root": SCOPE, "name": "A"})],
            SCOPE,
        )
        .unwrap();
        assert!(
            check_args(
                "create_taxpayer",
                &[json!({"tin_root": "999999999"})],
                SCOPE
            )
            .is_err()
        );
        check_args(
            "save_profile",
            &[json!({"tin": {"segment1": "123", "segment2": "456", "segment3": "788", "branch": "000"}})],
            SCOPE,
        )
        .unwrap();
    }

    #[test]
    fn draft_rule_reads_the_returns_tin() {
        let ours = json!({"tin": "123-456-788-00000"});
        let theirs = json!({"tin": "999999999"});
        check_args("screen_form_save", &[json!("1601C"), ours.clone()], SCOPE).unwrap();
        assert!(check_args("screen_form_save", &[json!("1601C"), theirs], SCOPE).is_err());
        check_args(
            "screen_form_use_profile",
            &[json!("1601C"), json!(2026), json!(1), ours],
            SCOPE,
        )
        .unwrap();
        assert!(check_args("screen_form_queue", &[json!("1601C"), json!({})], SCOPE).is_err());
    }

    #[test]
    fn rows_rule_filters_the_list() {
        let rows =
            json!([{"tin_root": SCOPE, "name": "A"}, {"tin_root": "999999999", "name": "B"}]);
        let kept = filter_result("list_taxpayers", &[], rows, SCOPE).unwrap();
        assert_eq!(kept, json!([{"tin_root": SCOPE, "name": "A"}]));
        let profiles = json!([{"tin": "123-456-788-000"}, {"tin": "999-999-999-000"}]);
        assert_eq!(
            filter_result("list_profiles", &[], profiles, SCOPE)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn opened_draft_rule_checks_the_answer() {
        let ours = json!({"draft": {"tin": "123456788"}});
        assert_eq!(
            filter_result(
                "screen_form_filed_original",
                &[json!("1601C")],
                ours.clone(),
                SCOPE
            )
            .unwrap(),
            ours
        );
        let theirs = json!({"draft": {"tin": "999999999"}});
        assert!(
            filter_result(
                "screen_form_filed_original",
                &[json!("1601C")],
                theirs,
                SCOPE
            )
            .is_err()
        );
    }

    #[test]
    fn refuse_rule_and_unknown_functions_are_closed_under_a_scope() {
        let error = check_args("set_setting", &[json!("k"), json!("v")], SCOPE).unwrap_err();
        assert!(error.contains("app-wide"), "{error}");
        assert!(check_args("no_such_function", &[], SCOPE).is_err());
    }
}
