//! MCP for `bir-mini-app`: JSON-RPC 2.0 over stdio or the loopback HTTP door.
//!
//! Every tool runs in this process. The form, profile and dues tools are
//! bir-mcp's descriptions and argument checks over bir-shell's agent
//! [`Session`]; the `bir_app_*` tools are the mini apps' named calls into
//! the `bir` host module ([`v1::app_tools`]); `bir_app_open` asks Rox to
//! open one of the TSX apps this binary ships as resources; `bir_resolve`
//! answers the commands and `@` scopes of the `rox://manifest` resource
//! ([`v1::resolve`] over [`resolve`]); `bir_drafts_list` lists a
//! taxpayer's unsubmitted returns. There is no app discovery and no connect
//! step: this process owns the database.
//!
//! Rox speaks the Mini App Protocol v1 ([`v1`]): `rox.provision` first, then
//! every call carries its caller (`_meta["rox/caller"]`), and [`MiniServer::authorize`]
//! holds each origin to what it may do before a tool runs.

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
use super::v1::{self, Caller, Origin, app_tools, manifest as v1_manifest};
use super::{answers, calendar, cards, confirm, lookup, payments, plain, resolve, return_view, scope, taxpayer};
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
the taxpayer database and files queued returns in the background. Taxpayers, branches and returns \
reach you as tickets (tkt_...), never as TINs: a token @bir:taxprofile:<ticket>, \
@bir:branchcode:<ticket> or @bir:taxform:<ticket> names one, and BIR's answers name taxpayers by \
ticket too. Pass a ticket wherever a tool takes `tin` (or in `tins`); a return's ticket also goes \
in bir_form_open's `code` and bir_app_open's `form`. Never ask the user for a TIN and never write \
one out; use the taxpayer's name when you talk about them. To let the user work visually, call \
bir_app_open: `forms` to prepare and file returns, `tax-profile` for a taxpayer's registration, \
`calendar` for the filing schedule, `deadlines` for a small card of the next returns due, \
`register` to sign in to Buwiz. Pass `tin` (a ticket) to open it for one taxpayer; Rox shows the \
user an Open button. You can read taxpayer profiles and dues and draft a return yourself with the \
bir_form_* tools: open, read fields and context, fill with a stated source, list what needs the \
user, validate, save the draft, dismiss. Only one form is open at a time. bir_drafts_list shows a \
taxpayer's unsubmitted returns. To answer what forms a taxpayer is registered for or what returns \
they have filed, call bir_profile_forms or bir_returns_list with the ticket from @bir:taxprofile; \
do not open an app unless the user asks to. Sending (bir_form_send) needs the user's standing OK for that \
taxpayer and form; without it the user presses Submit in the Forms app themselves. The tools \
whose description starts with \"Mini apps only\" and bir_resolve belong to the apps and to Rox: do \
not call them. The form calendar: bir_calendar_get lists returns and due dates with their status, \
bir_calendar_set turns forms on or off the calendar; to sync it to Google Calendar, check \
bir_calendar_changes, create, update or delete each event with the Google Workspace tools, then \
call bir_calendar_link with the change's key. BIR confirmation emails: search mail with \
bir_confirmation_search's query, read each email (file name, date and time received by BIR) and \
call bir_confirm_filing, then report. Payments: read each payment email with bir_payment_parse \
(null for a format bir does not know: read it yourself) and call bir_record_payment. A receipt the \
user attaches is recorded with its local path as `proof_path`; bir_mark_paid marks a return paid \
with no proof (offer to attach one). Payment is separate from BIR's filing confirmation. \
bir_filing_record makes the printable record of a return. Deadlines: a deadline the user moves is \
a rule for every taxpayer: bir_deadline_override_set moves one form's deadline for one period, \
bir_deadline_overrides_list shows what is moved, bir_deadline_override_remove puts one back. Only \
on the user's request.";

const BIR_STATUS: &str = "bir_status";
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
const BIR_PAYMENT_RECEIPT: &str = "bir_payment_receipt";
/// The payment and filing-record tools, as many as `tool_list` adds.
#[cfg(test)]
const PAYMENT_TOOLS: [&str; 7] = [
    BIR_PAYMENT_SEARCH,
    BIR_PAYMENT_PARSE,
    BIR_RECORD_PAYMENT,
    BIR_MARK_PAID,
    BIR_PAYMENTS_LIST,
    BIR_FILING_RECORD,
    BIR_PAYMENT_RECEIPT,
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
    v1: v1::Host,
    /// Send grants the user gave on an approve card, for the next send only.
    once_grants: Vec<(String, String)>,
    /// Spreadsheets an import card read, by import id, until it fills.
    imports: Vec<(String, return_view::Imported)>,
}

impl MiniServer {
    /// The stdio server: the debug test bypass, a `BUWIZ_ACCESS_TOKEN`
    /// agent token, or the desktop account, in that order. `rox` (stdio, as
    /// Rox spawns it) answers nothing before `rox.provision`.
    pub(crate) fn new(catalog: Catalog, rox: bool) -> Self {
        let gate = if crate::account_gate::auth_bypass_enabled() {
            Gate::Fixed(License::open())
        } else if let Some(token) = env_nonempty("BUWIZ_ACCESS_TOKEN") {
            let base = env_nonempty("BUWIZ_MCP_URL")
                .unwrap_or_else(|| signin::builtin_origin().to_string());
            Gate::Fixed(License::required(base, token))
        } else {
            Gate::Desktop
        };
        Self::with_gate(catalog, gate, rox)
    }

    fn with_gate(catalog: Catalog, gate: Gate, rox: bool) -> Self {
        let commands = UiQueue::new();
        Self {
            session: Session::named(commands.clone(), SERVER_NAME, MODE),
            commands,
            bridge: Bridge::new(),
            catalog,
            gate,
            v1: v1::Host::new(rox),
            once_grants: Vec::new(),
            imports: Vec::new(),
        }
    }

