//! The named tools the mini apps call through Rox's bridge, in place of the
//! generic `bir_call` (Q12: no generic bridge for apps).
//!
//! Each `bir` host function an app uses is one tool, `bir_app_<function>`,
//! with `{args: [...], ticket?}`: the function's positional arguments as
//! `app/` passes them, and the ticket the app was opened with. The manifest
//! lists, per app, exactly the tools its source calls, so the install review
//! shows what each app can do and the broker refuses anything else.
//!
//! These tools answer `origin: app` only (`_meta["rox/caller"]`, P1): the
//! assistant has its own tools and gets a refusal here. The ticket is the
//! scope (spec 16 item 19), and it is not optional: a function that names a
//! taxpayer runs only with a ticket this plugin issued, held to that
//! ticket's TIN root by [`super::super::scope`]. Without a ticket an app
//! reaches catalogs, the Buwiz account, app-wide setup and the lists across
//! taxpayers (whose rows then carry each taxpayer's ticket), and may create
//! a taxpayer that has no ticket yet.
//!
//! Two helpers turn tickets into what an app shows: `bir_ticket_subject`
//! (the TIN and kind of a ticket the app holds, never shown to the model)
//! and `bir_ticket_issue` (a ticket for another branch of the same
//! taxpayer). `bir_app_upload` carries a picked receipt's bytes in chunks
//! under the bridge's 256 KiB argument cap, for `bir_mark_paid`.

use std::collections::{HashMap, VecDeque};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bir_core::db::Database;
use chrono::NaiveDate;
use serde_json::{Map, Value, json};

use super::super::scope::{self, Rule};
use super::tickets::{self, Subject};

pub(crate) const PREFIX: &str = "bir_app_";
pub(crate) const TICKET_SUBJECT: &str = "bir_ticket_subject";
pub(crate) const TICKET_ISSUE: &str = "bir_ticket_issue";
pub(crate) const UPLOAD: &str = "bir_app_upload";
/// One receipt, as `bir_mark_paid` takes it.
pub(crate) const UPLOAD_MAX_BYTES: usize = 25 * 1024 * 1024;
/// Uploads kept at once; the oldest goes first.
const UPLOADS_KEPT: usize = 4;

/// The host functions each app calls, read off its source.
pub(crate) const FORMS: &[&str] = &[
    "as_needed_forms",
    "cancel_queued",
    "deadlines_between",
    "grant_send",
    "list_form_templates",
    "list_returns",
    "list_send_grants",
    "list_tax_years",
    "list_taxpayers",
    "queue_form_draft",
    "revoke_send",
    "screen_form_amend",
    "screen_form_cancel_queue",
    "screen_form_check_notice",
    "screen_form_filed_original",
    "screen_form_fill",
    "screen_form_notes_seen",
    "screen_form_open",
    "screen_form_queue",
    "screen_form_sample",
    "screen_form_sample_period",
    "screen_form_save",
    "screen_form_schema",
    "screen_form_use_profile",
    "screen_form_validate",
    "screen_forms_index",
    "yearly_forms",
];
pub(crate) const TAX_PROFILE: &[&str] = &[
    "add_tax_year",
    "as_needed_forms",
    "auth_state",
    "cancel_queued",
    "create_branch",
    "create_template_from_latest",
    "deadlines_between",
    "delete_form_template",
    "delete_taxpayer",
    "get_taxpayer",
    "grant_send",
    "list_form_templates",
    "list_rdos",
    "list_returns",
    "list_send_grants",
    "list_tax_years",
    "list_zip_codes",
    "move_branch_code",
    "move_taxpayer_tin",
    "revoke_send",
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
pub(crate) const CALENDAR: &[&str] = &[
    "deadline_override_remove",
    "deadline_override_set",
    "deadline_overrides",
    "deadlines_between",
];
pub(crate) const REGISTER: &[&str] = &[
    "auth_state",
    "cloud_account_begin_sign_in",
    "cloud_account_begin_sign_up",
    "cloud_account_finish_sign_in",
    "cloud_account_session_opened",
    "cloud_account_status",
    "create_taxpayer",
    "list_rdos",
    "list_taxpayers",
    "list_zip_codes",
    "setup_helpdesk",
    "setup_solo",
];

/// Functions that only read: their tools carry `readOnlyHint` (P8).
const READ_ONLY: &[&str] = &[
    "as_needed_forms",
    "auth_state",
    "cloud_account_status",
    "deadline_overrides",
    "deadlines_between",
    "get_taxpayer",
    "list_form_templates",
    "list_rdos",
    "list_returns",
    "list_send_grants",
    "list_tax_years",
    "list_taxpayers",
    "list_zip_codes",
    "screen_form_check_notice",
    "screen_form_schema",
    "screen_form_validate",
    "screen_forms_index",
    "starter_packs",
    "suggest_next_branch_code",
    "tax_year_bounds",
    "yearly_forms",
];

/// Functions that create the subject they name, so they may run without a
/// ticket (there is none yet).
const CREATES: &[&str] = &["create_taxpayer"];

pub(crate) fn tool_name(function: &str) -> String {
    format!("{PREFIX}{function}")
}

/// Every host function some app calls.
pub(crate) fn functions() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = [FORMS, TAX_PROFILE, CALENDAR, REGISTER].concat();
    all.sort_unstable();
    all.dedup();
    all
}

