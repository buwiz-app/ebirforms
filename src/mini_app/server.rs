//! MCP for `bir-mini-app`: JSON-RPC 2.0 over stdio or the loopback HTTP door.
//!
//! Every tool runs in this process. The form, profile and dues tools are
//! bir-mcp's descriptions and argument checks over bir-shell's agent
//! [`Session`]; `bir_call` is the mini apps' bridge to the `bir` host module;
//! `bir_app_open` asks Rox to open one of the TSX apps this binary ships as
//! resources; `bir_resolve` answers the commands and `@` scopes of the
//! `rox://manifest` resource ([`resolve`]); `bir_drafts_list` lists a
//! taxpayer's unsubmitted returns. There is no app discovery and no connect
//! step: this process owns the database.

use bir_core::forms::taxform::{self, TaxformAction};
use bir_mcp::license::{License, UNAUTHENTICATED};
use bir_mcp::server::{LATEST_PROTOCOL, SUPPORTED_PROTOCOLS};
use bir_mcp::signin::{self, DesktopStore};
use bir_mcp::tools::{self, Action, HostCall};
use gpui_agent::AgentHost;
use gpui_agent::protocol::Op;
use gpui_shell::HostError;
use serde_json::{Map, Value, json};

use super::apps::{self, Catalog};
use super::bridge::{Bridge, Call};
use super::{calendar, confirm, payments, resolve, scope};
use crate::agent::{Session, UiQueue};

pub(crate) const SERVER_NAME: &str = "bir-mini-app";
pub(crate) const MODE: &str = "headless";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// bir-mcp tools this host answers. Left out: discovery and connect (this
/// process is the app), the in-app review hand-off (no window), and the
/// preview and card ops bir-shell's session does not implement.
const HOST_TOOLS: [&str; 11] = [
    "bir_profiles_list",
    "bir_dues_list",
    "bir_form_open",
    "bir_form_fields",
    "bir_form_context",
    "bir_form_fill",
    "bir_form_needs_you",
    "bir_form_validate",
    "bir_form_send",
    "bir_form_save_draft",
    "bir_form_dismiss",
];

const INSTRUCTIONS: &str = "BIR (Philippine tax returns) running headless on this computer: it owns \
the taxpayer database and files queued returns in the background. To let the user work visually, \
call bir_app_open: `forms` to prepare and file returns, `tax-profile` for a taxpayer's \
registration, `calendar` for the filing schedule (when asked to show the schedule), `register` to \
sign in to Buwiz. Pass `tin` to open it for one taxpayer (only that \
taxpayer is reachable from the app then). You can also read taxpayer profiles and dues and draft \
a return yourself with the bir_form_* tools: open, read fields and context, fill with a stated \
source, list what needs the user, validate, save the draft, dismiss. Only one form is open at a \
time. bir_drafts_list shows a taxpayer's unsubmitted returns. A message may name a return as a \
token `@bir:taxform:<tin>/<FORM>-<period>` (e.g. @bir:taxform:000000000/2551Q-2026-Q3): pass its \
value as bir_form_open's `code`. Sending (bir_form_send) needs the user's standing OK for that \
taxpayer and form; without it the user presses Submit in the Forms app themselves. bir_call and \
bir_resolve are the mini apps' and Rox's own; do not call them yourself. The form calendar: \
bir_calendar_get lists returns and due dates with their status, bir_calendar_set turns forms on or \
off the calendar; to sync it to Google Calendar, a routine checks bir_calendar_changes and you \
create, update or delete each event with the Google Workspace tools, then call bir_calendar_link. \
BIR confirmation emails: search Gmail with bir_confirmation_search's query, read each email \
(file name, date and time received by BIR) and call bir_confirm_filing, then report. \
Payments: a routine searches Gmail with bir_payment_search's query; read each email with \
bir_payment_parse (null for a format bir does not know: read it yourself) and call \
bir_record_payment. A receipt the user attaches (image or PDF) is recorded with its local path as \
`proof_path`; bir_mark_paid marks a return paid with no proof (offer to attach one). Payment is \
separate from BIR's filing confirmation. bir_filing_record makes the printable record (form, \
confirmation and proof of payment) and returns a rox://view link for the user. \
Deadlines: a deadline the user moves (\"adjust the 2551Q Q3 deadline to Nov 3\") is a rule for every \
taxpayer: bir_deadline_override_set moves one form's deadline for one period, \
bir_deadline_overrides_list shows what is moved, bir_deadline_override_remove puts one back. The \
calendar, Google Calendar and every return's penalties follow it. Only on the user's request.";

const BIR_STATUS: &str = "bir_status";
const BIR_CALL: &str = "bir_call";
const BIR_APP_OPEN: &str = "bir_app_open";
const BIR_RESOLVE: &str = resolve::RESOLVE_TOOL;
const BIR_DRAFTS_LIST: &str = "bir_drafts_list";
const BIR_CALENDAR_GET: &str = "bir_calendar_get";
const BIR_CALENDAR_SET: &str = "bir_calendar_set";
const BIR_CALENDAR_CHANGES: &str = "bir_calendar_changes";
const BIR_CALENDAR_LINK: &str = "bir_calendar_link";
const BIR_CONFIRMATION_SEARCH: &str = "bir_confirmation_search";
const BIR_CONFIRM_FILING: &str = "bir_confirm_filing";
// Payments and the printable filing record (payments.rs).
const BIR_PAYMENT_SEARCH: &str = "bir_payment_search";
const BIR_PAYMENT_PARSE: &str = "bir_payment_parse";
const BIR_RECORD_PAYMENT: &str = "bir_record_payment";
const BIR_MARK_PAID: &str = "bir_mark_paid";
const BIR_PAYMENTS_LIST: &str = "bir_payments_list";
const BIR_FILING_RECORD: &str = "bir_filing_record";
/// The payment and filing-record tools, as many as `tool_list` adds.
#[cfg(test)]
const PAYMENT_TOOLS: [&str; 6] = [
    BIR_PAYMENT_SEARCH,
    BIR_PAYMENT_PARSE,
    BIR_RECORD_PAYMENT,
    BIR_MARK_PAID,
    BIR_PAYMENTS_LIST,
    BIR_FILING_RECORD,
];

// Deadline overrides (bir_core::deadline_overrides): global, one rule for
// every taxpayer.
const BIR_DEADLINE_OVERRIDE_SET: &str = "bir_deadline_override_set";
const BIR_DEADLINE_OVERRIDES_LIST: &str = "bir_deadline_overrides_list";
const BIR_DEADLINE_OVERRIDE_REMOVE: &str = "bir_deadline_override_remove";
/// The deadline override tools, as many as `tool_list` adds.
const DEADLINE_TOOLS: [&str; 3] = [
    BIR_DEADLINE_OVERRIDE_SET,
    BIR_DEADLINE_OVERRIDES_LIST,
    BIR_DEADLINE_OVERRIDE_REMOVE,
];

/// How model-facing tools are licensed.
enum Gate {
    /// The desktop Buwiz account, read from its store on every check, so a
    /// sign-in through the `register` app takes effect without a restart.
    Desktop,
    Fixed(License),
}

/// One answer. `Later` is an async host call still to run; the stdio loop
/// runs it on its own thread, the HTTP door runs it in place.
pub(crate) enum Reply {
    Now(Option<Value>),
    Later(Box<dyn FnOnce() -> Value + Send>),
}

impl Reply {
    pub(crate) fn wait(self) -> Option<Value> {
        match self {
            Reply::Now(value) => value,
            Reply::Later(run) => Some(run()),
        }
    }
}

pub(crate) struct MiniServer {
    session: Session,
    commands: UiQueue,
    bridge: Bridge,
    catalog: Catalog,
    gate: Gate,
}

