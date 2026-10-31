//! MCP for `buwiz-mcp`: JSON-RPC 2.0, one message per line.
//!
//! Every tool runs in this process over [`BirApp`], the same door the
//! Buwiz app uses:
//!
//! - the Forms app's calls: `bir_app_<function>` (one per function its
//!   source calls, held to its ticket by [`crate::scope`]), the ticket
//!   helpers, the print preview and the payments list;
//! - the assistant's tools: status, taxpayers, dues, drafts, opening the
//!   Forms app, and one open return to read, fill, validate, save or send;
//! - `bir_resolve` for the manifest's `taxprofile` scope;
//! - the reserved `rox.*` tools ([`crate::protocol`]).
//!
//! Buwiz Bot speaks the Mini App Protocol v1: `rox.provision` first, then
//! every call carries its caller (`_meta["rox/caller"]`), and
//! [`Server::authorize`] holds each origin to what it may do before a tool
//! runs. Writes take the database's owner lock for the call only
//! ([`crate::owner`]).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use bir_app_core::bir_core::forms::taxform::{self, TaxPeriod};
use bir_app_core::{BirApp, FieldValue, OpenReturn, OpenedReturn, ReturnFilter};
use chrono::NaiveDate;
use serde_json::{Map, Value, json};

use crate::manifest::{self, ManifestSource};
use crate::owner::{self, Sends};
use crate::protocol::{self, Caller, Host, Origin};
use crate::scope::{self, Access, Rule};
use crate::scrub::{self, Scrubber};
use crate::tickets::{self, Subject};

pub(crate) const SERVER_NAME: &str = "buwiz-mcp";
pub(crate) const LATEST_PROTOCOL: &str = "2025-06-18";
pub(crate) const SUPPORTED_PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

// The assistant's tools.
pub(crate) const STATUS: &str = "bir_status";
pub(crate) const APP_OPEN: &str = "bir_app_open";
pub(crate) const RESOLVE: &str = "bir_resolve";
pub(crate) const PROFILES_LIST: &str = "bir_profiles_list";
pub(crate) const DUES_LIST: &str = "bir_dues_list";
pub(crate) const DRAFTS_LIST: &str = "bir_drafts_list";
pub(crate) const FORM_OPEN: &str = "bir_form_open";
pub(crate) const FORM_FIELDS: &str = "bir_form_fields";
pub(crate) const FORM_FILL: &str = "bir_form_fill";
pub(crate) const FORM_VALIDATE: &str = "bir_form_validate";
pub(crate) const FORM_SAVE: &str = "bir_form_save_draft";
pub(crate) const FORM_SEND: &str = "bir_form_send";
pub(crate) const FORM_DISMISS: &str = "bir_form_dismiss";

// The Forms app's own tools (refused for the assistant).
pub(crate) const APP_PREFIX: &str = "bir_app_";
pub(crate) const TICKET_SUBJECT: &str = "bir_ticket_subject";
pub(crate) const TICKET_ISSUE: &str = "bir_ticket_issue";
pub(crate) const PRINT_PREVIEW: &str = "bir_app_print_preview";
/// Buwiz keeps no payments yet: the list is empty, so the Forms app's
/// payment section shows nothing paid.
pub(crate) const PAYMENTS_LIST: &str = "bir_payments_list";

/// The assistant's tools that change nothing (`readOnlyHint`, P8): the ones
/// a routine or resolve call may run.
const READ_ONLY: [&str; 7] = [STATUS, RESOLVE, PROFILES_LIST, DUES_LIST, DRAFTS_LIST, FORM_FIELDS, FORM_VALIDATE];

const INSTRUCTIONS: &str = "Buwiz (Philippine BIR tax returns) running headless on this computer \
over the taxpayer database Buwiz keeps. Taxpayers, branches and returns reach you as tickets \
(tkt_...), never as TINs: pass a ticket wherever a tool takes `tin`; a return's ticket also goes \
in bir_form_open's `code` and bir_app_open's `form`. Never ask the user for a TIN and never write \
one out; use the taxpayer's name. bir_profiles_list lists the taxpayers, bir_dues_list the returns \
due, bir_drafts_list a taxpayer's saved returns. To let the user work visually, call bir_app_open \
with `app: \"forms\"` and the taxpayer's ticket as `tin` (and `form`, `period` to open one \
return). You can draft a return yourself: bir_form_open, bir_form_fields, bir_form_fill (with its \
`source`), bir_form_validate, bir_form_save_draft, bir_form_dismiss; one return is open at a time. \
bir_form_send queues it for filing only when the user gave a standing OK for that taxpayer and \
form in the Forms app; otherwise the user presses Submit in the Forms app. While the Buwiz app is \
open on this computer, it owns the database: saving and queueing are refused here and the user \
does them in Buwiz. Tools whose description starts with \"Mini apps only\" and bir_resolve belong \
to the Forms app and to the Bot: do not call them.";

/// The return the assistant has open.
struct OpenForm {
    tin: String,
    form: String,
    opened: OpenedReturn,
    /// Edits since the last save.
    dirty: bool,
}

pub struct Server {
    app: Result<Arc<BirApp>, String>,
    db_path: PathBuf,
    manifest: ManifestSource,
    host: Host,
    form: Option<OpenForm>,
    sends: Sends,
}

impl Server {
    /// The server the Bot spawns: the live database the Buwiz app uses,
    /// opened without its owner lock ([`crate::owner`]). A database that
    /// cannot open (no Buwiz on this computer yet, a key that cannot be
    /// read) leaves the protocol working and every tool saying why.
    pub fn open_live(manifest: ManifestSource) -> Self {
        let db_path = owner::database_path();
        let app = if db_path.is_file() {
            BirApp::open_at(&db_path).map(Arc::new).map_err(|error| format!("the Buwiz database did not open: {error}"))
        } else {
            Err("Buwiz has no taxpayer database on this computer yet: open the Buwiz app once first.".to_string())
        };
        if let Err(problem) = &app {
            tracing::warn!("{problem}");
        }
        Self::new(app, db_path, manifest, true)
    }

    /// A server over `app` (whose file is `db_path`). `require_provision`:
    /// answer no tool before `rox.provision`, as the Bot expects.
    pub fn with_app(app: Arc<BirApp>, db_path: PathBuf, manifest: ManifestSource, require_provision: bool) -> Self {
        Self::new(Ok(app), db_path, manifest, require_provision)
    }

    fn new(app: Result<Arc<BirApp>, String>, db_path: PathBuf, manifest: ManifestSource, require_provision: bool) -> Self {
        Self { app, db_path, manifest, host: Host::new(require_provision), form: None, sends: Sends::default() }
    }

    /// Wait for the sends this process started (call before exiting).
    pub fn finish(&mut self) {
        self.sends.join();
    }

