//! The 2.0 manifest (spec 5): identity, the `/taxprofile` and `/taxform`
//! commands, the `taxprofile`, `branchcode` and `taxform` scopes, and the
//! apps this plugin ships (Forms, Tax Profile, Calendar and the Deadlines
//! card), each with only the tools its source calls.
//!
//! [`unsigned`] builds it from this build's code and the app sources it
//! serves: each fingerprint is the SHA-256 of the exact bytes
//! `resources/read` answers, so manifest and apps cannot drift.
//! `ebirforms-mini-app manifest` prints it for signing with the publisher key
//! (outside this repo). The signed file is what this process serves, byte
//! for byte, read on every request: `EBIRFORMS_MANIFEST`, else
//! `app.buwiz.ebirforms.manifest.json` next to the executable. Without one
//! the unsigned manifest is served and the Bot refuses the plugin, which is
//! the point: only a signed build runs in the Bot.
//!
//! The plugin id is `app.buwiz.ebirforms` (the owner's `app.buwiz.<repo>`
//! rule); the publisher is `app.buwiz`, signing with key `gc-2026-01`.

use std::path::PathBuf;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::jcs;
use crate::scope;
use crate::server::{
    ACCOUNT_SIGN_IN_POLL, ACCOUNT_SIGN_IN_START, ACCOUNT_SIGN_OUT, ACCOUNT_STATUS, CALENDAR_GET,
    PAYMENTS_LIST, PRINT_PREVIEW, RESOLVE, TICKET_ISSUE, TICKET_SUBJECT, app_tool_name,
};

pub(crate) const PROTOCOL: &str = "2.0.0";
pub const PLUGIN_ID: &str = "app.buwiz.ebirforms";
pub(crate) const SHORT: &str = "ebirforms";
/// The first version under the plugin id `app.buwiz.ebirforms`.
pub(crate) const VERSION: &str = "0.2.0";
pub(crate) const MANIFEST_URI: &str = "buwiz://manifest";
pub(crate) const MANIFEST_MIME: &str = "application/json";
/// The signed manifest file this process serves.
pub const MANIFEST_ENV: &str = "EBIRFORMS_MANIFEST";
/// The signed manifest's file name next to the executable, when
/// [`MANIFEST_ENV`] is not set.
pub const MANIFEST_FILE: &str = "app.buwiz.ebirforms.manifest.json";

/// The Forms app: `buwiz-app://forms`, the TSX module the Bot renders.
pub(crate) const FORMS_ID: &str = "forms";
pub(crate) const FORMS_TITLE: &str = "BIR Forms";
pub(crate) const FORMS_SOURCE: &str = include_str!("../../../apps/ebirforms/forms.tsx");
/// One taxpayer's registration, branches, tax years and forms.
pub(crate) const TAX_PROFILE_ID: &str = "tax-profile";
pub(crate) const TAX_PROFILE_SOURCE: &str = include_str!("../../../apps/ebirforms/tax-profile.tsx");
/// Every return's due date and status, month by month.
pub(crate) const CALENDAR_ID: &str = "calendar";
pub(crate) const CALENDAR_SOURCE: &str = include_str!("../../../apps/ebirforms/calendar.tsx");
/// Buwiz sign-in (account.buwiz.com's device flow).
pub(crate) const REGISTER_ID: &str = "register";
pub(crate) const REGISTER_SOURCE: &str = include_str!("../../../apps/ebirforms/register.tsx");
/// The only page the Sign-in app opens.
const ACCOUNT_SITE: &str = "https://account.buwiz.com";
/// The next returns due, as a card in the chat.
pub(crate) const DEADLINES_ID: &str = "deadlines";
pub(crate) const DEADLINES_SOURCE: &str = include_str!("../../../apps/ebirforms/deadlines.tsx");
pub(crate) const APP_URI_PREFIX: &str = "buwiz-app://";
pub(crate) const APP_MIME: &str = "text/x-nsh-tsx";