/// The host function behind a `bir_app_*` tool, if it is one.
pub(crate) fn function_of(tool: &str) -> Option<&'static str> {
    let name = tool.strip_prefix(PREFIX)?;
    functions().into_iter().find(|function| *function == name)
}

/// Whether a tool of this module only reads.
pub(crate) fn read_only(tool: &str) -> bool {
    tool == TICKET_SUBJECT
        || function_of(tool).is_some_and(|function| READ_ONLY.contains(&function))
}

/// `tools/list` entries. Every tool here (the `bir_app_*` tools and the
/// ticket helpers) carries `_meta["rox/audience"]: ["app"]` (spec 10.1):
/// Rox keeps it off the model's surface and refuses model calls to it.
pub(crate) fn descriptors() -> Vec<Value> {
    let mut list = vec![
        json!({
            "name": TICKET_SUBJECT,
            "description": "Mini apps only (refused for the assistant): the taxpayer, branch or return a ticket the app holds stands for, so the app can show it.",
            "inputSchema": {
                "type": "object",
                "properties": { "ticket": { "type": "string" } },
                "required": ["ticket"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true },
            "_meta": { "rox/audience": ["app"] },
        }),
        json!({
            "name": TICKET_ISSUE,
            "description": "Mini apps only (refused for the assistant): a ticket for another branch of the taxpayer the app's ticket names.",
            "inputSchema": {
                "type": "object",
                "properties": { "ticket": { "type": "string" }, "tin": { "type": "string" } },
                "required": ["ticket", "tin"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false },
            "_meta": { "rox/audience": ["app"] },
        }),
        json!({
            "name": UPLOAD,
            "description": "Mini apps only (refused for the assistant): one chunk of a receipt file the user picked, for bir_mark_paid's proof_upload.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "upload": { "type": "string" },
                    "data": { "type": "string", "description": "base64" }
                },
                "required": ["data"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false },
            "_meta": { "rox/audience": ["app"] },
        }),
    ];
    for function in functions() {
        list.push(json!({
            "name": tool_name(function),
            "description": format!("Mini apps only (refused for the assistant): BIR's `{function}`, held to the taxpayer of the app's ticket."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "args": { "type": "array", "description": "The function's positional arguments." },
                    "ticket": { "type": "string", "description": "The ticket the app was opened with." }
                },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": READ_ONLY.contains(&function) },
            "_meta": { "rox/audience": ["app"] },
        }));
    }
    list
}

/// One `bir_app_*` call, read from its arguments.
pub(crate) struct HostRequest {
    pub function: &'static str,
    pub args: Vec<Value>,
    pub ticket: Option<String>,
}

pub(crate) fn request(tool: &str, arguments: &Map<String, Value>) -> Result<HostRequest, String> {
    let function = function_of(tool).ok_or_else(|| format!("unknown tool: {tool}"))?;
    if let Some(key) = arguments.keys().find(|key| !["args", "ticket"].contains(&key.as_str())) {
        return Err(format!("unknown argument `{key}` (allowed: args, ticket)"));
    }
    let args = match arguments.get("args") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(args)) => args.clone(),
        Some(_) => return Err("`args` must be an array of the function's arguments".into()),
    };
    let ticket = match arguments.get("ticket") {
        None | Some(Value::Null) => None,
        Some(Value::String(ticket)) => Some(ticket.clone()),
        Some(_) => return Err("`ticket` must be a ticket string".into()),
    };
    Ok(HostRequest {
        function,
        args,
        ticket,
    })
}

/// A ticket this plugin issued, else why not.
pub(crate) fn subject_of(db: &Database, ticket: &str) -> Result<Subject, String> {
    tickets::subject(db, ticket)?
        .ok_or_else(|| "the ticket is not one BIR issued, or it was revoked".to_string())
}