    /// One line in, at most one line out.
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        if line.trim().is_empty() {
            return None;
        }
        let reply = match serde_json::from_str::<Value>(line) {
            Ok(message) => self.handle(message),
            Err(error) => Some(error_response(Value::Null, PARSE_ERROR, &format!("parse error: {error}"))),
        };
        reply.map(|value| value.to_string())
    }

    /// One JSON-RPC message; `None` for a notification.
    pub fn handle(&mut self, message: Value) -> Option<Value> {
        let Some(object) = message.as_object() else {
            return Some(error_response(Value::Null, INVALID_REQUEST, "expected a single JSON-RPC object"));
        };
        let id = object.get("id").cloned();
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return id.map(|id| error_response(id, INVALID_REQUEST, "missing method"));
        };
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        let id = id?;
        let outcome = match method {
            "initialize" => Ok(initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(tool_list()),
            "tools/call" => self.tools_call(&params),
            "resources/list" => Ok(resource_list()),
            "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
            "resources/read" => match params.get("uri").and_then(Value::as_str) {
                Some(manifest::MANIFEST_URI) => self
                    .manifest
                    .text()
                    .map(|text| json!({ "contents": [{ "uri": manifest::MANIFEST_URI, "mimeType": manifest::MANIFEST_MIME, "text": text }] }))
                    .map_err(|error| (INVALID_PARAMS, error)),
                Some(uri) if uri == forms_uri() => Ok(json!({
                    "contents": [{ "uri": uri, "mimeType": manifest::APP_MIME, "text": manifest::FORMS_SOURCE }],
                })),
                Some(uri) => Err((INVALID_PARAMS, format!("unknown resource `{uri}`"))),
                None => Err((INVALID_PARAMS, "resources/read requires params.uri".into())),
            },
            other => Err((METHOD_NOT_FOUND, format!("method not found: {other}"))),
        };
        Some(match outcome {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => error_response(id, code, &message),
        })
    }

    fn tools_call(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((INVALID_PARAMS, "tools/call requires params.name".to_string()))?;
        let empty = Map::new();
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(arguments)) => arguments,
            Some(_) => return Ok(tool_error("arguments must be a JSON object".into())),
        };
        if name.starts_with("rox.") {
            let app = self.app.as_deref().map_err(String::as_str);
            return match self.host.reserved(&self.manifest, app, name, arguments) {
                None => Err((METHOD_NOT_FOUND, format!("{name} is not a tool of this plugin"))),
                Some(Err(message)) => Err((INVALID_REQUEST, message)),
                Some(Ok(result)) => Ok(result),
            };
        }
        if !self.host.ready() {
            return Err((INVALID_REQUEST, "rox.provision must come first".into()));
        }
        let caller = Caller::of(params);
        if let Err(message) = self.authorize(name, &caller) {
            return Ok(protocol::fail("permission_denied", &message));
        }
        let app = match &self.app {
            Ok(app) => app.clone(),
            Err(message) => return Ok(tool_error(message.clone())),
        };
        if is_app_only(name) {
            return Ok(self.app_tool(&app, name, arguments));
        }
        let mut arguments = arguments.clone();
        let has_ticket = Value::Object(arguments.clone()).to_string().contains("tkt_");
        if (has_ticket || caller.origin == Origin::App)
            && let Err(message) = scrub::expand(&app, name, &mut arguments, caller.origin)
        {
            return Ok(tool_error(message));
        }
        let today = taxform::today();
        let mut result = self.agent_tool(&app, name, &arguments, today)?;
        // What the model or an app reads names taxpayers by ticket.
        if name != RESOLVE {
            Scrubber::new(&app, today).result(&mut result);
        }
        Ok(result)
    }

    /// Whether `caller` may call `name` at all (P1, P5, P6): app-only tools
    /// answer apps, an app calls only what its manifest entry lists and only
    /// from an open this process certified, and resolve calls and routine
    /// queries never cause side effects.
    fn authorize(&self, name: &str, caller: &Caller) -> Result<(), String> {
        if matches!(caller.origin, Origin::Resolve | Origin::Routine) && !read_only(name) {
            return Err(format!("{name} changes Buwiz data, so it does not run for a resolve or routine call"));
        }
        if caller.origin != Origin::App {
            return if is_app_only(name) { Err(format!("{name} is for the Forms mini app only")) } else { Ok(()) };
        }
        let app = caller.app.as_deref().ok_or("an app call names no app")?;
        if self.host.provisioned() && self.host.session_app(caller.open_id.as_deref().unwrap_or("")) != Some(app) {
            return Err("this app open was not certified by Buwiz".into());
        }
        let allowed = self
            .manifest
            .app(app)?
            .and_then(|entry| entry["permissions"]["tools"].as_array().cloned())
            .unwrap_or_default();
        if allowed.iter().any(|tool| tool == name) {
            Ok(())
        } else {
            Err(format!("{name} is not in the {app} app's permissions"))
        }
    }

    // ------------------------------------------------------- the Forms app

    fn app_tool(&mut self, app: &Arc<BirApp>, name: &str, arguments: &Map<String, Value>) -> Value {
        let today = taxform::today();
        let answer = match name {
            TICKET_SUBJECT => ticket_subject(app, arguments, today),
            TICKET_ISSUE => ticket_issue(app, arguments, today),
            PAYMENTS_LIST => payments_list(app, arguments),
            PRINT_PREVIEW => return print_preview(app, arguments, today),
            _ => self.host_call(app, name, arguments),
        };
        call_result(answer.map_err(|message| format!("{name}: {message}")))
    }

    /// One `bir_app_<function>` call: arguments read, the ticket turned into
    /// the scope, the owner lock taken for a write, the answer held to the
    /// scope and a list's rows given their tickets.
    fn host_call(&mut self, app: &Arc<BirApp>, name: &str, arguments: &Map<String, Value>) -> Result<Value, String> {
        let (function, rule, access) = name
            .strip_prefix(APP_PREFIX)
            .and_then(scope::find)
            .ok_or_else(|| format!("unknown tool: {name}"))?;
        let (args, ticket) = host_arguments(arguments)?;
        let root = ticket.as_deref().map(|ticket| tickets::subject_of(app, ticket).map(|subject| subject.root())).transpose()?;
        if scope::needs_ticket(rule) && root.is_none() {
            return Err(format!(
                "`{function}` names a taxpayer: pass the app's `ticket` (an app reaches a taxpayer only through a ticket Buwiz issued)"
            ));
        }
        if let Some(root) = &root {
            scope::check_args(function, rule, &args, root)?;
        }
        let lock = match access {
            Access::Write | Access::Queue => Some(owner::acquire(&self.db_path)?),
            Access::Read | Access::Bookkeeping => None,
        };
        let value = app.call(function, args.clone()).map_err(|error| error.to_string())?;
        let value = match &root {
            Some(root) => scope::filter_result(function, rule, &args, value, root)?,
            None => value,
        };
        if let (Access::Queue, Some(lock)) = (access, lock) {
            match self.sends.start(app, lock) {
                Ok(true) => tracing::info!("{function}: queued, sending through the relay"),
                Ok(false) => tracing::info!("{function}: queued; not signed in, so it waits for Buwiz"),
                Err(error) => tracing::warn!("{function}: queued, not sent: {error}"),
            }
        }
        if rule == Rule::Rows {
            return with_tickets(app, value);
        }
        Ok(value)
    }

    // ------------------------------------------------- the assistant's tools

    fn agent_tool(&mut self, app: &Arc<BirApp>, name: &str, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Value, (i64, String)> {
        let answer = match name {
            STATUS => Ok(self.status(app)),
            RESOLVE => return Ok(protocol::ok(resolve(app, arguments))),
            APP_OPEN => return Ok(app_open(app, arguments, today).unwrap_or_else(|message| tool_error(format!("{APP_OPEN}: {message}")))),
            PROFILES_LIST => no_arguments(arguments).and_then(|()| profiles_list(app)),
            DUES_LIST => dues_list(app, arguments, today),
            DRAFTS_LIST => drafts_list(app, arguments),
            FORM_OPEN => self.form_open(app, arguments, today),
            FORM_FIELDS => self.form_fields(arguments),
            FORM_FILL => self.form_fill(app, arguments),
            FORM_VALIDATE => no_arguments(arguments).and_then(|()| self.form_validate(app)),
            FORM_SAVE => no_arguments(arguments).and_then(|()| self.form_save(app)),
            FORM_SEND => no_arguments(arguments).and_then(|()| self.form_send(app)),
            FORM_DISMISS => self.form_dismiss(arguments),
            other => return Err((METHOD_NOT_FOUND, format!("unknown tool: {other}"))),
        };
        Ok(call_result(answer.map_err(|message| format!("{name}: {message}"))))
    }

    fn status(&self, app: &BirApp) -> Value {
        let custom = std::env::var_os("BUWIZ_DATABASE_PATH").is_some_and(|path| !path.is_empty());
        let mut status = json!({
            "app": SERVER_NAME,
            "mode": "headless",
            "version": env!("CARGO_PKG_VERSION"),
            "plugin_version": manifest::VERSION,
            "database": if custom { "BUWIZ_DATABASE_PATH" } else { "Buwiz (this computer's taxpayers)" },
            // false while the Buwiz app is open: it owns the database then
            "writable": owner::writable(&self.db_path),
        });
        match owner::sign_in(app) {
            Ok(Some(email)) => {
                status["signed_in"] = json!(true);
                if let Some(email) = email {
                    status["account"] = json!(email);
                }
            }
            Ok(None) => status["signed_in"] = json!(false),
            Err(error) => status["sign_in_error"] = json!(error),
        }
        if let Ok(queue) = app.list_queue() {
            status["queue"] = json!({ "waiting": queue.len() });
        }
        if let Ok(taxpayers) = app.list_taxpayers() {
            status["taxpayers"] = json!(taxpayers.iter().filter(|taxpayer| !taxpayer.archived).count());
        }
        status
    }

    fn open_form(&self) -> Result<&OpenForm, String> {
        self.form.as_ref().ok_or_else(|| format!("no return is open: open one with {FORM_OPEN}"))
    }

    fn form_open(&mut self, app: &BirApp, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Value, String> {
        reject_unknown(arguments, &["code", "tin", "year", "period"])?;
        if self.form.as_ref().is_some_and(|form| form.dirty) {
            return Err(format!(
                "form already open with unsaved edits: save it ({FORM_SAVE}) or dismiss it ({FORM_DISMISS}) first"
            ));
        }
        let code = text_arg(arguments, "code")?.ok_or("`code` is required: a BIR form code such as 2551Q")?;
        let named = taxform::parse(&code, today);
        let form = named
            .form
            .or_else(|| taxform::form_code_of(&code))
            .ok_or_else(|| format!("`{code}` names no BIR form Buwiz can open"))?;
        let tin = match text_arg(arguments, "tin")? {
            Some(tin) => taxform::tin_digits(&tin).ok_or_else(|| format!("`tin` must be a ticket from {PROFILES_LIST}"))?,
            None => named.tin.clone().ok_or_else(|| format!("`tin` is required: the taxpayer's ticket from {PROFILES_LIST}"))?,
        };
        let tin = if tin.len() == 9 { format!("{tin}00000") } else { tin };
        let year = match arguments.get("year") {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.as_u64().and_then(|y| u16::try_from(y).ok()).ok_or("`year` must be a year like 2026")?),
        };
        let (year, period) = match arguments.get("period") {
            Some(Value::Number(number)) => {
                let period = number.as_i64().ok_or("`period` must be a whole number")?;
                let year = match year {
                    Some(year) => year,
                    None => taxform::period_to_file(form, today).map(TaxPeriod::year).ok_or("`year` is required")?,
                };
                (i64::from(year), period)
            }
            Some(Value::String(text)) => {
                let period = taxform::parse(text, today)
                    .period
                    .ok_or_else(|| format!("`period` `{text}` names no period (try 2026-Q3, 2026-01 or 2025)"))?;
                open_args(period.for_form(form, today)?)
            }
            None | Some(Value::Null) => {
                let period = match (named.period, year) {
                    (Some(period), _) => period,
                    (None, Some(year)) => TaxPeriod::Year(year),
                    (None, None) => taxform::period_to_file(form, today).ok_or_else(|| format!("{form} has no period to file now"))?,
                };
                open_args(period.for_form(form, today)?)
            }
            Some(other) => return Err(format!("`period` must be a number or text (got {other})")),
        };
        let opened = app
            .open_return(&OpenReturn { tin: tin.clone(), form_code: form.to_string(), year, period, key: None })
            .map_err(|error| error.to_string())?;
        let summary = json!({
            "tin": tin,
            "form": form,
            "year": opened.year,
            "period": opened.period,
            "status": opened.status,
            "editable": opened.editable,
            "sample": opened.sample,
            "boxes": opened.values.len(),
            "errors": opened.errors.len(),
        });
        self.form = Some(OpenForm { tin, form: form.to_string(), opened, dirty: false });
        Ok(summary)
    }

    fn form_fields(&self, arguments: &Map<String, Value>) -> Result<Value, String> {
        reject_unknown(arguments, &["keys"])?;
        let form = self.open_form()?;
        let keys: Option<Vec<String>> = match arguments.get("keys") {
            None | Some(Value::Null) => None,
            Some(Value::Array(keys)) => Some(keys.iter().filter_map(Value::as_str).map(str::to_string).collect()),
            Some(_) => return Err("`keys` must be a list of box keys".into()),
        };
        let pick = |map: &BTreeMap<String, String>| -> BTreeMap<String, String> {
            map.iter()
                .filter(|(key, _)| keys.as_ref().is_none_or(|keys| keys.contains(key)))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        };
        let opened = &form.opened;
        Ok(json!({
            "tin": form.tin,
            "form": form.form,
            "status": opened.status,
            "editable": opened.editable,
            "values": pick(&opened.values),
            "locked": pick(&opened.locks),
            "off": pick(&opened.off),
            "options": opened.options.iter()
                .filter(|(key, _)| keys.as_ref().is_none_or(|keys| keys.contains(key)))
                .map(|(key, options)| (key.clone(), Value::Array(options.iter().map(|o| json!({"value": o.value, "label": o.label})).collect())))
                .collect::<Map<String, Value>>(),
        }))
    }

    fn form_fill(&mut self, app: &BirApp, arguments: &Map<String, Value>) -> Result<Value, String> {
        reject_unknown(arguments, &["fields", "source", "from_card"])?;
        let fields = arguments
            .get("fields")
            .and_then(Value::as_object)
            .filter(|fields| !fields.is_empty())
            .ok_or("`fields` must map box keys to values")?;
        let source = text_arg(arguments, "source")?.unwrap_or_else(|| "ai".into());
        if !["profile", "past_return", "document", "ai", "user"].contains(&source.as_str()) {
            return Err("`source` must be profile, past_return, document, ai or user".into());
        }
        let mut edits = BTreeMap::new();
        for (key, value) in fields {
            let edit = match value {
                Value::String(text) => FieldValue::Text(text.clone()),
                Value::Number(number) => FieldValue::Text(number.to_string()),
                Value::Bool(tick) => FieldValue::Tick(*tick),
                Value::Null => FieldValue::Clear,
                _ => return Err(format!("`{key}`: a value is text, a number, true/false or null")),
            };
            edits.insert(key.clone(), edit);
        }
        let form = self.form.as_mut().ok_or_else(|| format!("no return is open: open one with {FORM_OPEN}"))?;
        if !form.opened.editable {
            return Err(format!("this return is {}: it can no longer be edited", form.opened.status));
        }
        let filled = app.fill(&form.form, &form.opened.draft, &edits, &[]).map_err(|error| error.to_string())?;
        form.opened.draft = filled.draft;
        form.opened.values.extend(filled.values);
        form.dirty = true;
        let filled_keys: Vec<&String> = edits.keys().filter(|key| !filled.rejected.contains_key(*key)).collect();
        Ok(json!({ "filled": filled_keys, "rejected": filled.rejected, "source": source }))
    }

    fn form_validate(&mut self, app: &BirApp) -> Result<Value, String> {
        let form = self.form.as_mut().ok_or_else(|| format!("no return is open: open one with {FORM_OPEN}"))?;
        let checked = app.validate(&form.form, &form.opened.draft).map_err(|error| error.to_string())?;
        form.opened.draft = checked.draft;
        form.opened.values.extend(checked.values);
        let errors: Vec<Value> = checked.errors.iter().map(|error| json!({ "field": error.field, "message": error.message })).collect();
        form.opened.errors = checked.errors;
        Ok(json!({ "ok": errors.is_empty(), "error_count": errors.len(), "field_errors": errors }))
    }

    fn form_save(&mut self, app: &BirApp) -> Result<Value, String> {
        self.open_form()?;
        let _lock = owner::acquire(&self.db_path)?;
        self.save_open(app)
    }

    /// Save the open return (the owner lock held by the caller).
    fn save_open(&mut self, app: &BirApp) -> Result<Value, String> {
        let form = self.form.as_mut().ok_or_else(|| format!("no return is open: open one with {FORM_OPEN}"))?;
        let saved = app.save(&form.form, &form.opened.draft).map_err(|error| error.to_string())?;
        if saved.saved == Some(false) {
            return Err("a return of this period is already saved; open it with bir_form_open instead of this sample".into());
        }
        if let Some(draft) = saved.draft.clone() {
            form.opened.draft = draft;
        }
        if let Some(status) = &saved.status {
            form.opened.status = status.clone();
        }
        form.dirty = false;
        Ok(json!({ "saved": true, "id": saved.id, "status": form.opened.status }))
    }

    fn form_send(&mut self, app: &Arc<BirApp>) -> Result<Value, String> {
        let form = self.open_form()?;
        let granted = app
            .call("list_send_grants", vec![Value::String(form.tin.clone())])
            .map_err(|error| error.to_string())?
            .as_array()
            .is_some_and(|grants| grants.iter().any(|grant| grant["form"].as_str() == Some(form.form.as_str())));
        if !granted {
            return Ok(json!({
                "status": "approval_required",
                "pending": true,
                "message": "Nothing was queued: the user has not allowed the assistant to send this form for this taxpayer. They can press Submit in the Forms app themselves, or allow sending there.",
            }));
        }
        let lock = owner::acquire(&self.db_path)?;
        let checked = self.form_validate(app)?;
        if checked["ok"] != true {
            return Ok(json!({ "status": "invalid", "field_errors": checked["field_errors"] }));
        }
        let saved = self.save_open(app)?;
        let form = self.open_form()?;
        let id = app.queue_return(&form.form, &form.opened.draft).map_err(|error| error.to_string())?;
        self.form = None;
        let sending = self.sends.start(app, lock)?;
        Ok(json!({
            "status": "queued",
            "id": id,
            "saved_id": saved["id"],
            "sending": sending,
            "message": if sending {
                "Queued and going out now through Buwiz's relay."
            } else {
                "Queued. This computer is not signed in to Buwiz for sending, so it goes out once the user signs in in the Buwiz app."
            },
        }))
    }

    fn form_dismiss(&mut self, arguments: &Map<String, Value>) -> Result<Value, String> {
        reject_unknown(arguments, &["discard"])?;
        let discard = arguments.get("discard").and_then(Value::as_bool).unwrap_or(false);
        match &self.form {
            None => Ok(json!({ "dismissed": false, "message": "no return was open" })),
            Some(form) if form.dirty && !discard => Err(format!(
                "unsaved edits: save them with {FORM_SAVE}, or call {FORM_DISMISS} with `discard: true` once the user said to drop them"
            )),
            Some(_) => {
                self.form = None;
                Ok(json!({ "dismissed": true }))
            }
        }
    }
}

