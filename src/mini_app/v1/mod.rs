//! Rox Mini App Protocol v1 for `bir-mini-app` (`dev.goldcoders.bir`).
//!
//! Rox's spec (`rox-mini-app-protocol-v1.md`) asks four things of a plugin,
//! each in its own module here:
//!
//! - a signed manifest at `rox://manifest` naming every app with its
//!   fingerprint and permissions ([`manifest`], JCS in [`jcs`]);
//! - the reserved `rox.*` tools: the plugin key provisioned on every spawn
//!   and kept in memory only, session certificates per app open, ticket
//!   titles and uninstall ([`Host`], [`keys`]);
//! - tickets in place of TINs wherever Rox stores, links or shows the model
//!   something ([`tickets`]);
//! - named tools for the apps instead of the generic `bir_call`
//!   ([`app_tools`]).
//!
//! This file also holds the edge where BIR's existing tools meet v1: a
//! ticket passed where a tool takes a TIN is turned into that TIN on the
//! way in ([`expand`]), and the TINs in a result the model or an app reads
//! become tickets on the way out ([`Scrubber`]), so the tools themselves
//! still speak TINs and their tests stand. `bir_resolve` answers with the
//! spec's `result` shapes through [`resolve`], over the same resolver.
//!
//! Rox talks to this process over stdio only (14.3). `serve --http` remains
//! for the BIR desktop app's loopback door and is not a v1 transport.

pub(crate) mod app_tools;
pub(crate) mod jcs;
pub(crate) mod keys;
pub(crate) mod manifest;
pub(crate) mod tickets;

use std::collections::HashMap;

use bir_core::db::Database;
use bir_core::forms::taxform;
use chrono::NaiveDate;
use regex::Regex;
use serde_json::{Map, Value, json};

use super::apps::Catalog;
use super::resolve;
use keys::{OpenRequest, PluginKey};
use tickets::Subject;

/// Who a `tools/call` comes from (`_meta["rox/caller"].origin`, 8.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    App,
    Agent,
    Routine,
    Resolve,
    /// No `rox/caller`: not Rox (the HTTP door, tests).
    Unknown,
}

/// `_meta["rox/caller"]`.
#[derive(Debug, Clone)]
pub(crate) struct Caller {
    pub origin: Origin,
    pub app: Option<String>,
    pub open_id: Option<String>,
}

impl Caller {
    pub(crate) fn of(params: &Value) -> Self {
        let caller = params.get("_meta").and_then(|meta| meta.get("rox/caller"));
        let text = |key: &str| {
            caller
                .and_then(|caller| caller.get(key))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let origin = match text("origin").as_deref() {
            Some("app") => Origin::App,
            Some("agent") => Origin::Agent,
            Some("routine") => Origin::Routine,
            Some("resolve") => Origin::Resolve,
            _ => Origin::Unknown,
        };
        Self {
            origin,
            app: text("app"),
            open_id: text("open_id"),
        }
    }
}

/// A reserved tool's answer: `structuredContent` is the object itself, as
/// the broker reads it.
pub(crate) fn ok(structured: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": structured.to_string() }],
        "structuredContent": structured,
        "isError": false,
    })
}

/// A refusal with a spec error code.
pub(crate) fn fail(code: &str, message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "structuredContent": { "code": code, "message": message },
        "isError": true,
    })
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// One database read or write, its error as text.
fn database<R>(run: impl FnOnce(&Database) -> Result<R, String>) -> Result<R, String> {
    crate::bir_module::with_database(|db| run(db).map_err(gpui_shell::HostError::new))
        .map_err(|error| error.message().to_string())
}

/// The reserved tools this plugin implements (7.8).
pub(crate) const RESERVED: [&str; 5] = [
    "rox.provision",
    "rox.session.open",
    "rox.session.close",
    "rox.ticket.resolve",
    "rox.uninstall",
];

