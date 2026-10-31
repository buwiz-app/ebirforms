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
//! `bir_app_print_preview` answers Rox's `rox.preview` with a return's
//! print sheet as a static HTML document, made in memory.

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
/// The Forms app's print preview (`rox.preview`).
pub(crate) const PRINT_PREVIEW: &str = "bir_app_print_preview";
/// The approve card's "Allow this once" (spec 9.8).
pub(crate) const ALLOW_SEND: &str = "bir_card_allow_send";
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
    "cloud_account_sign_out",
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
        || tool == PRINT_PREVIEW
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
    list.push(json!({
        "name": PRINT_PREVIEW,
        "description": "Mini apps only (refused for the assistant): a return's print preview for Rox to show, as a static HTML document. `draft_id` prints a saved return as stored (`year` and `quarter` for 2551Q and 1601C, which are kept by period: quarter is the month for 1601C); `draft_json` (the page's draft as JSON text, with its staged `edits` and `unlocked` boxes) prints a Form library sample as the page holds it; `period` prints a sent return's filing record.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "ticket": { "type": "string", "description": "The ticket the app was opened with." },
                "form": { "type": "string" },
                "draft_id": { "type": "integer" },
                "year": { "type": "integer" },
                "quarter": { "type": "integer" },
                "draft_json": { "type": "string", "description": "The page's draft, JSON.stringify'd: a draft's amounts are not integers, which a signed call cannot carry as numbers." },
                "edits": { "type": "object" },
                "unlocked": { "type": "array", "items": { "type": "string" } },
                "period": { "type": "string" }
            },
            "required": ["ticket", "form"],
            "additionalProperties": false
        },
        "annotations": { "readOnlyHint": true },
        "_meta": { "rox/audience": ["app"] },
    }));
    list.push(json!({
        "name": ALLOW_SEND,
        "description": "Mini apps only (refused for the assistant): the approve card's Allow this once, a send grant for one taxpayer and form that ends with the next send.",
        "inputSchema": {
            "type": "object",
            "properties": { "tin": { "type": "string" }, "form": { "type": "string" } },
            "required": ["tin", "form"],
            "additionalProperties": false
        },
        "annotations": { "readOnlyHint": false },
        "_meta": { "rox/audience": ["app"] },
    }));
    for function in functions() {
        list.push(json!({
            "name": tool_name(function),
            "description": format!("Mini apps only (refused for the assistant): BIR's `{function}`, held to the taxpayer of the app's ticket."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "args": { "type": "array", "description": "The function's positional arguments." },
                    "args_json": { "type": "string", "description": "The same arguments as JSON text, for values a signed call can't carry (amounts with cents)." },
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
    if let Some(key) = arguments.keys().find(|key| !["args", "args_json", "ticket"].contains(&key.as_str())) {
        return Err(format!("unknown argument `{key}` (allowed: args, args_json, ticket)"));
    }
    // A signed call carries whole numbers only: a draft's amounts (629.94,
    // a 0.03 rate) come as `args_json`, the arguments as JSON text.
    let args = match (arguments.get("args"), arguments.get("args_json")) {
        (Some(args), Some(_)) if !args.is_null() => return Err("send `args` or `args_json`, not both".into()),
        (_, Some(Value::String(text))) => match serde_json::from_str(text) {
            Ok(Value::Array(args)) => args,
            _ => return Err("`args_json` must be a JSON array of the function's arguments".into()),
        },
        (_, Some(_)) => return Err("`args_json` must be a JSON array, as text".into()),
        (None | Some(Value::Null), None) => Vec::new(),
        (Some(Value::Array(args)), None) => args.clone(),
        (Some(_), None) => return Err("`args` must be an array of the function's arguments".into()),
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

/// What `bir_app_print_preview` prints, its ticket checked.
#[derive(Debug, PartialEq)]
pub(crate) enum PreviewAsk {
    /// A draft: `[form, draft, edits, unlocked]`, as
    /// `screen_form_print_sample` takes them (a saved return's, as stored,
    /// with no edits).
    Draft(Vec<Value>),
    /// A 2551Q or 1601C as stored for its period (`quarter` is the month
    /// for 1601C), of the ticket's branch.
    Period { tin: String, form: String, year: u16, quarter: u8 },
    /// A sent return's filing record: `{tin, form, period}` with the
    /// ticket's TIN.
    Record(Map<String, Value>),
}

/// `bir_app_print_preview {ticket, form, draft_id}` or `{ticket, form,
/// year, quarter}` (a saved return; the second for 2551Q and 1601C),
/// `{ticket, form, draft_json, edits?, unlocked?}` (a Form library sample,
/// which is not stored) or `{ticket, form, period}` (a sent return's filing
/// record), read and held to the ticket's taxpayer: a draft must be of that
/// taxpayer, and a record is of the ticket's own branch. A draft's amounts
/// are not integers, which a signed call carries only as text: a saved
/// return is read here, and a sample comes as JSON text.
pub(crate) fn preview_ask(db: &Database, arguments: &Map<String, Value>) -> Result<PreviewAsk, String> {
    const KEYS: [&str; 9] = ["ticket", "form", "draft_id", "year", "quarter", "draft_json", "edits", "unlocked", "period"];
    if let Some(key) = arguments.keys().find(|key| !KEYS.contains(&key.as_str())) {
        return Err(format!("unknown argument `{key}` (allowed: {})", KEYS.join(", ")));
    }
    let ticket = arguments
        .get("ticket")
        .and_then(Value::as_str)
        .ok_or("`ticket` is required: the ticket the app was opened with")?;
    let subject = subject_of(db, ticket)?;
    let form = arguments
        .get("form")
        .and_then(Value::as_str)
        .ok_or("`form` is required: a BIR form code")?;
    let given = ["draft_id", "year", "draft_json", "period"].iter().filter(|key| arguments.contains_key(**key)).count();
    if given != 1 {
        return Err("pass one of `draft_id` or `year` and `quarter` (a saved return), `draft_json` (a sample) or `period` (a sent return's record)".into());
    }
    if let Some(year) = arguments.get("year") {
        let year = year.as_u64().and_then(|y| u16::try_from(y).ok()).ok_or("`year` must be a year like 2026")?;
        let quarter = arguments
            .get("quarter")
            .and_then(Value::as_u64)
            .and_then(|q| u8::try_from(q).ok())
            .ok_or("`quarter` (the quarter, or the month for 1601C) must be a number")?;
        if !["2551Q", "1601C"].contains(&form) {
            return Err(format!("`year` and `quarter` name a 2551Q or 1601C; {form} prints by `draft_id`"));
        }
        if arguments.len() != 4 {
            return Err("a saved return prints as stored: `ticket`, `form`, `year` and `quarter` only".into());
        }
        return Ok(PreviewAsk::Period { tin: subject.full_tin(), form: form.to_string(), year, quarter });
    }
    if let Some(id) = arguments.get("draft_id") {
        let id = id.as_i64().ok_or("`draft_id` must be an integer")?;
        if arguments.contains_key("edits") || arguments.contains_key("unlocked") {
            return Err("a saved return prints as stored: no `edits` or `unlocked`".into());
        }
        let draft = db
            .draft_data_json(id)
            .map_err(|error| error.to_string())?
            .filter(Value::is_object)
            .ok_or_else(|| format!("no saved return has id {id}"))?;
        let args = vec![Value::String(form.into()), draft];
        scope::check_args("screen_form_print", &args, &subject.root())?;
        return Ok(PreviewAsk::Draft(args));
    }
    match (arguments.get("draft_json"), arguments.get("period")) {
        (Some(Value::String(text)), None) => {
            let draft = serde_json::from_str::<Value>(text)
                .ok()
                .filter(Value::is_object)
                .ok_or("`draft_json` must be a draft object as JSON text")?;
            let mut args = vec![Value::String(form.into()), draft];
            scope::check_args("screen_form_print", &args, &subject.root())?;
            let edits = arguments.get("edits").cloned().unwrap_or_else(|| json!({}));
            if !edits.is_object() {
                return Err("`edits` must be an object of box keys to values".into());
            }
            let unlocked = arguments.get("unlocked").cloned().unwrap_or_else(|| json!([]));
            if !unlocked.as_array().is_some_and(|keys| keys.iter().all(Value::is_string)) {
                return Err("`unlocked` must be a list of box keys".into());
            }
            args.extend([edits, unlocked]);
            Ok(PreviewAsk::Draft(args))
        }
        (None, Some(Value::String(period))) => {
            if arguments.contains_key("edits") || arguments.contains_key("unlocked") {
                return Err("a filing record takes no `edits` or `unlocked`".into());
            }
            let mut record = Map::new();
            record.insert("tin".into(), Value::String(subject.full_tin()));
            record.insert("form".into(), Value::String(form.into()));
            record.insert("period".into(), Value::String(period.clone()));
            Ok(PreviewAsk::Record(record))
        }
        _ => Err("`draft_json` and `period` are strings".into()),
    }
}

/// The tool result Rox's `rox.preview` reads: the page as an embedded
/// HTML resource, and its title.
pub(crate) fn preview_result(form: &str, html: String, title: String) -> Value {
    let form: String = form.chars().filter(char::is_ascii_alphanumeric).collect();
    json!({
        "content": [{
            "type": "resource",
            "resource": { "uri": format!("bir://print/{form}.html"), "mimeType": "text/html", "text": html },
        }],
        "structuredContent": { "title": title },
        "isError": false,
    })
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
    fn a_draft_with_cents_comes_as_json_text() {
        let args = |v: Value| v.as_object().unwrap().clone();
        let tool = "bir_app_screen_form_save";
        let sent = request(tool, &args(json!({"args_json": "[\"2551Q\", {\"tax_rate\": 0.03, \"amount\": 629.94}]", "ticket": "t"}))).unwrap();
        assert_eq!(sent.args, vec![json!("2551Q"), json!({"tax_rate": 0.03, "amount": 629.94})]);
        assert_eq!(sent.ticket.as_deref(), Some("t"));
        assert_eq!(request(tool, &args(json!({"args": [1, 2]}))).unwrap().args, vec![json!(1), json!(2)]);
        for bad in [json!({"args": [], "args_json": "[]"}), json!({"args_json": "{}"}), json!({"args_json": [1]}), json!({"args_json": "not json"})] {
            assert!(request(tool, &args(bad.clone())).is_err(), "{bad}");
        }
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