// ------------------------------------------------------------- helpers

fn forms_uri() -> String {
    format!("{}{}", manifest::APP_URI_PREFIX, manifest::FORMS_ID)
}

pub(crate) fn app_tool_name(function: &str) -> String {
    format!("{APP_PREFIX}{function}")
}

fn is_app_only(name: &str) -> bool {
    [TICKET_SUBJECT, TICKET_ISSUE, PRINT_PREVIEW, PAYMENTS_LIST].contains(&name)
        || name.strip_prefix(APP_PREFIX).and_then(scope::find).is_some()
}

fn read_only(name: &str) -> bool {
    READ_ONLY.contains(&name)
        || [TICKET_SUBJECT, PRINT_PREVIEW, PAYMENTS_LIST].contains(&name)
        || name
            .strip_prefix(APP_PREFIX)
            .and_then(scope::find)
            .is_some_and(|(_, _, access)| access == Access::Read)
}

fn open_args(period: TaxPeriod) -> (i64, i64) {
    let (year, period) = period.open_args();
    (i64::from(year), i64::from(period))
}

fn reject_unknown(arguments: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    match arguments.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) if allowed.is_empty() => Err(format!("this tool takes no arguments (got `{key}`)")),
        Some(key) => Err(format!("unknown argument `{key}` (allowed: {})", allowed.join(", "))),
        None => Ok(()),
    }
}