impl MiniServer {
    /// The stdio server: the debug test bypass, a `BUWIZ_ACCESS_TOKEN`
    /// agent token, or the desktop account, in that order.
    pub(crate) fn new(catalog: Catalog) -> Self {
        let gate = if crate::account_gate::auth_bypass_enabled() {
            Gate::Fixed(License::open())
        } else if let Some(token) = env_nonempty("BUWIZ_ACCESS_TOKEN") {
            let base = env_nonempty("BUWIZ_MCP_URL")
                .unwrap_or_else(|| signin::builtin_origin().to_string());
            Gate::Fixed(License::required(base, token))
        } else {
            Gate::Desktop
        };
        Self::with_gate(catalog, gate)
    }

    fn with_gate(catalog: Catalog, gate: Gate) -> Self {
        let commands = UiQueue::new();
        Self {
            session: Session::named(commands.clone(), SERVER_NAME, MODE),
            commands,
            bridge: Bridge::new(),
            catalog,
            gate,
        }
    }

    /// A server on this thread's scratch database, licensed and signed in.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        crate::bir_module::use_ephemeral_database();
        crate::account_gate::set_unlocked(true);
        Self::with_gate(Catalog::Embedded, Gate::Fixed(License::open()))
    }

    pub(crate) fn set_license(&mut self, license: License) {
        self.gate = Gate::Fixed(license);
    }

    pub(crate) fn handle_line(&mut self, line: &str) -> Reply {
        if line.trim().is_empty() {
            return Reply::Now(None);
        }
        match serde_json::from_str::<Value>(line) {
            Ok(message) => self.handle(message),
            Err(error) => Reply::Now(Some(error_response(
                Value::Null,
                PARSE_ERROR,
                &format!("parse error: {error}"),
            ))),
        }
    }

    pub(crate) fn handle(&mut self, message: Value) -> Reply {
        let Some(object) = message.as_object() else {
            return Reply::Now(Some(error_response(
                Value::Null,
                INVALID_REQUEST,
                "expected a single JSON-RPC object",
            )));
        };
        let id = object.get("id").cloned();
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return Reply::Now(id.map(|id| error_response(id, INVALID_REQUEST, "missing method")));
        };
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = id else {
            return Reply::Now(None);
        };
        let outcome = match method {
            "initialize" => Ok(initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(tool_list()),
            "tools/call" => match self.tools_call(&params) {
                Ok(Reply::Later(run)) => {
                    return Reply::Later(Box::new(
                        move || json!({ "jsonrpc": "2.0", "id": id, "result": run() }),
                    ));
                }
                Ok(Reply::Now(result)) => Ok(result.unwrap_or(Value::Null)),
                Err(error) => Err(error),
            },
            "resources/list" => self
                .catalog
                .list()
                .map(|mut listed| {
                    if let Some(resources) = listed["resources"].as_array_mut() {
                        resources.push(resolve::descriptor());
                    }
                    listed
                })
                .map_err(|error| (INVALID_PARAMS, error)),
            "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
            "resources/read" => match params.get("uri").and_then(Value::as_str) {
                Some(resolve::MANIFEST_URI) => Ok(resolve::read()),
                Some(uri) => self
                    .catalog
                    .read(uri)
                    .map_err(|error| (INVALID_PARAMS, error)),
                None => Err((INVALID_PARAMS, "resources/read requires params.uri".into())),
            },
            other => Err((METHOD_NOT_FOUND, format!("method not found: {other}"))),
        };
        Reply::Now(Some(match outcome {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => error_response(id, code, &message),
        }))
    }

    /// `Ok(Reply)` carries the tool result (never a JSON-RPC envelope).
    fn tools_call(&mut self, params: &Value) -> Result<Reply, (i64, String)> {
        let name = params.get("name").and_then(Value::as_str).ok_or((
            INVALID_PARAMS,
            "tools/call requires params.name".to_string(),
        ))?;
        let empty = Map::new();
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(arguments)) => arguments,
            Some(_) => {
                return Ok(now(tool_error("arguments must be a JSON object".into())));
            }
        };
        match name {
            BIR_STATUS => Ok(now(self.status())),
            BIR_CALL => Ok(self.bir_call(arguments)),
            BIR_APP_OPEN => Ok(now(self.app_open(arguments))),
            BIR_RESOLVE => Ok(now(self.resolve(arguments))),
            BIR_DRAFTS_LIST => Ok(now(self.drafts_list(arguments))),
            BIR_CALENDAR_GET => Ok(now(self.on_database(name, arguments, calendar::get))),
            BIR_CALENDAR_SET => Ok(now(self.on_database(name, arguments, |db, arguments, _| {
                calendar::set(db, arguments)
            }))),
            BIR_CALENDAR_CHANGES => Ok(now(self.on_database(name, arguments, calendar::changes))),
            BIR_CALENDAR_LINK => Ok(now(self.on_database(name, arguments, |db, arguments, _| {
                calendar::link(db, arguments)
            }))),
            BIR_CONFIRMATION_SEARCH => Ok(now(self.on_database(name, arguments, confirm::search))),
            BIR_CONFIRM_FILING => Ok(now(self.on_database(name, arguments, confirm::confirm))),
            // Payments and the printable filing record.
            BIR_PAYMENT_SEARCH => Ok(now(self.on_database(name, arguments, payments::search))),
            BIR_PAYMENT_PARSE => Ok(now(self.on_database(name, arguments, |_, arguments, today| {
                payments::parse(arguments, today)
            }))),
            BIR_RECORD_PAYMENT => Ok(now(self.on_database(name, arguments, |db, arguments, today| {
                payments::record(db, arguments, today, &payments::receipts_dir())
            }))),
            BIR_MARK_PAID => Ok(now(self.on_database(name, arguments, |db, arguments, today| {
                payments::mark_paid(db, arguments, today, &payments::receipts_dir())
            }))),
            BIR_PAYMENTS_LIST => Ok(now(self.on_database(name, arguments, payments::list))),
            BIR_FILING_RECORD => Ok(now(self.on_database(name, arguments, |db, arguments, today| {
                payments::filing_record(db, arguments, today, &std::env::temp_dir())
            }))),
            // Deadline overrides.
            name if DEADLINE_TOOLS.contains(&name) => {
                Ok(now(self.on_database(name, arguments, |db, arguments, today| {
                    match name {
                        BIR_DEADLINE_OVERRIDE_SET => {
                            bir_core::deadline_overrides::set(db, arguments, today)
                        }
                        BIR_DEADLINE_OVERRIDES_LIST => {
                            bir_core::deadline_overrides::list(db, arguments)
                        }
                        _ => bir_core::deadline_overrides::remove(db, arguments, today),
                    }
                })))
            }
            _ => {
                let tool = HOST_TOOLS
                    .contains(&name)
                    .then(|| tools::find(name))
                    .flatten()
                    .ok_or((INVALID_PARAMS, format!("unknown tool: {name}")))?;
                if let Err(message) = self.allow() {
                    return Ok(now(tool_error(message)));
                }
                let arguments = with_current_tin(name, arguments);
                let args = match tool.host_args(&arguments) {
                    Ok(args) => args,
                    Err(message) => return Ok(now(tool_error(format!("{name}: {message}")))),
                };
                let Action::Host(HostCall::Invoke(op)) = tool.action else {
                    return Err((INVALID_PARAMS, format!("unknown tool: {name}")));
                };
                Ok(now(match self.host(op, args) {
                    Ok(value) => tool_text(&value),
                    Err(message) => tool_error(message),
                }))
            }
        }
    }

    /// Buwiz license, then the account gate. Model-facing tools only.
    fn allow(&mut self) -> Result<(), String> {
        match &mut self.gate {
            // The saved account is read on every check, never kept: the
            // account gate in this process refreshes the same session, and
            // Buwiz rotates refresh tokens, so a copy held here goes stale and
            // reusing it ends the whole sign-in (RFC 9700).
            Gate::Desktop => {
                let (origin, _) = signin::configured_origins()?;
                let session = signin::ensure_fresh(&origin, &DesktopStore, signin::unix_now())?
                    .ok_or_else(|| UNAUTHENTICATED.to_string())?;
                License::desktop(origin, session.access_token, session.refresh_token, true)
                    .allow()?;
            }
            Gate::Fixed(license) => license.allow()?,
        }
        crate::account_gate::ensure_unlocked().map_err(|error| error.message().to_string())
    }

    /// One agent-session op, the way bir-shell's drain runs it. The screen
    /// commands it queues have no window here and are dropped.
    fn host(&mut self, op: &str, args: Value) -> Result<Value, String> {
        if self.session.needs_db() {
            let handle = crate::bir_module::database_handle()
                .map_err(|error| error.message().to_string())?;
            self.session.set_db(handle);
        }
        let result = self.session.dispatch(&Op::Invoke {
            name: op.to_string(),
            args,
        });
        while self.commands.pop().is_some() {}
        result.map(|result| result.value.unwrap_or(Value::Null))
    }

    fn status(&mut self) -> Value {
        let (signed_in, reason) = self.sign_in();
        let mut status = json!({
            "app": SERVER_NAME,
            "mode": MODE,
            "database": crate::agent::database_kind(),
            "signed_in": signed_in,
            "version": env!("CARGO_PKG_VERSION"),
        });
        if let Some(reason) = reason {
            status["reason"] = json!(reason);
        }
        tool_text(&status)
    }

    /// Whether this computer is signed in to Buwiz, and why not. The desktop
    /// account is read from its store (refreshed when stale, the way every
    /// licensed tool does): a rejected or unreachable refresh is not signed
    /// in, whatever the account gate opened with at launch.
    fn sign_in(&mut self) -> (bool, Option<String>) {
        let session = match &self.gate {
            Gate::Desktop => signin::configured_origins()
                .and_then(|(origin, _)| signin::signed_in(&origin, &DesktopStore, signin::unix_now())),
            Gate::Fixed(_) => Ok(()),
        };
        match session {
            Err(reason) => (false, Some(reason)),
            Ok(()) if crate::account_gate::is_unlocked() => (true, None),
            Ok(()) => (false, Some(crate::account_gate::LOCKED_MESSAGE.to_string())),
        }
    }

    fn bir_call(&mut self, arguments: &Map<String, Value>) -> Reply {
        let request = match bir_call_args(arguments) {
            Ok(request) => request,
            Err(message) => return now(tool_error(format!("{BIR_CALL}: {message}"))),
        };
        match self
            .bridge
            .call(&request.function, request.args, request.scope.as_deref())
        {
            Call::Ready(result) => now(call_result(result)),
            Call::Pending(run) => Reply::Later(Box::new(move || call_result(run()))),
        }
    }

    fn app_open(&mut self, arguments: &Map<String, Value>) -> Value {
        match self.try_app_open(arguments) {
            Ok(result) => result,
            Err(message) => tool_error(format!("{BIR_APP_OPEN}: {message}")),
        }
    }

    fn try_app_open(&mut self, arguments: &Map<String, Value>) -> Result<Value, String> {
        const ALLOWED: [&str; 7] = [
            "app", "tin", "form", "period", "action", "draft_id", "branch",
        ];
        if let Some(key) = arguments
            .keys()
            .find(|key| !ALLOWED.contains(&key.as_str()))
        {
            return Err(format!(
                "unknown argument `{key}` (allowed: {})",
                ALLOWED.join(", ")
            ));
        }
        let id = arguments
            .get("app")
            .and_then(Value::as_str)
            .filter(|id| apps::OPENABLE.contains(id))
            .ok_or_else(|| format!("`app` must be one of {}", apps::OPENABLE.join(", ")))?;
        let text = |key: &str| -> Result<Option<String>, String> {
            match arguments.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(text)) if !text.trim().is_empty() => {
                    Ok(Some(text.trim().to_string()))
                }
                Some(other) => Err(format!("`{key}` must be a string (got {other})")),
            }
        };
        let tin = text("tin")?;
        let form = text("form")?;
        let period = text("period")?;
        let branch = text("branch")?;
        let action = match text("action")? {
            None => None,
            Some(action) => Some(TaxformAction::parse(&action).ok_or_else(|| {
                format!("`action` must be view, file or submit (got `{action}`)")
            })?),
        };
        let draft_id = match arguments.get("draft_id") {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.as_i64().ok_or_else(|| {
                format!("`draft_id` must be a draft id from bir_drafts_list (got {value})")
            })?),
        };
        let return_args = period.is_some() || action.is_some() || draft_id.is_some();
        if (form.is_some() || return_args) && id != "forms" {
            return Err("`form`, `period`, `action` and `draft_id` open the `forms` app".into());
        }
        if return_args && form.is_none() {
            return Err("`period`, `action` and `draft_id` need `form`".into());
        }
        if branch.is_some() && (id != "tax-profile" || tin.is_none()) {
            return Err("`branch` opens the `tax-profile` app on one branch of `tin`".into());
        }
        // Signing in is what `register` is for, so it opens signed out.
        if id != "register"
            && let Err(message) = self.allow()
        {
            return Err(format!(
                "{message}\nOpen the sign-in app first: bir_app_open {{\"app\": \"register\"}}."
            ));
        }
        let app = self
            .catalog
            .find(id)?
            .ok_or_else(|| format!("the `{id}` mini app is not shipped in this build"))?;
        let today = taxform::today();
        let (props, text) = match (id, form, tin) {
            ("forms", Some(form), tin) => {
                let target = database(|db| {
                    resolve::app_return(db, tin.as_deref(), &form, period.as_deref(), action, today)
                })?;
                let mut props = target.props();
                if let Some(draft_id) = draft_id {
                    props["draft_id"] = json!(draft_id);
                }
                (props, format!("Opening {} on {}…", app.title, target.title))
            }
            (_, _, None) => (json!({}), format!("Opening {}…", app.title)),
            ("tax-profile", None, Some(tin)) => {
                let (mut props, name) = database(|db| resolve::app_profile(db, &tin))?
                    .ok_or_else(|| {
                        format!(
                            "no taxpayer with TIN {tin} is saved in BIR; list them with bir_profiles_list"
                        )
                    })?;
                if let Some(branch) = branch {
                    let digits: String = branch.chars().filter(char::is_ascii_digit).collect();
                    if digits.is_empty() || digits.len() > 5 {
                        return Err(format!(
                            "`branch` must be a branch code such as 00001 (got `{branch}`)"
                        ));
                    }
                    props["branch"] = json!(format!("{digits:0>5}"));
                }
                (props, format!("Opening {} for {name}…", app.title))
            }
            (_, _, Some(tin)) => {
                let (row, _) = self.taxpayer(&tin)?;
                let name = row["name"].as_str().unwrap_or("this taxpayer").to_string();
                (
                    json!({ "tin": row["tin"].clone() }),
                    format!("Opening {} for {name}…", app.title),
                )
            }
        };
        Ok(json!({
            "content": [{ "type": "text", "text": text }],
            "isError": false,
            "_meta": { "rox/open": open_meta(&app.uri(), id, &props) },
        }))
    }

    /// `bir_resolve`: Rox's resolver for the manifest's commands and
    /// scopes. Local only: the account gate and a database read, never the
    /// license check (which may refresh a token over the network).
    fn resolve(&mut self, arguments: &Map<String, Value>) -> Value {
        let request = match resolve::request(arguments) {
            Ok(request) => request,
            Err(message) => return tool_error(format!("{BIR_RESOLVE}: {message}")),
        };
        let today = taxform::today();
        let answer =
            crate::bir_module::with_database(|db| Ok(resolve::resolve(db, &request, today)))
                .unwrap_or_else(|error| json!({ "ok": false, "error": error.message() }));
        call_result(Ok(answer))
    }

    /// `bir_drafts_list`: a taxpayer's returns not yet submitted.
    fn drafts_list(&mut self, arguments: &Map<String, Value>) -> Value {
        match self.try_drafts_list(arguments) {
            Ok(result) => call_result(Ok(result)),
            Err(message) => tool_error(format!("{BIR_DRAFTS_LIST}: {message}")),
        }
    }

    fn try_drafts_list(&mut self, arguments: &Map<String, Value>) -> Result<Value, String> {
        if let Some(key) = arguments
            .keys()
            .find(|key| !["tin", "form", "status"].contains(&key.as_str()))
        {
            return Err(format!(
                "unknown argument `{key}` (allowed: tin, form, status)"
            ));
        }
        let text = |key: &str| -> Result<Option<&str>, String> {
            match arguments.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(text)) if !text.trim().is_empty() => Ok(Some(text.trim())),
                Some(other) => Err(format!("`{key}` must be a string (got {other})")),
            }
        };
        let all = match text("status")? {
            None | Some("draft") => false,
            Some("all") => true,
            Some(other) => return Err(format!("`status` must be draft or all (got `{other}`)")),
        };
        let (tin, form) = (text("tin")?, text("form")?);
        self.allow()?;
        let today = taxform::today();
        database(|db| resolve::drafts_list(db, tin, form, all, today))
    }

    /// A model-facing tool that reads or writes the database: licensed,
    /// then `run` with the arguments and today's date.
    fn on_database(
        &mut self,
        name: &str,
        arguments: &Map<String, Value>,
        run: impl FnOnce(&bir_core::db::Database, &Map<String, Value>, chrono::NaiveDate) -> Result<Value, String>,
    ) -> Value {
        if let Err(message) = self.allow() {
            return tool_error(message);
        }
        let today = taxform::today();
        match database(|db| run(db, arguments, today)) {
            Ok(result) => call_result(Ok(result)),
            Err(message) => tool_error(format!("{name}: {message}")),
        }
    }

    /// The saved branch `tin` names (a root picks its head office) and its root.
    fn taxpayer(&mut self, tin: &str) -> Result<(Value, String), String> {
        let root = scope::root_of(&Value::String(tin.to_string()))
            .ok_or_else(|| format!("`{tin}` is not a TIN (9 or 14 digits)"))?;
        let digits: String = tin.chars().filter(char::is_ascii_digit).collect();
        let rows = self.host("profile.list", json!({}))?;
        let rows: Vec<Value> = rows
            .as_array()
            .into_iter()
            .flatten()
            .filter(|row| {
                row["tin"]
                    .as_str()
                    .and_then(|tin| scope::root_of(&json!(tin)))
                    == Some(root.clone())
            })
            .cloned()
            .collect();
        let wanted = if digits.len() == 9 {
            format!("{digits}00000")
        } else {
            digits
        };
        let digits_of = |row: &Value| -> String {
            row["tin"]
                .as_str()
                .unwrap_or("")
                .chars()
                .filter(char::is_ascii_digit)
                .collect()
        };
        let row = rows
            .iter()
            .find(|row| digits_of(row) == wanted)
            .or_else(|| (wanted.ends_with("00000")).then(|| rows.first()).flatten())
            .cloned()
            .ok_or_else(|| {
                format!(
                    "no taxpayer with TIN {tin} is saved in BIR; list them with bir_profiles_list"
                )
            })?;
        Ok((row, root))
    }
}