/// What `rox.*` keeps between requests: the plugin key (memory only, P4),
/// the opens it certified, and receipts being uploaded.
pub(crate) struct Host {
    key: Option<PluginKey>,
    /// open_id -> app id.
    sessions: HashMap<String, String>,
    /// Stdio (Rox): nothing but `rox.provision` is answered before it (P7).
    require_provision: bool,
    pub(crate) uploads: app_tools::Uploads,
}

impl Host {
    pub(crate) fn new(require_provision: bool) -> Self {
        Self {
            key: None,
            sessions: HashMap::new(),
            require_provision,
            uploads: app_tools::Uploads::default(),
        }
    }

    /// Whether ordinary tools may run.
    pub(crate) fn ready(&self) -> bool {
        !self.require_provision || self.key.is_some()
    }

    /// Whether Rox provisioned this process (then app calls must name an
    /// open it certified).
    pub(crate) fn provisioned(&self) -> bool {
        self.key.is_some()
    }

    /// The app of an open this process certified.
    pub(crate) fn session_app(&self, open_id: &str) -> Option<&str> {
        self.sessions.get(open_id).map(String::as_str)
    }

    /// A reserved tool. `None`: not one of ours (the caller answers -32601).
    /// `Some(Err)`: refused before provisioning (-32600).
    pub(crate) fn reserved(
        &mut self,
        catalog: &Catalog,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Option<Result<Value, String>> {
        if !RESERVED.contains(&name) {
            return None;
        }
        if name != "rox.provision" && !self.ready() {
            return Some(Err("rox.provision must come first".into()));
        }
        Some(Ok(match name {
            "rox.provision" => self.provision(catalog, arguments),
            "rox.session.open" => self.session_open(catalog, arguments),
            "rox.session.close" => {
                if let Some(open_id) = arguments.get("open_id").and_then(Value::as_str) {
                    self.sessions.remove(open_id);
                }
                ok(json!({ "ok": true }))
            }
            "rox.ticket.resolve" => ticket_resolve(arguments),
            _ => self.uninstall(),
        }))
    }

    fn provision(&mut self, catalog: &Catalog, arguments: &Map<String, Value>) -> Value {
        if arguments.get("plugin").and_then(Value::as_str) != Some(manifest::PLUGIN_ID) {
            return fail("invalid_args", "this is dev.goldcoders.bir");
        }
        let major = arguments
            .get("protocol")
            .and_then(Value::as_str)
            .and_then(|version| version.split('.').next());
        if major != Some("1") {
            return fail("invalid_args", "this plugin implements protocol 1");
        }
        let key = match PluginKey::from_provision(arguments) {
            Ok(key) => key,
            Err(message) => return fail("invalid_args", &message),
        };
        if let Some(held) = &self.key
            && !held.same_as(&key)
        {
            return fail("invalid_args", "already provisioned with another key");
        }
        let sha256 = match manifest::served_sha256(catalog) {
            Ok(sha256) => sha256,
            Err(message) => return fail("manifest_invalid", &message),
        };
        self.key = Some(key);
        ok(json!({ "ok": true, "manifest_sha256": sha256 }))
    }

    fn session_open(&mut self, catalog: &Catalog, arguments: &Map<String, Value>) -> Value {
        let Some(key) = &self.key else {
            return fail("invalid_args", "rox.provision must come first");
        };
        let request = match OpenRequest::read(arguments, now_ms()) {
            Ok(request) => request,
            Err(message) => return fail("invalid_args", &message),
        };
        let app = match manifest::served_app(catalog, &request.app) {
            Ok(Some(app)) => app,
            Ok(None) => {
                let id: String = request.app.chars().take(60).collect();
                return fail("invalid_args", &format!("no app {id}"));
            }
            Err(message) => return fail("manifest_invalid", &message),
        };
        match manifest::permissions_digest(&app) {
            Ok(digest) if digest == request.perms_sha256 => {}
            Ok(_) => return fail("invalid_args", "perms_sha256 does not match the manifest"),
            Err(message) => return fail("manifest_invalid", &message),
        }
        let cert = request.certificate(manifest::PLUGIN_ID);
        let signature = match key.sign(&cert) {
            Ok(signature) => signature,
            Err(message) => return fail("invalid_args", &message),
        };
        self.sessions.insert(request.open_id.clone(), request.app.clone());
        ok(json!({ "cert": cert, "cert_sig": signature }))
    }

    fn uninstall(&mut self) -> Value {
        self.sessions.clear();
        self.uploads = app_tools::Uploads::default();
        match database(tickets::forget_all) {
            Ok(()) => ok(json!({ "ok": true })),
            Err(message) => fail("internal", &message),
        }
    }

    #[cfg(test)]
    pub(crate) fn public_key(&self) -> Option<String> {
        self.key.as_ref().map(PluginKey::public)
    }
}

/// `rox.ticket.resolve`: a title, never the subject (P3).
fn ticket_resolve(arguments: &Map<String, Value>) -> Value {
    let ticket = arguments
        .get("ticket")
        .and_then(Value::as_str)
        .unwrap_or("");
    let today = taxform::today();
    let title = database(|db| {
        Ok(tickets::subject(db, ticket)?.and_then(|subject| subject.title(db, today)))
    })
    .ok()
    .flatten();
    match title {
        Some(title) => ok(json!({ "valid": true, "title": title })),
        None => ok(json!({ "valid": false })),
    }
}

// ------------------------------------------------------------ tickets in

/// Arguments that hold a TIN or a return in BIR's own tools.
const TIN_ARGS: [&str; 1] = ["tin"];
const RETURN_ARGS: [&str; 3] = ["code", "form", "key"];

/// Turn the tickets in a BIR tool's arguments into what the tool reads: a
/// ticket in `tin` (or `tins`) becomes its TIN, a return's ticket in
/// `code`, `form` or `key` its `<tin>/<FORM>-<period>`. An app passes
/// tickets only (P1): a raw TIN from an app is refused.
pub(crate) fn expand(
    db: &Database,
    tool: &str,
    arguments: &mut Map<String, Value>,
    origin: Origin,
) -> Result<(), String> {
    let lookup = |ticket: &str| -> Result<Subject, String> {
        tickets::subject(db, ticket)?
            .ok_or_else(|| format!("{tool}: the ticket is not one BIR issued, or it was revoked"))
    };
    // `tin` takes the branch's 14 digits (a taxpayer is its head office);
    // `tins` the root, which covers every branch.
    let tin_of = |key: &str, value: &Value| -> Result<Option<(String, Subject)>, String> {
        match value.as_str() {
            Some(text) if tickets::is_ticket(text) => {
                let subject = lookup(text)?;
                let tin = if key == "tins" { subject.tin() } else { subject.full_tin() };
                Ok(Some((tin, subject)))
            }
            Some(_) if origin == Origin::App => Err(format!(
                "{tool}: `{key}` must be a ticket when an app calls (an app never names a TIN)"
            )),
            _ => Ok(None),
        }
    };
    let mut implied_return = None;
    for key in TIN_ARGS {
        let Some(value) = arguments.get(key).cloned() else {
            continue;
        };
        if let Some((tin, subject)) = tin_of(key, &value)? {
            implied_return = subject.return_value().map(str::to_string);
            arguments.insert(key.into(), Value::String(tin));
        }
    }
    if let Some(Value::Array(tins)) = arguments.get("tins").cloned() {
        let mut out = Vec::with_capacity(tins.len());
        for value in &tins {
            out.push(match tin_of("tins", value)? {
                Some((tin, _)) => Value::String(tin),
                None => value.clone(),
            });
        }
        arguments.insert("tins".into(), Value::Array(out));
    }
    for key in RETURN_ARGS {
        let Some(text) = arguments.get(key).and_then(Value::as_str).map(str::to_string) else {
            continue;
        };
        if !tickets::is_ticket(&text) {
            continue;
        }
        let subject = lookup(&text)?;
        let value = subject.return_value().ok_or_else(|| {
            format!("{tool}: `{key}` holds a taxpayer's ticket; pass it as `tin`")
        })?;
        arguments.insert(key.into(), Value::String(value.to_string()));
    }
    // A return's ticket passed as `tin` names its form and period too.
    if let Some(value) = implied_return {
        let key = match tool {
            "bir_form_open" => Some("code"),
            "bir_app_open" | "bir_drafts_list" => Some("form"),
            _ => None,
        };
        if let Some(key) = key
            && !arguments.contains_key(key)
        {
            arguments.insert(key.into(), Value::String(value));
        }
    }
    Ok(())
}

// ------------------------------------------------------------ tickets out

fn token_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"@bir:(taxprofile|branchcode|taxform):([0-9][0-9A-Za-z/\-]*)").expect("regex")
    })
}

