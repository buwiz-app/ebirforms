//! Mini App Protocol v1's reserved `rox.*` tools and the caller of a
//! `tools/call` (`_meta["rox/caller"]`): what Buwiz Bot (the protocol's
//! host) asks of a plugin before and around its own tools.
//!
//! - `rox.provision`: the plugin key, on every spawn, kept in memory only;
//!   nothing else runs before it.
//! - `rox.session.open` / `rox.session.close`: a session certificate per
//!   app open, signed with that key.
//! - `rox.ticket.resolve`: a ticket's title (never its subject).
//! - `rox.uninstall`: every ticket forgotten.

use std::collections::HashMap;

use bir_app_core::BirApp;
use bir_app_core::bir_core::forms::taxform;
use serde_json::{Map, Value, json};

use crate::keys::{OpenRequest, PluginKey};
use crate::manifest::{self, ManifestSource};
use crate::tickets;

/// Who a `tools/call` comes from (`_meta["rox/caller"].origin`, 8.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    App,
    Agent,
    Routine,
    Resolve,
    /// No `rox/caller` (tests, a plain MCP client).
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
        let text = |key: &str| caller.and_then(|caller| caller.get(key)).and_then(Value::as_str).map(str::to_string);
        let origin = match text("origin").as_deref() {
            Some("app") => Origin::App,
            Some("agent") => Origin::Agent,
            Some("routine") => Origin::Routine,
            Some("resolve") => Origin::Resolve,
            _ => Origin::Unknown,
        };
        Self { origin, app: text("app"), open_id: text("open_id") }
    }
}

/// A reserved tool's answer: `structuredContent` is the object itself.
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

/// The reserved tools this plugin implements (7.8).
pub(crate) const RESERVED: [&str; 5] = [
    "rox.provision",
    "rox.session.open",
    "rox.session.close",
    "rox.ticket.resolve",
    "rox.uninstall",
];

/// What `rox.*` keeps between requests: the plugin key (memory only, P4)
/// and the opens it certified.
pub(crate) struct Host {
    key: Option<PluginKey>,
    /// open_id -> app id.
    sessions: HashMap<String, String>,
    /// Spawned by the Bot: nothing but `rox.provision` runs before it (P7).
    require_provision: bool,
}

impl Host {
    pub(crate) fn new(require_provision: bool) -> Self {
        Self { key: None, sessions: HashMap::new(), require_provision }
    }

    /// Whether ordinary tools may run.
    pub(crate) fn ready(&self) -> bool {
        !self.require_provision || self.key.is_some()
    }

    /// Whether the Bot provisioned this process (then app calls must name
    /// an open it certified).
    pub(crate) fn provisioned(&self) -> bool {
        self.key.is_some()
    }

    /// The app of an open this process certified.
    pub(crate) fn session_app(&self, open_id: &str) -> Option<&str> {
        self.sessions.get(open_id).map(String::as_str)
    }

    /// A reserved tool. `None`: not one of ours (-32601). `Some(Err)`:
    /// refused before provisioning (-32600).
    pub(crate) fn reserved(
        &mut self,
        manifest: &ManifestSource,
        app: Result<&BirApp, &str>,
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
            "rox.provision" => self.provision(manifest, arguments),
            "rox.session.open" => self.session_open(manifest, arguments),
            "rox.session.close" => {
                if let Some(open_id) = arguments.get("open_id").and_then(Value::as_str) {
                    self.sessions.remove(open_id);
                }
                ok(json!({ "ok": true }))
            }
            "rox.ticket.resolve" => ticket_resolve(app.ok(), arguments),
            _ => {
                self.sessions.clear();
                match app.and_then(|app| tickets::forget_all(app).map_err(|_| "the tickets were not forgotten")) {
                    Ok(()) => ok(json!({ "ok": true })),
                    Err(message) => fail("internal", message),
                }
            }
        }))
    }

    fn provision(&mut self, manifest: &ManifestSource, arguments: &Map<String, Value>) -> Value {
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
        let sha256 = match manifest.sha256() {
            Ok(sha256) => sha256,
            Err(message) => return fail("manifest_invalid", &message),
        };
        self.key = Some(key);
        ok(json!({ "ok": true, "manifest_sha256": sha256 }))
    }

    fn session_open(&mut self, manifest: &ManifestSource, arguments: &Map<String, Value>) -> Value {
        let Some(key) = &self.key else {
            return fail("invalid_args", "rox.provision must come first");
        };
        let request = match OpenRequest::read(arguments, chrono::Utc::now().timestamp_millis()) {
            Ok(request) => request,
            Err(message) => return fail("invalid_args", &message),
        };
        let app = match manifest.app(&request.app) {
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
}

/// `rox.ticket.resolve`: a title, never the subject (P3).
fn ticket_resolve(app: Option<&BirApp>, arguments: &Map<String, Value>) -> Value {
    let ticket = arguments.get("ticket").and_then(Value::as_str).unwrap_or("");
    let title = app.and_then(|app| {
        tickets::subject(app, ticket).ok().flatten()?.title(app, taxform::today())
    });
    match title {
        Some(title) => ok(json!({ "valid": true, "title": title })),
        None => ok(json!({ "valid": false })),
    }
}
