//! MCP for `bir-mini-app`: JSON-RPC 2.0 over stdio or the loopback HTTP door.
//!
//! Every tool runs in this process. The form, profile and dues tools are
//! bir-mcp's descriptions and argument checks over bir-shell's agent
//! [`Session`]; `bir_call` is the mini apps' bridge to the `bir` host module;
//! `bir_app_open` asks Rox to open one of the TSX apps this binary ships as
//! resources. There is no app discovery and no connect step: this process
//! owns the database.

use bir_mcp::license::{License, UNAUTHENTICATED};
use bir_mcp::server::{LATEST_PROTOCOL, SUPPORTED_PROTOCOLS};
use bir_mcp::signin::{self, DesktopStore};
use bir_mcp::tools::{self, Action, HostCall};
use gpui_agent::AgentHost;
use gpui_agent::protocol::Op;
use serde_json::{Map, Value, json};

use super::apps::{self, Catalog};
use super::bridge::{Bridge, Call};
use super::scope;
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
registration, `register` to sign in to Buwiz. Pass `tin` to open it for one taxpayer (only that \
taxpayer is reachable from the app then). You can also read taxpayer profiles and dues and draft \
a return yourself with the bir_form_* tools: open, read fields and context, fill with a stated \
source, list what needs the user, validate, save the draft, dismiss. Only one form is open at a \
time. bir_call is the mini apps' own bridge; do not call it yourself.";

const BIR_STATUS: &str = "bir_status";
const BIR_CALL: &str = "bir_call";
const BIR_APP_OPEN: &str = "bir_app_open";

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
            "resources/list" => self.catalog.list().map_err(|error| (INVALID_PARAMS, error)),
            "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
            "resources/read" => match params.get("uri").and_then(Value::as_str) {
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
            _ => {
                let tool = HOST_TOOLS
                    .contains(&name)
                    .then(|| tools::find(name))
                    .flatten()
                    .ok_or((INVALID_PARAMS, format!("unknown tool: {name}")))?;
                if let Err(message) = self.allow() {
                    return Ok(now(tool_error(message)));
                }
                let args = match tool.host_args(arguments) {
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
        tool_text(&json!({
            "app": SERVER_NAME,
            "mode": MODE,
            "database": crate::agent::database_kind(),
            "signed_in": crate::account_gate::is_unlocked(),
            "version": env!("CARGO_PKG_VERSION"),
        }))
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
        if let Some(key) = arguments
            .keys()
            .find(|key| !["app", "tin"].contains(&key.as_str()))
        {
            return Err(format!("unknown argument `{key}` (allowed: app, tin)"));
        }
        let id = arguments
            .get("app")
            .and_then(Value::as_str)
            .filter(|id| apps::OPENABLE.contains(id))
            .ok_or_else(|| format!("`app` must be one of {}", apps::OPENABLE.join(", ")))?;
        let tin = match arguments.get("tin") {
            None | Some(Value::Null) => None,
            Some(Value::String(tin)) if !tin.trim().is_empty() => Some(tin.trim().to_string()),
            Some(other) => return Err(format!("`tin` must be a TIN string (got {other})")),
        };
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
        let (props, text) = match tin {
            None => (json!({}), format!("Opening {}…", app.title)),
            Some(tin) => {
                let (row, root) = self.taxpayer(&tin)?;
                let name = row["name"].as_str().unwrap_or("this taxpayer").to_string();
                (
                    json!({ "tin": row["tin"].clone(), "scope": root }),
                    format!("Opening {} for {name}…", app.title),
                )
            }
        };
        Ok(json!({
            "content": [{ "type": "text", "text": text }],
            "isError": false,
            "_meta": { "rox/open": { "uri": app.uri(), "props": props } },
        }))
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
            "description": "Open one of BIR's mini apps for the user in Rox: `forms` (prepare, validate, save and queue returns), `tax-profile` (a taxpayer's registration and yearly forms) or `register` (sign in to Buwiz on this computer; works signed out). With `tin`, the app opens for that saved taxpayer only (check it with bir_profiles_list first); without it, the app lets the user pick. If a tool says Buwiz sign-in is required, open `register`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "app": { "type": "string", "enum": apps::OPENABLE },
                    "tin": { "type": "string", "description": "Taxpayer TIN (9-digit root or 14-digit branch TIN) as bir_profiles_list returns it." }
                },
                "required": ["app"],
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
    json!({ "tools": list })
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
        assert_eq!(names.len(), HOST_TOOLS.len() + 3);
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
            json!({"uri": "nsh://bir/forms", "props": {"tin": TIN, "scope": ROOT}})
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