fn dashed_tin_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b\d{3}-\d{3}-\d{3}(?:-\d{3,5})?\b").expect("regex"))
}

/// The subject a v0 token (`@bir:<scope>:<value>`) names.
pub(crate) fn subject_of_token(scope: &str, value: &str, today: NaiveDate) -> Option<Subject> {
    match scope {
        "taxprofile" => Subject::of_tin(value.trim_end_matches('/')),
        "branchcode" => {
            let (root, code) = value.split_once('/')?;
            let root = taxform::tin_digits(root).filter(|root| root.len() == 9)?;
            let code: String = code.chars().filter(char::is_ascii_digit).collect();
            (!code.is_empty() && code.len() <= 5)
                .then(|| Subject::of_tin(&format!("{root}{code:0>5}")))
                .flatten()
        }
        "taxform" => Subject::of_return(value, today),
        _ => None,
    }
}

/// The keys whose string value is a TIN in BIR's results.
const TIN_KEYS: [&str; 5] = ["tin", "tin_root", "tin_display", "branch_tin", "root"];
/// The keys whose value is a v0 `rox://app/bir/` link.
const LINK_KEYS: [&str; 2] = ["open", "link"];

/// Rewrites what a BIR tool answered so it names taxpayers by ticket: TIN
/// fields, `@bir:` tokens, formatted TINs in text and v0 app links. Only
/// saved taxpayers get tickets; any other TIN becomes `[TIN]`.
pub(crate) struct Scrubber<'a> {
    db: &'a Database,
    today: NaiveDate,
    issued: HashMap<String, Option<String>>,
}