/// Whether a call needs the database before it runs: for its ticket, or
/// to add tickets to a list. The Buwiz account functions (the `register`
/// app, signed out) need neither.
pub(crate) fn needs_database(request: &HostRequest) -> bool {
    request.ticket.is_some()
        || !matches!(
            scope::rule(request.function),
            Some(Rule::Free) | Some(Rule::Refuse)
        )
}

/// What a host call is held to, worked out before it runs.
pub(crate) struct Prepared {
    /// The TIN root the call is held to (the bridge's scope).
    pub scope: Option<String>,
    /// The ticket's root (for a TIN move, the taxpayer that moves).
    root: Option<String>,
}

/// Before one host call for an app: the ticket checked and turned into the
/// scope, the rule's ticket requirement applied. The database lock is
/// released before the call runs (host functions take it themselves).
pub(crate) fn prepare(db: &Database, request: &HostRequest) -> Result<Prepared, String> {
    let function = request.function;
    let subject = request
        .ticket
        .as_deref()
        .map(|ticket| subject_of(db, ticket))
        .transpose()?;
    let root = subject.as_ref().map(Subject::root);
    let rule = scope::rule(function).ok_or_else(|| format!("`{function}` has no scope rule"))?;
    let needs_ticket = matches!(
        rule,
        Rule::Tin(_) | Rule::Tins(..) | Rule::Field(..) | Rule::Draft { .. } | Rule::OpenedDraft
    ) && !CREATES.contains(&function);
    if needs_ticket && root.is_none() {
        return Err(format!(
            "`{function}` names a taxpayer: pass the app's `ticket` (an app reaches a taxpayer only through a ticket BIR issued)"
        ));
    }
    if function == "move_taxpayer_tin" {
        // The new TIN is where the taxpayer goes: only the old one is held
        // to the scope, and the taxpayer's tickets follow the move.
        let held = root.clone().expect("checked above");
        let args = &request.args;
        scope::check_args("get_taxpayer", &args[..args.len().min(1)], &held)?;
        return Ok(Prepared { scope: None, root });
    }
    Ok(Prepared {
        scope: root.clone(),
        root,
    })
}

/// After a host call answered: tickets on the rows of a list, on a new
/// taxpayer, and moved with a TIN.
pub(crate) fn finish(
    db: &Database,
    request: &HostRequest,
    prepared: &Prepared,
    value: Value,
) -> Result<Value, String> {
    let function = request.function;
    if function == "move_taxpayer_tin" {
        if let (Some(from), Some(to)) = (&prepared.root, request.args.get(1).and_then(scope::root_of)) {
            tickets::move_root(db, from, &to)?;
        }
        return Ok(value);
    }
    match (scope::rule(function), value) {
        (Some(Rule::Rows(field)), rows) => with_tickets(db, rows, field),
        // A new taxpayer's ticket comes with it, so the app can open it.
        (_, Value::Object(mut created)) if CREATES.contains(&function) => {
            let subject = created
                .get("tin_root")
                .and_then(Value::as_str)
                .and_then(Subject::of_tin);
            if let Some(subject) = subject {
                created.insert("ticket".into(), Value::String(tickets::issue(db, &subject)?));
            }
            Ok(Value::Object(created))
        }
        (_, value) => Ok(value),
    }
}

/// A list across taxpayers with each row's ticket added (`ticket`), so the
/// app can open or scope the taxpayer it shows.
fn with_tickets(db: &Database, rows: Value, field: &str) -> Result<Value, String> {
    let Value::Array(mut rows) = rows else {
        return Ok(rows);
    };
    let subjects: Vec<Option<Subject>> = rows
        .iter()
        .map(|row| row.get(field).and_then(Value::as_str).and_then(Subject::of_tin))
        .collect();
    let issued = tickets::issue_all(db, &subjects)?;
    for (row, ticket) in rows.iter_mut().zip(issued) {
        if let (Some(row), Some(ticket)) = (row.as_object_mut(), ticket) {
            row.insert("ticket".into(), Value::String(ticket));
        }
    }
    Ok(Value::Array(rows))
}

/// `bir_ticket_subject {ticket}`.
pub(crate) fn ticket_subject(
    db: &Database,
    arguments: &Map<String, Value>,
    today: NaiveDate,
) -> Result<Value, String> {
    let ticket = arguments
        .get("ticket")
        .and_then(Value::as_str)
        .ok_or("`ticket` is required")?;
    let subject = subject_of(db, ticket)?;
    let title = subject
        .title(db, today)
        .ok_or("the taxpayer of this ticket is no longer saved in BIR")?;
    let mut out = subject.describe(today);
    out["title"] = Value::String(title);
    Ok(out)
}