impl bir_mcp::http::McpHandler for MiniServer {
    fn set_license(&mut self, license: License) {
        MiniServer::set_license(self, license);
    }

    fn handle(&mut self, message: Value) -> Option<Value> {
        MiniServer::handle(self, message).wait()
    }

    fn handle_line(&mut self, line: &str) -> Option<String> {
        MiniServer::handle_line(self, line)
            .wait()
            .map(|value| value.to_string())
    }
}

struct CallRequest {
    function: String,
    args: Vec<Value>,
    scope: Option<String>,
}

fn bir_call_args(arguments: &Map<String, Value>) -> Result<CallRequest, String> {
    if let Some(key) = arguments
        .keys()
        .find(|key| !["fn", "args", "scope"].contains(&key.as_str()))
    {
        return Err(format!(
            "unknown argument `{key}` (allowed: fn, args, scope)"
        ));
    }
    let function = arguments
        .get("fn")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or("`fn` is required: a `bir` host function name")?
        .to_string();
    let args = match arguments.get("args") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(args)) => args.clone(),
        Some(_) => return Err("`args` must be an array of the function's arguments".into()),
    };
    let scope = match arguments.get("scope") {
        None | Some(Value::Null) => None,
        Some(Value::String(raw)) => Some(scope::parse_scope(raw)?),
        Some(_) => return Err("`scope` must be a 9-digit TIN root string".into()),
    };
    Ok(CallRequest {
        function,
        args,
        scope,
    })
}

