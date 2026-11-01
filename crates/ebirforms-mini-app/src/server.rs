//! MCP for the eBIRForms mini app: JSON-RPC 2.0, one message per line.
//!
//! Every tool runs in this process over [`BirApp`], eBIRForms' typed door
//! into forms, on the mini app's own database:
//!
//! - the mini apps' calls: `ebirforms_app_<function>` (one per function an
//!   app's source calls, held to its ticket by [`crate::scope`]), the ticket
//!   helpers, the print preview and the payments list;
//! - the assistant's tools: status, taxpayers, dues, the filing calendar,
//!   drafts, opening an app, and one open return to read, fill, validate or
//!   save;
//! - `ebirforms_resolve` for the manifest's commands (`/taxprofile`,
//!   `/taxform`) and scopes (`taxprofile`, `branchcode`, `taxform`);
//! - the reserved `buwiz.*` tools ([`crate::protocol`]).
//!
//! Buwiz Bot speaks the Mini App Protocol 2.0: `buwiz.provision` first, then
//! every call carries its caller (`_meta["buwiz/caller"]`), and
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

use crate::account::Account;
use crate::cards;
use crate::manifest::{self, ManifestSource};
use crate::owner;
use crate::protocol::{self, Caller, Host, Origin};
use crate::scope::{self, Access, Rule};
use crate::scrub::{self, Scrubber};
use crate::tickets::{self, Subject};

pub(crate) const SERVER_NAME: &str = "ebirforms-mini-app";
pub(crate) const LATEST_PROTOCOL: &str = "2025-06-18";
pub(crate) const SUPPORTED_PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

// The assistant's tools.
pub(crate) const STATUS: &str = "ebirforms_status";
pub(crate) const APP_OPEN: &str = "ebirforms_app_open";
pub(crate) const RESOLVE: &str = "ebirforms_resolve";
pub(crate) const PROFILES_LIST: &str = "ebirforms_profiles_list";
pub(crate) const DUES_LIST: &str = "ebirforms_dues_list";
/// The filing calendar as the Deadlines card shows it.
pub(crate) const CALENDAR_GET: &str = "ebirforms_calendar_get";
/// A revenue district office by code or name, from eBIRForms' RDO table.
pub(crate) const RDO_LOOKUP: &str = "ebirforms_rdo_lookup";
pub(crate) const DRAFTS_LIST: &str = "ebirforms_drafts_list";
pub(crate) const FORM_OPEN: &str = "ebirforms_form_open";
pub(crate) const FORM_FIELDS: &str = "ebirforms_form_fields";
pub(crate) const FORM_FILL: &str = "ebirforms_form_fill";
pub(crate) const FORM_VALIDATE: &str = "ebirforms_form_validate";
pub(crate) const FORM_SAVE: &str = "ebirforms_form_save_draft";
pub(crate) const FORM_DISMISS: &str = "ebirforms_form_dismiss";

// The Forms app's own tools (refused for the assistant).
pub(crate) const APP_PREFIX: &str = "ebirforms_app_";
pub(crate) const TICKET_SUBJECT: &str = "ebirforms_ticket_subject";
pub(crate) const TICKET_ISSUE: &str = "ebirforms_ticket_issue";
pub(crate) const PRINT_PREVIEW: &str = "ebirforms_app_print_preview";
/// eBIRForms keeps no payments yet: the list is empty, so the Forms app's
/// payment section shows nothing paid.
pub(crate) const PAYMENTS_LIST: &str = "ebirforms_payments_list";
// The Sign-in app's tools (account.rs): who is signed in, and the device
// flow. Never the assistant's: it cannot sign the owner in or out.
pub(crate) const ACCOUNT_STATUS: &str = "ebirforms_account_status";
pub(crate) const ACCOUNT_SIGN_IN_START: &str = "ebirforms_account_sign_in_start";
pub(crate) const ACCOUNT_SIGN_IN_POLL: &str = "ebirforms_account_sign_in_poll";
pub(crate) const ACCOUNT_SIGN_OUT: &str = "ebirforms_account_sign_out";
const ACCOUNT_TOOLS: [&str; 4] = [
    ACCOUNT_STATUS,
    ACCOUNT_SIGN_IN_START,
    ACCOUNT_SIGN_IN_POLL,
    ACCOUNT_SIGN_OUT,
];

/// The assistant's tools that change nothing (`readOnlyHint`, P8): the ones
/// a routine or resolve call may run.
const READ_ONLY: [&str; 9] = [
    RDO_LOOKUP,
    STATUS,
    RESOLVE,
    PROFILES_LIST,
    DUES_LIST,
    CALENDAR_GET,
    DRAFTS_LIST,
    FORM_FIELDS,
    FORM_VALIDATE,
];

const INSTRUCTIONS: &str = "eBIRForms (Philippine BIR tax returns) running headless on this computer \
over the taxpayer database eBIRForms keeps. Taxpayers, branches and returns reach you as tickets \
(tkt_...), never as TINs: pass a ticket wherever a tool takes `tin`; a return's ticket also goes \
in ebirforms_form_open's `code` and ebirforms_app_open's `form`. Never ask the user for a TIN and never write \
one out; use the taxpayer's name. ebirforms_profiles_list lists the taxpayers, ebirforms_drafts_list a \
taxpayer's saved returns. To answer what is due or late, call ebirforms_calendar_get (`filter: overdue` \
for late returns): the user sees its card, so answer in a sentence and don't repeat the list; \
ebirforms_dues_list is the same calendar as plain rows, for your own checks. To let the user work visually, call ebirforms_app_open \
with `app: \"forms\"` and the taxpayer's ticket as `tin` (and `form`, `period` to open one \
return), `app: \"tax-profile\"` for a taxpayer's registration, `app: \"calendar\"` for the \
filing calendar, or `app: \"deadlines\"` for the next returns due as a card in the chat. You can draft a return yourself: ebirforms_form_open, ebirforms_form_fields, ebirforms_form_fill (with its \
`source`), ebirforms_form_validate, ebirforms_form_save_draft, ebirforms_form_dismiss; one return is open at a time. \
You never queue or send a return: the user does that in the Forms app (Queue), once signed in \
to Buwiz in the Sign-in app (ebirforms_app_open `register`); it is then sent to the BIR and comes back \
with a receipt. Tools whose description starts with \"Mini apps only\" and ebirforms_resolve belong \
to the mini apps and to the Bot: do not call them.";

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
    account: Account,
}

impl Server {
    /// The server the Bot spawns, over the mini app's own database
    /// ([`crate::owner`]): `database` is what [`owner::prepare`] gave. A
    /// database that cannot open leaves the protocol working and every tool
    /// saying why.
    pub fn open_own(manifest: ManifestSource, database: Result<PathBuf, String>) -> Self {
        let (app, db_path) = match database {
            Ok(path) => (
                BirApp::open_at(&path).map(Arc::new).map_err(|error| {
                    format!("the eBIRForms mini app's database did not open: {error}")
                }),
                path,
            ),
            Err(problem) => (Err(problem), PathBuf::new()),
        };
        if let Err(problem) = &app {
            tracing::warn!("{problem}");
        }
        let server = Self::new(app, db_path, manifest, true);
        // the queue: queued returns go out through Buwiz while signed in
        if let Ok(app) = &server.app
            && let Err(error) = app.start_queue_worker(server.account.transport())
        {
            tracing::warn!("the sending queue did not start: {error}");
        }
        server
    }

    /// A server over `app` (whose file is `db_path`). `require_provision`:
    /// answer no tool before `buwiz.provision`, as the Bot expects.
    pub fn with_app(
        app: Arc<BirApp>,
        db_path: PathBuf,
        manifest: ManifestSource,
        require_provision: bool,
    ) -> Self {
        Self::new(Ok(app), db_path, manifest, require_provision)
    }

    fn new(
        app: Result<Arc<BirApp>, String>,
        db_path: PathBuf,
        manifest: ManifestSource,
        require_provision: bool,
    ) -> Self {
        let dir = db_path.parent().map(PathBuf::from).unwrap_or_default();
        Self {
            app,
            db_path,
            manifest,
            host: Host::new(require_provision),
            form: None,
            account: Account::in_dir(dir),
        }
    }