/// One app this plugin ships: what `resources/list` names and
/// `resources/read` serves.
pub(crate) struct App {
    pub(crate) id: &'static str,
    pub(crate) title: &'static str,
    pub(crate) description: &'static str,
    pub(crate) source: &'static str,
}

/// The apps, in manifest order.
pub(crate) const APPS: [App; 5] = [
    App {
        id: FORMS_ID,
        title: FORMS_TITLE,
        description: "Prepare, check and queue a taxpayer's BIR returns: pick a form and period, fill it, validate it with the official rules, save the draft, and queue it for filing.",
        source: FORMS_SOURCE,
    },
    App {
        id: TAX_PROFILE_ID,
        title: "BIR Tax Profile",
        description: "View and edit a taxpayer's registration: name, TIN and branches, RDO and address, and the forms they file each year.",
        source: TAX_PROFILE_SOURCE,
    },
    App {
        id: CALENDAR_ID,
        title: "BIR Calendar",
        description: "See when each taxpayer's BIR returns are due and where each one stands, month by month, and move a deadline (or put it back) for every taxpayer.",
        source: CALENDAR_SOURCE,
    },
    App {
        id: DEADLINES_ID,
        title: "BIR Deadlines",
        description: "The next BIR returns due for one taxpayer (or all of them), as a small card in the chat, each with a button that opens it in BIR Forms.",
        source: DEADLINES_SOURCE,
    },
    App {
        id: REGISTER_ID,
        title: "Buwiz Sign-in",
        description: "Sign in to your Buwiz account on this computer, which eBIRForms will need to file returns. Preparing and saving returns works without it.",
        source: REGISTER_SOURCE,
    },
];

/// What the agent is told when a taxpayer token is in a message or attached.
const TAXPROFILE_HINT: &str = "An `@ebirforms:taxprofile:<ticket>` token names one taxpayer; the \
ticket stands for its TIN, which you never see. Attached to this chat, eBIRForms tools default to \
that taxpayer: pass the ticket as `tin` to ebirforms_drafts_list, ebirforms_dues_list, \
ebirforms_calendar_get, ebirforms_form_open and ebirforms_app_open (forms, tax-profile, calendar, \
deadlines), unless the user names another taxpayer. ebirforms_profiles_list lists the taxpayers \
(as tickets); ebirforms_app_open {\"app\": \"deadlines\", \"tin\": <ticket>} shows the next \
returns due as a card.";

/// The same, for one branch.
const BRANCHCODE_HINT: &str = "An `@ebirforms:branchcode:<ticket>` token names one branch of a \
taxpayer; the ticket stands for that branch's TIN. Attached to this chat, eBIRForms tools default \
to that branch: pass the ticket as `tin` to ebirforms_drafts_list, ebirforms_form_open, \
ebirforms_calendar_get and ebirforms_app_open, unless the user names another taxpayer.";

/// What the agent is told when a message holds a return's token.
const TAXFORM_HINT: &str = "An `@ebirforms:taxform:<ticket>` token names one return (taxpayer, \
form and period); the ticket stands for it and is all you see. Call ebirforms_form_open with the \
ticket as `code`, then ebirforms_form_fields. Walk the user through what the return still needs, \
one item at a time, with the official rule for each (ebirforms_form_validate's messages). Save \
with ebirforms_form_save_draft; nothing is queued or filed from a chat. ebirforms_app_open with \
`form` set to the ticket shows the return.";

/// `sha256:<hex>` of an app source as served.
pub(crate) fn fingerprint(source: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(source.as_bytes())))
}

/// The `ebirforms_app_<function>` tool of each host function an app calls.
fn host_tools(functions: &[&str]) -> Vec<String> {
    functions
        .iter()
        .map(|function| app_tool_name(function))
        .collect()
}

/// The tools the Forms app calls: the ticket helpers, the print preview,
/// the (empty) payments list and one `ebirforms_app_<function>` per function.
pub(crate) fn forms_tools() -> Vec<String> {
    let mut tools: Vec<String> = [TICKET_SUBJECT, TICKET_ISSUE, PRINT_PREVIEW, PAYMENTS_LIST]
        .iter()
        .map(|tool| tool.to_string())
        .collect();
    tools.extend(host_tools(scope::FORMS));
    tools
}