/// `bir_form_open` with no `tin` and no TIN in its token is for the
/// current taxpayer (`/taxprofile`), as a taxform without a TIN is.
fn with_current_tin(name: &str, arguments: &Map<String, Value>) -> Map<String, Value> {
    let mut arguments = arguments.clone();
    let code = arguments.get("code").and_then(Value::as_str).unwrap_or("");
    if name == "bir_form_open"
        && !arguments.contains_key("tin")
        && taxform::parse(code, taxform::today()).tin.is_none()
        && let Ok(Some(tin)) = database(resolve::current_tin)
    {
        arguments.insert("tin".into(), Value::String(tin));
    }
    arguments
}

/// One database read or write, its error as text.
fn database<R>(
    run: impl FnOnce(&bir_core::db::Database) -> Result<R, String>,
) -> Result<R, String> {
    crate::bir_module::with_database(|db| run(db).map_err(HostError::new))
        .map_err(|error| error.message().to_string())
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

/// `_meta["rox/open"]`: the app's resource, its props and, when the props
/// name one, the canonical token of what opens (`token`, and `&token=` on
/// `link`, the same `rox://app/` link the resolver answers).
fn open_meta(uri: &str, app: &str, props: &Value) -> Value {
    let mut open = json!({ "uri": uri, "props": props });
    if let Some(token) = resolve::app_token(app, props) {
        open["token"] = json!(token);
        open["link"] = json!(resolve::app_link(app, props));
    }
    open
}

fn now(result: Value) -> Reply {
    Reply::Now(Some(result))
}

fn tool_text(value: &Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(value).expect("json value") }],
        "isError": false,
    })
}