fn no_arguments(arguments: &Map<String, Value>) -> Result<(), String> {
    reject_unknown(arguments, &[])
}

fn text_arg(arguments: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(Some(text.trim().to_string())),
        Some(Value::String(_)) => Ok(None),
        Some(other) => Err(format!("`{key}` must be a string (got {other})")),
    }
}

/// A `bir_app_*` call's `{args | args_json, ticket?}`. A signed call carries
/// whole numbers only: a draft's amounts come as `args_json`, text.
fn host_arguments(arguments: &Map<String, Value>) -> Result<(Vec<Value>, Option<String>), String> {
    if let Some(key) = arguments.keys().find(|key| !["args", "args_json", "ticket"].contains(&key.as_str())) {
        return Err(format!("unknown argument `{key}` (allowed: args, args_json, ticket)"));
    }
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
    Ok((args, ticket))
}

/// A list across taxpayers with each row's ticket added (`ticket`).
fn with_tickets(app: &BirApp, rows: Value) -> Result<Value, String> {
    let Value::Array(mut rows) = rows else {
        return Ok(rows);
    };
    let subjects: Vec<Option<Subject>> = rows
        .iter()
        .map(|row| row.get("tin_root").or_else(|| row.get("tin")).and_then(Value::as_str).and_then(Subject::of_tin))
        .collect();
    let issued = tickets::issue_all(app, &subjects)?;
    for (row, ticket) in rows.iter_mut().zip(issued) {
        if let (Some(row), Some(ticket)) = (row.as_object_mut(), ticket) {
            row.insert("ticket".into(), Value::String(ticket));
        }
    }
    Ok(Value::Array(rows))
}