impl<'a> Scrubber<'a> {
    pub(crate) fn new(db: &'a Database, today: NaiveDate) -> Self {
        Self {
            db,
            today,
            issued: HashMap::new(),
        }
    }

    /// The ticket of a saved subject.
    fn ticket(&mut self, subject: &Subject) -> Option<String> {
        let key = format!("{subject:?}");
        if let Some(found) = self.issued.get(&key) {
            return found.clone();
        }
        let ticket = subject
            .title(self.db, self.today)
            .and_then(|_| tickets::issue(self.db, subject).ok());
        self.issued.insert(key, ticket.clone());
        ticket
    }

    /// A whole tool result: `structuredContent` and every text content.
    pub(crate) fn result(&mut self, result: &mut Value) {
        if let Some(structured) = result.get_mut("structuredContent") {
            self.value(structured);
        }
        if let Some(contents) = result.get_mut("content").and_then(Value::as_array_mut) {
            for content in contents {
                let Some(text) = content.get("text").and_then(Value::as_str) else {
                    continue;
                };
                let rewritten = match serde_json::from_str::<Value>(text) {
                    Ok(mut parsed) if parsed.is_object() || parsed.is_array() => {
                        self.value(&mut parsed);
                        if text.contains('\n') {
                            serde_json::to_string_pretty(&parsed).expect("json value")
                        } else {
                            parsed.to_string()
                        }
                    }
                    _ => self.text(text),
                };
                content["text"] = Value::String(rewritten);
            }
        }
    }