/// The Sign-in app's tools: never the assistant's.
pub(crate) fn account_tools() -> Vec<String> {
    [
        ACCOUNT_STATUS,
        ACCOUNT_SIGN_IN_START,
        ACCOUNT_SIGN_IN_POLL,
        ACCOUNT_SIGN_OUT,
    ]
    .iter()
    .map(|tool| tool.to_string())
    .collect()
}

fn ticket_schema() -> Value {
    json!({ "type": "string", "maxLength": 30 })
}

/// Each app's props, placement, inline size and permissions.
fn app_spec(id: &str) -> (Value, Value, Option<Value>, Value) {
    let ticket_only = json!({ "type": "object", "properties": { "ticket": ticket_schema() } });
    match id {
        FORMS_ID => (
            json!({
                "type": "object",
                "properties": {
                    "ticket": ticket_schema(),
                    "form": { "type": "string", "maxLength": 64 },
                    "period": { "maxLength": 32 },
                    "action": { "type": "string", "enum": ["view", "file", "submit"] },
                    "draft_id": { "type": "integer" },
                    "form_code": { "type": "string", "maxLength": 16 },
                    "year": { "type": "integer", "minimum": 1900, "maximum": 2200 },
                    "key": { "type": "integer" }
                }
            }),
            json!(["full"]),
            None,
            json!({
                "tools": forms_tools(),
                // the Tax profile button
                "open": { "apps": [TAX_PROFILE_ID] },
                // Ask the assistant: the taxpayer, or this return
                "attach": ["taxprofile", "taxform"],
                // the print preview in the Bot's document view
                "preview": {},
            }),
        ),
        TAX_PROFILE_ID => {
            let mut tools = vec![TICKET_SUBJECT.to_string(), TICKET_ISSUE.to_string()];
            tools.extend(host_tools(scope::TAX_PROFILE));
            (
                json!({
                    "type": "object",
                    "properties": {
                        "ticket": ticket_schema(),
                        "year": { "type": "integer", "minimum": 1900, "maximum": 2200 }
                    }
                }),
                json!(["full"]),
                None,
                json!({ "tools": tools, "open": { "apps": [FORMS_ID] }, "copy": true }),
            )
        }
        CALENDAR_ID => {
            let mut tools = vec![TICKET_SUBJECT.to_string()];
            tools.extend(host_tools(scope::CALENDAR));
            (
                ticket_only,
                json!(["full"]),
                None,
                json!({ "tools": tools, "open": { "apps": [FORMS_ID] } }),
            )
        }
        REGISTER_ID => (
            json!({ "type": "object", "properties": {} }),
            json!(["full"]),
            None,
            json!({
                "tools": account_tools(),
                // Open account.buwiz.com/device, Copy code
                "open": { "external": [ACCOUNT_SITE] },
                "copy": true,
            }),
        ),
        _ => (
            ticket_only,
            json!(["inline", "full"]),
            // held until its calendar is read (`buwiz.ready()`)
            Some(
                json!({ "width": 480, "height": 320, "min_width": 320, "resizable": true, "wait_ready": true }),
            ),
            json!({ "tools": [CALENDAR_GET], "open": { "apps": [FORMS_ID, CALENDAR_ID] } }),
        ),
    }
}

/// One app's manifest entry.
pub(crate) fn app_entry(app: &App) -> Value {
    let (props, placement, inline, permissions) = app_spec(app.id);
    let mut entry = json!({
        "id": app.id,
        "title": app.title,
        "description": app.description,
        "entry": format!("{APP_URI_PREFIX}{}", app.id),
        "mime": APP_MIME,
        "sdk": "1",
        "fingerprint": fingerprint(app.source),
        "props": props,
        "placement": placement,
        "permissions": permissions,
    });
    if let Some(inline) = inline {
        entry["inline"] = inline;
    }
    entry
}

/// The publisher key the release manifest names.
pub const RELEASE_KEY_ID: &str = "gc-2026-01";