fn ticket_arg(arguments: &Map<String, Value>) -> Result<&str, String> {
    arguments.get("ticket").and_then(Value::as_str).ok_or_else(|| "`ticket` is required".to_string())
}

/// `bir_ticket_subject {ticket}`: what the ticket stands for, for the app.
fn ticket_subject(app: &BirApp, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Value, String> {
    let subject = tickets::subject_of(app, ticket_arg(arguments)?)?;
    let title = subject.title(app, today).ok_or("the taxpayer of this ticket is no longer saved in Buwiz")?;
    let mut out = subject.describe(today);
    out["title"] = Value::String(title);
    Ok(out)
}

/// `bir_ticket_issue {ticket, tin}`: a ticket for another branch of the
/// same taxpayer.
fn ticket_issue(app: &BirApp, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Value, String> {
    let root = tickets::subject_of(app, ticket_arg(arguments)?)?.root();
    let tin = arguments.get("tin").and_then(Value::as_str).ok_or("`tin` is required")?;
    let subject = Subject::of_tin(tin).ok_or_else(|| format!("`{tin}` is not a TIN"))?;
    if subject.root() != root {
        return Err("`tin` is another taxpayer's: a ticket reaches its own taxpayer only".into());
    }
    let title = subject.title(app, today).ok_or("no such branch is saved in Buwiz")?;
    Ok(json!({ "ticket": tickets::issue(app, &subject)?, "title": title }))
}

/// `bir_payments_list {tin, form?, period?}`: Buwiz keeps no payments yet.
fn payments_list(app: &BirApp, arguments: &Map<String, Value>) -> Result<Value, String> {
    let tin = arguments.get("tin").and_then(Value::as_str).ok_or("`tin` is required: the app's ticket")?;
    tickets::subject_of(app, tin)?;
    Ok(json!({ "payments": [] }))
}

/// `bir_app_print_preview`: the return's print sheet as the HTML document
/// the Bot previews, made in memory. `draft_id` prints a saved return as
/// stored; `year` and `quarter` a 2551Q or 1601C as stored for its period;
/// `draft_json` (+ `edits`, `unlocked`) a Form library sample as the page
/// holds it; `period` the stored return of that period. Held to the
/// ticket's taxpayer.
fn print_preview(app: &BirApp, arguments: &Map<String, Value>, today: NaiveDate) -> Value {
    let form = arguments.get("form").and_then(Value::as_str).unwrap_or_default().to_string();
    match preview_page(app, arguments, today) {
        Ok((html, title)) => {
            let slug: String = form.chars().filter(char::is_ascii_alphanumeric).collect();
            json!({
                "content": [{
                    "type": "resource",
                    "resource": { "uri": format!("bir://print/{slug}.html"), "mimeType": "text/html", "text": html },
                }],
                "structuredContent": { "title": title },
                "isError": false,
            })
        }
        Err(message) => tool_error(format!("{PRINT_PREVIEW}: {message}")),
    }
}

fn preview_page(app: &BirApp, arguments: &Map<String, Value>, today: NaiveDate) -> Result<(String, String), String> {
    const KEYS: [&str; 9] = ["ticket", "form", "draft_id", "year", "quarter", "draft_json", "edits", "unlocked", "period"];
    if let Some(key) = arguments.keys().find(|key| !KEYS.contains(&key.as_str())) {
        return Err(format!("unknown argument `{key}` (allowed: {})", KEYS.join(", ")));
    }
    let subject = tickets::subject_of(app, ticket_arg(arguments)?)?;
    let root = subject.root();
    let form = arguments.get("form").and_then(Value::as_str).ok_or("`form` is required: a BIR form code")?;
    let form = taxform::form_code_of(form).ok_or_else(|| format!("`{form}` is not a BIR form Buwiz prints"))?;
    let given = ["draft_id", "year", "draft_json", "period"].iter().filter(|key| arguments.contains_key(**key)).count();
    if given != 1 {
        return Err("pass one of `draft_id`, `year` and `quarter` (a saved return), `draft_json` (a sample) or `period`".into());
    }
    let held = |draft: &Value| -> Result<(), String> {
        scope::check_args("screen_form_print", Rule::Draft { code: 0, draft: 1 }, &[json!(form), draft.clone()], &root)
    };
    let printed = if let Some(year) = arguments.get("year") {
        let year = year.as_u64().ok_or("`year` must be a year like 2026")?;
        let quarter = arguments
            .get("quarter")
            .and_then(Value::as_u64)
            .ok_or("`quarter` (the quarter, or the month for 1601C) must be a number")?;
        if !["2551Q", "1601C"].contains(&form) {
            return Err(format!("`year` and `quarter` name a 2551Q or 1601C; {form} prints by `draft_id`"));
        }
        app.call("form_print_html", vec![json!(subject.full_tin()), json!(form), json!(year), json!(quarter)])
    } else if let Some(id) = arguments.get("draft_id") {
        let id = id.as_i64().ok_or("`draft_id` must be an integer")?;
        let draft = app
            .draft_data(id)
            .map_err(|error| error.to_string())?
            .filter(Value::is_object)
            .ok_or_else(|| format!("no saved return has id {id}"))?;
        held(&draft)?;
        app.call("screen_form_print", vec![json!(form), draft])
    } else if let Some(text) = arguments.get("draft_json") {
        let draft = text
            .as_str()
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .filter(Value::is_object)
            .ok_or("`draft_json` must be a draft object as JSON text")?;
        held(&draft)?;
        let edits = arguments.get("edits").cloned().unwrap_or_else(|| json!({}));
        if !edits.is_object() {
            return Err("`edits` must be an object of box keys to values".into());
        }
        let unlocked = arguments.get("unlocked").cloned().unwrap_or_else(|| json!([]));
        if !unlocked.as_array().is_some_and(|keys| keys.iter().all(Value::is_string)) {
            return Err("`unlocked` must be a list of box keys".into());
        }
        app.call("screen_form_print_sample", vec![json!(form), draft, edits, unlocked])
    } else {
        let text = arguments.get("period").and_then(Value::as_str).ok_or("`period` must be text like 2026-Q3")?;
        let period = taxform::parse(text, today).period.ok_or_else(|| format!("`period` `{text}` names no period"))?;
        let (year, slot) = open_args(period.for_form(form, today)?);
        if ["2551Q", "1601C"].contains(&form) {
            app.call("form_print_html", vec![json!(subject.full_tin()), json!(form), json!(year), json!(slot)])
        } else {
            let opened = app
                .open_return(&OpenReturn { tin: subject.full_tin(), form_code: form.into(), year, period: slot, key: None })
                .map_err(|error| error.to_string())?;
            app.call("screen_form_print", vec![json!(form), opened.draft.0])
        }
    }
    .map_err(|error| error.to_string())?;
    let html = printed["html"].as_str().ok_or("the print sheet came back empty")?.to_string();
    let title = printed["title"].as_str().unwrap_or(form).to_string();
    Ok((html, title))
}

/// `bir_profiles_list`: every taxpayer and its branches.
fn profiles_list(app: &BirApp) -> Result<Value, String> {
    let taxpayers = app.list_taxpayers().map_err(|error| error.to_string())?;
    Ok(Value::Array(
        taxpayers
            .iter()
            .map(|taxpayer| {
                json!({
                    "tin_root": taxpayer.tin_root,
                    "name": taxpayer.name,
                    "taxpayer_type": taxpayer.taxpayer_type,
                    "archived": taxpayer.archived,
                    "branches": taxpayer.branches.iter().map(|branch| json!({
                        "tin": branch.tin,
                        "code": branch.code,
                        "name": branch.name,
                        "head_office": branch.is_head_office,
                        "rdo_code": branch.facts.rdo_code,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect(),
    ))
}

/// The 14 or 9 digits a `tin` argument holds, for matching rows.
fn tin_filter(arguments: &Map<String, Value>) -> Result<Option<String>, String> {
    text_arg(arguments, "tin")?
        .map(|tin| taxform::tin_digits(&tin).ok_or_else(|| format!("`tin` must be a ticket from {PROFILES_LIST}")))
        .transpose()
}

fn tin_matches(filter: &Option<String>, tin: &str) -> bool {
    let tin = tickets::digits(tin);
    match filter {
        None => true,
        Some(wanted) if wanted.len() == 9 => tin.starts_with(wanted.as_str()),
        Some(wanted) => &tin == wanted,
    }
}

/// `bir_dues_list {tin?, filter?}`: returns due by Buwiz's filing calendar.
fn dues_list(app: &BirApp, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Value, String> {
    reject_unknown(arguments, &["tin", "filter"])?;
    let tin = tin_filter(arguments)?;
    let filter = text_arg(arguments, "filter")?.unwrap_or_else(|| "upcoming".into());
    let day = |days: i64| (today + chrono::Duration::days(days)).format("%Y-%m-%d").to_string();
    let (from, to) = match filter.as_str() {
        "upcoming" => (day(0), day(45)),
        "overdue" => (day(-365), day(-1)),
        "all" => (day(-365), day(45)),
        other => return Err(format!("`filter` must be upcoming, overdue or all (got `{other}`)")),
    };
    let open = |status: &str| matches!(status, "Not started" | "Draft");
    let rows = app.deadlines_between(&from, &to).map_err(|error| error.to_string())?;
    Ok(Value::Array(
        rows.into_iter()
            .filter(|row| tin_matches(&tin, &row.tin))
            .filter(|row| filter != "overdue" || open(&row.status))
            .map(|row| {
                json!({
                    "tin": row.tin,
                    "taxpayer": row.taxpayer_name,
                    "branch": row.branch_name,
                    "form": row.form_code,
                    "form_name": row.form_name,
                    "period": row.period_label,
                    "due_date": row.due_date,
                    "status": row.status,
                    "deadline": row.deadline_status,
                })
            })
            .collect(),
    ))
}

/// `bir_drafts_list {tin, form?, status?}`: a taxpayer's saved returns.
fn drafts_list(app: &BirApp, arguments: &Map<String, Value>) -> Result<Value, String> {
    reject_unknown(arguments, &["tin", "form", "status"])?;
    let tin = tin_filter(arguments)?.ok_or_else(|| format!("`tin` is required: the taxpayer's ticket from {PROFILES_LIST}"))?;
    let form = text_arg(arguments, "form")?
        .map(|form| {
            let named = taxform::parse(&form, taxform::today());
            named.form.or_else(|| taxform::form_code_of(&form)).ok_or_else(|| format!("`{form}` is not a BIR form"))
        })
        .transpose()?;
    let filter = match text_arg(arguments, "status")?.as_deref() {
        None | Some("draft") => ReturnFilter::Saved,
        Some("all") => ReturnFilter::All,
        Some(other) => return Err(format!("`status` must be draft or all (got `{other}`)")),
    };
    let rows = app.list_returns(filter).map_err(|error| error.to_string())?;
    let tin = Some(tin);
    Ok(Value::Array(
        rows.into_iter()
            .filter(|row| tin_matches(&tin, &row.tin))
            .filter(|row| form.is_none_or(|form| row.form_code == form))
            .map(|row| {
                json!({
                    "draft_id": row.id,
                    "tin": row.tin,
                    "form": row.form_code,
                    "year": row.taxable_year,
                    "period": row.period_label,
                    "status": row.status,
                    "updated_at": row.updated_at,
                })
            })
            .collect(),
    ))
}

/// `bir_app_open {app: "forms", tin?, form?, period?, action?, draft_id?}`:
/// asks the Bot to open the Forms app, on one taxpayer (a ticket) and
/// optionally one return.
fn app_open(app: &BirApp, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Value, String> {
    reject_unknown(arguments, &["app", "tin", "form", "period", "action", "draft_id"])?;
    match text_arg(arguments, "app")?.as_deref() {
        Some(manifest::FORMS_ID) => {}
        _ => return Err("`app` must be forms (the one app Buwiz ships)".into()),
    }
    let action = text_arg(arguments, "action")?
        .map(|action| {
            taxform::TaxformAction::parse(&action).ok_or_else(|| format!("`action` must be view, file or submit (got `{action}`)"))
        })
        .transpose()?;
    let draft_id = match arguments.get("draft_id") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_i64().ok_or_else(|| format!("`draft_id` must be a draft id from {DRAFTS_LIST}"))?),
    };
    let form = text_arg(arguments, "form")?;
    let period = text_arg(arguments, "period")?;
    if form.is_none() && (period.is_some() || action.is_some() || draft_id.is_some()) {
        return Err("`period`, `action` and `draft_id` need `form`".into());
    }
    let mut props = Map::new();
    let tin = tin_filter(arguments)?;
    let (subject, title) = match (tin, form) {
        (None, Some(_)) => return Err(format!("`form` needs `tin`: the taxpayer's ticket from {PROFILES_LIST}")),
        (None, None) => (None, manifest::FORMS_TITLE.to_string()),
        (Some(tin), None) => {
            let subject = Subject::of_tin(&tin).ok_or("`tin` is not a taxpayer")?;
            let name = subject.title(app, today).ok_or_else(|| format!("that taxpayer is not saved in Buwiz; list them with {PROFILES_LIST}"))?;
            (Some(subject), format!("{} · {name}", manifest::FORMS_TITLE))
        }
        (Some(tin), Some(form_text)) => {
            let named = taxform::parse(&form_text, today);
            let form = named
                .form
                .or_else(|| taxform::form_code_of(&form_text))
                .ok_or_else(|| format!("`{form_text}` is not a BIR form"))?;
            let period = match period.as_deref() {
                Some(text) => taxform::parse(text, today).period.ok_or_else(|| format!("`period` `{text}` names no period"))?,
                None => match named.period {
                    Some(period) => period,
                    None => taxform::period_to_file(form, today).ok_or_else(|| format!("{form} has no period to file now"))?,
                },
            }
            .for_form(form, today)?;
            let subject = Subject::Return(taxform::canonical_value(&tin, form, period));
            let title = subject.title(app, today).ok_or_else(|| format!("that taxpayer is not saved in Buwiz; list them with {PROFILES_LIST}"))?;
            props.insert("form".into(), json!(form));
            props.insert("period".into(), json!(period.canonical()));
            if let Some(action) = action {
                props.insert("action".into(), json!(action.as_str()));
            }
            if let Some(draft_id) = draft_id {
                props.insert("draft_id".into(), json!(draft_id));
            }
            (Some(subject), format!("BIR Form {title}"))
        }
    };
    let ticket = subject.map(|subject| tickets::issue(app, &subject)).transpose()?;
    if let Some(ticket) = &ticket {
        props.insert("ticket".into(), json!(ticket));
    }
    let mut open = json!({
        "app": manifest::FORMS_ID,
        "props": props,
        "title": title.chars().take(60).collect::<String>(),
        "placement": "full",
    });
    if let Some(ticket) = ticket {
        open["ticket"] = json!(ticket);
    }
    Ok(json!({
        "content": [{ "type": "text", "text": format!("Opening {title}…") }],
        "isError": false,
        "_meta": { "rox/open": open },
    }))
}

/// `bir_resolve` (spec 9.2) for the `taxprofile` scope: a taxpayer or
/// branch by name or TIN, as a ticket.
fn resolve(app: &BirApp, arguments: &Map<String, Value>) -> Value {
    try_resolve(app, arguments).unwrap_or_else(|message| json!({ "result": "error", "message": message.chars().take(200).collect::<String>() }))
}

fn try_resolve(app: &BirApp, arguments: &Map<String, Value>) -> Result<Value, String> {
    reject_unknown(arguments, &["kind", "name", "value", "complete", "context"])?;
    let kind = arguments.get("kind").and_then(Value::as_str).unwrap_or("scope");
    let name = arguments.get("name").and_then(Value::as_str).unwrap_or("").trim().to_ascii_lowercase();
    if kind != "scope" || name != "taxprofile" {
        return Err("Buwiz resolves the taxprofile scope only".into());
    }
    let value = arguments.get("value").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let complete = arguments.get("complete").and_then(Value::as_bool).unwrap_or(false);
    let today = taxform::today();
    if tickets::is_ticket(&value) {
        let subject = tickets::subject_of(app, &value).map_err(|_| "That taxpayer is no longer available".to_string())?;
        let title = subject.title(app, today).ok_or("That taxpayer is no longer saved in Buwiz")?;
        return Ok(json!({ "result": "token", "ticket": value, "title": title }));
    }
    let wanted_digits = tickets::digits(&value);
    let wanted_text = value.to_lowercase();
    let mut found: Vec<(Subject, String, String)> = Vec::new();
    for taxpayer in app.list_taxpayers().map_err(|error| error.to_string())? {
        if taxpayer.archived {
            continue;
        }
        let by_name = wanted_text.is_empty() || taxpayer.name.to_lowercase().contains(&wanted_text);
        for branch in &taxpayer.branches {
            let by_tin = !wanted_digits.is_empty() && tickets::digits(&branch.tin).starts_with(&wanted_digits);
            let by_branch = !wanted_text.is_empty() && branch.name.to_lowercase().contains(&wanted_text);
            if !(by_name || by_tin || by_branch) {
                continue;
            }
            let Some(subject) = Subject::of_tin(&branch.tin) else { continue };
            let detail = if branch.is_head_office { "Head office".to_string() } else { format!("Branch {} {}", branch.code, branch.name).trim().to_string() };
            found.push((subject, taxpayer.name.clone(), detail));
        }
    }
    if !complete && found.len() == 1 {
        let (subject, ..) = &found[0];
        let ticket = tickets::issue(app, subject)?;
        let title = subject.title(app, today).unwrap_or_default();
        return Ok(json!({ "result": "token", "ticket": ticket, "title": title }));
    }
    if found.is_empty() && !complete {
        return Err(format!("No taxpayer in Buwiz matches “{}”", value.chars().take(60).collect::<String>()));
    }
    found.truncate(20);
    let subjects: Vec<Option<Subject>> = found.iter().map(|(subject, ..)| Some(subject.clone())).collect();
    let issued = tickets::issue_all(app, &subjects)?;
    let choices: Vec<Value> = found
        .iter()
        .zip(issued)
        .map(|((_, label, detail), ticket)| {
            json!({
                "label": label.chars().take(80).collect::<String>(),
                "detail": detail.chars().take(160).collect::<String>(),
                "value": ticket.unwrap_or_default(),
            })
        })
        .collect();
    Ok(json!({ "result": "choices", "choices": choices }))
}

fn call_result(result: Result<Value, String>) -> Value {
    match result {
        Ok(value) => json!({
            "content": [{ "type": "text", "text": value.to_string() }],
            "structuredContent": { "result": value },
            "isError": false,
        }),
        Err(message) => tool_error(message),
    }
}

fn tool_error(message: String) -> Value {
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn initialize(params: &Value) -> Value {
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|version| SUPPORTED_PROTOCOLS.contains(version))
        .unwrap_or(LATEST_PROTOCOL);
    json!({
        "protocolVersion": version,
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "listChanged": false, "subscribe": false },
        },
        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

fn resource_list() -> Value {
    json!({ "resources": [
        {
            "uri": forms_uri(),
            "name": manifest::FORMS_ID,
            "title": manifest::FORMS_TITLE,
            "description": manifest::FORMS_DESCRIPTION,
            "mimeType": manifest::APP_MIME,
        },
        {
            "uri": manifest::MANIFEST_URI,
            "name": "manifest",
            "title": "BIR plugin manifest",
            "description": "The Mini App Protocol v1 manifest: identity, the taxprofile scope and the Forms app.",
            "mimeType": manifest::MANIFEST_MIME,
        },
    ] })
}

fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value {
    json!({ "name": name, "description": description, "inputSchema": schema, "annotations": { "readOnlyHint": read_only } })
}

fn app_only(name: &str, description: &str, schema: Value, read_only: bool) -> Value {
    let mut tool = tool(name, &format!("Mini apps only (refused for the assistant): {description}"), schema, read_only);
    tool["_meta"] = json!({ "rox/audience": ["app"] });
    tool
}

fn empty_schema() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn tool_list() -> Value {
    let ticket = json!({ "type": "string", "description": "The taxpayer's ticket (tkt_...) from bir_profiles_list or an @bir:taxprofile token." });
    let mut tools = vec![
        tool(STATUS, "Report this Buwiz host: headless mode, whether the Buwiz app is open (then `writable` is false and saving or queueing happens in Buwiz), whether this computer is signed in to Buwiz for sending, how many taxpayers are saved and how many returns wait in the sending queue. Use it for \"is Buwiz working?\".", empty_schema(), true),
        tool(PROFILES_LIST, "List the taxpayers saved in Buwiz with their branches; each comes back as a ticket. Pass a ticket as `tin` to the other tools.", empty_schema(), true),
        tool(DUES_LIST, "List returns due by Buwiz's filing calendar, with their status. `filter`: upcoming (default, the next 45 days), overdue (past due and not filed) or all. `tin` limits it to one taxpayer.", json!({
            "type": "object",
            "properties": { "tin": ticket, "filter": { "type": "string", "enum": ["upcoming", "overdue", "all"] } },
            "additionalProperties": false
        }), true),
        tool(DRAFTS_LIST, "List a taxpayer's saved returns: drafts by default, or every saved and filed return with `status: all`. `form` limits it to one form.", json!({
            "type": "object",
            "properties": { "tin": ticket, "form": { "type": "string" }, "status": { "type": "string", "enum": ["draft", "all"] } },
            "required": ["tin"],
            "additionalProperties": false
        }), true),
        tool(APP_OPEN, "Ask the Bot to open the Forms app for the user (it shows an Open button): `app: \"forms\"`, the taxpayer's ticket as `tin`, and `form` (+ `period`, `action` view|file|submit, `draft_id`) to open one return.", json!({
            "type": "object",
            "properties": {
                "app": { "type": "string", "enum": ["forms"] },
                "tin": ticket,
                "form": { "type": "string", "description": "A BIR form code such as 2551Q, or a return's ticket." },
                "period": { "type": "string", "description": "2026-Q3, 2026-01, 2025 or 2026-10-26; omitted: the period to file now." },
                "action": { "type": "string", "enum": ["view", "file", "submit"] },
                "draft_id": { "type": "integer" }
            },
            "required": ["app"],
            "additionalProperties": false
        }), false),
        tool(FORM_OPEN, "Open a return for one taxpayer and period, as Buwiz's form page opens it (prefilled from the taxpayer profile; the saved return if there is one). `code`: a form code (1601C, 2551Q, 1701Q, ...) or a return's ticket. `period`: month 1-12, quarter 1-4, 0 for annual, or text (2026-Q3, 2026-01, 2025); omitted, the return to file now. One return is open at a time.", json!({
            "type": "object",
            "properties": {
                "code": { "type": "string" },
                "tin": ticket,
                "year": { "type": "integer", "minimum": 2000, "maximum": 2100 },
                "period": { "type": ["integer", "string"] }
            },
            "required": ["code"],
            "additionalProperties": false
        }), false),
        tool(FORM_FIELDS, "Read the open return's boxes: values by official key, the locked boxes (with why), boxes switched off by another answer, and each drop-down's options. `keys` limits it to some boxes.", json!({
            "type": "object",
            "properties": { "keys": { "type": "array", "items": { "type": "string" } } },
            "additionalProperties": false
        }), true),
        tool(FORM_FILL, "Fill boxes of the open return: `fields` maps official box keys (from bir_form_fields) to values (amounts like \"1234.50\", dates MM/DD/YYYY, checkboxes true/false, null clears). Buwiz recomputes the return; refused boxes come back in `rejected` with why. `source`: profile, past_return, document, ai (default) or user. Nothing is saved.", json!({
            "type": "object",
            "properties": {
                "fields": { "type": "object", "additionalProperties": { "type": ["string", "number", "boolean", "null"] }, "minProperties": 1 },
                "source": { "type": "string", "enum": ["profile", "past_return", "document", "ai", "user"] },
                "from_card": { "type": "boolean" }
            },
            "required": ["fields"],
            "additionalProperties": false
        }), false),
        tool(FORM_VALIDATE, "Validate the open return with the form's official rules, as the page's Validate does: `ok`, `error_count` and `field_errors` ({field, message}). Explain each error in plain words.", empty_schema(), true),
        tool(FORM_SAVE, "Save the open return as a draft in Buwiz. It never queues or files anything. Refused while the Buwiz app is open (it owns the database then).", empty_schema(), false),
        tool(FORM_SEND, "Queue the open return for filing, after validating and saving it, and send it through Buwiz's relay when this computer is signed in. Needs the user's standing OK for this taxpayer and form (given in the Forms app); without it the result is approval_required and nothing is queued. Refused while the Buwiz app is open.", empty_schema(), false),
        tool(FORM_DISMISS, "Close the open return without saving. Refused when it has unsaved edits unless `discard` is true, which you set only after the user said to drop them.", json!({
            "type": "object",
            "properties": { "discard": { "type": "boolean" } },
            "additionalProperties": false
        }), false),
        tool(RESOLVE, "The Bot's resolver for the manifest's taxprofile scope (a taxpayer by name or TIN, as a ticket). Do not call it yourself.", json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string" }, "name": { "type": "string" }, "value": { "type": "string" },
                "complete": { "type": "boolean" }, "context": { "type": "object" }
            },
            "additionalProperties": false
        }), true),
        app_only(TICKET_SUBJECT, "the taxpayer, branch or return a ticket the app holds stands for, so the app can show it.", json!({
            "type": "object", "properties": { "ticket": { "type": "string" } }, "required": ["ticket"], "additionalProperties": false
        }), true),
        app_only(TICKET_ISSUE, "a ticket for another branch of the taxpayer the app's ticket names.", json!({
            "type": "object", "properties": { "ticket": { "type": "string" }, "tin": { "type": "string" } }, "required": ["ticket", "tin"], "additionalProperties": false
        }), false),
        app_only(PRINT_PREVIEW, "a return's print sheet for the Bot's document view, as a static HTML document: `draft_id` (a saved return as stored), `year` and `quarter` (a 2551Q or 1601C as stored), `draft_json` with `edits` and `unlocked` (a Form library sample as the page holds it) or `period` (the stored return of that period).", json!({
            "type": "object",
            "properties": {
                "ticket": { "type": "string" }, "form": { "type": "string" }, "draft_id": { "type": "integer" },
                "year": { "type": "integer" }, "quarter": { "type": "integer" }, "draft_json": { "type": "string" },
                "edits": { "type": "object" }, "unlocked": { "type": "array", "items": { "type": "string" } }, "period": { "type": "string" }
            },
            "required": ["ticket", "form"],
            "additionalProperties": false
        }), true),
        app_only(PAYMENTS_LIST, "a return's recorded payments (Buwiz keeps none yet: always empty).", json!({
            "type": "object",
            "properties": { "tin": { "type": "string" }, "form": { "type": "string" }, "period": { "type": "string" } },
            "required": ["tin"],
            "additionalProperties": false
        }), true),
    ];
    for (function, _, access) in scope::FORMS {
        tools.push(app_only(
            &app_tool_name(function),
            &format!("Buwiz's `{function}`, held to the taxpayer of the app's ticket."),
            json!({
                "type": "object",
                "properties": {
                    "args": { "type": "array", "description": "The function's positional arguments." },
                    "args_json": { "type": "string", "description": "The same arguments as JSON text, for values a signed call can't carry (amounts with cents)." },
                    "ticket": { "type": "string", "description": "The ticket the app was opened with." }
                },
                "additionalProperties": false
            }),
            *access == Access::Read,
        ));
    }
    json!({ "tools": tools })
}