    /// A server on this thread's scratch database, licensed and signed in.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        crate::bir_module::use_ephemeral_database();
        crate::account_gate::set_unlocked(true);
        Self::with_gate(Catalog::Embedded, Gate::Fixed(License::open()), false)
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
                        resources.push(manifest_descriptor());
                    }
                    listed
                })
                .map_err(|error| (INVALID_PARAMS, error)),
            "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
            "resources/read" => match params.get("uri").and_then(Value::as_str) {
                Some(v1_manifest::MANIFEST_URI) => v1_manifest::served_text(&self.catalog)
                    .map(|text| {
                        json!({ "contents": [{
                            "uri": v1_manifest::MANIFEST_URI,
                            "mimeType": v1_manifest::MANIFEST_MIME,
                            "text": text,
                        }] })
                    })
                    .map_err(|error| (INVALID_PARAMS, error)),
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
        // Reserved tools first; an unknown `rox.*` is method-not-found (7.9).
        if name.starts_with("rox.") {
            return match self.v1.reserved(&self.catalog, name, arguments) {
                None => Err((METHOD_NOT_FOUND, format!("{name} is not a tool of this plugin"))),
                Some(Err(message)) => Err((INVALID_REQUEST, message)),
                Some(Ok(result)) => Ok(now(result)),
            };
        }
        // P7: nothing before a successful provision.
        if !self.v1.ready() {
            return Err((INVALID_REQUEST, "rox.provision must come first".into()));
        }
        let caller = Caller::of(params);
        if let Err(message) = self.authorize(name, &caller) {
            return Ok(now(v1::fail("permission_denied", &message)));
        }
        if is_app_only(name) {
            return Ok(match self.app_tool(name, arguments) {
                Reply::Now(Some(mut result)) => {
                    plain::rewrite(name, &mut result, true);
                    now(result)
                }
                Reply::Later(run) => {
                    let tool = name.to_string();
                    Reply::Later(Box::new(move || {
                        let mut result = run();
                        plain::rewrite(&tool, &mut result, true);
                        result
                    }))
                }
                other => other,
            });
        }
        let asked = arguments.clone();
        let mut arguments = arguments.clone();
        let has_ticket = Value::Object(arguments.clone()).to_string().contains("tkt_");
        if (has_ticket || caller.origin == Origin::App)
            && let Err(message) = database(|db| v1::expand(db, name, &mut arguments, caller.origin))
        {
            return Ok(now(tool_error(message)));
        }
        let upload = match self.take_upload(name, &mut arguments, caller.origin) {
            Ok(upload) => upload,
            Err(message) => return Ok(now(tool_error(format!("{name}: {message}")))),
        };
        // The assistant does not mark a return paid: it offers the card the
        // user presses Save on (P5, spec 9.8).
        let reply = if name == BIR_MARK_PAID && caller.origin == Origin::Agent {
            Ok(now(cards::mark_paid_card(&arguments)))
        } else if name == BIR_PAYMENT_RECEIPT {
            Ok(now(self.payment_receipt(&arguments, caller.origin == Origin::App)))
        } else {
            self.dispatch(name, &arguments)
        };
        if let Some(path) = upload {
            let _ = std::fs::remove_file(path);
        }
        match reply {
            Ok(Reply::Now(Some(mut result))) => {
                // An "allow this once" lasts until the next send.
                if name == "bir_form_send" && !result.to_string().contains("approval_required") {
                    self.end_once_grants();
                }
                // Signed out or offline, in plain words.
                plain::rewrite(name, &mut result, caller.origin == Origin::App);
                // The answer's card (spec 9.8), for the assistant's answers.
                if caller.origin == Origin::Agent {
                    cards::decorate(name, &asked, &mut result);
                }
                // What the model or an app reads names taxpayers by ticket.
                // A receipt's page is for Rox's viewer (rox.preview) and
                // keeps the payment email as it reads.
                let viewer_page = name == BIR_PAYMENT_RECEIPT && caller.origin == Origin::App;
                if matches!(caller.origin, Origin::Agent | Origin::App) && !viewer_page {
                    let today = taxform::today();
                    let _ = database(|db| {
                        let mut scrubber = v1::Scrubber::new(db, today);
                        scrubber.result(&mut result);
                        if let Some(props) = result.pointer_mut("/_meta/rox~1open/props") {
                            scrubber.value(props);
                        }
                        let roots: Vec<String> = resolve::branches(db, None)
                            .unwrap_or_default()
                            .into_iter()
                            .map(|branch| branch.root)
                            .collect();
                        cards::redact(&mut result, &roots);
                        Ok(())
                    });
                    cards::fit(&mut result);
                }
                Ok(now(result))
            }
            other => other,
        }
    }

    /// `bir_payment_receipt`: for an app (Rox's `rox.preview`), the receipt
    /// page as an embedded text/html resource with its title and Gmail
    /// link; for the assistant, the return's payments without the page (its
    /// receipt card offers View receipt).
    fn payment_receipt(&mut self, arguments: &Map<String, Value>, for_app: bool) -> Value {
        if let Err(message) = self.allow() {
            return tool_error(message);
        }
        let today = taxform::today();
        match database(|db| payments::receipt(db, arguments, today)) {
            Err(message) => tool_error(format!("{BIR_PAYMENT_RECEIPT}: {message}")),
            Ok(receipt) if for_app => {
                let form = receipt.summary["form"].as_str().unwrap_or("receipt").to_string();
                let mut result = app_tools::preview_result(&form, receipt.html, receipt.title);
                result["structuredContent"]["gmail_url"] = receipt.summary["gmail_url"].clone();
                result
            }
            Ok(receipt) => call_result(Ok(receipt.summary)),
        }
    }

    /// The approve card's "Allow this once" (`bir_card_allow_send {tin:
    /// ticket, form}`): a send grant for that taxpayer and form that ends
    /// with the next send, whatever it answers. The user pressed it; the
    /// assistant cannot.
    fn allow_send_once(&mut self, arguments: &Map<String, Value>) -> Result<Value, String> {
        let form = arguments
            .get("form")
            .and_then(Value::as_str)
            .and_then(taxform::form_code_of)
            .ok_or("`form` must be a BIR form code")?;
        let ticket = arguments.get("tin").and_then(Value::as_str).ok_or("`tin` must be the taxpayer's ticket")?;
        let tin = database(|db| {
            let subject = app_tools::subject_of(db, ticket)?;
            let tin = subject.full_tin();
            db.grant_send(&tin, form).map_err(|error| error.to_string())?;
            Ok(tin)
        })?;
        self.once_grants.push((tin, form.to_string()));
        Ok(json!({ "allowed": true, "form": form, "once": true }))
    }

    /// The outbox card's Cancel (`bir_card_cancel_send {ticket}`): the
    /// queued return taken out of the sending queue, back to Draft, the way
    /// BIR Forms cancels it (`cancel_queued`).
    fn cancel_send(&mut self, arguments: &Map<String, Value>) -> Result<Value, String> {
        let today = taxform::today();
        let args = database(|db| answers::queued_row(db, arguments, today))?;
        match self.bridge.call("cancel_queued", args, None) {
            Call::Ready(result) => result?,
            Call::Pending(run) => run()?,
        };
        Ok(json!({ "cancelled": true }))
    }

    /// The attach card's Upload (`bir_card_attach`): the uploaded bytes
    /// kept as one of the return's attachments, next to the receipts.
    fn card_attach(&mut self, arguments: &Map<String, Value>) -> Result<Value, String> {
        let upload = arguments.get("upload").and_then(Value::as_str).ok_or("`upload` is required")?;
        let bytes = self.v1.uploads.take(upload).ok_or("`upload` is not an upload in progress")?;
        let dir = payments::receipts_dir().parent().map(|dir| dir.join("attachments")).unwrap_or_else(|| "attachments".into());
        let mut rest = arguments.clone();
        rest.remove("upload");
        let today = taxform::today();
        database(|db| return_view::attach(db, &rest, &bytes, &dir, today))
    }

    /// The import card's read (`bir_card_import`): rows read and skipped;
    /// the total is kept for its Fill under a new import id.
    fn card_import(&mut self, arguments: &Map<String, Value>) -> Result<Value, String> {
        let upload = arguments.get("upload").and_then(Value::as_str).ok_or("`upload` is required")?;
        let bytes = self.v1.uploads.take(upload).ok_or("`upload` is not an upload in progress")?;
        let mut rest = arguments.clone();
        rest.remove("upload");
        let today = taxform::today();
        let (mut answer, imported) = database(|db| return_view::import_read(db, &rest, &bytes, today))?;
        let mut stamp = [0u8; 8];
        getrandom::fill(&mut stamp).map_err(|error| error.to_string())?;
        let id = format!("imp_{}", hex::encode(stamp));
        if self.imports.len() >= 4 {
            self.imports.remove(0);
        }
        self.imports.push((id.clone(), imported));
        answer["import"] = json!(id);
        Ok(answer)
    }

    /// The import card's Fill (`bir_card_import_fill {ticket, import}`).
    fn card_import_fill(&mut self, arguments: &Map<String, Value>) -> Result<Value, String> {
        let id = arguments.get("import").and_then(Value::as_str).ok_or("`import` is required")?;
        let today = taxform::today();
        let mut target_args = arguments.clone();
        target_args.remove("import");
        let value = database(|db| return_view::target(db, &target_args, today).map(|target| target.value()))?;
        let at = self
            .imports
            .iter()
            .position(|(known, imported)| known == id && imported.value == value)
            .ok_or("this import is gone: pick the file again")?;
        let filled = database(|db| return_view::import_fill(db, &self.imports[at].1, today))?;
        self.imports.remove(at);
        Ok(filled)
    }

    /// Take back the "allow this once" grants.
    fn end_once_grants(&mut self) {
        let grants = std::mem::take(&mut self.once_grants);
        if grants.is_empty() {
            return;
        }
        let _ = database(|db| {
            for (tin, form) in &grants {
                db.revoke_send(tin, form).map_err(|error| error.to_string())?;
            }
            Ok(())
        });
    }

    /// Whether `caller` may call `name` at all (P1, P5, P6): app-only tools
    /// answer apps, an app calls only what its manifest entry lists and only
    /// from an open this process certified, and resolve and routine calls
    /// never cause side effects.
    fn authorize(&self, name: &str, caller: &Caller) -> Result<(), String> {
        if matches!(caller.origin, Origin::Resolve | Origin::Routine)
            && name != BIR_RESOLVE
            && !read_only(name)
        {
            return Err(format!(
                "{name} changes BIR data, so it does not run for a resolve or routine call"
            ));
        }
        if caller.origin != Origin::App {
            return if is_app_only(name) {
                Err(format!("{name} is for BIR's mini apps only"))
            } else {
                Ok(())
            };
        }
        let app = caller.app.as_deref().ok_or("an app call names no app")?;
        if self.v1.provisioned() {
            let open = caller.open_id.as_deref().unwrap_or("");
            if self.v1.session_app(open) != Some(app) {
                return Err("this app open was not certified by BIR".into());
            }
        }
        let allowed = v1_manifest::served_app(&self.catalog, app)?
            .and_then(|entry| entry["permissions"]["tools"].as_array().cloned())
            .unwrap_or_default();
        if allowed.iter().any(|tool| tool == name) {
            Ok(())
        } else {
            Err(format!("{name} is not in the {app} app's permissions"))
        }
    }

    /// A `bir_app_*`, ticket or upload tool, for an app.
    fn app_tool(&mut self, name: &str, arguments: &Map<String, Value>) -> Reply {
        let today = taxform::today();
        match name {
            app_tools::TICKET_SUBJECT => now(call_result(database(|db| {
                app_tools::ticket_subject(db, arguments, today)
            }))),
            app_tools::TICKET_ISSUE => now(call_result(database(|db| {
                app_tools::ticket_issue(db, arguments, today)
            }))),
            app_tools::UPLOAD => now(call_result(self.v1.uploads.append(arguments))),
            app_tools::PRINT_PREVIEW => now(print_preview(arguments, today)),
            app_tools::ALLOW_SEND => now(call_result(self.allow_send_once(arguments))),
            answers::ADD_TAXPAYER => now(call_result(database(|db| answers::add_taxpayer(db, arguments)))),
            answers::CANCEL_SEND => now(call_result(self.cancel_send(arguments))),
            return_view::ATTACH => now(call_result(self.card_attach(arguments))),
            return_view::IMPORT => now(call_result(self.card_import(arguments))),
            return_view::IMPORT_FILL => now(call_result(self.card_import_fill(arguments))),
            _ => {
                let request = match app_tools::request(name, arguments) {
                    Ok(request) => request,
                    Err(message) => return now(tool_error(format!("{name}: {message}"))),
                };
                // The ticket is read, then the lock let go: the host function
                // takes the database itself.
                let prepared = if app_tools::needs_database(&request) {
                    match database(|db| app_tools::prepare(db, &request)) {
                        Ok(prepared) => Some(prepared),
                        Err(message) => return now(tool_error(format!("{name}: {message}"))),
                    }
                } else {
                    None
                };
                let scope = prepared.as_ref().and_then(|prepared| prepared.scope.clone());
                match self.bridge.call(request.function, request.args.clone(), scope.as_deref()) {
                    Call::Ready(Ok(value)) => now(call_result(match &prepared {
                        Some(prepared) => {
                            database(|db| app_tools::finish(db, &request, prepared, value))
                        }
                        None => Ok(value),
                    })),
                    Call::Ready(Err(message)) => now(call_result(Err(message))),
                    Call::Pending(run) => Reply::Later(Box::new(move || call_result(run()))),
                }
            }
        }
    }

    /// `bir_mark_paid` from the Forms app: the receipt it uploaded
    /// (`proof_upload`, `proof_name`) written where `proof_path` reads it.
    /// An app never names a file path itself. The temporary file's path
    /// comes back so it is removed after the call.
    fn take_upload(
        &mut self,
        name: &str,
        arguments: &mut Map<String, Value>,
        origin: Origin,
    ) -> Result<Option<std::path::PathBuf>, String> {
        if name != BIR_MARK_PAID {
            return Ok(None);
        }
        if origin == Origin::App && arguments.contains_key("proof_path") {
            return Err("an app attaches a receipt with proof_upload, not a path".into());
        }
        let Some(id) = arguments.remove("proof_upload") else {
            arguments.remove("proof_name");
            return Ok(None);
        };
        let file_name = arguments
            .remove("proof_name")
            .and_then(|name| name.as_str().map(str::to_string))
            .unwrap_or_default();
        let ext = std::path::Path::new(&file_name)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .ok_or("`proof_name` must be the picked file's name, with its extension")?;
        let bytes = id
            .as_str()
            .and_then(|id| self.v1.uploads.take(id))
            .ok_or("`proof_upload` is not an upload in progress")?;
        let mut stamp = [0u8; 8];
        getrandom::fill(&mut stamp).map_err(|error| error.to_string())?;
        let path = std::env::temp_dir().join(format!("bir-receipt-{}.{ext}", hex::encode(stamp)));
        std::fs::write(&path, bytes).map_err(|error| format!("cannot keep the receipt: {error}"))?;
        arguments.insert("proof_path".into(), Value::String(path.display().to_string()));
        Ok(Some(path))
    }

    /// One tool of this server's own, its arguments already in TINs.
    fn dispatch(&mut self, name: &str, arguments: &Map<String, Value>) -> Result<Reply, (i64, String)> {
        match name {
            BIR_STATUS => Ok(now(self.status())),
            BIR_APP_OPEN => Ok(now(self.app_open(arguments))),
            BIR_RESOLVE => Ok(now(self.resolve(arguments))),
            BIR_DRAFTS_LIST => Ok(now(self.drafts_list(arguments))),
            BIR_CALENDAR_GET => Ok(now(self.on_database(name, arguments, calendar::get))),
            taxpayer::PROFILE_FORMS => Ok(now(self.on_database(name, arguments, taxpayer::profile_forms))),
            answers::FIND_TAXPAYER => Ok(now(self.on_database(name, arguments, answers::find_taxpayer))),
            answers::TAXPAYER => Ok(now(self.on_database(name, arguments, answers::taxpayer))),
            answers::PENALTY => Ok(now(self.on_database(name, arguments, answers::penalty))),
            answers::UNPAID => Ok(now(self.on_database(name, arguments, answers::unpaid))),
            answers::BOARD => Ok(now(self.on_database(name, arguments, answers::board))),
            answers::QUICK_ADD => Ok(now(self.on_database(name, arguments, answers::quick_add))),
            answers::OUTBOX => Ok(now(self.on_database(name, arguments, answers::outbox))),
            answers::DIGEST => Ok(now(self.on_database(name, arguments, answers::digest))),
            lookup::FORM_INFO => Ok(now(self.on_database(name, arguments, lookup::form_info))),
            lookup::LOOKUP => Ok(now(self.on_database(name, arguments, lookup::lookup))),
            return_view::RETURN => Ok(now(self.on_database(name, arguments, return_view::view))),
            calendar::DEADLINE => Ok(now(self.on_database(name, arguments, calendar::deadline))),
            taxpayer::RETURNS_LIST => Ok(now(self.on_database(name, arguments, taxpayer::returns_list))),
            BIR_CALENDAR_SET => Ok(now(self.on_database(name, arguments, |db, arguments, _| {
                calendar::set(db, arguments)
            }))),
            BIR_CALENDAR_CHANGES => Ok(now(self.on_database(name, arguments, calendar::changes))),
            BIR_CALENDAR_LINK => Ok(now(self.on_database(name, arguments, |db, arguments, _| {
                calendar::link(db, arguments)
            }))),
            BIR_CONFIRMATION_SEARCH => Ok(now(routine_query(self.on_database(name, arguments, confirm::search)))),
            BIR_CONFIRM_FILING => Ok(now(self.on_database(name, arguments, confirm::confirm))),
            // Payments and the printable filing record.
            BIR_PAYMENT_SEARCH => Ok(now(routine_query(self.on_database(name, arguments, payments::search)))),
            BIR_PAYMENT_PARSE => Ok(now(self.on_database(name, arguments, |db, arguments, today| {
                payments::parse(arguments, today).map(|parsed| payments::with_recorded(db, parsed, today))
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
            "plugin_version": v1_manifest::VERSION,
        });
        if let Some(reason) = reason {
            status["reason"] = json!(reason);
        }
        // For the health card: the Buwiz server it signs in with, and (once
        // the database is open) the taxpayers and the sending queue.
        if let Ok((origin, _)) = signin::configured_origins() {
            status["server"] = json!(origin.trim_start_matches("https://").trim_start_matches("http://"));
        }
        if signed_in {
            let _ = database(|db| {
                let queue = db.list_all_queued_submissions().map_err(|error| error.to_string())?;
                status["queue"] = json!({
                    "waiting": queue.iter().filter(|row| row.status == bir_core::forms::FilingStatus::Queued).count(),
                    "sent_unconfirmed": queue.iter().filter(|row| row.status == bir_core::forms::FilingStatus::Submitted).count(),
                });
                status["taxpayers"] = json!(resolve::branches(db, None)?.iter().filter(|branch| branch.is_head_office).count());
                Ok(())
            });
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
        // the card names what opens: the return, or whose profile
        let (props, text, title) = match (id, form, tin) {
            ("forms", Some(form), tin) => {
                let target = database(|db| {
                    resolve::app_return(db, tin.as_deref(), &form, period.as_deref(), action, today)
                })?;
                let mut props = target.props();
                if let Some(draft_id) = draft_id {
                    props["draft_id"] = json!(draft_id);
                }
                (props, format!("Opening {} on {}…", app.title, target.title), format!("BIR Form {}", target.title))
            }
            (_, _, None) => (json!({}), format!("Opening {}…", app.title), app.title.clone()),
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
                (props, format!("Opening {} for {name}…", app.title), format!("{} · {name}", app.title))
            }
            (_, _, Some(tin)) => {
                let (row, _) = self.taxpayer(&tin)?;
                let name = row["name"].as_str().unwrap_or("this taxpayer").to_string();
                (
                    json!({ "tin": row["tin"].clone() }),
                    format!("Opening {} for {name}…", app.title),
                    format!("{} · {name}", app.title),
                )
            }
        };
        let opened = database(|db| v1::v1_props(db, id, &props, today))?;
        let placement = if id == "deadlines" { "inline" } else { "full" };
        Ok(json!({
            "content": [{ "type": "text", "text": text }],
            "isError": false,
            "_meta": { "rox/open": v1::open_meta(id, &title, &opened, placement) },
        }))
    }

    /// `bir_resolve`: Rox's resolver for the manifest's commands and
    /// scopes, answering spec 9.2's `result` shapes in `structuredContent`.
    /// Local only: the account gate and a database read, never the license
    /// check (which may refresh a token over the network).
    fn resolve(&mut self, arguments: &Map<String, Value>) -> Value {
        let today = taxform::today();
        let answer = crate::bir_module::with_database(|db| Ok(v1::resolve(db, arguments, today)))
            .unwrap_or_else(|error| json!({ "result": "error", "message": error.message() }));
        v1::ok(answer)
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

/// `bir_app_print_preview`: the return's print sheet (or a sent return's
/// filing record) as the HTML document Rox previews, made in memory. The
/// ticket is read, then the lock let go: the sheet takes the database
/// itself. The page keeps its TINs and names as printed: it goes to Rox's
/// viewer, never to the app or the model.
fn print_preview(arguments: &Map<String, Value>, today: chrono::NaiveDate) -> Value {
    let form = arguments.get("form").and_then(Value::as_str).unwrap_or_default().to_string();
    let page = database(|db| app_tools::preview_ask(db, arguments)).and_then(|ask| match ask {
        app_tools::PreviewAsk::Draft(args) => {
            let arguments = gpui_shell::HostArguments::new(args.into_iter().map(crate::bir_module::json));
            crate::screen_form_module::print_preview_page(&arguments).map_err(|error| error.message().to_string())
        }
        app_tools::PreviewAsk::Period { tin, form, year, quarter } => crate::bir_module::with_database(|db| {
            let (fields, receipt) = crate::bir_module::print_fields(db, &tin, &form, year, Some(quarter))?;
            let slug = crate::bir_module::print_slug_for(&form).ok_or_else(|| HostError::new(format!("Form {form} has no print sheet")))?;
            let name = db.get_profile_by_tin(&tin).ok().flatten().map(|profile| profile.full_name.clone());
            crate::bir_module::print_preview_page(slug, &form, &fields, receipt.as_ref(), name)
        })
        .map_err(|error| error.message().to_string()),
        app_tools::PreviewAsk::Record(args) => database(|db| payments::filing_record_page(db, &args, today)).map(|record| {
            let title = format!("BIR Form {} \u{2014} {} \u{b7} filing record", record.form, record.taxpayer);
            (record.html, title)
        }),
    });
    match page {
        Ok((html, title)) => app_tools::preview_result(&form, html, title),
        Err(message) => tool_error(format!("{}: {message}", app_tools::PRINT_PREVIEW)),
    }
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

/// A routine takes its search text from `structuredContent.query` (spec
/// 5.9 step 1), but BIR's answers nest everything under `result`: the
/// search tools carry `query` at the top as well.
fn routine_query(mut result: Value) -> Value {
    if let Some(query) = result.pointer("/structuredContent/result/query").cloned() {
        result["structuredContent"]["query"] = query;
    }
    result
}

fn now(result: Value) -> Reply {
    Reply::Now(Some(result))
}

/// Tools that answer BIR's mini apps only (`bir_app_open` is the
/// assistant's, not one of them).
fn is_app_only(name: &str) -> bool {
    app_tools::function_of(name).is_some()
        || [app_tools::TICKET_SUBJECT, app_tools::TICKET_ISSUE, app_tools::UPLOAD, app_tools::PRINT_PREVIEW, app_tools::ALLOW_SEND]
            .contains(&name)
        || CARD_TOOLS.contains(&name)
}

/// The answer cards' own app-only tools (spec 9.8): the buttons the user
/// presses that change something, and the import card's read.
const CARD_TOOLS: [&str; 5] = [
    answers::ADD_TAXPAYER,
    answers::CANCEL_SEND,
    return_view::ATTACH,
    return_view::IMPORT,
    return_view::IMPORT_FILL,
];

/// This server's tools that change nothing (`readOnlyHint`, P8): the ones a
/// routine or resolve call may run, and the reads a routine may name.
const READ_ONLY: [&str; 18] = [
    taxpayer::PROFILE_FORMS,
    taxpayer::RETURNS_LIST,
    BIR_STATUS,
    BIR_DRAFTS_LIST,
    BIR_CALENDAR_GET,
    BIR_CALENDAR_CHANGES,
    BIR_CONFIRMATION_SEARCH,
    BIR_PAYMENT_SEARCH,
    BIR_PAYMENT_PARSE,
    BIR_PAYMENTS_LIST,
    BIR_PAYMENT_RECEIPT,
    BIR_DEADLINE_OVERRIDES_LIST,
    "bir_profiles_list",
    "bir_dues_list",
    "bir_form_fields",
    "bir_form_context",
    "bir_form_needs_you",
    "bir_form_validate",
];

fn read_only(name: &str) -> bool {
    READ_ONLY.contains(&name)
        || answers::TOOLS.contains(&name)
        || lookup::TOOLS.contains(&name)
        || [return_view::RETURN, return_view::IMPORT, calendar::DEADLINE].contains(&name)
        || app_tools::read_only(name)
}

/// The manifest's `resources/list` entry.
fn manifest_descriptor() -> Value {
    json!({
        "uri": v1_manifest::MANIFEST_URI,
        "name": "manifest",
        "title": "BIR plugin manifest",
        "description": "The Rox Mini App Protocol v1 manifest: identity, commands, scopes, routines and apps.",
        "mimeType": v1_manifest::MANIFEST_MIME,
    })
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
            "description": "Report this BIR host: headless mode, whether it uses a test or the REAL taxpayer database, whether this computer is signed in to Buwiz (and the Buwiz server), how many taxpayers are saved and what waits in the sending queue. Use it for \"is BIR working?\".",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        }),
        json!({
            "name": BIR_APP_OPEN,
            "description": "Open one of BIR's mini apps for the user in Rox (Rox shows an Open button; nothing opens by itself): `forms` (prepare, validate, save and queue returns), `tax-profile` (a taxpayer's registration and yearly forms), `calendar` (the filing schedule, month by month; open it when the user asks to see the schedule), `deadlines` (a small card of the next returns due) or `register` (sign in to Buwiz on this computer; works signed out). With `tin` (a taxpayer's ticket) the app opens for that taxpayer only; without it, the app lets the user pick. `forms` with `form` opens one return: `form` is a code (2551Q) or a return's ticket; `period` names its period (2026-Q3, jan 2026, 2025; default: an unsubmitted draft of that form, else the return to file now); `action` is view (read-only), file (the default) or submit (validate, then the user presses Submit); `draft_id` comes from bir_drafts_list. Without `tin` a return is for the current taxpayer. If a tool says Buwiz sign-in is required, open `register`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "app": { "type": "string", "enum": apps::OPENABLE },
                    "tin": { "type": "string", "description": "The taxpayer's ticket (from a token or a BIR answer)." },
                    "form": { "type": "string", "description": "forms only: a form code (2551Q) or a return's ticket." },
                    "period": { "type": "string", "description": "forms only: the return's period, e.g. 2026-Q3, jan 2026, 2026-01, 2025." },
                    "action": { "type": "string", "enum": ["view", "file", "submit"], "description": "forms only: view (read-only), file (fill it; default) or submit (validate and show the user the Submit button)." },
                    "draft_id": { "type": "integer", "description": "forms only: a saved return's id from bir_drafts_list." },
                    "branch": { "type": "string", "description": "tax-profile only: a branch code, e.g. 00001 (or pass the branch's ticket as `tin`)." }
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
        "name": BIR_RESOLVE,
        "description": "Rox's resolver for the commands and @ scopes in the rox://manifest resource (/taxprofile, /taxform, @bir:taxprofile:, @bir:branchcode:, @bir:taxform:). Rox calls it as the user types; it reads only this computer's database and answers with tickets. Do not call it yourself.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "kind": { "type": "string", "enum": ["command", "scope"] },
                "name": { "type": "string", "enum": ["taxprofile", "branchcode", "taxform"], "description": "The command or scope name." },
                "value": { "type": "string", "description": "The text after `/name ` or after `@bir:name:`; may be empty." },
                "complete": { "type": "boolean", "default": false, "description": "True for suggestions (at most 20) for a partial `value`." },
                "context": {
                    "type": "object",
                    "properties": { "attached": { "type": "array", "items": { "type": "object" } } }
                }
            },
            "required": ["name"],
            "additionalProperties": false
        },
    }));
    list.extend(taxpayer::descriptors());
    list.extend(answers::descriptors());
    list.extend(lookup::descriptors());
    list.push(return_view::descriptor());
    list.push(json!({
        "name": calendar::DEADLINE,
        "description": "One deadline explained: the date BIR's rule gives (`official`), the date that applies (`applies`) and why they differ (a weekend or holiday moves it to the next working day; a BIR advisory or the user moved it), and a move saved in BIR (`saved_move`). Use it for \"is the 2551Q deadline really Sunday?\". `form`, `period` (default the latest one closed), `tin` (a taxpayer's ticket, for their own rules). Rox shows a card where the user can move it or put it back.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "form": { "type": "string", "description": "Form code, e.g. 2551Q." },
                "period": { "type": "string", "description": "e.g. 2026-Q3, 2026-09." },
                "tin": { "type": "string", "description": "The taxpayer's ticket." }
            },
            "required": ["form"],
            "additionalProperties": false
        },
    }));
    list.extend(app_tools::descriptors());
    list.extend(return_view::app_descriptors());
    list.push(json!({
        "name": answers::ADD_TAXPAYER,
        "description": "Mini apps only (refused for the assistant): the quickadd card's Save, adding the taxpayer whose TIN the card's ticket names.",
        "inputSchema": {
            "type": "object",
            "properties": { "ticket": { "type": "string" }, "name": { "type": "string" }, "type": { "type": "string" }, "rdo": { "type": "string" } },
            "required": ["ticket", "name"],
            "additionalProperties": false
        },
        "annotations": { "readOnlyHint": false },
        "_meta": { "rox/audience": ["app"] },
    }));
    list.push(json!({
        "name": answers::CANCEL_SEND,
        "description": "Mini apps only (refused for the assistant): the outbox card's Cancel, taking a queued return out of the sending queue before it is sent.",
        "inputSchema": {
            "type": "object",
            "properties": { "ticket": { "type": "string" } },
            "required": ["ticket"],
            "additionalProperties": false
        },
        "annotations": { "readOnlyHint": false },
        "_meta": { "rox/audience": ["app"] },
    }));
    for name in v1::RESERVED {
        list.push(json!({
            "name": name,
            "description": "Reserved for Rox.",
            "inputSchema": { "type": "object" },
        }));
    }
    for tool in &mut list {
        let name = tool["name"].as_str().unwrap_or("").to_string();
        if tool.get("annotations").is_none() && !name.starts_with("rox.") {
            tool["annotations"] = json!({ "readOnlyHint": read_only(&name) });
        }
    }
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
            "description": "Read one payment email's text into a payment, when bir knows its channel's format (MyEG today): channel, tin (14 digits with the branch), form, period (the return period's end, e.g. 09/30/2026, read as 2026-Q3), paid_at, reference, transaction_number, amount (the tax), fee, total, token, and `record`: the arguments for bir_record_payment, with the email's text (`raw`) and its `message_id` when you pass it here. Answers null for an email it does not know: then read the email yourself and call bir_record_payment with what it says.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "The email's text (subject and body)." },
                    "message_id": { "type": "string", "description": "The email's Gmail message id: it rides in `record`." }
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
                    "proof_path": { "type": "string", "description": "Absolute path of a receipt image or PDF to keep as proof." },
                    "proof_upload": { "type": "string", "description": "Mini apps only: a receipt sent with bir_app_upload, in place of proof_path." },
                    "proof_name": { "type": "string", "description": "Mini apps only: the uploaded receipt's file name." }
                },
                "required": ["tin", "form", "period"],
                "additionalProperties": false
            },
        }),
        json!({
            "name": BIR_PAYMENTS_LIST,
            "description": "The recorded payments, newest first: channel, reference, amount, fee, total, paid_at, proof (email, file or none), source, token; and their sums (`totals`: all, by form, by month). `tin` (a 9-digit root covers every branch), `year` (paid in that year), `form` and `period` (needs `form`) narrow the list. For \"how much tax has she paid this year?\" pass `tin` and `year`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tin": { "type": "string", "description": "Taxpayer TIN." },
                    "year": { "type": "integer", "description": "Paid in this year, e.g. 2026." },
                    "form": { "type": "string", "description": "Form code." },
                    "period": { "type": "string", "description": "The return's period." }
                },
                "additionalProperties": false
            },
        }),
        json!({
            "name": BIR_PAYMENT_RECEIPT,
            "description": "Show a return's receipt: every payment recorded for it (channel, amount, fee, total, paid_at, reference, transaction number, when it was recorded) and its proof (the receipt file, or the payment email). Answers the payments and `gmail_url` (the payment email in Gmail, when it came from one); the user sees the receipt on a card with View receipt. A return with no payment recorded answers an error that says so.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tin": { "type": "string", "description": "Taxpayer TIN." },
                    "form": { "type": "string", "description": "Form code, e.g. 1601C." },
                    "period": { "type": "string", "description": "The return's period, e.g. 2026-01." }
                },
                "required": ["tin", "form", "period"],
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
    use crate::mini_app::v1::keys::{self, b64url};
    use crate::mini_app::v1::{jcs, tickets};

    const TIN: &str = "12345678800000";
    const ROOT: &str = "123456788";
    const OTHER: &str = "987654321";

    fn server() -> MiniServer {
        crate::bir_module::use_ephemeral_database();
        crate::account_gate::set_unlocked(true);
        MiniServer::with_gate(Catalog::Embedded, Gate::Fixed(License::open()), false)
    }

    /// As Rox spawns it: nothing answers before `rox.provision`.
    fn rox_server() -> MiniServer {
        crate::bir_module::use_ephemeral_database();
        crate::account_gate::set_unlocked(true);
        MiniServer::with_gate(Catalog::Embedded, Gate::Fixed(License::open()), true)
    }

    fn request(server: &mut MiniServer, params: Value) -> Value {
        server
            .handle(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": params }))
            .wait()
            .expect("a request is answered")
    }

    fn call(server: &mut MiniServer, name: &str, arguments: Value) -> Value {
        request(server, json!({ "name": name, "arguments": arguments }))["result"].clone()
    }

    /// A call with `_meta["rox/caller"]` as the broker sends it.
    fn call_as(server: &mut MiniServer, caller: Value, name: &str, arguments: Value) -> Value {
        request(
            server,
            json!({ "name": name, "arguments": arguments, "_meta": { "rox/caller": caller } }),
        )["result"]
            .clone()
    }

    fn app(id: &str) -> Value {
        json!({ "origin": "app", "plugin": "dev.goldcoders.bir", "app": id, "open_id": b64url(&[9; 16]), "ts": 1 })
    }

    fn agent() -> Value {
        json!({ "origin": "agent", "plugin": "dev.goldcoders.bir", "ts": 1 })
    }

    fn save_taxpayer(server: &mut MiniServer, tin: &str, name: &str) {
        let saved = server
            .host(
                "profile.save",
                json!({"tin": tin, "name": name, "rdo_code": "040"}),
            )
            .unwrap();
        assert_eq!(saved["status"], "ok");
    }

    fn ticket_of(root: &str) -> String {
        crate::bir_module::with_database(|db| {
            tickets::issue(db, &tickets::Subject::Taxpayer(root.into())).map_err(HostError::new)
        })
        .unwrap()
    }

    fn names() -> Vec<String> {
        tool_list()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_string))
            .collect()
    }

    #[test]
    fn initialize_offers_tools_resources_and_capped_instructions() {
        let mut server = server();
        let reply = server
            .handle(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))
            .wait()
            .unwrap();
        let capabilities = &reply["result"]["capabilities"];
        assert!(capabilities["tools"].is_object());
        assert!(capabilities["resources"].is_object());
        assert_eq!(reply["result"]["serverInfo"]["name"], SERVER_NAME);
        let instructions = reply["result"]["instructions"].as_str().unwrap();
        assert!(instructions.chars().count() <= 4000, "{}", instructions.chars().count());
        assert!(!instructions.contains("bir_call"));
        assert!(!instructions.contains("000000000"));
    }

    #[test]
    fn tools_list_has_named_app_tools_and_no_generic_bridge() {
        let names = names();
        for wanted in [BIR_STATUS, BIR_APP_OPEN, BIR_RESOLVE, BIR_DRAFTS_LIST, "bir_form_open", "bir_form_fill"] {
            assert!(names.iter().any(|name| name == wanted), "{wanted} missing");
        }
        for wanted in PAYMENT_TOOLS.iter().chain(DEADLINE_TOOLS.iter()) {
            assert!(names.iter().any(|name| name == wanted), "{wanted} missing");
        }
        for gone in ["bir_call", "bir_list_apps", "bir_connect", "bir_form_review", "bir_form_preview"] {
            assert!(!names.iter().any(|name| name == gone), "{gone} must not be offered");
        }
        for reserved in v1::RESERVED {
            assert!(names.iter().any(|name| name == reserved));
        }
        // the assistant's tools are not mistaken for the apps' own
        for agent_tool in [BIR_APP_OPEN, BIR_STATUS, BIR_RESOLVE, "bir_form_open"] {
            assert!(!is_app_only(agent_tool), "{agent_tool}");
        }
        assert!(is_app_only("bir_app_list_taxpayers") && is_app_only(app_tools::UPLOAD));
        // 10.1: the apps' tools, and nothing else, are for apps only
        for tool in tool_list()["tools"].as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            let app_only = is_app_only(name);
            assert_eq!(app_only, name.starts_with(app_tools::PREFIX) && name != BIR_APP_OPEN
                || name == app_tools::TICKET_SUBJECT || name == app_tools::TICKET_ISSUE || name == app_tools::ALLOW_SEND
                || name.starts_with("bir_card_"), "{name}");
            let audience = &tool["_meta"]["rox/audience"];
            if app_only {
                assert_eq!(audience, &json!(["app"]), "{name}");
            } else {
                assert!(audience.is_null(), "{name} must stay on the agent's surface");
            }
        }
        let unique: std::collections::HashSet<&String> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "a tool name is listed twice");
        for tool in tool_list()["tools"].as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            if name.starts_with("rox.") {
                continue;
            }
            assert!(name.starts_with("bir_") && name.len() <= 63, "{name}");
            assert!(tool["annotations"]["readOnlyHint"].is_boolean(), "{name} has no readOnlyHint");
            assert!(tool["description"].as_str().unwrap().chars().count() <= 1024, "{name}");
        }
        // every tool an app or routine names is served
        let manifest = v1_manifest::unsigned(&Catalog::Embedded).unwrap();
        for app in manifest["apps"].as_array().unwrap() {
            for tool in app["permissions"]["tools"].as_array().unwrap() {
                assert!(names.iter().any(|name| name == tool), "{} names {tool}", app["id"]);
            }
        }
        for routine in manifest["routines"].as_array().unwrap() {
            let tool = routine["query"]["tool"].as_str().unwrap();
            assert!(read_only(tool), "{tool} must be read-only for a routine");
        }
    }

    #[test]
    fn app_tools_answer_apps_only_and_only_what_the_app_lists() {
        let mut server = server();
        let index = call_as(&mut server, app("forms"), "bir_app_screen_forms_index", json!({}));
        assert_eq!(index["isError"], false, "{index}");
        assert!(index["structuredContent"]["result"].is_array());
        // the assistant, or no caller at all, is refused
        for caller in [agent(), json!(null)] {
            let refused = call_as(&mut server, caller, "bir_app_screen_forms_index", json!({}));
            assert_eq!(refused["structuredContent"]["code"], "permission_denied", "{refused}");
        }
        // the calendar does not list it
        let other = call_as(&mut server, app("calendar"), "bir_app_screen_forms_index", json!({}));
        assert_eq!(other["structuredContent"]["code"], "permission_denied");
        // an app may not call a tool its manifest entry leaves out
        let profiles = call_as(&mut server, app("deadlines"), "bir_profiles_list", json!({}));
        assert_eq!(profiles["structuredContent"]["code"], "permission_denied");
    }

    #[test]
    fn a_ticket_is_the_mandatory_scope_of_an_app_call() {
        let mut server = server();
        save_taxpayer(&mut server, TIN, "Scratch Taxpayer");
        save_taxpayer(&mut server, &format!("{OTHER}00000"), "Other Taxpayer");
        let mine = ticket_of(ROOT);
        // naming a taxpayer needs a ticket
        let bare = call_as(&mut server, app("forms"), "bir_app_list_tax_years", json!({"args": [TIN]}));
        assert_eq!(bare["isError"], true, "{bare}");
        let held = call_as(
            &mut server,
            app("forms"),
            "bir_app_list_tax_years",
            json!({"args": [TIN], "ticket": mine}),
        );
        assert_eq!(held["isError"], false, "{held}");
        // the ticket's taxpayer and no other
        let other = call_as(
            &mut server,
            app("forms"),
            "bir_app_list_tax_years",
            json!({"args": [format!("{OTHER}00000")], "ticket": mine}),
        );
        assert_eq!(other["isError"], true);
        assert!(other["content"][0]["text"].as_str().unwrap().contains("outside this app's scope"));
        // a ticket BIR did not issue
        let forged = call_as(
            &mut server,
            app("forms"),
            "bir_app_list_tax_years",
            json!({"args": [TIN], "ticket": format!("tkt_{}", "0".repeat(26))}),
        );
        assert_eq!(forged["isError"], true);
        // a list across taxpayers carries each one's ticket; with a ticket, only that one
        let all = call_as(&mut server, app("forms"), "bir_app_list_taxpayers", json!({}));
        let rows = all["structuredContent"]["result"].as_array().unwrap().clone();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row["ticket"].as_str().is_some_and(tickets::is_ticket)));
        let scoped = call_as(&mut server, app("forms"), "bir_app_list_taxpayers", json!({"ticket": mine}));
        let rows = scoped["structuredContent"]["result"].as_array().unwrap().clone();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["ticket"], mine);
        // the app learns its ticket's TIN from BIR, the assistant does not
        let subject = call_as(&mut server, app("forms"), app_tools::TICKET_SUBJECT, json!({"ticket": mine}));
        assert_eq!(subject["structuredContent"]["result"]["tin"], TIN, "{subject}");
        assert_eq!(subject["structuredContent"]["result"]["title"], "Scratch Taxpayer");
        let refused = call_as(&mut server, agent(), app_tools::TICKET_SUBJECT, json!({"ticket": mine}));
        assert_eq!(refused["isError"], true);
    }

    #[test]
    fn async_app_tools_are_answered_later() {
        let mut server = server();
        save_taxpayer(&mut server, TIN, "Scratch Taxpayer");
        let ticket = ticket_of(ROOT);
        let reply = server.handle(json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "bir_app_screen_form_open",
                "arguments": {"args": [TIN, "0619E", 2026, 1], "ticket": ticket},
                "_meta": { "rox/caller": app("forms") },
            },
        }));
        assert!(matches!(reply, Reply::Later(_)));
        let reply = reply.wait().unwrap();
        assert_eq!(reply["id"], 7);
        assert_eq!(reply["result"]["structuredContent"]["result"]["form_code"], "0619E");
    }

    #[test]
    fn bir_app_print_preview_answers_forms_with_a_static_sheet_held_to_its_ticket() {
        let mut server = server();
        save_taxpayer(&mut server, TIN, "Scratch Taxpayer");
        let ticket = ticket_of(ROOT);
        let sample = call_as(
            &mut server,
            app("forms"),
            "bir_app_screen_form_sample",
            json!({"args": [TIN, "2551Q"], "ticket": ticket}),
        );
        let draft = sample["structuredContent"]["result"]["draft"].clone();
        assert!(draft.is_object(), "{sample}");
        let ask = json!({"ticket": ticket, "form": "2551Q", "draft_json": draft.to_string()});
        let shown = call_as(&mut server, app("forms"), app_tools::PRINT_PREVIEW, ask.clone());
        assert_eq!(shown["isError"], false, "{}", shown["content"]);
        let resource = &shown["content"][0]["resource"];
        assert_eq!(shown["content"][0]["type"], "resource");
        assert_eq!(resource["mimeType"], "text/html");
        let html = resource["text"].as_str().unwrap().to_ascii_lowercase();
        assert!(html.contains("<html") && html.contains("</html>"), "a whole page");
        assert!(!html.contains("<script"), "a static page");
        assert_eq!(shown["structuredContent"]["title"], "BIR Form 2551Q \u{2014} Scratch Taxpayer");
        // a 2551Q as stored for its quarter: this one has none yet
        let period = call_as(
            &mut server,
            app("forms"),
            app_tools::PRINT_PREVIEW,
            json!({"ticket": ticket, "form": "2551Q", "year": 2026, "quarter": 3}),
        );
        assert_eq!(period["isError"], true);
        assert!(period["content"][0]["text"].as_str().unwrap().contains("No saved 2551Q draft"), "{period}");
        // a saved return prints as stored, by its id
        let opened = call_as(
            &mut server,
            app("forms"),
            "bir_app_screen_form_open",
            json!({"args": [TIN, "0619E", 2026, 1], "ticket": ticket}),
        );
        let opened = opened["structuredContent"]["result"]["draft"].clone();
        let saved = call_as(
            &mut server,
            app("forms"),
            "bir_app_screen_form_save",
            json!({"args": ["0619E", opened], "ticket": ticket}),
        );
        let id = saved["structuredContent"]["result"]["id"].as_i64().expect("a saved return's id");
        let stored = call_as(
            &mut server,
            app("forms"),
            app_tools::PRINT_PREVIEW,
            json!({"ticket": ticket, "form": "0619E", "draft_id": id}),
        );
        assert_eq!(stored["isError"], false, "{}", stored["content"]);
        assert_eq!(stored["structuredContent"]["title"], "BIR Form 0619E \u{2014} Scratch Taxpayer");
        assert!(stored["content"][0]["resource"]["text"].as_str().unwrap().len() > 1000);
        // the assistant, another app, another taxpayer's ticket, no ticket,
        // or nothing to print: refused
        assert_eq!(call_as(&mut server, agent(), app_tools::PRINT_PREVIEW, ask.clone())["isError"], true);
        assert_eq!(call_as(&mut server, app("calendar"), app_tools::PRINT_PREVIEW, ask.clone())["isError"], true);
        save_taxpayer(&mut server, &format!("{OTHER}00000"), "Other Taxpayer");
        let other = ticket_of(OTHER);
        let mut theirs = ask.clone();
        theirs["ticket"] = json!(other);
        for bad in [
            theirs,
            json!({"form": "2551Q", "draft_json": ask["draft_json"]}),
            json!({"ticket": ticket, "form": "2551Q"}),
            json!({"ticket": ticket, "form": "2551Q", "draft_json": ask["draft_json"], "period": "2026-Q3"}),
            json!({"ticket": ticket, "form": "2551Q", "draft_json": ask["draft_json"], "path": "/tmp/x.html"}),
            json!({"ticket": ticket, "form": "2551Q", "draft_json": "[1]"}),
            json!({"ticket": ticket, "form": "0619E", "draft_id": id, "edits": {}}),
            json!({"ticket": other, "form": "0619E", "draft_id": id}),
            json!({"ticket": ticket, "form": "0619E", "draft_id": id + 1000}),
        ] {
            let refused = call_as(&mut server, app("forms"), app_tools::PRINT_PREVIEW, bad.clone());
            assert_eq!(refused["isError"], true, "{bad}");
        }
        // a sent return's filing record: this one has none yet
        let record = call_as(
            &mut server,
            app("forms"),
            app_tools::PRINT_PREVIEW,
            json!({"ticket": ticket, "form": "2551Q", "period": "2026-Q3"}),
        );
        assert_eq!(record["isError"], true);
        assert!(record["content"][0]["text"].as_str().unwrap().contains("has no 2551Q return"), "{record}");
    }

    #[test]
    fn bir_payment_receipt_is_a_card_for_the_assistant_and_a_page_for_rox() {
        let mut server = server();
        save_taxpayer(&mut server, TIN, "Scratch Taxpayer");
        let ticket = ticket_of(ROOT);
        let ask = json!({"tin": ticket, "form": "1601C", "period": "2026-01"});
        let none = call_as(&mut server, agent(), BIR_PAYMENT_RECEIPT, ask.clone());
        assert_eq!(none["isError"], true);
        assert!(none["content"][0]["text"].as_str().unwrap().contains("no payment is recorded"), "{none}");
        let email = format!("Payment received. TIN {ROOT}, Form 1601C, period 01/31/2026, PHP 1,000.00, ref TESTREF9");
        let recorded = call_as(
            &mut server,
            agent(),
            BIR_RECORD_PAYMENT,
            json!({"tin": ticket, "form": "1601C", "period": "2026-01", "channel": "GCash", "reference": "TESTREF9",
                   "amount": 1000, "paid_at": "2026-02-05", "message_id": "18f0a1b2c3d4e5f6", "raw": email}),
        );
        assert_eq!(recorded["isError"], false, "{recorded}");
        // the assistant: the payments and the receipt card, never the email
        let card = call_as(&mut server, agent(), BIR_PAYMENT_RECEIPT, ask.clone());
        assert_card(&card, BIR_PAYMENT_RECEIPT, "receipt");
        assert_no_tin(&card, BIR_PAYMENT_RECEIPT);
        let snapshot = &card["_meta"]["rox/open"]["props"]["snapshot"];
        assert_eq!(snapshot["gmail_url"], "https://mail.google.com/mail/u/0/#all/18f0a1b2c3d4e5f6");
        assert_eq!(snapshot["payments"][0]["reference"], "TESTREF9");
        assert!(!card.to_string().contains("Payment received"), "no email text");
        assert_eq!(card["_meta"]["rox/open"]["props"]["ticket"], ticket);
        // Forms (rox.preview): the page, its title and the Gmail link
        let page = call_as(&mut server, app("forms"), BIR_PAYMENT_RECEIPT, ask.clone());
        assert_eq!(page["isError"], false, "{page}");
        assert_eq!(page["content"][0]["resource"]["mimeType"], "text/html");
        let html = page["content"][0]["resource"]["text"].as_str().unwrap();
        assert!(html.contains("Payment received. TIN") && html.contains("TESTREF9"), "the email as it reads");
        assert!(!html.to_ascii_lowercase().contains("<script"));
        assert_eq!(page["structuredContent"]["title"], "Receipt \u{b7} 1601C \u{b7} Jan 2026 \u{b7} Scratch Taxpayer");
        assert_eq!(page["structuredContent"]["gmail_url"], snapshot["gmail_url"]);
        // the parsed payment email's card leaves the email's text out
        let parsed = call_as(
            &mut server,
            agent(),
            BIR_PAYMENT_PARSE,
            json!({"text": "Agency Name BUREAU OF INTERNAL REVENUE (BIR) | TIN 123456788 | Branch Code 00000 | RDO Code 040 - PASIG | Form Type 2551Q | Tax Type PT | Return Period 09/30/2026 | Transaction Date 10/26/2026 18:57:10 | Transaction Number TEST2026102618575200000001 | Reference Number TESTREF001 | Payment Channel Mobile | Transaction Amount PHP 629.94 | Bank Fee PHP 0.00 | Transaction Fee PHP 20.00 | Total Amount PHP 649.94", "message_id": "test-3"}),
        );
        assert!(parsed["structuredContent"]["result"]["record"]["raw"].is_string(), "the record keeps the email");
        let props = &parsed["_meta"]["rox/open"]["props"]["snapshot"];
        assert!(props["record"].get("raw").is_none() && props.get("raw").is_none(), "{props}");
        assert_eq!(props["record"]["message_id"], "test-3");
    }

    #[test]
    fn bir_app_open_hands_rox_a_v1_card_with_a_ticket() {
        let mut server = server();
        save_taxpayer(&mut server, TIN, "Scratch Taxpayer");
        let ticket = ticket_of(ROOT);
        let result = call_as(&mut server, agent(), BIR_APP_OPEN, json!({"app": "forms", "tin": ticket}));
        assert_eq!(result["isError"], false, "{result}");
        assert_eq!(
            result["_meta"]["rox/open"],
            json!({"app": "forms", "props": {"ticket": ticket}, "ticket": ticket, "title": "BIR Forms · Scratch Taxpayer", "placement": "full"})
        );
        assert_eq!(result["content"][0]["text"], "Opening BIR Forms for Scratch Taxpayer…");
        let plain = call(&mut server, BIR_APP_OPEN, json!({"app": "calendar"}));
        assert_eq!(
            plain["_meta"]["rox/open"],
            json!({"app": "calendar", "props": {}, "title": "BIR Calendar", "placement": "full"})
        );
        // a return: its own ticket, the form and period as props
        let form = call_as(
            &mut server,
            agent(),
            BIR_APP_OPEN,
            json!({"app": "forms", "tin": ticket, "form": "2551Q", "period": "2026-Q3", "action": "submit"}),
        );
        let open = &form["_meta"]["rox/open"];
        assert_ne!(open["ticket"], ticket, "a return has its own ticket");
        assert_eq!(open["props"]["form"], "2551Q");
        assert_eq!(open["props"]["action"], "submit");
        // its card names the return and whose it is
        assert_eq!(open["title"], "BIR Form 2551Q · Q3 2026 · Scratch Taxpayer");
        assert!(!open.to_string().contains(ROOT), "{open}");
        // the deadline card opens inline
        let card = call_as(&mut server, agent(), BIR_APP_OPEN, json!({"app": "deadlines", "tin": ticket}));
        assert_eq!(card["_meta"]["rox/open"]["placement"], "inline");
        assert_eq!(card["_meta"]["rox/open"]["props"]["ticket"], ticket);
        for bad in [
            json!({"app": "tax-profile", "tin": ticket, "form": "2551Q"}),
            json!({"app": "forms", "tin": ticket, "period": "2026-Q3"}),
            json!({"app": "settings"}),
            json!({"app": "forms", "tin": format!("{OTHER}00000")}),
        ] {
            assert_eq!(call(&mut server, BIR_APP_OPEN, bad.clone())["isError"], true, "{bad}");
        }
    }

    #[test]
    fn bir_resolve_answers_v1_results_locally() {
        let mut server = server();
        save_taxpayer(&mut server, TIN, "Scratch Taxpayer");
        let resolve = json!({ "origin": "resolve", "plugin": "dev.goldcoders.bir", "ts": 1 });
        let token = call_as(&mut server, resolve.clone(), BIR_RESOLVE, json!({"kind": "scope", "name": "taxprofile", "value": ROOT}));
        let answer = &token["structuredContent"];
        assert_eq!(answer["result"], "token", "{token}");
        assert!(tickets::is_ticket(answer["ticket"].as_str().unwrap()));
        assert!(!answer.to_string().contains(ROOT));
        // A closed license does not stop it: it never leaves this computer.
        server.set_license(License::settled(Err(UNAUTHENTICATED.to_string())));
        let suggested = call_as(
            &mut server,
            resolve.clone(),
            BIR_RESOLVE,
            json!({"kind": "scope", "name": "taxform", "value": "255", "complete": true}),
        );
        assert_eq!(suggested["structuredContent"]["result"], "choices", "{suggested}");
        let missing = call_as(
            &mut server,
            resolve,
            BIR_RESOLVE,
            json!({"kind": "command", "name": "taxform", "value": "999999999/2551Q-2026-Q3"}),
        );
        assert_eq!(
            missing["structuredContent"],
            json!({"result": "error", "message": "No taxpayer with TIN 999-999-999"})
        );
    }

    #[test]
    fn the_assistant_cannot_send_without_the_users_grant() {
        let mut server = server();
        save_taxpayer(&mut server, TIN, "Scratch Taxpayer");
        let ticket = ticket_of(ROOT);
        let opened = call_as(
            &mut server,
            agent(),
            "bir_form_open",
            json!({"tin": ticket, "code": "2551Q-2026-Q1"}),
        );
        assert_eq!(opened["isError"], false, "{opened}");
        let refused = call_as(&mut server, agent(), "bir_form_send", json!({}));
        let text: Value = serde_json::from_str(refused["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text["status"], "approval_required");
        assert_eq!(text["queued"], false);
        // The Forms app's toggle grants it, held to its ticket.
        let granted = call_as(
            &mut server,
            app("forms"),
            "bir_app_grant_send",
            json!({"args": [TIN, "2551Q"], "ticket": ticket}),
        );
        assert_eq!(granted["isError"], false, "{granted}");
        let checked = call_as(&mut server, agent(), "bir_form_send", json!({}));
        let text: Value = serde_json::from_str(checked["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text["status"], "invalid", "past the grant, the page's Validate decides");
        call_as(&mut server, app("forms"), "bir_app_revoke_send", json!({"args": [TIN, "2551Q"], "ticket": ticket}));
        let again = call_as(&mut server, agent(), "bir_form_send", json!({}));
        let text: Value = serde_json::from_str(again["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text["status"], "approval_required");
        // what the assistant reads names the taxpayer by ticket
        let drafts = call_as(&mut server, agent(), BIR_DRAFTS_LIST, json!({"tin": ticket, "form": "2551Q"}));
        let listed = &drafts["structuredContent"]["result"]["drafts"];
        assert_eq!(listed[0]["period"], "2026-Q1", "{drafts}");
        assert!(listed[0]["token"].as_str().unwrap().starts_with("@bir:taxform:tkt_"));
        assert!(!drafts.to_string().contains(ROOT), "{drafts}");
    }

    #[test]
    fn routines_and_resolvers_cause_no_side_effects() {
        let mut server = server();
        save_taxpayer(&mut server, TIN, "Scratch Taxpayer");
        let routine = json!({ "origin": "routine", "plugin": "dev.goldcoders.bir", "ts": 1 });
        let search = call_as(&mut server, routine.clone(), BIR_CONFIRMATION_SEARCH, json!({}));
        assert_eq!(search["isError"], false, "{search}");
        assert!(search["structuredContent"]["result"]["query"].is_string());
        // the broker reads the search text at the top (spec 5.9 step 1)
        for tool in [BIR_CONFIRMATION_SEARCH, BIR_PAYMENT_SEARCH] {
            let search = call_as(&mut server, routine.clone(), tool, json!({}));
            assert!(search["structuredContent"]["query"].is_string(), "{tool}: {search}");
        }
        for (tool, args) in [
            (BIR_CALENDAR_SET, json!({"tin": ROOT, "forms": ["2551Q"], "enabled": false})),
            (BIR_MARK_PAID, json!({"tin": ROOT, "form": "2551Q", "period": "2026-Q3"})),
            ("bir_form_send", json!({})),
        ] {
            let refused = call_as(&mut server, routine.clone(), tool, args);
            assert_eq!(refused["structuredContent"]["code"], "permission_denied", "{tool}");
        }
    }

    #[test]
    fn a_closed_license_stops_model_tools_but_not_the_app_tools() {
        let mut server = server();
        server.set_license(License::settled(Err(UNAUTHENTICATED.to_string())));
        // signed out: plain words and the sign-in app's card, not an error
        let refused = call(&mut server, "bir_profiles_list", json!({}));
        assert_eq!(refused["isError"], false, "{refused}");
        assert_eq!(refused["content"][0]["text"], plain::signed_out_text("bir_profiles_list"));
        assert_eq!(refused["_meta"]["rox/open"]["props"]["card"], "signin");
        let opened = call(&mut server, BIR_APP_OPEN, json!({"app": "forms"}));
        assert_eq!(opened["isError"], false);
        assert_eq!(opened["_meta"]["rox/open"]["props"]["card"], "signin", "{opened}");
        // (bir_app_list_rdos, not the account status: that reads the
        // Buwiz account from the Keychain, which tests leave alone)
        let rdos = call_as(&mut server, app("register"), "bir_app_list_rdos", json!({}));
        assert_eq!(rdos["isError"], false, "{rdos}");
    }

    #[test]
    fn resources_serve_the_manifest_and_apps_whose_bytes_match_their_fingerprints() {
        let mut server = server();
        let listed = server
            .handle(json!({"jsonrpc": "2.0", "id": 1, "method": "resources/list"}))
            .wait()
            .unwrap();
        let resources = listed["result"]["resources"].as_array().unwrap().clone();
        assert!(resources.iter().any(|resource| resource["uri"] == "rox-app://forms"));
        assert!(resources.iter().any(|resource| resource["uri"] == v1_manifest::MANIFEST_URI));
        let read = server
            .handle(json!({"jsonrpc": "2.0", "id": 4, "method": "resources/read", "params": {"uri": v1_manifest::MANIFEST_URI}}))
            .wait()
            .unwrap();
        assert_eq!(read["result"]["contents"][0]["mimeType"], "application/json");
        let manifest: Value =
            serde_json::from_str(read["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
        for app in manifest["apps"].as_array().unwrap() {
            let source = server
                .handle(json!({"jsonrpc": "2.0", "id": 2, "method": "resources/read", "params": {"uri": app["entry"]}}))
                .wait()
                .unwrap();
            let content = &source["result"]["contents"][0];
            assert_eq!(content["mimeType"], apps::MIME_TYPE);
            assert_eq!(
                app["fingerprint"],
                v1_manifest::fingerprint(content["text"].as_str().unwrap()),
                "{}",
                app["id"]
            );
        }
        let missing = server
            .handle(json!({"jsonrpc": "2.0", "id": 3, "method": "resources/read", "params": {"uri": "rox-app://none"}}))
            .wait()
            .unwrap();
        assert_eq!(missing["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn answers_carry_their_inline_card_with_a_small_snapshot() {
        let mut server = server();
        save_taxpayer(&mut server, TIN, "Dela Cruz Jane");
        crate::bir_module::with_database(|db| {
            db.set_yearly_forms(TIN, 2026, &["2551Q", "1601C"]).map_err(|error| HostError::new(error.to_string()))?;
            Ok(())
        })
        .unwrap();
        let ticket = ticket_of(ROOT);
        let cases = [
            ("bir_profile_forms", json!({"ticket": ticket, "year": 2026}), "forms"),
            ("bir_returns_list", json!({"ticket": ticket, "year": 2026}), "matrix"),
            ("bir_returns_list", json!({"ticket": ticket, "form": "2551Q", "period": "2026-Q3"}), "status"),
            (BIR_CALENDAR_GET, json!({"tin": ticket, "from": "2026-11-01", "to": "2026-11-30"}), "month"),
            (BIR_CALENDAR_GET, json!({"tin": ticket}), "dues"),
            ("bir_taxpayer", json!({"ticket": ticket}), "taxpayer"),
            ("bir_find_taxpayer", json!({"name": "jane deli cruz"}), "pick"),
            ("bir_penalty", json!({"ticket": ticket}), "overdue"),
            ("bir_penalty", json!({"form": "0619E", "due": "2026-09-10", "filed_on": "2026-11-10", "tax_due": 10000}), "penalty"),
            ("bir_unpaid", json!({}), "unpaid"),
            ("bir_board", json!({}), "board"),
            (BIR_MARK_PAID, json!({"tin": ticket, "form": "2551Q", "period": "2026-Q3"}), "markpaid"),
        ];
        for (tool, args, card) in cases {
            let result = call_as(&mut server, agent(), tool, args.clone());
            assert_eq!(result["isError"], false, "{tool} {args}: {result}");
            let open = &result["_meta"]["rox/open"];
            assert_eq!(open["placement"], "inline", "{tool}: {open}");
            assert_eq!(open["props"]["card"], card, "{tool} {args}");
            assert!(open["app"].as_str().unwrap().starts_with("card-"));
            assert!(open["props"]["snapshot"].is_object(), "{tool}");
            // only a card that can re-read its answer has "as of" and Refresh
            assert_eq!(open["props"]["as_of"].is_i64(), !open["props"]["tool"].is_null(), "{tool}");
            // the card may re-read its answer: the tool is in its app's grants
            let granted = v1_manifest::served_app(&Catalog::Embedded, open["app"].as_str().unwrap()).unwrap().unwrap();
            if !open["props"]["tool"].is_null() {
                assert!(
                    granted["permissions"]["tools"].as_array().unwrap().iter().any(|t| *t == open["props"]["tool"]),
                    "{} cannot refresh with {}", open["app"], open["props"]["tool"]
                );
            }
            let props = open["props"].to_string();
            assert!(props.len() <= 8 * 1024, "{tool}: {} bytes", props.len());
            assert_no_tin(&result, tool);
            if let Some(chips) = result["_meta"]["rox/suggest"].as_array() {
                assert!(chips.len() <= 3);
            }
        }
        // the penalty is BIR's own engine: late tax costs something
        let estimate = call_as(&mut server, agent(), "bir_penalty", json!({"form": "0619E", "due": "2026-09-10", "filed_on": "2026-11-10", "tax_due": 10000}));
        assert!(estimate["structuredContent"]["result"]["total_penalties"].as_f64().unwrap() > 0.0, "{estimate}");
        // the assistant's mark paid records nothing: the user presses Save
        let payments = call_as(&mut server, agent(), BIR_PAYMENTS_LIST, json!({"tin": ticket}));
        assert_eq!(payments["structuredContent"]["result"]["payments"], json!([]));
        // a ticket BIR did not issue
        let forged = format!("tkt_{}", "0".repeat(26));
        for tool in ["bir_profile_forms", "bir_returns_list", "bir_taxpayer", "bir_penalty", "bir_unpaid"] {
            let refused = call_as(&mut server, agent(), tool, json!({"ticket": forged}));
            assert_eq!(refused["isError"], true, "{tool}");
        }
        // apps get no card: they draw their own
        let from_app = call_as(&mut server, app("card-forms"), "bir_profile_forms", json!({"ticket": ticket}));
        assert!(from_app["_meta"]["rox/open"].is_null());
    }

    /// The TIN roots the card tests save.
    const SEEDED: [&str; 3] = [ROOT, OTHER, "111222333"];

    /// Whether `text` holds a seeded TIN in any form: the 9-digit root
    /// alone, dashed (`123-456-788`), or inside a longer run of digits and
    /// dashes (`12345678800000-2551Qv2018-…`, `123-456-788-00000`).
    fn names_a_tin(text: &str) -> Option<&'static str> {
        let runs = regex::Regex::new(r"\d(?:[\d-]*\d)?").unwrap();
        SEEDED.into_iter().find(|root| {
            runs.find_iter(text).any(|run| run.as_str().chars().filter(char::is_ascii_digit).collect::<String>().contains(root))
        })
    }

    /// Spec 6.6 for an answer with a card: neither its props (kept in the
    /// chat's history), its chips nor what the assistant reads name a TIN.
    fn assert_no_tin(result: &Value, tool: &str) {
        for (part, value) in [
            ("props", &result["_meta"]["rox/open"]["props"]),
            ("chips", &result["_meta"]["rox/suggest"]),
            ("content", &result["content"]),
            ("structuredContent", &result["structuredContent"]),
        ] {
            let text = value.to_string();
            if let Some(root) = names_a_tin(&text) {
                panic!("{tool}: its {part} name the TIN {root}: {text}");
            }
        }
    }

    /// One card answer's checks (spec 9.8): inline, the expected card, a
    /// snapshot under 8 KiB that names no TIN, and a Refresh its app may run.
    fn assert_card(result: &Value, tool: &str, card: &str) {
        assert_eq!(result["isError"], false, "{tool}: {result}");
        let open = &result["_meta"]["rox/open"];
        assert_eq!(open["placement"], "inline", "{tool}: {open}");
        assert_eq!(open["props"]["card"], card, "{tool}: {open}");
        let app = open["app"].as_str().unwrap();
        let (_, kind_app, small) = cards::KINDS.iter().find(|(id, ..)| *id == card).unwrap();
        assert_eq!(app, *kind_app, "{card}");
        assert_eq!(open["size"] == "small", *small, "{card}");
        assert!(open["props"]["snapshot"].is_object(), "{tool}");
        assert_eq!(open["props"]["as_of"].is_i64(), !open["props"]["tool"].is_null(), "{tool}");
        let granted = v1_manifest::served_app(&Catalog::Embedded, app).unwrap().unwrap();
        if !open["props"]["tool"].is_null() {
            assert!(
                granted["permissions"]["tools"].as_array().unwrap().iter().any(|t| *t == open["props"]["tool"]),
                "{app} cannot refresh with {}", open["props"]["tool"]
            );
        }
        let props = open["props"].to_string();
        assert!(props.len() <= 8 * 1024, "{tool}: {} bytes", props.len());
        assert_no_tin(result, tool);
        if let Some(chips) = result["_meta"]["rox/suggest"].as_array() {
            assert!(chips.len() <= 3);
        }
    }

    #[test]
    fn wave_two_answers_carry_their_cards() {
        use base64::Engine as _;
        let mut server = server();
        save_taxpayer(&mut server, TIN, "Dela Cruz Jane");
        crate::bir_module::with_database(|db| {
            db.set_yearly_forms(TIN, 2026, &["2551Q", "1601C", "0619E"]).map_err(|error| HostError::new(error.to_string()))?;
            Ok(())
        })
        .unwrap();
        let ticket = ticket_of(ROOT);
        // a saved 2551Q Q3 draft to read
        let opened = call_as(&mut server, agent(), "bir_form_open", json!({"tin": ticket, "code": "2551Q-2026-Q3"}));
        assert_eq!(opened["isError"], false, "{opened}");
        let cases = [
            ("bir_taxpayer", json!({"ticket": ticket, "view": "branches"}), "branches"),
            ("bir_quick_add", json!({"name": "Reyes Mario", "tin": "111-222-333", "rdo": "40"}), "quickadd"),
            ("bir_board", json!({"view": "on_time"}), "streak"),
            ("bir_board", json!({"form": "0619E", "period": "2026-10"}), "bulk"),
            ("bir_form_info", json!({"form": "2551Q", "ticket": ticket}), "explain"),
            (BIR_DRAFTS_LIST, json!({"tin": ticket}), "drafts"),
            ("bir_outbox", json!({}), "outbox"),
            (BIR_CALENDAR_GET, json!({"from": "2026-11-01", "to": "2026-11-07"}), "week"),
            ("bir_penalty", json!({"form": "0619E", "due": "2026-09-10", "tax_due": 100}), "penalty"),
            ("bir_deadline", json!({"form": "2551Q", "period": "2026-Q3", "tin": ticket}), "shift"),
            ("bir_return", json!({"ticket": ticket, "form": "2551Q", "period": "2026-Q3", "view": "summary"}), "summary"),
            ("bir_return", json!({"ticket": ticket, "form": "2551Q", "period": "2026-Q3", "view": "paper"}), "preview"),
            ("bir_return", json!({"ticket": ticket, "form": "2551Q", "period": "2026-Q3", "view": "amend"}), "amend"),
            ("bir_return", json!({"ticket": ticket, "form": "1701Q", "period": "2026-Q2", "view": "attachments"}), "attach"),
            ("bir_return", json!({"ticket": ticket, "form": "2551Q", "period": "2026-Q3", "view": "import"}), "import"),
            (BIR_PAYMENTS_LIST, json!({"tin": ticket, "year": 2026}), "paidsum"),
            (BIR_STATUS, json!({}), "health"),
            ("bir_lookup", json!({"atc": "WC158"}), "atc"),
            ("bir_lookup", json!({"rdo": "040"}), "rdo"),
            ("bir_lookup", json!({"amount": "186,000", "rate": 3}), "rate"),
            ("bir_digest", json!({}), "digest"),
            (BIR_CALENDAR_SET, json!({"tin": ticket, "reminder_days": [3, 1]}), "remind"),
        ];
        for (tool, args, card) in cases {
            let result = call_as(&mut server, agent(), tool, args.clone());
            assert_card(&result, tool, card);
        }
        // wave 1's cards whose answers carry what the assistant read: a BIR
        // file name and a payment email, both of which embed the TIN
        let wave_one = [
            ("bir_form_needs_you", json!({}), "needs"),
            ("bir_form_send", json!({}), "approve"),
            (BIR_PAYMENT_PARSE, json!({"text": "Agency Name BUREAU OF INTERNAL REVENUE (BIR) | TIN 123456788 | Branch Code 00000 | RDO Code 040 - PASIG | Form Type 2551Q | Tax Type PT | Return Period 09/30/2026 | Transaction Date 10/26/2026 18:57:10 | Transaction Number TEST2026102618575200000001 | Reference Number TESTREF001 | Payment Channel Mobile | Transaction Amount PHP 629.94 | Bank Fee PHP 0.00 | Transaction Fee PHP 20.00 | Total Amount PHP 649.94"}), "match"),
            (BIR_CONFIRM_FILING, json!({"tin": ticket, "form": "2551Q", "period": "2026-Q3", "reference": "12345678800000-2551Qv2018-092026Q3.xml", "received_at": "2026-10-20T10:00:00+08:00", "message_id": "test-1"}), "confirm"),
            (BIR_CONFIRM_FILING, json!({"tin": ticket, "form": "2551Q", "period": "2026-Q3", "reference": "123-456-788-00000-2551Qv2018-062026Q2.xml", "received_at": "2026-10-20T10:00:00+08:00", "message_id": "test-2"}), "confirm"),
        ];
        for (tool, args, card) in wave_one {
            let result = call_as(&mut server, agent(), tool, args.clone());
            assert_card(&result, tool, card);
        }
        // the sign-in card (a signed-out answer) names no one
        let signin = json!({"_meta": {"rox/open": cards::open("signin", "Buwiz sign-in", None, json!({"message": plain::signed_out_text("bir_calendar_get")}), "bir_status", json!({}))}, "content": [], "structuredContent": {}});
        assert_no_tin(&signin, "signin");
        // the guard itself: a TIN in any form, inside other text, is caught
        for leak in ["123456788", "123-456-788", "12345678800000-2551Qv2018-092026Q3", "x 123-456-788-00000 y", "TIN987654321"] {
            assert!(names_a_tin(leak).is_some(), "{leak}");
            let mut guarded = json!({"_meta": {"rox/open": {"props": {"snapshot": {"note": leak}}}}, "content": [{"type": "text", "text": leak}]});
            cards::redact(&mut guarded, &SEEDED.map(str::to_string));
            assert!(names_a_tin(&guarded.to_string()).is_none(), "{guarded}");
        }
        assert!(names_a_tin("2026-10-26 ₱186,000.00 TEST2026102618575200000001").is_none());
        // the rate card's sum and the ATC's forms come from BIR's tables
        let rate = call_as(&mut server, agent(), "bir_lookup", json!({"amount": 186000, "rate": "3%"}));
        assert_eq!(rate["structuredContent"]["result"]["tax"], 5580.0, "{rate}");
        let deadline = call_as(&mut server, agent(), "bir_deadline", json!({"form": "2551Q", "period": "2026-Q3"}));
        let deadline = &deadline["structuredContent"]["result"];
        assert_eq!((deadline["official"].as_str(), deadline["applies"].as_str()), (Some("2026-10-25"), Some("2026-10-26")), "{deadline}");
        assert_eq!(deadline["kind"], "weekend");
        // a ticket BIR did not issue
        let forged = format!("tkt_{}", "0".repeat(26));
        for (tool, args) in [
            ("bir_taxpayer", json!({"ticket": forged, "view": "branches"})),
            ("bir_form_info", json!({"form": "2551Q", "ticket": forged})),
            ("bir_return", json!({"ticket": forged, "view": "summary"})),
            ("bir_deadline", json!({"form": "2551Q", "tin": forged})),
            (BIR_PAYMENTS_LIST, json!({"tin": forged})),
        ] {
            let refused = call_as(&mut server, agent(), tool, args);
            assert_eq!(refused["isError"], true, "{tool}: {refused}");
        }
        // the quickadd card saves nothing; its Save is the user's, through
        // an app-only tool the assistant cannot call
        let offered = call_as(&mut server, agent(), "bir_quick_add", json!({"name": "Reyes Mario", "tin": "111222333"}));
        let new_ticket = offered["structuredContent"]["result"]["ticket"].as_str().unwrap().to_string();
        let save = json!({"ticket": new_ticket, "name": "Reyes Mario", "type": "individual", "rdo": "040"});
        for tool in [answers::ADD_TAXPAYER, answers::CANCEL_SEND, return_view::ATTACH, return_view::IMPORT, return_view::IMPORT_FILL] {
            let refused = call_as(&mut server, agent(), tool, save.clone());
            assert_eq!(refused["isError"], true, "{tool} answers the assistant: {refused}");
        }
        let still = call_as(&mut server, agent(), "bir_find_taxpayer", json!({"name": "reyes mario"}));
        assert_eq!(still["structuredContent"]["result"]["matches"], json!([]), "{still}");
        let added = call_as(&mut server, app("card-people"), answers::ADD_TAXPAYER, save);
        assert_eq!(added["isError"], false, "{added}");
        let found = call_as(&mut server, agent(), "bir_find_taxpayer", json!({"name": "reyes mario"}));
        assert_eq!(found["structuredContent"]["result"]["matches"].as_array().unwrap().len(), 1, "{found}");
        // the import card: read a sheet, then Fill puts the total on the draft
        let return_ticket = crate::bir_module::with_database(|db| {
            tickets::issue(db, &tickets::Subject::Return(format!("{ROOT}/2551Q-2026-Q3"))).map_err(HostError::new)
        })
        .unwrap();
        let csv = "Date,Amount\n2026-07-02,\"100,000.00\"\n2026-08-15,86000\n2026-08-16,\"12,4OO.00\"\n2026-10-02,500\n";
        let up = call_as(&mut server, app("card-return"), app_tools::UPLOAD, json!({"data": base64::engine::general_purpose::STANDARD.encode(csv)}));
        let upload = up["structuredContent"]["result"]["upload"].as_str().map(str::to_string).unwrap_or_else(|| {
            serde_json::from_str::<Value>(up["content"][0]["text"].as_str().unwrap()).unwrap()["upload"].as_str().unwrap().to_string()
        });
        let read = call_as(&mut server, app("card-return"), return_view::IMPORT, json!({"ticket": return_ticket, "upload": upload, "name": "sales-q3.csv"}));
        assert_eq!(read["isError"], false, "{read}");
        let read = &read["structuredContent"]["result"];
        assert_eq!((read["rows"].as_u64(), read["total"].as_f64()), (Some(2), Some(186000.0)), "{read}");
        assert_eq!(read["skipped"].as_array().unwrap().len(), 2, "{read}");
        let fill = call_as(&mut server, app("card-return"), return_view::IMPORT_FILL, json!({"ticket": return_ticket, "import": read["import"]}));
        assert_eq!(fill["isError"], false, "{fill}");
        let paper = call_as(&mut server, agent(), "bir_return", json!({"ticket": return_ticket, "view": "paper"}));
        assert!(paper["structuredContent"]["result"].to_string().contains("186,000") || paper["structuredContent"]["result"].to_string().contains("186000"), "{paper}");
    }

    #[test]
    fn rox_provision_comes_first_and_certifies_opens() {
        let mut server = rox_server();
        save_taxpayer(&mut server, TIN, "Dela Cruz Jane");
        // nothing before provision; an unknown rox.* is method-not-found
        let early = request(&mut server, json!({"name": BIR_STATUS, "arguments": {}}));
        assert_eq!(early["error"]["code"], INVALID_REQUEST);
        let early = request(&mut server, json!({"name": "rox.ticket.resolve", "arguments": {"ticket": "x"}}));
        assert_eq!(early["error"]["code"], INVALID_REQUEST);
        let unknown = request(&mut server, json!({"name": "rox.revoke", "arguments": {}}));
        assert_eq!(unknown["error"]["code"], METHOD_NOT_FOUND);
        // provision: the manifest's JCS hash; again with the same key is fine
        let args = Value::Object(keys::tests::provision_args([5; 32]));
        let provisioned = call(&mut server, "rox.provision", args.clone());
        let expected = jcs::sha256_hex(&v1_manifest::served(&Catalog::Embedded).unwrap()).unwrap();
        assert_eq!(provisioned["structuredContent"], json!({"ok": true, "manifest_sha256": expected}));
        assert_eq!(call(&mut server, "rox.provision", args)["structuredContent"]["ok"], true);
        let other = call(&mut server, "rox.provision", Value::Object(keys::tests::provision_args([6; 32])));
        assert_eq!(other["structuredContent"]["code"], "invalid_args");
        assert!(!other.to_string().contains(&b64url(&[6; 32])), "the seed is never echoed");
        // a session certificate over the open, signed by the plugin key
        let forms = v1_manifest::served_app(&Catalog::Embedded, "forms").unwrap().unwrap();
        let open = json!({
            "app": "forms", "open_id": b64url(&[9; 16]), "ks_pub": b64url(&[3; 32]),
            "perms_sha256": v1_manifest::permissions_digest(&forms).unwrap(),
            "not_after": chrono::Utc::now().timestamp_millis() + 60_000,
        });
        let cert = call(&mut server, "rox.session.open", open.clone());
        let cert = &cert["structuredContent"];
        assert_eq!(cert["cert"]["typ"], "rox-session");
        assert_eq!(cert["cert"]["plugin"], "dev.goldcoders.bir");
        assert_eq!(cert["cert"]["perms_sha256"], open["perms_sha256"]);
        let public = server.v1.public_key().unwrap();
        assert!(keys::tests::verify(&public, &cert["cert"], cert["cert_sig"].as_str().unwrap()));
        let mut unknown_app = open.clone();
        unknown_app["app"] = json!("nope");
        assert_eq!(call(&mut server, "rox.session.open", unknown_app)["structuredContent"]["code"], "invalid_args");
        let mut wrong = open.clone();
        wrong["perms_sha256"] = json!("0".repeat(64));
        assert_eq!(call(&mut server, "rox.session.open", wrong)["structuredContent"]["code"], "invalid_args");
        // a certified open may call; an uncertified one may not
        assert_eq!(call_as(&mut server, app("forms"), "bir_app_screen_forms_index", json!({}))["isError"], false);
        let mut stranger = app("forms");
        stranger["open_id"] = json!(b64url(&[8; 16]));
        let refused = call_as(&mut server, stranger, "bir_app_screen_forms_index", json!({}));
        assert_eq!(refused["structuredContent"]["code"], "permission_denied");
        // tickets: a title and nothing else; uninstall forgets them
        let ticket = ticket_of(ROOT);
        let title = call(&mut server, "rox.ticket.resolve", json!({"ticket": ticket}));
        assert_eq!(title["structuredContent"], json!({"valid": true, "title": "Dela Cruz Jane"}));
        assert_eq!(
            call(&mut server, "rox.session.close", json!({"open_id": b64url(&[9; 16])}))["structuredContent"],
            json!({"ok": true})
        );
        assert_eq!(call(&mut server, "rox.uninstall", json!({}))["structuredContent"], json!({"ok": true}));
        let gone = call(&mut server, "rox.ticket.resolve", json!({"ticket": ticket}));
        assert_eq!(gone["structuredContent"], json!({"valid": false}));
    }
}