/// `bir_ticket_issue {ticket, tin}`: a branch of the same taxpayer.
pub(crate) fn ticket_issue(
    db: &Database,
    arguments: &Map<String, Value>,
    today: NaiveDate,
) -> Result<Value, String> {
    let ticket = arguments
        .get("ticket")
        .and_then(Value::as_str)
        .ok_or("`ticket` is required")?;
    let root = subject_of(db, ticket)?.root();
    let tin = arguments
        .get("tin")
        .and_then(Value::as_str)
        .ok_or("`tin` is required")?;
    let subject = Subject::of_tin(tin).ok_or_else(|| format!("`{tin}` is not a TIN"))?;
    if subject.root() != root {
        return Err("`tin` is another taxpayer's: a ticket reaches its own taxpayer only".into());
    }
    let title = subject
        .title(db, today)
        .ok_or("no such branch is saved in BIR")?;
    Ok(json!({ "ticket": tickets::issue(db, &subject)?, "title": title }))
}

/// Receipt chunks in flight, by upload id.
#[derive(Default)]
pub(crate) struct Uploads {
    files: HashMap<String, Vec<u8>>,
    order: VecDeque<String>,
}

impl Uploads {
    /// `bir_app_upload {upload?, data}`: append one base64 chunk.
    pub(crate) fn append(&mut self, arguments: &Map<String, Value>) -> Result<Value, String> {
        let data = arguments
            .get("data")
            .and_then(Value::as_str)
            .ok_or("`data` is required: a base64 chunk")?;
        let chunk = STANDARD
            .decode(data)
            .map_err(|_| "`data` is not base64".to_string())?;
        let id = match arguments.get("upload").and_then(Value::as_str) {
            Some(id) if self.files.contains_key(id) => id.to_string(),
            Some(_) => return Err("unknown `upload`: start a new one without it".into()),
            None => {
                let mut bytes = [0u8; 12];
                getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
                let id = format!("up_{}", hex::encode(bytes));
                if self.order.len() >= UPLOADS_KEPT
                    && let Some(old) = self.order.pop_front()
                {
                    self.files.remove(&old);
                }
                self.order.push_back(id.clone());
                self.files.insert(id.clone(), Vec::new());
                id
            }
        };
        let file = self.files.get_mut(&id).expect("present");
        if file.len() + chunk.len() > UPLOAD_MAX_BYTES {
            self.files.remove(&id);
            self.order.retain(|kept| kept != &id);
            return Err("the receipt is over 25 MB; attach a smaller scan".into());
        }
        file.extend_from_slice(&chunk);
        Ok(json!({ "upload": id, "bytes": file.len() }))
    }

    /// The finished upload's bytes, taken out.
    pub(crate) fn take(&mut self, id: &str) -> Option<Vec<u8>> {
        self.order.retain(|kept| kept != id);
        self.files.remove(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_app_function_has_a_scope_rule_and_a_grammatical_tool_name() {
        let module = crate::bir_module::module();
        for function in functions() {
            assert!(scope::rule(function).is_some(), "{function} has no rule");
            assert!(module.has(function), "{function} is not a host function");
            let name = tool_name(function);
            assert!(name.len() <= 63 && name.starts_with("bir_"), "{name}");
            assert!(name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'));
        }
        assert_eq!(function_of("bir_app_list_taxpayers"), Some("list_taxpayers"));
        assert_eq!(function_of("bir_app_set_setting"), None);
        assert!(read_only("bir_app_list_taxpayers") && !read_only("bir_app_delete_taxpayer"));
    }

    #[test]
    fn uploads_append_and_cap() {
        let mut uploads = Uploads::default();
        let first = uploads
            .append(json!({"data": STANDARD.encode(b"abc")}).as_object().unwrap())
            .unwrap();
        let id = first["upload"].as_str().unwrap().to_string();
        let more = uploads
            .append(json!({"upload": id, "data": STANDARD.encode(b"de")}).as_object().unwrap())
            .unwrap();
        assert_eq!(more["bytes"], 5);
        assert_eq!(uploads.take(&id).unwrap(), b"abcde");
        assert!(uploads.take(&id).is_none());
        assert!(
            uploads
                .append(json!({"upload": "up_nope", "data": ""}).as_object().unwrap())
                .is_err()
        );
    }
}