    pub(crate) fn value(&mut self, value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, item) in map.iter_mut() {
                    if TIN_KEYS.contains(&key.as_str())
                        && let Some(text) = item.as_str()
                        && let Some(subject) = Subject::of_tin(text)
                    {
                        *item = Value::String(self.ticket(&subject).unwrap_or_else(|| "[TIN]".into()));
                        continue;
                    }
                    if key == "tins"
                        && let Value::Array(tins) = item
                    {
                        for tin in tins.iter_mut() {
                            if let Some(subject) = tin.as_str().and_then(Subject::of_tin) {
                                *tin = Value::String(self.ticket(&subject).unwrap_or_else(|| "[TIN]".into()));
                            }
                        }
                        continue;
                    }
                    if key == "key"
                        && let Some(text) = item.as_str()
                        && let Some(subject) = Subject::of_return(text, self.today)
                    {
                        *item = Value::String(self.ticket(&subject).unwrap_or_else(|| "[return]".into()));
                        continue;
                    }
                    if LINK_KEYS.contains(&key.as_str())
                        && let Some(text) = item.as_str()
                        && text.starts_with(resolve::V0_LINK_PREFIX)
                    {
                        *item = Value::String(self.link(text));
                        continue;
                    }
                    self.value(item);
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| self.value(item)),
            Value::String(text) => *text = self.text(text),
            _ => {}
        }
    }

    /// Free text: tokens become canonical tokens, formatted TINs tickets.
    pub(crate) fn text(&mut self, text: &str) -> String {
        let today = self.today;
        let tokens = token_re().replace_all(text, |caps: &regex::Captures<'_>| {
            let scope = &caps[1];
            match subject_of_token(scope, &caps[2], today).and_then(|subject| self.ticket(&subject)) {
                Some(ticket) => format!("@bir:{scope}:{ticket}"),
                None => format!("@bir:{scope}:[unresolved]"),
            }
        });
        dashed_tin_re()
            .replace_all(&tokens, |caps: &regex::Captures<'_>| {
                Subject::of_tin(&caps[0])
                    .and_then(|subject| self.ticket(&subject))
                    .unwrap_or_else(|| "[TIN]".into())
            })
            .into_owned()
    }

    /// A v0 `rox://app/bir/<app>?props=` link as a v1 link with a ticket.
    fn link(&mut self, link: &str) -> String {
        let Some((app, props)) = resolve::parse_v0_link(link) else {
            return String::new();
        };
        match v1_props(self.db, &app, &props, self.today) {
            Ok(opened) => app_link(&app, &opened),
            Err(_) => String::new(),
        }
    }
}

// ------------------------------------------------------------ opening apps

/// An app's v1 props and the ticket of what it opens on.
pub(crate) struct Opened {
    pub props: Value,
    pub ticket: Option<String>,
}

/// v0 props (`{tin, branch?, form?, period?, ...}`) as v1 props: the TIN
/// becomes a ticket (a return's when `form` and `period` name one), and
/// what is left that names no taxpayer stays.
pub(crate) fn v1_props(db: &Database, app: &str, v0: &Value, today: NaiveDate) -> Result<Opened, String> {
    let mut props = Map::new();
    for (key, value) in v0.as_object().into_iter().flatten() {
        if !["tin", "branch", "scope", "server"].contains(&key.as_str()) {
            props.insert(key.clone(), value.clone());
        }
    }
    let tin = v0.get("tin").and_then(Value::as_str).and_then(taxform::tin_digits);
    let Some(tin) = tin else {
        return Ok(Opened {
            props: Value::Object(props),
            ticket: None,
        });
    };
    let branch: String = v0
        .get("branch")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    let who = if branch.is_empty() {
        tin.clone()
    } else {
        format!("{}{branch:0>5}", &tin[..9])
    };
    let form = v0.get("form").and_then(Value::as_str);
    let period = v0.get("period").and_then(Value::as_str);
    let subject = match (app, form, period) {
        ("forms", Some(form), Some(period)) => {
            Subject::of_return(&format!("{}/{form}-{period}", taxform::token_tin(&who)), today)
        }
        _ => None,
    }
    .or_else(|| Subject::of_tin(&who))
    .ok_or_else(|| format!("`{who}` is not a TIN"))?;
    let ticket = tickets::issue(db, &subject)?;
    props.insert(manifest::TICKET_PROP.into(), Value::String(ticket.clone()));
    Ok(Opened {
        props: Value::Object(props),
        ticket: Some(ticket),
    })
}