/// The manifest without its signature, under the release key.
pub fn unsigned() -> Value {
    unsigned_for(RELEASE_KEY_ID)
}

/// The app `id`, if this plugin ships it.
pub(crate) fn app(id: &str) -> Option<&'static App> {
    APPS.iter().find(|app| app.id == id)
}

/// The source of the app `id`, exactly as `resources/read` serves it.
pub fn app_source(id: &str) -> Option<&'static str> {
    app(id).map(|app| app.source)
}

/// The manifest without its signature, naming publisher key `key_id` (a
/// test key for conformance; the release key otherwise).
pub fn unsigned_for(key_id: &str) -> Value {
    json!({
        "protocol": PROTOCOL,
        "id": PLUGIN_ID,
        "short": SHORT,
        "version": VERSION,
        "title": "eBIRForms",
        "description": "Philippine BIR returns for the taxpayers kept in the eBIRForms mini app on this computer.",
        "publisher": { "id": "app.buwiz", "key_id": key_id, "name": "Buwiz" },
        "commands": [
            { "name": "taxprofile", "args": "[tin or name]", "description": "Open a taxpayer's tax profile", "resolve": RESOLVE },
            { "name": "taxform", "args": "[form] [period] [view|file|submit]", "description": "Open a BIR return", "resolve": RESOLVE },
        ],
        "scopes": [
            { "name": "taxprofile", "description": "Taxpayer by TIN or name", "resolve": RESOLVE, "attach": true, "hint": TAXPROFILE_HINT },
            { "name": "branchcode", "description": "Branch of a taxpayer", "resolve": RESOLVE, "attach": true, "hint": BRANCHCODE_HINT },
            { "name": "taxform", "description": "Return: form and period", "resolve": RESOLVE, "attach": true, "hint": TAXFORM_HINT },
        ],
        "routines": [],
        "apps": APPS.iter().map(app_entry).collect::<Vec<_>>(),
    })
}

/// Where the signed manifest is read from.
#[derive(Debug, Clone)]
pub enum ManifestSource {
    /// A signed file (re-read on every request).
    File(PathBuf),
    /// No signed file: the unsigned manifest (the Bot refuses it).
    Unsigned,
}

impl ManifestSource {
    /// [`MANIFEST_ENV`], else [`MANIFEST_FILE`] next to the executable when
    /// it exists, else unsigned.
    pub fn from_env() -> Self {
        if let Some(path) = std::env::var_os(MANIFEST_ENV).filter(|path| !path.is_empty()) {
            return Self::File(PathBuf::from(path));
        }
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join(MANIFEST_FILE)))
            .filter(|path| path.is_file())
            .map_or(Self::Unsigned, Self::File)
    }

    /// The manifest text this process serves.
    pub(crate) fn text(&self) -> Result<String, String> {
        match self {
            Self::File(path) => std::fs::read_to_string(path)
                .map_err(|error| format!("the signed manifest {}: {error}", path.display())),
            Self::Unsigned => Ok(serde_json::to_string_pretty(&unsigned()).expect("json value")),
        }
    }

    /// The served manifest, parsed.
    pub(crate) fn served(&self) -> Result<Value, String> {
        let value: Value = serde_json::from_str(&self.text()?)
            .map_err(|error| format!("the manifest is not JSON: {error}"))?;
        if !value.is_object() {
            return Err("the manifest is not a JSON object".into());
        }
        Ok(value)
    }

    /// `manifest_sha256` (5.2 step 7): SHA-256 of the served JCS bytes.
    pub(crate) fn sha256(&self) -> Result<String, String> {
        jcs::sha256_hex(&self.served()?)
    }

    /// One app of the served manifest.
    pub(crate) fn app(&self, id: &str) -> Result<Option<Value>, String> {
        Ok(self.served()?["apps"]
            .as_array()
            .and_then(|apps| apps.iter().find(|app| app["id"] == id))
            .cloned())
    }

    /// Why the signed manifest is not this build's, if it is not: a changed
    /// app, tool or text needs a new signature.
    pub fn staleness(&self) -> Option<String> {
        let Self::File(_) = self else {
            return None;
        };
        let served = match self.served() {
            Ok(served) => served,
            Err(error) => return Some(error),
        };
        if served.get("signature").is_none() {
            return Some("the signed manifest has no signature".into());
        }
        let mut body = served;
        if let Some(map) = body.as_object_mut() {
            map.remove("signature");
        }
        (jcs::canonical(&body).ok() != jcs::canonical(&unsigned()).ok()).then(|| {
            "the signed manifest is not this build's (its app or tools changed): print it with `ebirforms-mini-app manifest` and sign it again".to_string()
        })
    }
}

