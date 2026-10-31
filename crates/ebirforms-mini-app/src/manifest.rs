//! The v1 manifest (spec 5): identity, the `taxprofile` scope and the one
//! app this plugin ships, Forms, with what it may reach.
//!
//! [`unsigned`] builds it from this build's code and the app source it
//! serves: the fingerprint is the SHA-256 of the exact bytes
//! `resources/read` answers, so manifest and app cannot drift.
//! `buwiz-mcp print-manifest` prints it for signing with the publisher key
//! (outside this repo). The signed file is what this process serves, byte
//! for byte, read on every request: `BUWIZ_MCP_MANIFEST`, else
//! `dev.goldcoders.bir.manifest.json` next to the executable. Without one
//! the unsigned manifest is served and the Bot refuses the plugin, which is
//! the point: only a signed build runs in the Bot.
//!
//! The plugin id stays `dev.goldcoders.bir`, so a Bot install of the old
//! `bir-mini-app` updates in place.

use std::path::PathBuf;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::jcs;
use crate::scope;
use crate::server::{
    PAYMENTS_LIST, PRINT_PREVIEW, RESOLVE, TICKET_ISSUE, TICKET_SUBJECT, app_tool_name,
};

pub(crate) const PROTOCOL: &str = "1.0.0";
pub const PLUGIN_ID: &str = "dev.goldcoders.bir";
pub(crate) const SHORT: &str = "bir";
/// Past the last `bir-mini-app` manifest (2.6.3): the first one served by
/// Buwiz.
pub(crate) const VERSION: &str = "3.0.0";
pub(crate) const MANIFEST_URI: &str = "rox://manifest";
pub(crate) const MANIFEST_MIME: &str = "application/json";
/// The signed manifest file this process serves.
pub const MANIFEST_ENV: &str = "BUWIZ_MCP_MANIFEST";
/// The signed manifest's file name next to the executable, when
/// [`MANIFEST_ENV`] is not set.
pub const MANIFEST_FILE: &str = "dev.goldcoders.bir.manifest.json";

/// The Forms app: `rox-app://forms`, the TSX module the Bot renders.
pub(crate) const FORMS_ID: &str = "forms";
pub(crate) const FORMS_TITLE: &str = "BIR Forms";
pub(crate) const FORMS_DESCRIPTION: &str = "Prepare, check and queue a taxpayer's BIR returns: pick a form and period, fill it, validate it with the official rules, save the draft, and queue it for filing.";
pub(crate) const FORMS_SOURCE: &str = include_str!("../mini-apps/forms.tsx");
pub(crate) const APP_URI_PREFIX: &str = "rox-app://";
pub(crate) const APP_MIME: &str = "text/x-nsh-tsx";

/// What the agent is told when a taxpayer token is in a message or attached.
const TAXPROFILE_HINT: &str = "An `@bir:taxprofile:<ticket>` token names one taxpayer; the \
ticket stands for its TIN, which you never see. Attached to this chat, Buwiz tools default to \
that taxpayer: pass the ticket as `tin` to bir_drafts_list, bir_dues_list, bir_form_open and \
bir_app_open (forms), unless the user names another taxpayer. bir_profiles_list lists the \
taxpayers (as tickets).";

/// `sha256:<hex>` of an app source as served.
pub(crate) fn fingerprint(source: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(source.as_bytes())))
}

/// The tools the Forms app calls: the ticket helpers, the print preview,
/// the (empty) payments list and one `bir_app_<function>` per function.
pub(crate) fn forms_tools() -> Vec<String> {
    let mut tools: Vec<String> = [TICKET_SUBJECT, TICKET_ISSUE, PRINT_PREVIEW, PAYMENTS_LIST]
        .iter()
        .map(|tool| tool.to_string())
        .collect();
    tools.extend(scope::FORMS.iter().map(|(function, ..)| app_tool_name(function)));
    tools
}

/// The Forms app's manifest entry.
pub(crate) fn forms_entry() -> Value {
    json!({
        "id": FORMS_ID,
        "title": FORMS_TITLE,
        "description": FORMS_DESCRIPTION,
        "entry": format!("{APP_URI_PREFIX}{FORMS_ID}"),
        "mime": APP_MIME,
        "sdk": "1",
        "fingerprint": fingerprint(FORMS_SOURCE),
        "props": {
            "type": "object",
            "properties": {
                "ticket": { "type": "string", "maxLength": 30 },
                "form": { "type": "string", "maxLength": 64 },
                "period": { "maxLength": 32 },
                "action": { "type": "string", "enum": ["view", "file", "submit"] },
                "draft_id": { "type": "integer" },
                "form_code": { "type": "string", "maxLength": 16 },
                "year": { "type": "integer", "minimum": 1900, "maximum": 2200 },
                "key": { "type": "integer" }
            }
        },
        "placement": ["full"],
        "permissions": {
            "tools": forms_tools(),
            "attach": ["taxprofile"],
            // the print preview in the Bot's document view
            "preview": {},
        },
    })
}

/// The manifest without its signature.
pub fn unsigned() -> Value {
    json!({
        "protocol": PROTOCOL,
        "id": PLUGIN_ID,
        "short": SHORT,
        "version": VERSION,
        "title": "BIR",
        "description": "Philippine BIR returns for the taxpayers kept in Buwiz on this computer.",
        "publisher": { "id": "dev.goldcoders", "key_id": "gc-2026-01", "name": "Goldcoders" },
        "commands": [],
        "scopes": [
            { "name": "taxprofile", "description": "Taxpayer by TIN or name", "resolve": RESOLVE, "attach": true, "hint": TAXPROFILE_HINT },
        ],
        "routines": [],
        "apps": [forms_entry()],
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
        let Self::File(_) = self else { return None };
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
            "the signed manifest is not this build's (its app or tools changed): print it with `buwiz-mcp print-manifest` and sign it again".to_string()
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
    fn the_manifest_keeps_the_plugin_id_and_names_only_forms() {
        let m = unsigned();
        assert_eq!(m["id"], "dev.goldcoders.bir");
        assert_eq!(m["protocol"], PROTOCOL);
        let apps = m["apps"].as_array().unwrap();
        assert_eq!(apps.len(), 1);
        let forms = &apps[0];
        assert_eq!(forms["entry"], "rox-app://forms");
        assert_eq!(forms["fingerprint"], fingerprint(FORMS_SOURCE));
        let tools = forms["permissions"]["tools"].as_array().unwrap();
        assert!(!tools.is_empty() && tools.len() <= 64, "{} tools", tools.len());
        assert!(tools.iter().all(|tool| tool.as_str().is_some_and(|t| t.starts_with("bir_"))));
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