fn tool_error(message: String) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true,
    })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn initialize(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = requested
        .filter(|v| SUPPORTED_PROTOCOLS.contains(v))
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

fn tool_list() -> Value {
    let mut list = vec![
        json!({
            "name": BIR_STATUS,
            "description": "Report this BIR host: headless mode, whether it uses a test or the REAL taxpayer database, and whether this computer is signed in to Buwiz.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        }),
        json!({
            "name": BIR_APP_OPEN,
            "description": "Open one of BIR's mini apps for the user in Rox: `forms` (prepare, validate, save and queue returns), `tax-profile` (a taxpayer's registration and yearly forms), `calendar` (the filing schedule: when each return is due and its status, month by month, and the deadlines moved for every taxpayer; open it when the user asks to see the schedule) or `register` (sign in to Buwiz on this computer; works signed out). With `tin`, the app opens for that saved taxpayer only (check it with bir_profiles_list first); without it, the app lets the user pick. `forms` with `form` opens one return: `form` is a code (2551Q) or a return token's value (000000000/2551Q-2026-Q3); `period` names its period (2026-Q3, jan 2026, 2025; default: an unsubmitted draft of that form, else the return to file now); `action` is view (read-only), file (fill it, the default) or submit (validate, then the user presses Submit); `draft_id` comes from bir_drafts_list. Without `tin` a return is for the current taxpayer. `tax-profile` with `branch` opens on that branch. If a tool says Buwiz sign-in is required, open `register`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "app": { "type": "string", "enum": apps::OPENABLE },
                    "tin": { "type": "string", "description": "Taxpayer TIN (9-digit root or 14-digit branch TIN) as bir_profiles_list returns it." },
                    "form": { "type": "string", "description": "forms only: a form code (2551Q) or a return token's value (000000000/2551Q-2026-Q3)." },
                    "period": { "type": "string", "description": "forms only: the return's period, e.g. 2026-Q3, jan 2026, 2026-01, 2025." },
                    "action": { "type": "string", "enum": ["view", "file", "submit"], "description": "forms only: view (read-only), file (fill it; default) or submit (validate and show the user the Submit button)." },
                    "draft_id": { "type": "integer", "description": "forms only: a saved return's id from bir_drafts_list." },
                    "branch": { "type": "string", "description": "tax-profile only: the branch code, e.g. 00001." }
                },
                "required": ["app"],
                "additionalProperties": false
            },
        }),
        json!({
            "name": BIR_DRAFTS_LIST,
            "description": "List a taxpayer's saved returns that are not submitted yet (drafts), newest first: id, form, period, label, status, when last saved, and the return's token (@bir:taxform:...). `form` narrows the list to one form: a code (2551Q) or a return token's value (000000000/2551Q-2026-Q3, whose period narrows it too). Without `tin`, the current taxpayer's. `status: all` lists queued and filed returns too. Open one with bir_form_open (`code`: its token's value) or show it to the user with bir_app_open (`form` and `draft_id`).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tin": { "type": "string", "description": "Taxpayer TIN (9-digit root or 14-digit branch TIN)." },
                    "form": { "type": "string", "description": "A form code or a return token's value." },
                    "status": { "type": "string", "enum": ["draft", "all"], "description": "draft (default): unsubmitted only; all: every stored return." }
                },
                "additionalProperties": false
            },
        }),
    ];
    list.extend(
        HOST_TOOLS
            .iter()
            .filter_map(|name| tools::find(name))
            .map(|tool| tool.descriptor()),
    );
    list.extend([
        json!({
            "name": BIR_CALENDAR_GET,
            "description": "The form calendar: every return due between `from` and `to` (dates, default this year and next) for one taxpayer (`tin`) or all of them, from each taxpayer's forms for the year and BIR's official deadlines. Each event has `key` (the return token's value, stable per taxpayer, form and period), tin, taxpayer, form, period, due (with moved, rule_due and moved_reason when a deadline override moved it), status (not_started, draft, queued, filed, confirmed, paid), status_detail, overdue, enabled (whether the form is on the calendar), reminder_days, token and `open` (a link that opens the return). `taxpayers` lists each one's forms with enabled and reminder_days. To show the schedule to the user, call bir_app_open with app `calendar` instead.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tin": { "type": "string", "description": "Taxpayer TIN (9-digit root = head office, or 14 digits). Omit for every taxpayer." },
                    "from": { "type": "string", "description": "First due date, YYYY-MM-DD." },
                    "to": { "type": "string", "description": "Last due date, YYYY-MM-DD." }
                },
                "additionalProperties": false
            },
        }),
        json!({
            "name": BIR_CALENDAR_SET,
            "description": "Turn forms on or off a taxpayer's calendar, one or a batch at once (`forms` with `enabled`), and/or set its reminders (`reminder_days`, days before the due date, e.g. [7, 1]). Returns the taxpayer's forms with their state. Only on the user's request.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tin": { "type": "string", "description": "Taxpayer TIN." },
                    "forms": { "type": "array", "items": { "type": "string" }, "description": "Form codes, e.g. 2551Q, 1601C." },
                    "enabled": { "type": "boolean", "description": "true puts the forms on the calendar, false takes them off." },
                    "reminder_days": { "type": "array", "items": { "type": "integer", "minimum": 0, "maximum": 60 }, "description": "Reminder days before each due date." }
                },
                "required": ["tin"],
                "additionalProperties": false
            },
        }),
        json!({
            "name": BIR_CALENDAR_CHANGES,
            "description": "For a Google Calendar sync: the calendar events (of forms that are on the calendar) added, changed or removed that Google does not have yet. Each change: `id` (new for every change), `change` (added, changed, removed), `key`, `event_id` (the Google event recorded with bir_calendar_link, if any) and `event` (title, description, due as an all-day date, reminder_days). For added: create the Google event and call bir_calendar_link with its id. For changed: update that event (create one when event_id is null) and link it. For removed: delete event_id, then call bir_calendar_link with event_id null. A change stays listed until linked. `since` (RFC 3339, date or Unix seconds) lists only changes after it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "since": { "type": ["string", "integer"], "description": "Only changes after this time." }
                },
                "additionalProperties": false
            },
        }),
        json!({
            "name": BIR_CALENDAR_LINK,
            "description": "Record the Google Calendar event id of a calendar event (`key` from bir_calendar_changes), after creating or updating it. `event_id` null or empty forgets it: use that after deleting a removed event.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "The change's key, e.g. 000000000/2551Q-2026-Q3." },
                    "event_id": { "type": ["string", "null"], "description": "The Google event id; null after deleting it." }
                },
                "required": ["key"],
                "additionalProperties": false
            },
        }),
        json!({
            "name": BIR_CONFIRMATION_SEARCH,
            "description": "The Gmail search (`query`) for BIR's eBIRForms confirmation emails about these taxpayers (all of them without `tins`), from `since` (30 days back), and `awaiting`: the returns sent or queued and not confirmed yet. Search Gmail with it through the Google Workspace tools; for each email read its File name, Date and Time received by BIR, and call bir_confirm_filing.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tins": { "type": "array", "items": { "type": "string" }, "description": "Taxpayer TINs (a 9-digit root covers every branch)." }
                },
                "additionalProperties": false
            },
        }),
        json!({
            "name": BIR_CONFIRM_FILING,
            "description": "Mark a filed return confirmed from BIR's confirmation email, the same way BIR's own email check does: the return must have been sent (filed), and BIR's received time must be after it was queued. `reference` is the email's File name (e.g. 00000000000000-2551Qv2018-092026Q3.xml); `received_at` is BIR's date and time received (e.g. 2026-10-12T14:52:00+08:00); `message_id` is the Gmail message id (the same email again answers the same). Returns the return with `confirmed` and, when not confirmed, `reason`. Report the result to the user.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tin": { "type": "string", "description": "Taxpayer TIN from the file name." },
                    "form": { "type": "string", "description": "Form code, e.g. 2551Q." },
                    "period": { "type": "string", "description": "The return's period, e.g. 2026-Q3, 2026-09, 2025." },
                    "reference": { "type": "string", "description": "The email's File name." },
                    "received_at": { "type": "string", "description": "BIR's received date and time." },
                    "message_id": { "type": "string", "description": "The Gmail message id." }
                },
                "required": ["tin", "form", "period", "reference", "received_at", "message_id"],
                "additionalProperties": false
            },
        }),
    ]);
    list.extend(payment_tools());
    list.extend(deadline_tools());
    list.push(json!({
        "name": BIR_CALL,
        "description": "The BIR mini apps' bridge to the host: calls one `bir` host function by name with its arguments and returns its JSON result. It is used by the mini apps opened with bir_app_open, not by the assistant: do not call it yourself. `scope` holds the call to one taxpayer's 9-digit TIN root.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "fn": { "type": "string", "description": "A `bir` host function name, e.g. list_taxpayers or screen_form_open." },
                "args": { "type": "array", "description": "The function's positional arguments, as JSON." },
                "scope": { "type": "string", "description": "9-digit TIN root this call is held to." }
            },
            "required": ["fn"],
            "additionalProperties": false
        },
    }));
    list.push(json!({
        "name": BIR_RESOLVE,
        "description": "Rox's resolver for the commands and @ scopes in the rox://manifest resource (/taxprofile, /taxform, @bir:taxprofile:, @bir:branchcode:, @bir:taxform:). Rox calls it locally as the user types; it reads only this computer's database. Do not call it yourself.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": { "type": "string", "enum": ["taxprofile", "branchcode", "taxform"], "description": "The command or scope name." },
                "args": { "type": "string", "description": "The text after `/name ` or after `@bir:name:`; may be empty." },
                "complete": { "type": "boolean", "default": false, "description": "True for suggestions (at most 20) for a partial `args`." },
                "context": {
                    "type": "object",
                    "description": "Where the user types: `attached` lists the tokens attached there; an attached taxpayer or branch is the default taxpayer.",
                    "properties": { "attached": { "type": "array", "items": { "type": "string" } } }
                }
            },
            "required": ["name"],
            "additionalProperties": false
        },
    }));
    json!({ "tools": list })
}