/// `rox://app/dev.goldcoders.bir/<app>[?ticket=…][&props=…]` (9.4): the
/// props other than the ticket ride along when they fit 1024 bytes.
pub(crate) fn app_link(app: &str, opened: &Opened) -> String {
    let mut link = format!("rox://app/{}/{app}", manifest::PLUGIN_ID);
    let mut rest = opened.props.as_object().cloned().unwrap_or_default();
    rest.remove(manifest::TICKET_PROP);
    let mut sep = '?';
    if let Some(ticket) = &opened.ticket {
        link.push_str(&format!("?ticket={ticket}"));
        sep = '&';
    }
    if !rest.is_empty() {
        let encoded = resolve::encode_component(&Value::Object(rest).to_string());
        if encoded.len() <= 1024 {
            link.push(sep);
            link.push_str("props=");
            link.push_str(&encoded);
        }
    }
    link
}

/// `_meta["rox/open"]` (9.6).
pub(crate) fn open_meta(app: &str, title: &str, opened: &Opened, placement: &str) -> Value {
    let mut open = json!({
        "app": app,
        "props": opened.props,
        "title": title.chars().take(60).collect::<String>(),
        "placement": placement,
    });
    if let Some(ticket) = &opened.ticket {
        open["ticket"] = json!(ticket);
    }
    open
}

// ------------------------------------------------------------ resolve