    /// One line in, at most one line out.
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        if line.trim().is_empty() {
            return None;
        }
        let reply = match serde_json::from_str::<Value>(line) {
            Ok(message) => self.handle(message),
            Err(error) => Some(error_response(
                Value::Null,
                PARSE_ERROR,
                &format!("parse error: {error}"),
            )),
        };
        reply.map(|value| value.to_string())
    }

    /// One JSON-RPC message; `None` for a notification.
    pub fn handle(&mut self, message: Value) -> Option<Value> {
        let Some(object) = message.as_object() else {
            return Some(error_response(
                Value::Null,
                INVALID_REQUEST,
                "expected a single JSON-RPC object",
            ));
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
                Some(uri) if let Some(app) = uri.strip_prefix(manifest::APP_URI_PREFIX).and_then(manifest::app) => Ok(json!({
                    "contents": [{ "uri": uri, "mimeType": manifest::APP_MIME, "text": app.source }],
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
        let name = params.get("name").and_then(Value::as_str).ok_or((
            INVALID_PARAMS,
            "tools/call requires params.name".to_string(),
        ))?;
        let empty = Map::new();
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(arguments)) => arguments,
            Some(_) => return Ok(tool_error("arguments must be a JSON object".into())),
        };
        if name.starts_with("buwiz.") {
            let app = self.app.as_deref().map_err(String::as_str);
            return match self.host.reserved(&self.manifest, app, name, arguments) {
                None => Err((
                    METHOD_NOT_FOUND,
                    format!("{name} is not a tool of this plugin"),
                )),
                Some(Err(message)) => Err((INVALID_REQUEST, message)),
                Some(Ok(result)) => Ok(result),
            };
        }
        if !self.host.ready() {
            return Err((INVALID_REQUEST, "buwiz.provision must come first".into()));
        }
        let caller = Caller::of(params);
        if let Err(message) = self.authorize(name, &caller) {
            return Ok(protocol::fail("permission_denied", &message));
        }
        let app = match &self.app {
            Ok(app) => app.clone(),
            Err(message) => return Ok(tool_error(message.clone())),
        };
        if ACCOUNT_TOOLS.contains(&name) {
            return Ok(self.account_tool(name, arguments));
        }
        if is_app_only(name) {
            return Ok(self.app_tool(&app, name, arguments));
        }
        // as the caller sent them (tickets), for the card the answer carries
        let sent = arguments.clone();
        let mut arguments = arguments.clone();
        let has_ticket = Value::Object(arguments.clone())
            .to_string()
            .contains("tkt_");
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
        // the assistant's answer carries its card; an app's own read does not
        if matches!(caller.origin, Origin::Agent | Origin::Unknown) {
            cards::decorate(name, &sent, &mut result);
        }
        Ok(result)
    }

    /// Whether `caller` may call `name` at all (P1, P5, P6): app-only tools
    /// answer apps, an app calls only what its manifest entry lists and only
    /// from an open this process certified, and resolve calls and routine
    /// queries never cause side effects.
    fn authorize(&self, name: &str, caller: &Caller) -> Result<(), String> {
        if matches!(caller.origin, Origin::Resolve | Origin::Routine) && !read_only(name) {
            return Err(format!(
                "{name} changes eBIRForms data, so it does not run for a resolve or routine call"
            ));
        }
        if caller.origin != Origin::App {
            return if is_app_only(name) {
                Err(format!("{name} is for the eBIRForms mini apps only"))
            } else {
                Ok(())
            };
        }
        let app = caller.app.as_deref().ok_or("an app call names no app")?;
        if self.host.provisioned()
            && self
                .host
                .session_app(caller.open_id.as_deref().unwrap_or(""))
                != Some(app)
        {
            return Err("this app open was not certified by eBIRForms".into());
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

    // ------------------------------------------------------- the mini apps

    /// The Sign-in app's calls: no arguments, and never a token in the answer.
    fn account_tool(&mut self, name: &str, arguments: &Map<String, Value>) -> Value {
        let answer = no_arguments(arguments).and_then(|()| match name {
            ACCOUNT_STATUS => Ok(self.account.status()),
            ACCOUNT_SIGN_IN_START => self.account.start(),
            ACCOUNT_SIGN_IN_POLL => self.account.poll(),
            _ => self.account.sign_out(),
        });
        call_result(answer.map_err(|message| format!("{name}: {message}")))
    }

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

    /// One `ebirforms_app_<function>` call: arguments read, the ticket turned into
    /// the scope, the owner lock taken for a write, the answer held to the
    /// scope and a list's rows given their tickets.
    fn host_call(
        &mut self,
        app: &Arc<BirApp>,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, String> {
        let (function, rule, access) = name
            .strip_prefix(APP_PREFIX)
            .and_then(scope::find)
            .ok_or_else(|| format!("unknown tool: {name}"))?;
        let (args, ticket) = host_arguments(arguments)?;
        let root = ticket
            .as_deref()
            .map(|ticket| tickets::subject_of(app, ticket).map(|subject| subject.root()))
            .transpose()?;
        if scope::needs_ticket(rule) && root.is_none() {
            return Err(format!(
                "`{function}` names a taxpayer: pass the app's `ticket` (an app reaches a taxpayer only through a ticket eBIRForms issued)"
            ));
        }
        if let Some(root) = &root {
            scope::check_args(function, rule, &args, root)?;
        }
        if access == Access::Queue && !self.account.signed_in() {
            return Err(owner::SIGN_IN_FIRST.to_string());
        }
        let lock = match access {
            Access::Queue | Access::Write => Some(owner::acquire(&self.db_path)?),
            Access::Read | Access::Bookkeeping => None,
        };
        let value = app
            .call(function, args.clone())
            .map_err(|error| error.to_string())?;
        drop(lock);
        // a return just queued goes out now, not at the queue's next minute
        if access == Access::Queue {
            self.account.send_now();
        }
        let value = match &root {
            Some(root) => scope::filter_result(function, rule, &args, value, root)?,
            None => value,
        };
        if rule == Rule::Rows {
            return with_tickets(app, value);
        }
        Ok(value)
    }

    // ------------------------------------------------- the assistant's tools

    fn agent_tool(
        &mut self,
        app: &Arc<BirApp>,
        name: &str,
        arguments: &Map<String, Value>,
        today: NaiveDate,
    ) -> Result<Value, (i64, String)> {
        let answer = match name {
            STATUS => Ok(self.status(app)),
            RESOLVE => return Ok(protocol::ok(resolve(app, arguments))),
            APP_OPEN => {
                return Ok(app_open(app, arguments, today)
                    .unwrap_or_else(|message| tool_error(format!("{APP_OPEN}: {message}"))));
            }
            PROFILES_LIST => no_arguments(arguments).and_then(|()| profiles_list(app)),
            DUES_LIST => dues_list(app, arguments, today),
            CALENDAR_GET => calendar_get(app, arguments, today),
            RDO_LOOKUP => rdo_lookup(app, arguments),
            DRAFTS_LIST => drafts_list(app, arguments),
            FORM_OPEN => self.form_open(app, arguments, today),
            FORM_FIELDS => self.form_fields(arguments),
            FORM_FILL => self.form_fill(app, arguments),
            FORM_VALIDATE => no_arguments(arguments).and_then(|()| self.form_validate(app)),
            FORM_SAVE => no_arguments(arguments).and_then(|()| self.form_save(app)),
            FORM_DISMISS => self.form_dismiss(arguments),
            other => return Err((METHOD_NOT_FOUND, format!("unknown tool: {other}"))),
        };
        Ok(call_result(
            answer.map_err(|message| format!("{name}: {message}")),
        ))
    }

    fn status(&self, app: &BirApp) -> Value {
        let mut status = json!({
            "app": SERVER_NAME,
            "mode": "headless",
            "version": env!("CARGO_PKG_VERSION"),
            "plugin_version": manifest::VERSION,
            // the mini app's own taxpayers, not either desktop app's
            "database": "eBIRForms mini app (its own data)",
            // false only for a moment while another process of this mini app saves
            "writable": owner::writable(&self.db_path),
            // signed in to Buwiz: a return queued in the Forms app is sent
            "sending": self.account.signed_in(),
            // the Sign-in app's account
            "signed_in": self.account.signed_in(),
        });
        if let Ok(taxpayers) = app.list_taxpayers() {
            status["taxpayers"] = json!(
                taxpayers
                    .iter()
                    .filter(|taxpayer| !taxpayer.archived)
                    .count()
            );
        }
        status
    }

    fn open_form(&self) -> Result<&OpenForm, String> {
        self.form
            .as_ref()
            .ok_or_else(|| format!("no return is open: open one with {FORM_OPEN}"))
    }

    fn form_open(
        &mut self,
        app: &BirApp,
        arguments: &Map<String, Value>,
        today: NaiveDate,
    ) -> Result<Value, String> {
        reject_unknown(arguments, &["code", "tin", "year", "period"])?;
        if self.form.as_ref().is_some_and(|form| form.dirty) {
            return Err(format!(
                "form already open with unsaved edits: save it ({FORM_SAVE}) or dismiss it ({FORM_DISMISS}) first"
            ));
        }
        let code = text_arg(arguments, "code")?
            .ok_or("`code` is required: a BIR form code such as 2551Q")?;
        let named = taxform::parse(&code, today);
        let form = named
            .form
            .or_else(|| taxform::form_code_of(&code))
            .ok_or_else(|| format!("`{code}` names no BIR form eBIRForms can open"))?;
        let tin = match text_arg(arguments, "tin")? {
            Some(tin) => taxform::tin_digits(&tin)
                .ok_or_else(|| format!("`tin` must be a ticket from {PROFILES_LIST}"))?,
            None => named.tin.clone().ok_or_else(|| {
                format!("`tin` is required: the taxpayer's ticket from {PROFILES_LIST}")
            })?,
        };
        let tin = if tin.len() == 9 {
            format!("{tin}00000")
        } else {
            tin
        };
        let year = match arguments.get("year") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .and_then(|y| u16::try_from(y).ok())
                    .ok_or("`year` must be a year like 2026")?,
            ),
        };
        let (year, period) = match arguments.get("period") {
            Some(Value::Number(number)) => {
                let period = number.as_i64().ok_or("`period` must be a whole number")?;
                let year = match year {
                    Some(year) => year,
                    None => taxform::period_to_file(form, today)
                        .map(TaxPeriod::year)
                        .ok_or("`year` is required")?,
                };
                (i64::from(year), period)
            }
            Some(Value::String(text)) => {
                let period = taxform::parse(text, today).period.ok_or_else(|| {
                    format!("`period` `{text}` names no period (try 2026-Q3, 2026-01 or 2025)")
                })?;
                open_args(period.for_form(form, today)?)
            }
            None | Some(Value::Null) => {
                let period = match (named.period, year) {
                    (Some(period), _) => period,
                    (None, Some(year)) => TaxPeriod::Year(year),
                    (None, None) => taxform::period_to_file(form, today)
                        .ok_or_else(|| format!("{form} has no period to file now"))?,
                };
                open_args(period.for_form(form, today)?)
            }
            Some(other) => return Err(format!("`period` must be a number or text (got {other})")),
        };
        let opened = app
            .open_return(&OpenReturn {
                tin: tin.clone(),
                form_code: form.to_string(),
                year,
                period,
                key: None,
            })
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
        self.form = Some(OpenForm {
            tin,
            form: form.to_string(),
            opened,
            dirty: false,
        });
        Ok(summary)
    }

    fn form_fields(&self, arguments: &Map<String, Value>) -> Result<Value, String> {
        reject_unknown(arguments, &["keys"])?;
        let form = self.open_form()?;
        let keys: Option<Vec<String>> = match arguments.get("keys") {
            None | Some(Value::Null) => None,
            Some(Value::Array(keys)) => Some(
                keys.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
            ),
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
                _ => {
                    return Err(format!(
                        "`{key}`: a value is text, a number, true/false or null"
                    ));
                }
            };
            edits.insert(key.clone(), edit);
        }
        let form = self
            .form
            .as_mut()
            .ok_or_else(|| format!("no return is open: open one with {FORM_OPEN}"))?;
        if !form.opened.editable {
            return Err(format!(
                "this return is {}: it can no longer be edited",
                form.opened.status
            ));
        }
        let filled = app
            .fill(&form.form, &form.opened.draft, &edits, &[])
            .map_err(|error| error.to_string())?;
        form.opened.draft = filled.draft;
        form.opened.values.extend(filled.values);
        form.dirty = true;
        let filled_keys: Vec<&String> = edits
            .keys()
            .filter(|key| !filled.rejected.contains_key(*key))
            .collect();
        Ok(json!({ "filled": filled_keys, "rejected": filled.rejected, "source": source }))
    }

    fn form_validate(&mut self, app: &BirApp) -> Result<Value, String> {
        let form = self
            .form
            .as_mut()
            .ok_or_else(|| format!("no return is open: open one with {FORM_OPEN}"))?;
        let checked = app
            .validate(&form.form, &form.opened.draft)
            .map_err(|error| error.to_string())?;
        form.opened.draft = checked.draft;
        form.opened.values.extend(checked.values);
        let errors: Vec<Value> = checked
            .errors
            .iter()
            .map(|error| json!({ "field": error.field, "message": error.message }))
            .collect();
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
        let form = self
            .form
            .as_mut()
            .ok_or_else(|| format!("no return is open: open one with {FORM_OPEN}"))?;
        let saved = app
            .save(&form.form, &form.opened.draft)
            .map_err(|error| error.to_string())?;
        if saved.saved == Some(false) {
            return Err("a return of this period is already saved; open it with ebirforms_form_open instead of this sample".into());
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

    fn form_dismiss(&mut self, arguments: &Map<String, Value>) -> Result<Value, String> {
        reject_unknown(arguments, &["discard"])?;
        let discard = arguments
            .get("discard")
            .and_then(Value::as_bool)
            .unwrap_or(false);
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

pub(crate) fn app_tool_name(function: &str) -> String {
    format!("{APP_PREFIX}{function}")
}

fn is_app_only(name: &str) -> bool {
    [TICKET_SUBJECT, TICKET_ISSUE, PRINT_PREVIEW, PAYMENTS_LIST].contains(&name)
        || ACCOUNT_TOOLS.contains(&name)
        || name
            .strip_prefix(APP_PREFIX)
            .and_then(scope::find)
            .is_some()
}

fn read_only(name: &str) -> bool {
    READ_ONLY.contains(&name)
        || [TICKET_SUBJECT, PRINT_PREVIEW, PAYMENTS_LIST, ACCOUNT_STATUS].contains(&name)
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
    match arguments
        .keys()
        .find(|key| !allowed.contains(&key.as_str()))
    {
        Some(key) if allowed.is_empty() => {
            Err(format!("this tool takes no arguments (got `{key}`)"))
        }
        Some(key) => Err(format!(
            "unknown argument `{key}` (allowed: {})",
            allowed.join(", ")
        )),
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

/// A `ebirforms_app_*` call's `{args | args_json, ticket?}`. A signed call carries
/// whole numbers only: a draft's amounts come as `args_json`, text.
fn host_arguments(arguments: &Map<String, Value>) -> Result<(Vec<Value>, Option<String>), String> {
    if let Some(key) = arguments
        .keys()
        .find(|key| !["args", "args_json", "ticket"].contains(&key.as_str()))
    {
        return Err(format!(
            "unknown argument `{key}` (allowed: args, args_json, ticket)"
        ));
    }
    let args = match (arguments.get("args"), arguments.get("args_json")) {
        (Some(args), Some(_)) if !args.is_null() => {
            return Err("send `args` or `args_json`, not both".into());
        }
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
        .map(|row| {
            row.get("tin_root")
                .or_else(|| row.get("tin"))
                .and_then(Value::as_str)
                .and_then(Subject::of_tin)
        })
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
    arguments
        .get("ticket")
        .and_then(Value::as_str)
        .ok_or_else(|| "`ticket` is required".to_string())
}

/// `ebirforms_ticket_subject {ticket}`: what the ticket stands for, for the app.
fn ticket_subject(
    app: &BirApp,
    arguments: &Map<String, Value>,
    today: NaiveDate,
) -> Result<Value, String> {
    let subject = tickets::subject_of(app, ticket_arg(arguments)?)?;
    let title = subject
        .title(app, today)
        .ok_or("the taxpayer of this ticket is no longer saved in eBIRForms")?;
    let mut out = subject.describe(today);
    out["title"] = Value::String(title);
    Ok(out)
}

/// `ebirforms_ticket_issue {ticket, tin}`: a ticket for another branch of the
/// same taxpayer.
fn ticket_issue(
    app: &BirApp,
    arguments: &Map<String, Value>,
    today: NaiveDate,
) -> Result<Value, String> {
    let root = tickets::subject_of(app, ticket_arg(arguments)?)?.root();
    let tin = arguments
        .get("tin")
        .and_then(Value::as_str)
        .ok_or("`tin` is required")?;
    let subject = Subject::of_tin(tin).ok_or_else(|| format!("`{tin}` is not a TIN"))?;
    if subject.root() != root {
        return Err("`tin` is another taxpayer's: a ticket reaches its own taxpayer only".into());
    }
    let title = subject
        .title(app, today)
        .ok_or("no such branch is saved in eBIRForms")?;
    Ok(json!({ "ticket": tickets::issue(app, &subject)?, "title": title }))
}

/// `ebirforms_payments_list {tin, form?, period?}`: eBIRForms keeps no payments yet.
fn payments_list(app: &BirApp, arguments: &Map<String, Value>) -> Result<Value, String> {
    let tin = arguments
        .get("tin")
        .and_then(Value::as_str)
        .ok_or("`tin` is required: the app's ticket")?;
    tickets::subject_of(app, tin)?;
    Ok(json!({ "payments": [] }))
}

/// `ebirforms_app_print_preview`: the return's print sheet as the HTML document
/// the Bot previews, made in memory. `draft_id` prints a saved return as
/// stored; `year` and `quarter` a 2551Q or 1601C as stored for its period;
/// `draft_json` (+ `edits`, `unlocked`) a Form library sample as the page
/// holds it; `period` the stored return of that period. Held to the
/// ticket's taxpayer.
fn print_preview(app: &BirApp, arguments: &Map<String, Value>, today: NaiveDate) -> Value {
    let form = arguments
        .get("form")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
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

fn preview_page(
    app: &BirApp,
    arguments: &Map<String, Value>,
    today: NaiveDate,
) -> Result<(String, String), String> {
    const KEYS: [&str; 9] = [
        "ticket",
        "form",
        "draft_id",
        "year",
        "quarter",
        "draft_json",
        "edits",
        "unlocked",
        "period",
    ];
    if let Some(key) = arguments.keys().find(|key| !KEYS.contains(&key.as_str())) {
        return Err(format!(
            "unknown argument `{key}` (allowed: {})",
            KEYS.join(", ")
        ));
    }
    let subject = tickets::subject_of(app, ticket_arg(arguments)?)?;
    let root = subject.root();
    let form = arguments
        .get("form")
        .and_then(Value::as_str)
        .ok_or("`form` is required: a BIR form code")?;
    let form = taxform::form_code_of(form)
        .ok_or_else(|| format!("`{form}` is not a BIR form eBIRForms prints"))?;
    let given = ["draft_id", "year", "draft_json", "period"]
        .iter()
        .filter(|key| arguments.contains_key(**key))
        .count();
    if given != 1 {
        return Err("pass one of `draft_id`, `year` and `quarter` (a saved return), `draft_json` (a sample) or `period`".into());
    }
    let held = |draft: &Value| -> Result<(), String> {
        scope::check_args(
            "screen_form_print",
            Rule::Draft { code: 0, draft: 1 },
            &[json!(form), draft.clone()],
            &root,
        )
    };
    let printed = if let Some(year) = arguments.get("year") {
        let year = year.as_u64().ok_or("`year` must be a year like 2026")?;
        let quarter = arguments
            .get("quarter")
            .and_then(Value::as_u64)
            .ok_or("`quarter` (the quarter, or the month for 1601C) must be a number")?;
        if !["2551Q", "1601C"].contains(&form) {
            return Err(format!(
                "`year` and `quarter` name a 2551Q or 1601C; {form} prints by `draft_id`"
            ));
        }
        app.call(
            "form_print_html",
            vec![
                json!(subject.full_tin()),
                json!(form),
                json!(year),
                json!(quarter),
            ],
        )
    } else if let Some(id) = arguments.get("draft_id") {
        let id = id.as_i64().ok_or("`draft_id` must be an integer")?;
        // BirApp on eBIRForms main has no draft-by-id read yet: a second
        // connection to this mini app's own database file reads it.
        let draft = bir_app_core::bir_core::db::Database::open(app.database_path())
            .and_then(|db| db.draft_data_json(id))
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
        let unlocked = arguments
            .get("unlocked")
            .cloned()
            .unwrap_or_else(|| json!([]));
        if !unlocked
            .as_array()
            .is_some_and(|keys| keys.iter().all(Value::is_string))
        {
            return Err("`unlocked` must be a list of box keys".into());
        }
        app.call(
            "screen_form_print_sample",
            vec![json!(form), draft, edits, unlocked],
        )
    } else {
        let text = arguments
            .get("period")
            .and_then(Value::as_str)
            .ok_or("`period` must be text like 2026-Q3")?;
        let period = taxform::parse(text, today)
            .period
            .ok_or_else(|| format!("`period` `{text}` names no period"))?;
        let (year, slot) = open_args(period.for_form(form, today)?);
        if ["2551Q", "1601C"].contains(&form) {
            app.call(
                "form_print_html",
                vec![
                    json!(subject.full_tin()),
                    json!(form),
                    json!(year),
                    json!(slot),
                ],
            )
        } else {
            let opened = app
                .open_return(&OpenReturn {
                    tin: subject.full_tin(),
                    form_code: form.into(),
                    year,
                    period: slot,
                    key: None,
                })
                .map_err(|error| error.to_string())?;
            app.call("screen_form_print", vec![json!(form), opened.draft.0])
        }
    }
    .map_err(|error| error.to_string())?;
    let html = printed["html"]
        .as_str()
        .ok_or("the print sheet came back empty")?
        .to_string();
    let title = printed["title"].as_str().unwrap_or(form).to_string();
    Ok((html, title))
}

/// `ebirforms_profiles_list`: every taxpayer and its branches.
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
        .map(|tin| {
            taxform::tin_digits(&tin)
                .ok_or_else(|| format!("`tin` must be a ticket from {PROFILES_LIST}"))
        })
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

/// `ebirforms_dues_list {tin?, filter?}`: returns due by eBIRForms's filing calendar.
fn dues_list(
    app: &BirApp,
    arguments: &Map<String, Value>,
    today: NaiveDate,
) -> Result<Value, String> {
    reject_unknown(arguments, &["tin", "filter"])?;
    let tin = tin_filter(arguments)?;
    let filter = text_arg(arguments, "filter")?.unwrap_or_else(|| "upcoming".into());
    let day = |days: i64| {
        (today + chrono::Duration::days(days))
            .format("%Y-%m-%d")
            .to_string()
    };
    let (from, to) = match filter.as_str() {
        "upcoming" => (day(0), day(45)),
        "overdue" => (day(-365), day(-1)),
        "all" => (day(-365), day(45)),
        other => {
            return Err(format!(
                "`filter` must be upcoming, overdue or all (got `{other}`)"
            ));
        }
    };
    let open = |status: &str| matches!(status, "Not started" | "Draft");
    let rows = app
        .deadlines_between(&from, &to)
        .map_err(|error| error.to_string())?;
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

/// The period a calendar row is for, as its form is filed.
fn row_period(form: &str, year: i64, period: i64) -> Option<TaxPeriod> {
    let year = u16::try_from(year).ok()?;
    let index = u8::try_from(period).ok()?;
    Some(match taxform::cadence(form)? {
        taxform::Cadence::Monthly => TaxPeriod::Month { year, month: index },
        taxform::Cadence::Quarterly => TaxPeriod::Quarter {
            year,
            quarter: index,
        },
        taxform::Cadence::Annual => TaxPeriod::Year(year),
        taxform::Cadence::AsNeeded => return None,
    })
}

/// A date argument, `YYYY-MM-DD`.
fn date_arg(arguments: &Map<String, Value>, key: &str) -> Result<Option<NaiveDate>, String> {
    text_arg(arguments, key)?
        .map(|raw| {
            NaiveDate::parse_from_str(&raw, "%Y-%m-%d")
                .map_err(|_| format!("`{key}` must be a date like 2026-10-31 (got `{raw}`)"))
        })
        .transpose()
}

/// `ebirforms_calendar_get {tin?, from?, to?}`: each return due in the
/// window, with its status, as the Deadlines card lists it. A return is named
/// by its ticket (`key`), which opens it in Forms.
fn calendar_get(
    app: &BirApp,
    arguments: &Map<String, Value>,
    today: NaiveDate,
) -> Result<Value, String> {
    reject_unknown(arguments, &["tin", "from", "to", "filter"])?;
    let tin = tin_filter(arguments)?;
    let overdue_only = match text_arg(arguments, "filter")?.as_deref() {
        None => false,
        Some("overdue") => true,
        Some(other) => return Err(format!("`filter` must be overdue (got `{other}`)")),
    };
    let back = if overdue_only { 365 } else { 92 };
    let from = date_arg(arguments, "from")?.unwrap_or(today - chrono::Duration::days(back));
    let to = date_arg(arguments, "to")?.unwrap_or(today + chrono::Duration::days(183));
    if to < from {
        return Err("`to` is before `from`".into());
    }
    let rows: Vec<_> = app
        .deadlines_between(
            &from.format("%Y-%m-%d").to_string(),
            &to.format("%Y-%m-%d").to_string(),
        )
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|row| tin_matches(&tin, &row.tin))
        .filter_map(|row| {
            let form = taxform::form_code_of(&row.form_code)?;
            let period = row_period(form, row.taxable_year, row.period)?;
            Some((row, form, period))
        })
        .collect();
    let subjects: Vec<Option<Subject>> = rows
        .iter()
        .map(|(row, form, period)| {
            Some(Subject::Return(taxform::canonical_value(
                &row.tin, form, *period,
            )))
        })
        .collect();
    let keys = tickets::issue_all(app, &subjects)?;
    let events: Vec<Value> = rows
        .iter()
        .zip(keys)
        .map(|((row, form, period), key)| {
            let status = match row.status.as_str() {
                "Draft" => "draft",
                "Queued" => "queued",
                "Sent" => "filed",
                "Confirmed" => "confirmed",
                "Paid" => "paid",
                _ => "not_started",
            };
            let filed = matches!(status, "filed" | "confirmed" | "paid");
            json!({
                "key": key.unwrap_or_default(),
                "taxpayer": row.taxpayer_name,
                "form": form,
                "form_name": row.form_name,
                "period": period.canonical(),
                "label": format!("{form} · {}", period.label()),
                "due": row.due_date,
                "status": status,
                "status_detail": row.status,
                "overdue": !filed && row.due_date.as_str() < today.format("%Y-%m-%d").to_string().as_str(),
                "enabled": true,
            })
        })
        .filter(|event| !overdue_only || event["overdue"] == true)
        .collect();
    Ok(json!({
        "from": from.format("%Y-%m-%d").to_string(),
        "to": to.format("%Y-%m-%d").to_string(),
        "events": events,
    }))
}

/// `ebirforms_rdo_lookup {query}`: the revenue district offices whose code
/// or name matches, and how many saved taxpayers each one has.
fn rdo_lookup(app: &BirApp, arguments: &Map<String, Value>) -> Result<Value, String> {
    reject_unknown(arguments, &["query"])?;
    let raw = text_arg(arguments, "query")?
        .map(|query| query.trim().to_string())
        .filter(|query| !query.is_empty())
        .ok_or("`query` is required: an RDO code (43A, 043) or part of its name")?;
    // a number is padded as the table writes it (43 -> 043); 43A stays
    let code = if raw.len() <= 3 && raw.chars().all(|c| c.is_ascii_digit()) {
        format!("{raw:0>3}")
    } else {
        raw.to_ascii_uppercase()
    };
    let wanted = raw.to_lowercase();
    // a plain number also finds the offices it was split into (43 -> 043,
    // 43A East Pasig, 43B West Pasig)
    let family = |rdo: &str| {
        rdo.trim_start_matches('0')
            .trim_end_matches(|c: char| c.is_ascii_alphabetic())
            .to_string()
    };
    let number = (raw.len() <= 3 && raw.chars().all(|c| c.is_ascii_digit())).then(|| family(&raw));
    let mut matches: Vec<(String, String)> = BirApp::rdos()
        .into_iter()
        .map(|rdo| {
            let office = rdo
                .title
                .split_once(" · ")
                .map_or(String::new(), |(_, name)| name.to_string());
            (rdo.value, office)
        })
        .filter(|(rdo, office)| {
            rdo.eq_ignore_ascii_case(&code)
                || rdo.eq_ignore_ascii_case(&raw)
                || number
                    .as_deref()
                    .is_some_and(|number| family(rdo) == number)
                || office.to_lowercase().contains(&wanted)
        })
        .collect();
    // the code asked for first; a code (not a name) asked for alone finds
    // only itself and the offices split from it
    if let Some(exact) = matches
        .iter()
        .position(|(rdo, _)| rdo.eq_ignore_ascii_case(&code))
    {
        let first = matches.remove(exact);
        let wanted_family = family(&first.0);
        matches.retain(|(rdo, _)| number.is_some() && family(rdo) == wanted_family);
        matches.insert(0, first);
    }
    if matches.is_empty() {
        return Err(format!(
            "no RDO matches `{}`",
            raw.chars().take(40).collect::<String>()
        ));
    }
    matches.truncate(20);
    let taxpayers = app.list_taxpayers().map_err(|error| error.to_string())?;
    let registered = |rdo: &str| {
        taxpayers
            .iter()
            .filter(|taxpayer| !taxpayer.archived)
            .flat_map(|taxpayer| &taxpayer.branches)
            .filter(|branch| branch.facts.rdo_code.eq_ignore_ascii_case(rdo))
            .count()
    };
    Ok(json!({
        "mode": "rdo",
        "matches": matches.iter().map(|(rdo, office)| {
            let mut out = json!({ "code": rdo, "office": office, "taxpayers": registered(rdo) });
            // an old code with no name of its own: the offices split from it
            let split: Vec<String> = matches
                .iter()
                .filter(|(other, name)| office.is_empty() && other != rdo && !name.is_empty() && family(other) == family(rdo))
                .map(|(other, name)| format!("{other} {name}"))
                .collect();
            if !split.is_empty() {
                out["split_into"] = json!(split);
            }
            out
        }).collect::<Vec<_>>(),
    }))
}

/// `ebirforms_drafts_list {tin, form?, status?}`: a taxpayer's saved returns.
fn drafts_list(app: &BirApp, arguments: &Map<String, Value>) -> Result<Value, String> {
    reject_unknown(arguments, &["tin", "form", "status"])?;
    let tin = tin_filter(arguments)?
        .ok_or_else(|| format!("`tin` is required: the taxpayer's ticket from {PROFILES_LIST}"))?;
    let form = text_arg(arguments, "form")?
        .map(|form| {
            let named = taxform::parse(&form, taxform::today());
            named
                .form
                .or_else(|| taxform::form_code_of(&form))
                .ok_or_else(|| format!("`{form}` is not a BIR form"))
        })
        .transpose()?;
    let filter = match text_arg(arguments, "status")?.as_deref() {
        None | Some("draft") => ReturnFilter::Saved,
        Some("all") => ReturnFilter::All,
        Some(other) => return Err(format!("`status` must be draft or all (got `{other}`)")),
    };
    let rows = app
        .list_returns(filter)
        .map_err(|error| error.to_string())?;
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

/// `ebirforms_app_open {app, tin?, form?, period?, action?, draft_id?}`:
/// asks the Bot to open one of this plugin's apps, on one taxpayer (a
/// ticket) and, for Forms, optionally one return.
fn app_open(
    app: &BirApp,
    arguments: &Map<String, Value>,
    today: NaiveDate,
) -> Result<Value, String> {
    reject_unknown(
        arguments,
        &["app", "tin", "form", "period", "action", "draft_id"],
    )?;
    let target = text_arg(arguments, "app")?.unwrap_or_default();
    let action = text_arg(arguments, "action")?
        .map(|action| {
            taxform::TaxformAction::parse(&action)
                .ok_or_else(|| format!("`action` must be view, file or submit (got `{action}`)"))
        })
        .transpose()?;
    let draft_id = match arguments.get("draft_id") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_i64()
                .ok_or_else(|| format!("`draft_id` must be a draft id from {DRAFTS_LIST}"))?,
        ),
    };
    let form = text_arg(arguments, "form")?;
    let period = text_arg(arguments, "period")?;
    let tin = tin_filter(arguments)?;
    let open = match target.as_str() {
        manifest::FORMS_ID => {
            if form.is_none() && (period.is_some() || action.is_some() || draft_id.is_some()) {
                return Err("`period`, `action` and `draft_id` need `form`".into());
            }
            let ret = ReturnAsk {
                form,
                period,
                action,
                draft_id,
            };
            forms_open(app, tin.as_deref(), &ret, today)?
        }
        manifest::REGISTER_ID => {
            if tin.is_some()
                || form.is_some()
                || period.is_some()
                || action.is_some()
                || draft_id.is_some()
            {
                return Err("the Buwiz Sign-in app takes no taxpayer or return".into());
            }
            app_request(app, &target, None, today)?
        }
        manifest::TAX_PROFILE_ID | manifest::CALENDAR_ID | manifest::DEADLINES_ID => {
            if form.is_some() || period.is_some() || action.is_some() || draft_id.is_some() {
                return Err(format!(
                    "`form`, `period`, `action` and `draft_id` are for forms, not {target}"
                ));
            }
            let subject = tin
                .map(|tin| Subject::of_tin(&tin).ok_or("`tin` is not a taxpayer"))
                .transpose()?;
            app_request(app, &target, subject.as_ref(), today)?
        }
        _ => {
            return Err("`app` must be forms, tax-profile, calendar, deadlines or register".into());
        }
    };
    let title = open["title"].as_str().unwrap_or_default().to_string();
    Ok(json!({
        "content": [{ "type": "text", "text": format!("Opening {title}…") }],
        "isError": false,
        "_meta": { "buwiz/open": open },
    }))
}

/// What a request names of one return: form, period, action, draft.
#[derive(Default)]
struct ReturnAsk {
    form: Option<String>,
    period: Option<String>,
    action: Option<taxform::TaxformAction>,
    draft_id: Option<i64>,
}

/// An open request (spec 6.6) for a taxpayer app: `tax-profile` needs the
/// taxpayer; `calendar` and `deadlines` without one show every taxpayer.
fn app_request(
    app: &BirApp,
    target: &str,
    subject: Option<&Subject>,
    today: NaiveDate,
) -> Result<Value, String> {
    let entry = manifest::app(target).ok_or_else(|| format!("{target} is not an app"))?;
    let (ticket, title) = match subject {
        None if target == manifest::TAX_PROFILE_ID => {
            return Err(format!(
                "`tin` is required: the taxpayer's ticket from {PROFILES_LIST}"
            ));
        }
        None => (None, entry.title.to_string()),
        Some(subject) => {
            let name = subject.title(app, today).ok_or_else(|| {
                format!("that taxpayer is not saved in eBIRForms; list them with {PROFILES_LIST}")
            })?;
            (
                Some(tickets::issue(app, subject)?),
                format!("{} · {name}", entry.title),
            )
        }
    };
    let mut open = json!({
        "app": target,
        "props": {},
        "title": title.chars().take(60).collect::<String>(),
        "placement": if target == manifest::DEADLINES_ID { "inline" } else { "full" },
    });
    if let Some(ticket) = ticket {
        open["props"]["ticket"] = json!(ticket);
        open["ticket"] = json!(ticket);
    }
    Ok(open)
}

/// An open request for the Forms app: on the taxpayer `tin` (its digits),
/// and on one return when `ret` names a form.
fn forms_open(
    app: &BirApp,
    tin: Option<&str>,
    ret: &ReturnAsk,
    today: NaiveDate,
) -> Result<Value, String> {
    let mut props = Map::new();
    let (subject, title) = match (tin, &ret.form) {
        (None, Some(_)) => {
            return Err(format!(
                "`form` needs `tin`: the taxpayer's ticket from {PROFILES_LIST}"
            ));
        }
        (None, None) => (None, manifest::FORMS_TITLE.to_string()),
        (Some(tin), None) => {
            let subject = Subject::of_tin(tin).ok_or("`tin` is not a taxpayer")?;
            let name = subject.title(app, today).ok_or_else(|| {
                format!("that taxpayer is not saved in eBIRForms; list them with {PROFILES_LIST}")
            })?;
            (Some(subject), format!("{} · {name}", manifest::FORMS_TITLE))
        }
        (Some(tin), Some(form_text)) => {
            let named = taxform::parse(form_text, today);
            let form = named
                .form
                .or_else(|| taxform::form_code_of(form_text))
                .ok_or_else(|| format!("`{form_text}` is not a BIR form"))?;
            let period = match ret.period.as_deref() {
                Some(text) => taxform::parse(text, today)
                    .period
                    .ok_or_else(|| format!("`period` `{text}` names no period"))?,
                None => match named.period {
                    Some(period) => period,
                    None => taxform::period_to_file(form, today)
                        .ok_or_else(|| format!("{form} has no period to file now"))?,
                },
            }
            .for_form(form, today)?;
            let subject = Subject::Return(taxform::canonical_value(tin, form, period));
            let title = subject.title(app, today).ok_or_else(|| {
                format!("that taxpayer is not saved in eBIRForms; list them with {PROFILES_LIST}")
            })?;
            props.insert("form".into(), json!(form));
            props.insert("period".into(), json!(period.canonical()));
            if let Some(action) = ret.action {
                props.insert("action".into(), json!(action.as_str()));
            }
            if let Some(draft_id) = ret.draft_id {
                props.insert("draft_id".into(), json!(draft_id));
            }
            (Some(subject), format!("BIR Form {title}"))
        }
    };
    let ticket = subject
        .map(|subject| tickets::issue(app, &subject))
        .transpose()?;
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
    Ok(open)
}

/// `ebirforms_resolve` (spec 9.2): the `/taxprofile` and `/taxform`
/// commands, and the `taxprofile`, `branchcode` and `taxform` scopes.
fn resolve(app: &BirApp, arguments: &Map<String, Value>) -> Value {
    try_resolve(app, arguments).unwrap_or_else(|message| json!({ "result": "error", "message": message.chars().take(200).collect::<String>() }))
}

/// What one resolve call asks.
struct Ask {
    value: String,
    complete: bool,
    /// The tickets of this plugin's tokens attached to the chat.
    attached: Vec<String>,
    today: NaiveDate,
}

fn try_resolve(app: &BirApp, arguments: &Map<String, Value>) -> Result<Value, String> {
    reject_unknown(arguments, &["kind", "name", "value", "complete", "context"])?;
    let kind = arguments
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("scope");
    let name = arguments
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let prefix = format!("@{}:", manifest::SHORT);
    let attached = arguments
        .get("context")
        .and_then(|context| context.get("attached"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("token").and_then(Value::as_str))
        .filter(|token| token.starts_with(&prefix))
        .filter_map(|token| token.rsplit(':').next())
        .filter(|ticket| tickets::is_ticket(ticket))
        .map(str::to_string)
        .collect();
    let ask = Ask {
        value: arguments
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
        complete: arguments
            .get("complete")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        attached,
        today: taxform::today(),
    };
    match (kind, name.as_str()) {
        ("scope", "taxprofile") => scope_taxpayer(app, &ask, false),
        ("scope", "branchcode") => scope_taxpayer(app, &ask, true),
        ("scope", "taxform") => scope_taxform(app, &ask),
        ("command", "taxprofile") => command_taxprofile(app, &ask),
        ("command", "taxform") => command_taxform(app, &ask),
        _ => Err(format!("eBIRForms has no {kind} {name}")),
    }
}

/// The taxpayers (or with `branches`, the branches other than head offices)
/// whose name, branch name or TIN matches `value`: (subject, label, detail).
fn find_taxpayers(
    app: &BirApp,
    value: &str,
    branches: bool,
) -> Result<Vec<(Subject, String, String)>, String> {
    let wanted_digits = tickets::digits(value);
    let wanted_text = value.to_lowercase();
    let mut found = Vec::new();
    for taxpayer in app.list_taxpayers().map_err(|error| error.to_string())? {
        if taxpayer.archived {
            continue;
        }
        let by_name = wanted_text.is_empty() || taxpayer.name.to_lowercase().contains(&wanted_text);
        for branch in &taxpayer.branches {
            if branches && branch.is_head_office {
                continue;
            }
            let by_tin = !wanted_digits.is_empty()
                && tickets::digits(&branch.tin).starts_with(&wanted_digits);
            let by_branch =
                !wanted_text.is_empty() && branch.name.to_lowercase().contains(&wanted_text);
            if !(by_name || by_tin || by_branch) {
                continue;
            }
            let Some(subject) = Subject::of_tin(&branch.tin) else {
                continue;
            };
            let detail = if branch.is_head_office {
                "Head office".to_string()
            } else {
                format!("Branch {} {}", branch.code, branch.name)
                    .trim()
                    .to_string()
            };
            found.push((subject, taxpayer.name.clone(), detail));
        }
    }
    found.truncate(20);
    Ok(found)
}

/// Choices whose value is each subject's ticket, after `prefix`, before
/// `suffix` (a command's other words).
fn choices_of(
    app: &BirApp,
    found: &[(Subject, String, String)],
    suffix: &str,
) -> Result<Value, String> {
    let subjects: Vec<Option<Subject>> = found
        .iter()
        .map(|(subject, ..)| Some(subject.clone()))
        .collect();
    let issued = tickets::issue_all(app, &subjects)?;
    let choices: Vec<Value> = found
        .iter()
        .zip(issued)
        .map(|((_, label, detail), ticket)| {
            let value = format!("{} {suffix}", ticket.unwrap_or_default());
            json!({
                "label": label.chars().take(80).collect::<String>(),
                "detail": detail.chars().take(160).collect::<String>(),
                "value": value.trim(),
            })
        })
        .collect();
    Ok(json!({ "result": "choices", "choices": choices }))
}

/// A ticket's subject and live title, refused when it no longer resolves.
fn ticket_subject_title(
    app: &BirApp,
    ticket: &str,
    today: NaiveDate,
) -> Result<(Subject, String), String> {
    let subject = tickets::subject_of(app, ticket)
        .map_err(|_| "That taxpayer is no longer available".to_string())?;
    let title = subject
        .title(app, today)
        .ok_or("That taxpayer is no longer saved in eBIRForms")?;
    Ok((subject, title))
}

/// The `taxprofile` and `branchcode` scopes: a taxpayer or branch by name
/// or TIN, as a ticket.
fn scope_taxpayer(app: &BirApp, ask: &Ask, branches: bool) -> Result<Value, String> {
    if tickets::is_ticket(&ask.value) {
        let (_, title) = ticket_subject_title(app, &ask.value, ask.today)?;
        return Ok(json!({ "result": "token", "ticket": ask.value, "title": title }));
    }
    let found = find_taxpayers(app, &ask.value, branches)?;
    if !ask.complete && found.len() == 1 {
        let subject = &found[0].0;
        let ticket = tickets::issue(app, subject)?;
        let title = subject.title(app, ask.today).unwrap_or_default();
        return Ok(json!({ "result": "token", "ticket": ticket, "title": title }));
    }
    if found.is_empty() && !ask.complete {
        let what = if branches { "branch" } else { "taxpayer" };
        return Err(format!(
            "No {what} in eBIRForms matches “{}”",
            ask.value.chars().take(60).collect::<String>()
        ));
    }
    choices_of(app, &found, "")
}

/// A return's subject: `<ticket>/<FORM>-<period>` (the Forms app's Ask the
/// assistant) or `<FORM> <period>` for the attached taxpayer.
fn return_subject(app: &BirApp, ask: &Ask) -> Result<Subject, String> {
    let (ticket, rest) = match ask.value.split_once('/') {
        Some((head, rest)) if tickets::is_ticket(head) => (Some(head.to_string()), rest),
        _ => (ask.attached.first().cloned(), ask.value.as_str()),
    };
    let ticket = ticket.ok_or("Name the taxpayer first: attach @ebirforms:taxprofile")?;
    let (subject, _) = ticket_subject_title(app, &ticket, ask.today)?;
    let named = taxform::parse(rest, ask.today);
    let form = named.form.ok_or_else(|| {
        format!(
            "“{}” names no BIR form",
            rest.chars().take(60).collect::<String>()
        )
    })?;
    let period = named
        .period
        .or_else(|| taxform::period_to_file(form, ask.today))
        .ok_or_else(|| format!("{form} has no period to file now"))?
        .for_form(form, ask.today)?;
    Ok(Subject::Return(taxform::canonical_value(
        &subject.tin(),
        form,
        period,
    )))
}

/// The `taxform` scope: one return, as a ticket.
fn scope_taxform(app: &BirApp, ask: &Ask) -> Result<Value, String> {
    if tickets::is_ticket(&ask.value) {
        let (subject, title) = ticket_subject_title(app, &ask.value, ask.today)?;
        if subject.return_value().is_none() {
            return Err("That ticket is a taxpayer, not a return".into());
        }
        return Ok(json!({ "result": "token", "ticket": ask.value, "title": title }));
    }
    if ask.complete {
        return Ok(json!({ "result": "choices", "choices": [] }));
    }
    let subject = return_subject(app, ask)?;
    let title = subject
        .title(app, ask.today)
        .ok_or("That taxpayer is no longer saved in eBIRForms")?;
    let ticket = tickets::issue(app, &subject)?;
    Ok(json!({ "result": "token", "ticket": ticket, "title": title }))
}

/// `/taxprofile [tin or name]`: opens that taxpayer's Tax Profile, or offers
/// the taxpayers that match.
fn command_taxprofile(app: &BirApp, ask: &Ask) -> Result<Value, String> {
    let subject = if tickets::is_ticket(&ask.value) {
        Some(ticket_subject_title(app, &ask.value, ask.today)?.0)
    } else if ask.value.is_empty() && ask.attached.len() == 1 && !ask.complete {
        Some(ticket_subject_title(app, &ask.attached[0], ask.today)?.0)
    } else {
        let found = find_taxpayers(app, &ask.value, false)?;
        if !ask.complete && found.len() == 1 {
            Some(found[0].0.clone())
        } else if found.is_empty() && !ask.complete {
            return Err(if ask.value.is_empty() {
                "No taxpayer is saved in eBIRForms yet".to_string()
            } else {
                format!(
                    "No taxpayer in eBIRForms matches “{}”",
                    ask.value.chars().take(60).collect::<String>()
                )
            });
        } else {
            return choices_of(app, &found, "");
        }
    };
    let subject = subject.expect("a subject");
    let mut open = app_request(app, manifest::TAX_PROFILE_ID, Some(&subject), ask.today)?;
    open["result"] = json!("open");
    Ok(open)
}

/// `/taxform [form] [period] [view|file|submit]`: opens Forms on that
/// return for the taxpayer named first (a ticket), attached to the chat, or
/// the only one saved; otherwise offers the taxpayers.
fn command_taxform(app: &BirApp, ask: &Ask) -> Result<Value, String> {
    let (head, tail) = ask
        .value
        .split_once(char::is_whitespace)
        .unwrap_or((ask.value.as_str(), ""));
    // a return's ticket (a taxform token's value) opens that return
    if tickets::is_ticket(head) {
        let (subject, _) = ticket_subject_title(app, head, ask.today)?;
        if let Some(value) = subject.return_value() {
            let named = taxform::parse(value, ask.today);
            let words = taxform::parse(tail, ask.today);
            let ret = ReturnAsk {
                form: named.form.map(str::to_string),
                period: named.period.map(TaxPeriod::canonical),
                action: words.action,
                draft_id: None,
            };
            return open_result(forms_open(app, Some(&subject.tin()), &ret, ask.today)?);
        }
    }
    let (subject, rest) = if tickets::is_ticket(head) {
        (
            Some(ticket_subject_title(app, head, ask.today)?.0),
            tail.trim(),
        )
    } else {
        let only = match ask.attached.first() {
            Some(ticket) => Some(ticket_subject_title(app, ticket, ask.today)?.0),
            None => {
                let all = find_taxpayers(app, "", false)?;
                let heads: Vec<_> = all
                    .into_iter()
                    .filter(|(subject, ..)| matches!(subject, Subject::Taxpayer(_)))
                    .collect();
                (heads.len() == 1).then(|| heads[0].0.clone())
            }
        };
        (only, ask.value.as_str())
    };
    let named = taxform::parse(rest, ask.today);
    let subject = match (subject, &named.tin) {
        (_, Some(tin)) => Subject::of_tin(tin).ok_or("That TIN is not a taxpayer")?,
        (Some(subject), None) => subject,
        (None, None) => {
            // which taxpayer: the choice keeps the form and period typed
            let found = find_taxpayers(app, "", false)?;
            if found.is_empty() {
                return Err("No taxpayer is saved in eBIRForms yet".into());
            }
            return choices_of(app, &found, rest);
        }
    };
    if ask.complete {
        return Ok(json!({ "result": "choices", "choices": [] }));
    }
    if !named.unknown.is_empty() && named.form.is_none() {
        return Err(format!(
            "“{}” names no BIR form",
            named.unknown.join(" ").chars().take(60).collect::<String>()
        ));
    }
    let ret = ReturnAsk {
        form: named.form.map(str::to_string),
        period: named.period.map(TaxPeriod::canonical),
        action: named.action,
        draft_id: None,
    };
    open_result(forms_open(app, Some(&subject.tin()), &ret, ask.today)?)
}

/// An open request as a resolve's `open` result.
fn open_result(mut open: Value) -> Result<Value, String> {
    open["result"] = json!("open");
    Ok(open)
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
    let mut resources: Vec<Value> = manifest::APPS
        .iter()
        .map(|app| {
            json!({
                "uri": format!("{}{}", manifest::APP_URI_PREFIX, app.id),
                "name": app.id,
                "title": app.title,
                "description": app.description,
                "mimeType": manifest::APP_MIME,
            })
        })
        .collect();
    resources.push(json!({
        "uri": manifest::MANIFEST_URI,
        "name": "manifest",
        "title": "BIR plugin manifest",
        "description": "The Mini App Protocol 2.0 manifest: identity, commands, scopes and the apps.",
        "mimeType": manifest::MANIFEST_MIME,
    }));
    json!({ "resources": resources })
}

fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> Value {
    json!({ "name": name, "description": description, "inputSchema": schema, "annotations": { "readOnlyHint": read_only } })
}

fn app_only(name: &str, description: &str, schema: Value, read_only: bool) -> Value {
    let mut tool = tool(
        name,
        &format!("Mini apps only (refused for the assistant): {description}"),
        schema,
        read_only,
    );
    tool["_meta"] = json!({ "buwiz/audience": ["app"] });
    tool
}

fn empty_schema() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn tool_list() -> Value {
    let ticket = json!({ "type": "string", "description": "The taxpayer's ticket (tkt_...) from ebirforms_profiles_list or an @bir:taxprofile token." });
    let mut tools = vec![
        tool(
            STATUS,
            "Report the eBIRForms mini app: its version, that it keeps its own data, whether saving works right now, whether this computer is signed in to Buwiz (and so can send), and how many taxpayers are saved. Use it for \"is eBIRForms working?\".",
            empty_schema(),
            true,
        ),
        tool(
            PROFILES_LIST,
            "List the taxpayers saved in eBIRForms with their branches; each comes back as a ticket. Pass a ticket as `tin` to the other tools.",
            empty_schema(),
            true,
        ),
        tool(
            DUES_LIST,
            "The filing calendar as plain rows, for your own checks (no card): returns due with their status. `filter`: upcoming (default, the next 45 days), overdue (past due and not filed) or all. `tin` limits it to one taxpayer. To show the user what's due or late, use ebirforms_calendar_get instead.",
            json!({
                "type": "object",
                "properties": { "tin": ticket, "filter": { "type": "string", "enum": ["upcoming", "overdue", "all"] } },
                "additionalProperties": false
            }),
            true,
        ),
        tool(
            CALENDAR_GET,
            "The filing calendar between `from` and `to` (YYYY-MM-DD; default about three months back to six ahead): each return due, its status (not_started, draft, queued, filed, confirmed, paid) and whether it is overdue. `filter: overdue` keeps only late returns (from a year back). Each return comes back as a ticket (`key`) that ebirforms_form_open and ebirforms_app_open take as `form`. `tin` limits it to one taxpayer. The answer shows the user a card (what's due, a week, a month, or late returns): don't repeat it as a list.",
            json!({
                "type": "object",
                "properties": { "tin": ticket, "from": { "type": "string" }, "to": { "type": "string" }, "filter": { "type": "string", "enum": ["overdue"] } },
                "additionalProperties": false
            }),
            true,
        ),
        tool(
            RDO_LOOKUP,
            "Look up a BIR revenue district office by code (43A, 043) or part of its name, from eBIRForms' RDO table, with how many saved taxpayers are registered there. The answer shows the user a card.",
            json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"],
                "additionalProperties": false
            }),
            true,
        ),
        tool(
            DRAFTS_LIST,
            "List a taxpayer's saved returns: drafts by default, or every saved and filed return with `status: all`. `form` limits it to one form.",
            json!({
                "type": "object",
                "properties": { "tin": ticket, "form": { "type": "string" }, "status": { "type": "string", "enum": ["draft", "all"] } },
                "required": ["tin"],
                "additionalProperties": false
            }),
            true,
        ),
        tool(
            APP_OPEN,
            "Ask the Bot to open one of eBIRForms's apps for the user: `forms` (the taxpayer's ticket as `tin`, and `form` + `period`, `action` view|file|submit, `draft_id` to open one return), `tax-profile` (needs `tin`), `calendar`, `deadlines` (the next returns due, as a card in the chat; `tin` for one taxpayer) or `register` (the Buwiz Sign-in screen, where the user signs in themselves).",
            json!({
                "type": "object",
                "properties": {
                    "app": { "type": "string", "enum": ["forms", "tax-profile", "calendar", "deadlines", "register"] },
                    "tin": ticket,
                    "form": { "type": "string", "description": "A BIR form code such as 2551Q, or a return's ticket." },
                    "period": { "type": "string", "description": "2026-Q3, 2026-01, 2025 or 2026-10-26; omitted: the period to file now." },
                    "action": { "type": "string", "enum": ["view", "file", "submit"] },
                    "draft_id": { "type": "integer" }
                },
                "required": ["app"],
                "additionalProperties": false
            }),
            false,
        ),
        tool(
            FORM_OPEN,
            "Open a return for one taxpayer and period, as eBIRForms's form page opens it (prefilled from the taxpayer profile; the saved return if there is one). `code`: a form code (1601C, 2551Q, 1701Q, ...) or a return's ticket. `period`: month 1-12, quarter 1-4, 0 for annual, or text (2026-Q3, 2026-01, 2025); omitted, the return to file now. One return is open at a time.",
            json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string" },
                    "tin": ticket,
                    "year": { "type": "integer", "minimum": 2000, "maximum": 2100 },
                    "period": { "type": ["integer", "string"] }
                },
                "required": ["code"],
                "additionalProperties": false
            }),
            false,
        ),
        tool(
            FORM_FIELDS,
            "Read the open return's boxes: values by official key, the locked boxes (with why), boxes switched off by another answer, and each drop-down's options. `keys` limits it to some boxes.",
            json!({
                "type": "object",
                "properties": { "keys": { "type": "array", "items": { "type": "string" } } },
                "additionalProperties": false
            }),
            true,
        ),
        tool(
            FORM_FILL,
            "Fill boxes of the open return: `fields` maps official box keys (from ebirforms_form_fields) to values (amounts like \"1234.50\", dates MM/DD/YYYY, checkboxes true/false, null clears). eBIRForms recomputes the return; refused boxes come back in `rejected` with why. `source`: profile, past_return, document, ai (default) or user. Nothing is saved.",
            json!({
                "type": "object",
                "properties": {
                    "fields": { "type": "object", "additionalProperties": { "type": ["string", "number", "boolean", "null"] }, "minProperties": 1 },
                    "source": { "type": "string", "enum": ["profile", "past_return", "document", "ai", "user"] },
                    "from_card": { "type": "boolean" }
                },
                "required": ["fields"],
                "additionalProperties": false
            }),
            false,
        ),
        tool(
            FORM_VALIDATE,
            "Validate the open return with the form's official rules, as the page's Validate does: `ok`, `error_count` and `field_errors` ({field, message}). Explain each error in plain words.",
            empty_schema(),
            true,
        ),
        tool(
            FORM_SAVE,
            "Save the open return as a draft in the eBIRForms mini app. It never queues or files anything.",
            empty_schema(),
            false,
        ),
        tool(
            FORM_DISMISS,
            "Close the open return without saving. Refused when it has unsaved edits unless `discard` is true, which you set only after the user said to drop them.",
            json!({
                "type": "object",
                "properties": { "discard": { "type": "boolean" } },
                "additionalProperties": false
            }),
            false,
        ),
        tool(
            RESOLVE,
            "The Bot's resolver for the manifest's taxprofile scope (a taxpayer by name or TIN, as a ticket). Do not call it yourself.",
            json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string" }, "name": { "type": "string" }, "value": { "type": "string" },
                    "complete": { "type": "boolean" }, "context": { "type": "object" }
                },
                "additionalProperties": false
            }),
            true,
        ),
        app_only(
            TICKET_SUBJECT,
            "the taxpayer, branch or return a ticket the app holds stands for, so the app can show it.",
            json!({
                "type": "object", "properties": { "ticket": { "type": "string" } }, "required": ["ticket"], "additionalProperties": false
            }),
            true,
        ),
        app_only(
            TICKET_ISSUE,
            "a ticket for another branch of the taxpayer the app's ticket names.",
            json!({
                "type": "object", "properties": { "ticket": { "type": "string" }, "tin": { "type": "string" } }, "required": ["ticket", "tin"], "additionalProperties": false
            }),
            false,
        ),
        app_only(
            PRINT_PREVIEW,
            "a return's print sheet for the Bot's document view, as a static HTML document: `draft_id` (a saved return as stored), `year` and `quarter` (a 2551Q or 1601C as stored), `draft_json` with `edits` and `unlocked` (a Form library sample as the page holds it) or `period` (the stored return of that period).",
            json!({
                "type": "object",
                "properties": {
                    "ticket": { "type": "string" }, "form": { "type": "string" }, "draft_id": { "type": "integer" },
                    "year": { "type": "integer" }, "quarter": { "type": "integer" }, "draft_json": { "type": "string" },
                    "edits": { "type": "object" }, "unlocked": { "type": "array", "items": { "type": "string" } }, "period": { "type": "string" }
                },
                "required": ["ticket", "form"],
                "additionalProperties": false
            }),
            true,
        ),
        app_only(
            PAYMENTS_LIST,
            "a return's recorded payments (eBIRForms keeps none yet: always empty).",
            json!({
                "type": "object",
                "properties": { "tin": { "type": "string" }, "form": { "type": "string" }, "period": { "type": "string" } },
                "required": ["tin"],
                "additionalProperties": false
            }),
            true,
        ),
    ];
    for (name, description, read_only) in [
        (
            ACCOUNT_STATUS,
            "whether this computer is signed in to Buwiz (and as whom), or the sign-in code waiting for approval.",
            true,
        ),
        (
            ACCOUNT_SIGN_IN_START,
            "starts a Buwiz sign-in: a code to approve at account.buwiz.com/device.",
            false,
        ),
        (
            ACCOUNT_SIGN_IN_POLL,
            "checks once whether the sign-in code was approved: pending, signed_in, expired or denied.",
            false,
        ),
        (ACCOUNT_SIGN_OUT, "signs this computer out of Buwiz.", false),
    ] {
        tools.push(app_only(name, description, empty_schema(), read_only));
    }
    for (function, _, access) in scope::FUNCTIONS {
        tools.push(app_only(
            &app_tool_name(function),
            &format!("eBIRForms's `{function}`, held to the taxpayer of the app's ticket."),
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