/// The deadline override tools ([`DEADLINE_TOOLS`]).
fn deadline_tools() -> Vec<Value> {
    vec![
        json!({
            "name": BIR_DEADLINE_OVERRIDE_SET,
            "description": "Move one form's deadline for one period, for EVERY taxpayer (a deadline is a rule: \"adjust the deadline of 2551Q Q3 2026 to Nov 3\"). Name the deadline by `period` (2026-Q3, 2026-09, 2025) or by its `original_deadline` (the date BIR's calendar gives, YYYY-MM-DD); `new_deadline` is the new due date (a weekend still moves it to the Monday); `reason` is what the user cites (a BIR advisory), shown on the calendar. Setting it again moves it again. The calendar, the Google Calendar sync (as a changed event) and the penalties of every return of that form and period follow it. As-needed forms (0605, 2200A, 1800, ...) have no calendar deadline and are refused. Returns the rule's date, the due date before (`was`) and after (`due`). Only on the user's request.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "form": { "type": "string", "description": "Form code, e.g. 2551Q, 1601C." },
                    "period": { "type": "string", "description": "The period whose deadline moves: 2026-Q3, 2026-09 or 2025." },
                    "original_deadline": { "type": "string", "description": "Or the deadline's current date in BIR's calendar, YYYY-MM-DD." },
                    "new_deadline": { "type": "string", "description": "The new due date, YYYY-MM-DD." },
                    "reason": { "type": "string", "description": "Why it moved, e.g. the BIR advisory the user cites." }
                },
                "required": ["form", "new_deadline"],
                "additionalProperties": false
            },
        }),
        json!({
            "name": BIR_DEADLINE_OVERRIDES_LIST,
            "description": "The deadline overrides stored on this computer (they apply to every taxpayer): each `id`, `kind` (moved: a deadline moved by the user or a BIR extension; holiday: an official holiday the app ships, which moves whatever falls on it), title, reason, forms, original_deadline, new_deadline and `periods` (the form and period each moved deadline is). `year` lists those whose original or new date falls in it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "year": { "type": "integer", "description": "Only overrides dated in this year." }
                },
                "additionalProperties": false
            },
        }),
        json!({
            "name": BIR_DEADLINE_OVERRIDE_REMOVE,
            "description": "Put a moved deadline back to BIR's calendar, for every taxpayer: `form` with `period` (or `original_deadline`) restores that deadline (an extension that names other forms too keeps them), or `id` (from bir_deadline_overrides_list) removes that one override, a holiday included. Returns what was removed and the due date now. Only on the user's request.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "An override's id from bir_deadline_overrides_list." },
                    "form": { "type": "string", "description": "Form code, e.g. 2551Q." },
                    "period": { "type": "string", "description": "The period: 2026-Q3, 2026-09 or 2025." },
                    "original_deadline": { "type": "string", "description": "Or the deadline's date in BIR's calendar, YYYY-MM-DD." }
                },
                "additionalProperties": false
            },
        }),
    ]
}