/// `bir_resolve` as spec 9.2 asks: `{kind, name, value, complete,
/// context: {attached: [{scope, token}]}}` in, one `result` out. The
/// resolver behind it is [`resolve::resolve`]; tickets in the value and the
/// attached tokens are turned into the TINs it reads, and what it answers
/// back into tickets.
pub(crate) fn resolve(db: &Database, arguments: &Map<String, Value>, today: NaiveDate) -> Value {
    match try_resolve(db, arguments, today) {
        Ok(answer) => answer,
        Err(message) => json!({ "result": "error", "message": clip(&message, 200) }),
    }
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn try_resolve(db: &Database, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Value, String> {
    if let Some(key) = arguments
        .keys()
        .find(|key| !["kind", "name", "value", "complete", "context"].contains(&key.as_str()))
    {
        return Err(format!("unknown argument `{key}`"));
    }
    let kind = arguments.get("kind").and_then(Value::as_str).unwrap_or("scope");
    if !["command", "scope"].contains(&kind) {
        return Err("`kind` must be command or scope".into());
    }
    let name = arguments
        .get("name")
        .and_then(Value::as_str)
        .ok_or("`name` is required")?
        .trim()
        .to_ascii_lowercase();
    let value = arguments.get("value").and_then(Value::as_str).unwrap_or("");
    let complete = arguments.get("complete").and_then(Value::as_bool).unwrap_or(false);
    let attached = arguments
        .get("context")
        .and_then(|context| context.get("attached"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("token").or(Some(entry)).and_then(Value::as_str))
        .filter_map(|token| {
            let ticket = token.rsplit(':').next()?;
            match tickets::subject(db, ticket).ok()?? {
                subject @ (Subject::Taxpayer(_) | Subject::Branch(_)) => Some(subject.tin()),
                Subject::Return(_) => None,
            }
        })
        .collect();
    let request = resolve::Request {
        args: expand_value(db, &name, value)?,
        name: name.clone(),
        complete,
        attached,
    };
    let answer = resolve::resolve(db, &request, today);
    let mut scrubber = Scrubber::new(db, today);
    if let Some(choices) = answer.get("choices").and_then(Value::as_array) {
        let choices: Vec<Value> = choices
            .iter()
            .map(|choice| {
                let value = choice["value"].as_str().unwrap_or("");
                json!({
                    "label": clip(choice["label"].as_str().unwrap_or(value), 80),
                    "detail": clip(choice["detail"].as_str().unwrap_or(""), 160),
                    "value": choice_value(&mut scrubber, &name, value),
                })
            })
            .take(20)
            .collect();
        if complete || !choices.is_empty() {
            return Ok(json!({ "result": "choices", "choices": choices }));
        }
    }
    if answer["ok"] != true {
        let message = answer["error"].as_str().unwrap_or("Could not resolve that");
        return Ok(json!({ "result": "error", "message": clip(message, 200) }));
    }
    let token = answer["token"].as_str().unwrap_or("");
    let (scope, token_value) = token
        .strip_prefix("@bir:")
        .and_then(|rest| rest.split_once(':'))
        .ok_or("the resolver named nothing")?;
    let subject = subject_of_token(scope, token_value, today).ok_or("the resolver named nothing")?;
    let ticket = tickets::issue(db, &subject)?;
    let title = subject
        .title(db, today)
        .unwrap_or_else(|| clip(answer["title"].as_str().unwrap_or(""), 60));
    if kind == "scope" {
        return Ok(json!({ "result": "token", "ticket": ticket, "title": title }));
    }
    let link = answer["open"].as_str().unwrap_or("");
    let (app, props) = resolve::parse_v0_link(link).ok_or("the resolver opened nothing")?;
    let opened = v1_props(db, &app, &props, today)?;
    Ok(json!({
        "result": "open",
        "app": app,
        "props": opened.props,
        "ticket": opened.ticket.unwrap_or(ticket),
        "title": title,
        "placement": "full",
    }))
}

/// A typed value with its leading ticket turned back into what the resolver
/// reads: the root, a branch's `root/code` (`branchcode`) or 14 digits, or
/// a return's `<tin>/<FORM>-<period>`.
fn expand_value(db: &Database, name: &str, value: &str) -> Result<String, String> {
    let value = value.trim();
    let Some(ticket) = value.get(..30).filter(|head| tickets::is_ticket(head)) else {
        return Ok(value.to_string());
    };
    let rest = &value[30..];
    let subject = tickets::subject(db, ticket)?
        .ok_or("That item is no longer available")?;
    let head = match (&subject, name) {
        (Subject::Branch(tin), "branchcode") if rest.is_empty() => format!("{}/{}", &tin[..9], &tin[9..]),
        (Subject::Return(value), _) => value.clone(),
        (subject, _) => subject.tin(),
    };
    Ok(format!("{head}{rest}"))
}

/// A choice's value with its TIN turned into a ticket: a whole subject (a
/// taxpayer, a branch, a complete return) is its ticket alone; a step
/// along the way (`<tin>/` then `<tin>/<FORM>`) keeps its tail.
fn choice_value(scrubber: &mut Scrubber<'_>, name: &str, value: &str) -> String {
    let today = scrubber.today;
    let whole = match name {
        "taxform" => Subject::of_return(value, today),
        "branchcode" => subject_of_token("branchcode", value, today),
        _ => Subject::of_tin(value),
    };
    if let Some(ticket) = whole.and_then(|subject| scrubber.ticket(&subject)) {
        return ticket;
    }
    let (head, tail) = match value.split_once('/') {
        Some((head, tail)) => (head, format!("/{tail}")),
        None => (value, String::new()),
    };
    match Subject::of_tin(head).and_then(|subject| scrubber.ticket(&subject)) {
        Some(ticket) => format!("{ticket}{tail}"),
        None => value.to_string(),
    }
}

#[cfg(test)]
mod tests;