/// `sha256(JCS(permissions))`, an absent object being `{}` (4.4).
pub(crate) fn permissions_digest(app: &Value) -> Result<String, String> {
    let empty = Value::Object(Map::new());
    jcs::sha256_hex(app.get("permissions").unwrap_or(&empty))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_keeps_the_plugin_id_and_lists_every_app() {
        let m = unsigned();
        assert_eq!(m["id"], "app.buwiz.ebirforms");
        assert_eq!(m["protocol"], PROTOCOL);
        let apps = m["apps"].as_array().unwrap();
        let ids: Vec<&str> = apps.iter().map(|app| app["id"].as_str().unwrap()).collect();
        assert_eq!(
            ids,
            ["forms", "tax-profile", "calendar", "deadlines", "register"]
        );
        for (entry, app) in apps.iter().zip(&APPS) {
            assert_eq!(entry["entry"], format!("buwiz-app://{}", app.id));
            assert_eq!(entry["fingerprint"], fingerprint(app.source));
            let tools = entry["permissions"]["tools"].as_array().unwrap();
            assert!(
                !tools.is_empty() && tools.len() <= 64,
                "{}: {} tools",
                app.id,
                tools.len()
            );
            for tool in tools {
                let tool = tool.as_str().unwrap();
                assert!(tool.starts_with("ebirforms_"), "{tool}");
                // every tool an app may call is one its source names
                let named = app.source.contains(&format!("\"{tool}\""))
                    || tool
                        .strip_prefix("ebirforms_account_")
                        .is_some_and(|action| app.source.contains(&format!("\"{action}\"")))
                    || tool
                        .strip_prefix("ebirforms_app_")
                        .is_some_and(|function| app.source.contains(&format!("\"{function}\"")));
                assert!(named, "{} lists {tool} but never calls it", app.id);
            }
        }
        let deadlines = &apps[3];
        assert_eq!(deadlines["placement"], json!(["inline", "full"]));
        assert_eq!(deadlines["inline"]["width"], 480);
        assert_eq!(
            deadlines["permissions"]["tools"],
            json!(["ebirforms_calendar_get"])
        );
        let commands: Vec<&str> = m["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|command| command["name"].as_str().unwrap())
            .collect();
        assert_eq!(commands, ["taxprofile", "taxform"]);
        let scopes: Vec<&str> = m["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|scope| scope["name"].as_str().unwrap())
            .collect();
        assert_eq!(scopes, ["taxprofile", "branchcode", "taxform"]);
        for scope in m["scopes"].as_array().unwrap() {
            assert!(scope["hint"].as_str().unwrap().chars().count() <= 1500);
        }
        // integers only, so it can be signed, and under 256 KiB
        assert!(jcs::canonical(&m).unwrap().len() < 256 * 1024);
        assert!(ManifestSource::Unsigned.staleness().is_none());
    }

    #[test]
    fn a_signed_file_that_is_not_this_build_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MANIFEST_FILE);
        let mut signed = unsigned();
        signed["signature"] = json!({"alg": "ed25519", "sig": "x"});
        std::fs::write(&path, signed.to_string()).unwrap();
        let source = ManifestSource::File(path.clone());
        assert!(source.staleness().is_none());
        assert_eq!(source.sha256().unwrap(), jcs::sha256_hex(&signed).unwrap());
        signed["version"] = json!("0.0.1");
        std::fs::write(&path, signed.to_string()).unwrap();
        assert!(source.staleness().is_some());
    }
}