/// The payment and filing-record tools ([`PAYMENT_TOOLS`]).
fn payment_tools() -> Vec<Value> {
    vec![
        json!({
            "name": BIR_PAYMENT_SEARCH,
            "description": "The Gmail search (`query`) for payment emails about these taxpayers' BIR returns (all of them without `tins`), from `since` (30 days back): MyEG's \"BIR Transaction Success\" and any BIR payment email (bank, GCash, Maya, LandBank Link.BizPortal) naming their TIN. `awaiting` lists the returns queued, sent or confirmed with no payment recorded yet. Search Gmail with it through the Google Workspace tools; give each email's text to bir_payment_parse, then record it with bir_record_payment.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tins": { "type": "array", "items": { "type": "string" }, "description": "Taxpayer TINs (a 9-digit root covers every branch)." }
                },
                "additionalProperties": false
            },
        }),
        json!({
            "name": BIR_PAYMENT_PARSE,
            "description": "Read one payment email's text into a payment, when bir knows its channel's format (MyEG today): channel, tin (14 digits with the branch), form, period (the return period's end, e.g. 09/30/2026, read as 2026-Q3), paid_at, reference, transaction_number, amount (the tax), fee, total, token, and `record`: the arguments for bir_record_payment (add the email's message_id). Answers null for an email it does not know: then read the email yourself and call bir_record_payment with what it says.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "The email's text (subject and body)." }
                },
                "required": ["text"],
                "additionalProperties": false
            },
        }),
        json!({
            "name": BIR_RECORD_PAYMENT,
            "description": "Record a payment of a return, matched by TIN (with branch), form and period. Use it for a payment email (with `message_id`) or a receipt the user gave you (`proof_path`: the image or PDF's absolute path, e.g. a file the user attached in chat; bir copies it into its own storage). The same message_id, or the same channel and reference for the same return, answers the payment already recorded (adding `proof_path` when it had no receipt); a return marked paid with no details takes these details instead. A payment is kept even when the return is not in BIR, and never changes the return's filing status or confirmation. Returns the payment and the return's status.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tin": { "type": "string", "description": "Taxpayer TIN: 9-digit root (head office) or 14 digits with the branch." },
                    "form": { "type": "string", "description": "Form code, e.g. 2551Q." },
                    "period": { "type": "string", "description": "The return's period: 2026-Q3, 2026-09, 2025, or the return period's last day as payment emails print it (09/30/2026)." },
                    "channel": { "type": "string", "description": "Who took the payment: MyEG, LandBank, GCash, Maya, a bank's name, …" },
                    "reference": { "type": "string", "description": "The payment's reference number." },
                    "amount": { "type": ["number", "string"], "description": "The tax paid in pesos, without fees (e.g. 629.94)." },
                    "fee": { "type": ["number", "string"], "description": "Fees on top (bank and transaction fees), in pesos." },
                    "paid_at": { "type": "string", "description": "When it was paid, e.g. 2026-10-26T18:57:10+08:00 or 10/26/2026 18:57:10 (Manila time)." },
                    "message_id": { "type": "string", "description": "The payment email's Gmail message id." },
                    "proof_path": { "type": "string", "description": "Absolute path of a receipt image or PDF to keep as proof." },
                    "transaction_number": { "type": "string", "description": "The channel's transaction number, if any." },
                    "note": { "type": "string", "description": "A note for the record." },
                    "raw": { "type": "string", "description": "The payment email's text, kept with the record." }
                },
                "required": ["tin", "form", "period", "channel", "amount", "paid_at"],
                "additionalProperties": false
            },
        }),
        json!({
            "name": BIR_MARK_PAID,
            "description": "Mark a return paid on the user's word, with no payment details. `proof_path` (a receipt image or PDF's absolute path) keeps a receipt as its proof; on a return already paid it attaches that receipt to the payment. Without it, a return already paid answers its payment. A payment email recorded later (bir_record_payment) takes the mark's place and keeps its receipt. Proof is recommended: offer to attach a receipt.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tin": { "type": "string", "description": "Taxpayer TIN." },
                    "form": { "type": "string", "description": "Form code, e.g. 2551Q." },
                    "period": { "type": "string", "description": "The return's period, e.g. 2026-Q3." },
                    "note": { "type": "string", "description": "A note, e.g. where it was paid." },
                    "proof_path": { "type": "string", "description": "Absolute path of a receipt image or PDF to keep as proof." }
                },
                "required": ["tin", "form", "period"],
                "additionalProperties": false
            },
        }),
        json!({
            "name": BIR_PAYMENTS_LIST,
            "description": "The recorded payments, newest first: channel, reference, amount, fee, total, paid_at, proof (email, file or none), source, token. `tin` (a 9-digit root covers every branch), `form` and `period` (needs `form`) narrow the list.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tin": { "type": "string", "description": "Taxpayer TIN." },
                    "form": { "type": "string", "description": "Form code." },
                    "period": { "type": "string", "description": "The return's period." }
                },
                "additionalProperties": false
            },
        }),
        json!({
            "name": BIR_FILING_RECORD,
            "description": "Make the printable filing record of a return: the filled form, BIR's filing confirmation (its receipt email, or the filing's state) and the proof of payment (each payment and its receipt image), as one document. Returns `link` (rox://view?path=…: give it to the user to open and print it in Rox's viewer), path and title.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tin": { "type": "string", "description": "Taxpayer TIN." },
                    "form": { "type": "string", "description": "Form code, e.g. 2551Q." },
                    "period": { "type": "string", "description": "The return's period, e.g. 2026-Q3." }
                },
                "required": ["tin", "form", "period"],
                "additionalProperties": false
            },
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIN: &str = "12345678800000";
    const ROOT: &str = "123456788";

    fn server() -> MiniServer {
        crate::bir_module::use_ephemeral_database();
        crate::account_gate::set_unlocked(true);
        MiniServer::with_gate(Catalog::Embedded, Gate::Fixed(License::open()))
    }

    fn call(server: &mut MiniServer, name: &str, arguments: Value) -> Value {
        let reply = server
            .handle(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": name, "arguments": arguments },
            }))
            .wait()
            .expect("a request is answered");
        reply["result"].clone()
    }

    fn save_taxpayer(server: &mut MiniServer) {
        let saved = server
            .host(
                "profile.save",
                json!({"tin": TIN, "name": "Scratch Taxpayer", "rdo_code": "040"}),
            )
            .unwrap();
        assert_eq!(saved["status"], "ok");
    }

    #[test]
    fn initialize_offers_tools_and_resources() {
        let mut server = server();
        let reply = server
            .handle(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))
            .wait()
            .unwrap();
        let capabilities = &reply["result"]["capabilities"];
        assert!(capabilities["tools"].is_object());
        assert!(capabilities["resources"].is_object());
        assert_eq!(reply["result"]["serverInfo"]["name"], SERVER_NAME);
    }

    #[test]
    fn tools_list_has_the_bridge_and_no_discovery_or_review() {
        let listed = tool_list();
        let names: Vec<&str> = listed["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        for wanted in [
            BIR_STATUS,
            BIR_CALL,
            BIR_APP_OPEN,
            "bir_form_open",
            "bir_form_fill",
        ] {
            assert!(names.contains(&wanted), "{wanted} missing from {names:?}");
        }
        for gone in [
            "bir_list_apps",
            "bir_connect",
            "bir_form_review",
            "bir_form_preview",
        ] {
            assert!(!names.contains(&gone), "{gone} must not be offered");
        }
        for wanted in [BIR_RESOLVE, BIR_DRAFTS_LIST] {
            assert!(names.contains(&wanted), "{wanted} missing from {names:?}");
        }
        for wanted in [
            BIR_CALENDAR_GET,
            BIR_CALENDAR_SET,
            BIR_CALENDAR_CHANGES,
            BIR_CALENDAR_LINK,
            BIR_CONFIRMATION_SEARCH,
            BIR_CONFIRM_FILING,
        ] {
            assert!(names.contains(&wanted), "{wanted} missing from {names:?}");
        }
        for wanted in PAYMENT_TOOLS {
            assert!(names.contains(&wanted), "{wanted} missing from {names:?}");
        }
        for wanted in DEADLINE_TOOLS {
            assert!(names.contains(&wanted), "{wanted} missing from {names:?}");
        }
        assert_eq!(
            names.len(),
            HOST_TOOLS.len() + 11 + PAYMENT_TOOLS.len() + DEADLINE_TOOLS.len()
        );
    }

    #[test]
    fn bir_call_returns_text_and_structured_content() {
        let mut server = server();
        let result = call(&mut server, BIR_CALL, json!({"fn": "forms_catalog"}));
        assert_eq!(result["isError"], false);
        let forms = &result["structuredContent"]["result"];
        assert!(forms.as_array().is_some_and(|forms| !forms.is_empty()));
        let text: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(&text, forms);
        let bad = call(
            &mut server,
            BIR_CALL,
            json!({"fn": "forms_catalog", "scope": "12"}),
        );
        assert_eq!(bad["isError"], true);
        let extra = call(
            &mut server,
            BIR_CALL,
            json!({"fn": "forms_catalog", "x": 1}),
        );
        assert_eq!(extra["isError"], true);
    }

    #[test]
    fn bir_call_async_functions_are_answered_later() {
        let mut server = server();
        save_taxpayer(&mut server);
        let reply = server.handle(json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": BIR_CALL,
                "arguments": {"fn": "screen_form_open", "args": [TIN, "0619E", 2026, 1], "scope": ROOT},
            },
        }));
        assert!(matches!(reply, Reply::Later(_)));
        let reply = reply.wait().unwrap();
        assert_eq!(reply["id"], 7);
        assert_eq!(
            reply["result"]["structuredContent"]["result"]["form_code"],
            "0619E"
        );
    }

    #[test]
    fn bir_app_open_names_the_resource_and_the_scope() {
        let mut server = server();
        save_taxpayer(&mut server);
        let result = call(
            &mut server,
            BIR_APP_OPEN,
            json!({"app": "forms", "tin": ROOT}),
        );
        assert_eq!(result["isError"], false, "{result}");
        assert_eq!(
            result["_meta"]["rox/open"],
            json!({"uri": "nsh://bir/forms", "props": {"tin": TIN}})
        );
        assert_eq!(
            result["content"][0]["text"],
            "Opening BIR Forms for Scratch Taxpayer…"
        );
        let plain = call(&mut server, BIR_APP_OPEN, json!({"app": "forms"}));
        assert_eq!(
            plain["_meta"]["rox/open"],
            json!({"uri": "nsh://bir/forms", "props": {}})
        );
        let missing = call(
            &mut server,
            BIR_APP_OPEN,
            json!({"app": "forms", "tin": "999999999"}),
        );
        assert_eq!(missing["isError"], true);
        let unknown = call(&mut server, BIR_APP_OPEN, json!({"app": "settings"}));
        assert_eq!(unknown["isError"], true);
    }

    #[test]
    fn bir_app_open_takes_the_contract_props() {
        let mut server = server();
        save_taxpayer(&mut server);
        let opened = call(
            &mut server,
            BIR_APP_OPEN,
            json!({"app": "forms", "tin": ROOT, "form": "2551Q", "period": "2026-Q3", "action": "submit"}),
        );
        assert_eq!(opened["isError"], false, "{opened}");
        assert_eq!(
            opened["_meta"]["rox/open"]["props"],
            json!({"tin": TIN, "form": "2551Q", "period": "2026-Q3", "action": "submit"})
        );
        // A token's value names the taxpayer and the period.
        let token = call(
            &mut server,
            BIR_APP_OPEN,
            json!({"app": "forms", "form": format!("{ROOT}/2551Q-2026-Q3"), "draft_id": 7}),
        );
        assert_eq!(
            token["_meta"]["rox/open"]["props"],
            json!({"tin": TIN, "form": "2551Q", "period": "2026-Q3", "action": "file", "draft_id": 7})
        );
        let profile = call(
            &mut server,
            BIR_APP_OPEN,
            json!({"app": "tax-profile", "tin": TIN, "branch": "1"}),
        );
        assert_eq!(profile["_meta"]["rox/open"]["uri"], "nsh://bir/tax-profile");
        assert_eq!(
            profile["_meta"]["rox/open"]["props"],
            json!({"tin": ROOT, "branch": "00001"})
        );
        assert_eq!(
            profile["_meta"]["rox/open"]["token"],
            format!("@bir:branchcode:{ROOT}/00001")
        );
        assert!(
            profile["_meta"]["rox/open"]["link"]
                .as_str()
                .unwrap()
                .ends_with("&token=%40bir%3Abranchcode%3A123456788%2F00001")
        );
        assert_eq!(
            opened["_meta"]["rox/open"]["token"],
            format!("@bir:taxform:{ROOT}/2551Q-2026-Q3")
        );
        let calendar = call(&mut server, BIR_APP_OPEN, json!({"app": "calendar", "tin": ROOT}));
        assert_eq!(calendar["_meta"]["rox/open"]["uri"], "nsh://bir/calendar", "{calendar}");
        assert_eq!(calendar["_meta"]["rox/open"]["props"], json!({"tin": TIN}));
        for bad in [
            json!({"app": "tax-profile", "tin": ROOT, "form": "2551Q"}),
            json!({"app": "forms", "tin": ROOT, "period": "2026-Q3"}),
            json!({"app": "forms", "tin": ROOT, "form": "2551Q", "action": "send"}),
            json!({"app": "forms", "tin": ROOT, "form": "1601C", "period": "Q3 2026"}),
            json!({"app": "forms", "form": "2551Q"}),
        ] {
            assert_eq!(
                call(&mut server, BIR_APP_OPEN, bad.clone())["isError"],
                true,
                "{bad}"
            );
        }
    }

    #[test]
    fn bir_resolve_answers_locally_and_remembers_the_current_taxpayer() {
        let mut server = server();
        save_taxpayer(&mut server);
        let result = call(
            &mut server,
            BIR_RESOLVE,
            json!({"name": "taxprofile", "args": ROOT}),
        );
        assert_eq!(result["isError"], false, "{result}");
        let answer = &result["structuredContent"]["result"];
        assert_eq!(answer["ok"], true, "{answer}");
        assert_eq!(answer["token"], format!("@bir:taxprofile:{ROOT}"));
        let form = call(
            &mut server,
            BIR_RESOLVE,
            json!({"name": "taxform", "args": "2551Q Q3 2026 file"}),
        );
        let answer = &form["structuredContent"]["result"];
        assert_eq!(
            answer["token"],
            format!("@bir:taxform:{ROOT}/2551Q-2026-Q3")
        );
        let missing = call(
            &mut server,
            BIR_RESOLVE,
            json!({"name": "taxform", "args": "999999999/2551Q-2026-Q3"}),
        );
        assert_eq!(
            missing["structuredContent"]["result"],
            json!({"ok": false, "error": "No taxpayer with TIN 999-999-999"})
        );
        // A closed license does not stop it: it never leaves this computer.
        server.set_license(License::settled(Err(UNAUTHENTICATED.to_string())));
        let suggested = call(
            &mut server,
            BIR_RESOLVE,
            json!({"name": "taxform", "args": "255", "complete": true}),
        );
        assert!(
            suggested["structuredContent"]["result"]["choices"]
                .as_array()
                .unwrap()
                .iter()
                .any(|choice| choice["value"] == format!("{ROOT}/2551Q")),
            "{suggested}"
        );
        assert_eq!(
            call(&mut server, BIR_RESOLVE, json!({"args": "x"}))["isError"],
            true
        );
    }

    #[test]
    fn the_assistant_cannot_send_without_the_users_grant() {
        let mut server = server();
        save_taxpayer(&mut server);
        // The current taxpayer stands in for a missing TIN, and the token
        // names the return.
        call(
            &mut server,
            BIR_RESOLVE,
            json!({"name": "taxprofile", "args": ROOT}),
        );
        let opened = call(
            &mut server,
            "bir_form_open",
            json!({"code": "@bir:taxform:2551Q-2026-Q1"}),
        );
        assert_eq!(opened["isError"], false, "{opened}");
        let refused = call(&mut server, "bir_form_send", json!({}));
        let text: Value =
            serde_json::from_str(refused["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text["status"], "approval_required");
        assert_eq!(text["queued"], false);
        assert!(
            text["message"].as_str().unwrap().starts_with(
                "Needs you: press Submit in the Forms app, or grant the assistant permission"
            ),
            "{text}"
        );
        // The Forms app's toggle grants it, through the scoped bridge.
        let granted = call(
            &mut server,
            BIR_CALL,
            json!({"fn": "grant_send", "args": [TIN, "2551Q"], "scope": ROOT}),
        );
        assert_eq!(granted["isError"], false, "{granted}");
        let checked = call(&mut server, "bir_form_send", json!({}));
        let text: Value =
            serde_json::from_str(checked["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            text["status"], "invalid",
            "past the grant, the page's Validate decides"
        );
        assert_eq!(text["queued"], false);
        // Revoked, it refuses again.
        call(
            &mut server,
            BIR_CALL,
            json!({"fn": "revoke_send", "args": [TIN, "2551Q"], "scope": ROOT}),
        );
        let again = call(&mut server, "bir_form_send", json!({}));
        let text: Value =
            serde_json::from_str(again["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text["status"], "approval_required");
        let drafts = call(&mut server, BIR_DRAFTS_LIST, json!({"form": "2551Q"}));
        let listed = &drafts["structuredContent"]["result"]["drafts"];
        assert_eq!(listed[0]["period"], "2026-Q1", "{drafts}");
        assert_eq!(
            listed[0]["token"],
            format!("@bir:taxform:{ROOT}/2551Q-2026-Q1")
        );
    }

    #[test]
    fn a_closed_license_stops_model_tools_but_not_the_account_bridge() {
        let mut server = server();
        server.set_license(License::settled(Err(UNAUTHENTICATED.to_string())));
        let refused = call(&mut server, "bir_profiles_list", json!({}));
        assert_eq!(refused["isError"], true);
        assert_eq!(refused["content"][0]["text"], UNAUTHENTICATED);
        let opened = call(&mut server, BIR_APP_OPEN, json!({"app": "forms"}));
        assert_eq!(opened["isError"], true);
        let status = call(&mut server, BIR_CALL, json!({"fn": "cloud_account_status"}));
        assert_eq!(status["isError"], false, "{status}");
    }

    #[test]
    fn resources_list_and_read_over_json_rpc() {
        let mut server = server();
        let listed = server
            .handle(json!({"jsonrpc": "2.0", "id": 1, "method": "resources/list"}))
            .wait()
            .unwrap();
        assert!(
            listed["result"]["resources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|resource| resource["uri"] == "nsh://bir/forms")
        );
        assert!(
            listed["result"]["resources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|resource| resource["uri"] == resolve::MANIFEST_URI)
        );
        let manifest = server
            .handle(json!({"jsonrpc": "2.0", "id": 4, "method": "resources/read", "params": {"uri": resolve::MANIFEST_URI}}))
            .wait()
            .unwrap();
        assert_eq!(
            manifest["result"]["contents"][0]["mimeType"],
            "application/json"
        );
        let read = server
            .handle(json!({"jsonrpc": "2.0", "id": 2, "method": "resources/read", "params": {"uri": "nsh://bir/forms"}}))
            .wait()
            .unwrap();
        assert_eq!(read["result"]["contents"][0]["mimeType"], apps::MIME_TYPE);
        let missing = server
            .handle(json!({"jsonrpc": "2.0", "id": 3, "method": "resources/read", "params": {"uri": "nsh://bir/none"}}))
            .wait()
            .unwrap();
        assert_eq!(missing["error"]["code"], INVALID_PARAMS);
    }
}
