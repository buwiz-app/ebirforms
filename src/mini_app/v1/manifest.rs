//! BIR's v1 manifest (spec 5): identity, commands, scopes, routines and the
//! apps with what each may reach.
//!
//! [`unsigned`] builds the manifest from this build's code and the app
//! sources it serves: every fingerprint is the SHA-256 of the exact bytes
//! `resources/read` answers, so the manifest and the apps cannot drift.
//! `bir-mini-app print-v1-manifest` prints it, and
//! `scripts/sign-v1-manifest.sh` signs it with the publisher key through the
//! broker's `sign-manifest.mjs`. The signed file is what this process
//! serves (`BIR_V1_MANIFEST`), byte for byte, read on every request as the
//! broker's fixture does. Without it the unsigned manifest is served and Rox
//! refuses the plugin, which is the point: only a signed BIR runs in Rox.
//!
//! Each app's `permissions.tools` names the tools its source calls and
//! nothing more; [`super::app_tools`] holds the host functions behind the
//! `bir_app_*` ones, and `the_permissions_name_only_listed_tools` keeps both
//! in step with `tools/list`.

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::super::apps::{Catalog, MIME_TYPE};
use super::super::resolve::RESOLVE_TOOL;
use super::app_tools;
use super::jcs;

pub(crate) const PROTOCOL: &str = "1.0.0";
pub(crate) const PLUGIN_ID: &str = "dev.goldcoders.bir";
pub(crate) const SHORT: &str = "bir";
pub(crate) const VERSION: &str = "2.4.2";
pub(crate) const MANIFEST_URI: &str = "rox://manifest";
pub(crate) const MANIFEST_MIME: &str = "application/json";
/// The signed manifest file this process serves.
pub(crate) const MANIFEST_ENV: &str = "BIR_V1_MANIFEST";
/// The Google Workspace plugin the routines read mail through (Q1).
pub(crate) const GWS_PLUGIN: &str = "dev.goldcoders.google-workspace";
/// The props key every app takes its subject from.
pub(crate) const TICKET_PROP: &str = "ticket";
/// A receipt scan or photo, up to 25 MB (`bir_mark_paid`'s own limit).
const RECEIPT_EXTENSIONS: [&str; 7] = [".pdf", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".heic"];
const RECEIPT_MAX_BYTES: u64 = 25 * 1024 * 1024;
/// Gmail, where Open in Gmail shows a payment's email.
const GMAIL: &str = "https://mail.google.com";
/// A return's attachment (a scan or a PDF) or a sales sheet to import.
const RETURN_FILE_EXTENSIONS: [&str; 7] = [".pdf", ".png", ".jpg", ".jpeg", ".heic", ".xlsx", ".csv"];

/// `sha256:<hex>` of an app source as served.
pub(crate) fn fingerprint(source: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(source.as_bytes())))
}

/// One app's manifest entry, apart from what the catalog supplies (title,
/// description, source).
struct AppSpec {
    id: &'static str,
    placement: &'static [&'static str],
    inline: Option<Value>,
    props: Value,
    permissions: Value,
}

fn ticket_schema() -> Value {
    json!({ "type": "string", "maxLength": 30 })
}

/// The `bir_app_*` tool of each host function an app calls.
fn host_tools(functions: &[&str]) -> Vec<String> {
    functions.iter().map(|name| app_tools::tool_name(name)).collect()
}

/// An answer card app (spec 9.8, [`super::super::cards`]): inline, its
/// props the card kind, the snapshot and how Refresh re-reads it.
fn card(id: &'static str, permissions: Value) -> AppSpec {
    AppSpec {
        id,
        placement: &["inline"],
        inline: Some(super::super::cards::inline(id)),
        props: json!({
            "type": "object",
            "properties": {
                "card": { "type": "string", "maxLength": 32 },
                "ticket": ticket_schema(),
                "snapshot": { "type": "object" },
                "as_of": { "type": "integer" },
                "tool": { "type": "string", "maxLength": 64 },
                "args": { "type": "object" }
            }
        }),
        permissions,
    }
}

fn specs() -> Vec<AppSpec> {
    let mut forms_tools = vec![
        app_tools::TICKET_SUBJECT.to_string(),
        app_tools::TICKET_ISSUE.to_string(),
        app_tools::UPLOAD.to_string(),
        app_tools::PRINT_PREVIEW.to_string(),
        "bir_payment_receipt".to_string(),
        "bir_payments_list".to_string(),
        "bir_mark_paid".to_string(),
        "bir_record_payment".to_string(),
    ];
    forms_tools.extend(host_tools(app_tools::FORMS));
    let mut profile_tools = vec![
        app_tools::TICKET_SUBJECT.to_string(),
        app_tools::TICKET_ISSUE.to_string(),
    ];
    profile_tools.extend(host_tools(app_tools::TAX_PROFILE));
    let mut calendar_tools = vec![app_tools::TICKET_SUBJECT.to_string()];
    calendar_tools.extend(host_tools(app_tools::CALENDAR));
    vec![
        AppSpec {
            id: "forms",
            placement: &["full"],
            inline: None,
            props: json!({
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
            permissions: json!({
                "tools": forms_tools,
                // Open in Gmail: a payment's email (asks each time)
                "open": { "apps": ["tax-profile"], "external": [GMAIL] },
                "attach": ["taxprofile", "taxform"],
                "pick_file": { "extensions": RECEIPT_EXTENSIONS, "max_bytes": RECEIPT_MAX_BYTES, "multiple": false },
                // Print preview and the receipt in Rox's document view
                // (`bir_app_print_preview`, `bir_payment_receipt`); the
                // largest sheets are ~1.7 MB
                "preview": {},
            }),
        },
        AppSpec {
            id: "tax-profile",
            placement: &["full"],
            inline: None,
            props: json!({
                "type": "object",
                "properties": {
                    "ticket": ticket_schema(),
                    "year": { "type": "integer", "minimum": 1900, "maximum": 2200 }
                }
            }),
            permissions: json!({
                "tools": profile_tools,
                "open": { "apps": ["forms"] },
                "copy": true,
            }),
        },
        AppSpec {
            id: "calendar",
            placement: &["full"],
            inline: None,
            props: json!({ "type": "object", "properties": { "ticket": ticket_schema() } }),
            permissions: json!({
                "tools": calendar_tools,
                "open": { "apps": ["forms"] },
            }),
        },
        AppSpec {
            id: "deadlines",
            placement: &["inline", "full"],
            inline: Some(json!({ "width": 480, "height": 320, "min_width": 320, "resizable": true })),
            props: json!({ "type": "object", "properties": { "ticket": ticket_schema() } }),
            permissions: json!({
                "tools": ["bir_calendar_get"],
                "open": { "apps": ["forms", "calendar"] },
            }),
        },
        card("card-people", json!({
            "tools": ["bir_find_taxpayer", "bir_taxpayer", "bir_board", "bir_quick_add", "bir_card_add_taxpayer"],
            "open": { "apps": ["tax-profile", "forms", "register", "deadlines"] },
            "attach": ["taxprofile"],
        })),
        card("card-forms", json!({
            "tools": [
                "bir_profile_forms", "bir_returns_list", "bir_form_needs_you", "bir_mark_paid", "bir_form_info",
                "bir_drafts_list", "bir_outbox", "bir_card_cancel_send", "bir_board",
            ],
            "open": { "apps": ["forms", "calendar", "tax-profile"] },
        })),
        card("card-dates", json!({
            "tools": ["bir_calendar_get", "bir_penalty"],
            "open": { "apps": ["forms", "calendar"] },
        })),
        card("card-money", json!({
            "tools": ["bir_unpaid", "bir_mark_paid", "bir_record_payment", "bir_app_upload", "bir_payments_list", "bir_digest", "bir_payment_receipt"],
            "open": { "apps": ["forms"], "external": [GMAIL] },
            // the receipt card's View receipt
            "preview": {},
            "pick_file": { "extensions": RECEIPT_EXTENSIONS, "max_bytes": RECEIPT_MAX_BYTES, "multiple": false },
        })),
        card("card-help", json!({
            "tools": ["bir_status"],
            "open": { "apps": ["register", "forms"] },
        })),
        card("card-return", json!({
            "tools": ["bir_return", "bir_app_upload", "bir_card_attach", "bir_card_import", "bir_card_import_fill"],
            "open": { "apps": ["forms"] },
            "pick_file": { "extensions": RETURN_FILE_EXTENSIONS, "max_bytes": RECEIPT_MAX_BYTES, "multiple": false },
        })),
        card("card-quick", json!({
            "tools": [
                "bir_card_allow_send", "bir_mark_paid", "bir_record_payment", "bir_payments_list", "bir_app_upload", "bir_payment_receipt",
                "bir_status", "bir_deadline", "bir_deadline_override_set", "bir_deadline_override_remove", "bir_calendar_set",
            ],
            "open": { "apps": ["forms", "register", "calendar"] },
            // Payment found remembers "Not this return"
            "storage": {},
            // and shows the receipt once recorded
            "preview": {},
            "pick_file": { "extensions": RECEIPT_EXTENSIONS, "max_bytes": RECEIPT_MAX_BYTES, "multiple": false },
        })),
        card("card-calc", json!({
            "tools": ["bir_lookup"],
        })),
        AppSpec {
            id: "register",
            placement: &["full"],
            inline: None,
            props: json!({ "type": "object", "properties": {} }),
            permissions: json!({
                "tools": host_tools(app_tools::REGISTER),
                "open": { "apps": ["forms", "tax-profile"] },
                "copy": true,
            }),
        },
    ]
}

/// What the agent is told when a message holds an `@bir:taxform:` token.
const TAXFORM_HINT: &str = "An `@bir:taxform:<ticket>` token names one return (taxpayer, form \
and period); the ticket stands for it and is all you see. Call bir_form_open with the ticket as \
`code`, then bir_form_needs_you and bir_form_fields. Walk the user through what the return still \
needs, one item at a time, with the official rule for each (bir_form_validate's messages). Offer \
to read an attached Excel or CSV and fill from it with bir_form_fill (`source: document`). Never \
submit unless a send grant exists (bir_form_send answers approval_required otherwise); then tell \
the user to press Submit in the Forms app. bir_app_open with `form` set to the ticket shows the \
return.";

/// What the agent is told when a taxpayer token is in a message or attached.
const TAXPROFILE_HINT: &str = "An `@bir:taxprofile:<ticket>` token names one taxpayer; the \
ticket stands for its TIN, which you never see. Attached to this chat, BIR tools default to that \
taxpayer: pass the ticket as `tin` to bir_drafts_list, bir_form_open, bir_calendar_get, \
bir_calendar_set, bir_payments_list, bir_mark_paid, bir_record_payment, bir_filing_record and \
bir_app_open (forms, tax-profile, calendar, deadlines), and in `tins` to bir_confirmation_search \
and bir_payment_search, unless the user names another taxpayer. bir_dues_list and \
bir_profiles_list read deadlines and profiles (taxpayers come back as tickets); bir_app_open \
{\"app\": \"deadlines\", \"tin\": <ticket>} shows the next returns due as a card.";

/// The same, for one branch.
const BRANCHCODE_HINT: &str = "An `@bir:branchcode:<ticket>` token names one branch of a \
taxpayer; the ticket stands for that branch's TIN. Attached to this chat, BIR tools default to \
that branch: pass the ticket as `tin` to bir_drafts_list, bir_form_open, bir_calendar_get and \
bir_app_open, unless the user names another taxpayer.";

const CONFIRMATIONS_PROMPT: &str = "New emails matched the BIR confirmation search. For each one: \
read it with gws_read_mail and find BIR's File name (e.g. 00000000000000-2551Qv2018-092026Q3.xml) \
and the Date and Time received by BIR. The file name starts with the TIN (with the branch) and \
names the form and period (092026Q3 is Q3 2026, 092026 is September 2026). Call \
bir_confirm_filing with tin, form, period, reference (the File name), received_at (BIR's date \
and time, Manila) and message_id (the email's id). If the email is not a filing confirmation, \
skip it. Then post a short summary, one line per email: the return (form, period, taxpayer name, \
never the TIN) and the result: confirmed, already confirmed, or not confirmed with the tool's \
reason. Do not file, submit or change anything else.";

const PAYMENTS_PROMPT: &str = "New emails matched the BIR payment search. For each one: read it \
with gws_read_mail and pass its text (subject and body) and message_id (the email's id) to \
bir_payment_parse. If it returns a payment, call bir_record_payment with its `record` as it is (it \
carries the message_id and the email's text). If it returns null, read the email yourself: when it is a payment of a BIR \
return, call bir_record_payment with tin (and branch), form, period (the return period; its last \
day such as 09/30/2026 is fine), channel (who took it), reference, amount (the tax, without \
fees), fee, paid_at, message_id and raw; when it is not, skip it. A payment never confirms a \
filing: leave confirmation to BIR's own email. Then post a short summary, one line per payment: \
the return (form, period, taxpayer name, never the TIN), amount and channel, and whether it was \
recorded, already recorded, or for a return not in BIR.";

fn routine(id: &str, name: &str, description: &str, tool: &str, prompt: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "description": description,
        "every_s": 300,
        "query": { "tool": tool, "field": "query" },
        "prompt": prompt,
        "reads": [{ "plugin": GWS_PLUGIN, "tools": ["gws_search_mail", "gws_read_mail"] }],
    })
}

/// The manifest without its signature, fingerprints taken from `catalog`.
pub(crate) fn unsigned(catalog: &Catalog) -> Result<Value, String> {
    let sources = catalog.apps()?;
    let mut apps = Vec::new();
    for spec in specs() {
        let Some(app) = sources.iter().find(|app| app.id == spec.id) else {
            continue;
        };
        let mut entry = json!({
            "id": spec.id,
            "title": app.title,
            "description": app.description,
            "entry": format!("rox-app://{}", spec.id),
            "mime": MIME_TYPE,
            "sdk": "1",
            "fingerprint": fingerprint(&app.source),
            "props": spec.props,
            "placement": spec.placement,
            "permissions": spec.permissions,
        });
        if let Some(inline) = spec.inline {
            entry["inline"] = inline;
        }
        apps.push(entry);
    }
    Ok(json!({
        "protocol": PROTOCOL,
        "id": PLUGIN_ID,
        "short": SHORT,
        "version": VERSION,
        "title": "BIR",
        "description": "Philippine BIR returns, filing calendar and taxpayer profiles, kept on this computer.",
        "publisher": { "id": "dev.goldcoders", "key_id": "gc-2026-01", "name": "Goldcoders" },
        "commands": [
            { "name": "taxprofile", "args": "[tin or name]", "description": "Set or open the current taxpayer", "resolve": RESOLVE_TOOL },
            { "name": "taxform", "args": "[form] [period] [view|file|submit]", "description": "Open a BIR return", "resolve": RESOLVE_TOOL },
        ],
        "scopes": [
            { "name": "taxprofile", "description": "Taxpayer by TIN or name", "resolve": RESOLVE_TOOL, "attach": true, "hint": TAXPROFILE_HINT },
            { "name": "branchcode", "description": "Branch of a taxpayer", "resolve": RESOLVE_TOOL, "attach": true, "hint": BRANCHCODE_HINT },
            { "name": "taxform", "description": "Return: form and period", "resolve": RESOLVE_TOOL, "attach": true, "hint": TAXFORM_HINT },
        ],
        "routines": [
            routine(
                "bir-confirmations",
                "BIR filing confirmations",
                "Checks mail for BIR's eBIRForms confirmation emails and marks each filed return confirmed in BIR.",
                "bir_confirmation_search",
                CONFIRMATIONS_PROMPT,
            ),
            routine(
                "bir-payments",
                "BIR payments (MyEG and banks)",
                "Checks mail for payment emails (MyEG, banks, GCash, Maya, LandBank Link.BizPortal) and records each payment on its BIR return.",
                "bir_payment_search",
                PAYMENTS_PROMPT,
            ),
        ],
        "apps": apps,
    }))
}

/// The manifest text this process serves: the signed file named by
/// `BIR_V1_MANIFEST`, else the unsigned manifest (which Rox refuses).
pub(crate) fn served_text(catalog: &Catalog) -> Result<String, String> {
    match std::env::var_os(MANIFEST_ENV).filter(|path| !path.is_empty()) {
        Some(path) => std::fs::read_to_string(&path)
            .map_err(|error| format!("{MANIFEST_ENV} {}: {error}", path.to_string_lossy())),
        None => Ok(serde_json::to_string_pretty(&unsigned(catalog)?).expect("json value")),
    }
}

/// The served manifest, parsed.
pub(crate) fn served(catalog: &Catalog) -> Result<Value, String> {
    let text = served_text(catalog)?;
    let value: Value =
        serde_json::from_str(&text).map_err(|error| format!("the manifest is not JSON: {error}"))?;
    if !value.is_object() {
        return Err("the manifest is not a JSON object".into());
    }
    Ok(value)
}

/// `manifest_sha256` (5.2 step 7): SHA-256 of the served manifest's JCS bytes.
pub(crate) fn served_sha256(catalog: &Catalog) -> Result<String, String> {
    jcs::sha256_hex(&served(catalog)?)
}

/// One app of the served manifest.
pub(crate) fn served_app(catalog: &Catalog, id: &str) -> Result<Option<Value>, String> {
    Ok(served(catalog)?["apps"]
        .as_array()
        .and_then(|apps| apps.iter().find(|app| app["id"] == id))
        .cloned())
}

/// `sha256(JCS(permissions))`, an absent object being `{}` (4.4).
pub(crate) fn permissions_digest(app: &Value) -> Result<String, String> {
    let empty = Value::Object(Map::new());
    let permissions = app.get("permissions").unwrap_or(&empty);
    jcs::sha256_hex(permissions)
}

/// Why the signed manifest is not this build's, if it is not: a changed
/// app, tool or text needs a new signature. `None` when it matches (or none
/// is configured).
pub(crate) fn staleness(catalog: &Catalog) -> Option<String> {
    std::env::var_os(MANIFEST_ENV).filter(|path| !path.is_empty())?;
    let served = match served(catalog) {
        Ok(served) => served,
        Err(error) => return Some(error),
    };
    let mut body = served.clone();
    if let Some(map) = body.as_object_mut() {
        map.remove("signature");
    }
    let built = unsigned(catalog).ok()?;
    if served.get("signature").is_none() {
        return Some(format!("{MANIFEST_ENV} has no signature"));
    }
    (jcs::canonical(&body).ok() != jcs::canonical(&built).ok()).then(|| {
        format!("{MANIFEST_ENV} is not this build's manifest (its apps or tools changed); sign it again with scripts/sign-v1-manifest.sh")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(value: &Value) -> usize {
        value.as_str().map(|text| text.chars().count()).unwrap_or(0)
    }

    #[test]
    fn the_manifest_has_the_v1_identity_and_caps() {
        let m = unsigned(&Catalog::Embedded).unwrap();
        assert_eq!(m["protocol"], PROTOCOL);
        assert_eq!(m["id"], PLUGIN_ID);
        assert_eq!(m["short"], SHORT);
        assert_eq!(m["version"], VERSION);
        assert_eq!(
            m["publisher"],
            json!({"id": "dev.goldcoders", "key_id": "gc-2026-01", "name": "Goldcoders"})
        );
        assert!(m.get("needs").is_none());
        for command in m["commands"].as_array().unwrap() {
            assert!(chars(&command["description"]) <= 200);
            assert!(chars(&command["args"]) <= 120);
        }
        for scope in m["scopes"].as_array().unwrap() {
            assert!(chars(&scope["description"]) <= 120, "{scope}");
            assert!(chars(&scope["hint"]) <= 1500, "{} is {}", scope["name"], chars(&scope["hint"]));
        }
        for routine in m["routines"].as_array().unwrap() {
            assert!(chars(&routine["prompt"]) <= 4000);
            assert!(chars(&routine["description"]) <= 300);
            assert!(routine.get("needs").is_none());
            assert_eq!(routine["query"]["field"], "query");
            assert_eq!(
                routine["reads"],
                json!([{"plugin": GWS_PLUGIN, "tools": ["gws_search_mail", "gws_read_mail"]}])
            );
        }
        // the whole manifest is JCS-able (integers only) and under 256 KiB
        let text = jcs::canonical(&m).unwrap();
        assert!(text.len() < 256 * 1024);
    }

    #[test]
    fn every_app_has_entry_fingerprint_placement_and_bounded_permissions() {
        let catalog = Catalog::Embedded;
        let m = unsigned(&catalog).unwrap();
        let apps = m["apps"].as_array().unwrap();
        let ids: Vec<&str> = apps.iter().map(|app| app["id"].as_str().unwrap()).collect();
        assert_eq!(
            ids,
            [
                "forms", "tax-profile", "calendar", "deadlines", "card-people", "card-forms", "card-dates", "card-money", "card-help",
                "card-return", "card-quick", "card-calc", "register"
            ]
        );
        for app in apps {
            let id = app["id"].as_str().unwrap();
            assert_eq!(app["entry"], format!("rox-app://{id}"));
            assert_eq!(app["mime"], "text/x-nsh-tsx");
            assert_eq!(app["sdk"], "1");
            assert!(chars(&app["title"]) <= 60 && chars(&app["description"]) <= 300);
            let source = catalog.find(id).unwrap().unwrap().source;
            assert_eq!(app["fingerprint"], fingerprint(&source));
            let tools = app["permissions"]["tools"].as_array().unwrap();
            assert!(!tools.is_empty() && tools.len() <= 64, "{id}: {} tools", tools.len());
            assert!(tools.iter().all(|tool| tool != "bir_call"), "{id} names bir_call");
            assert!(app["props"]["properties"].as_object().unwrap().len() <= 16);
        }
        let deadlines = &apps[3];
        assert_eq!(deadlines["placement"], json!(["inline", "full"]));
        assert_eq!(deadlines["inline"]["width"], 480);
        assert_eq!(deadlines["permissions"]["tools"], json!(["bir_calendar_get"]));
        assert_eq!(deadlines["permissions"]["open"]["apps"], json!(["forms", "calendar"]));
    }

    #[test]
    fn print_previews_and_receipts_go_to_the_apps_that_show_them() {
        let m = unsigned(&Catalog::Embedded).unwrap();
        for app in m["apps"].as_array().unwrap() {
            let id = app["id"].as_str().unwrap();
            let permissions = &app["permissions"];
            let tools = permissions["tools"].as_array().unwrap();
            let has = |tool: &str| tools.iter().any(|t| t == tool);
            let shows = ["forms", "card-money", "card-quick"].contains(&id);
            assert_eq!(permissions.get("preview").is_some(), shows, "{id}: preview");
            if shows {
                assert_eq!(permissions["preview"], json!({}));
            }
            // only Forms prints a return; the three show receipts
            assert_eq!(has(app_tools::PRINT_PREVIEW), id == "forms", "{id}");
            assert_eq!(has("bir_payment_receipt"), shows, "{id}");
            // Open in Gmail, from the apps that show a payment's email
            let gmail = permissions["open"]["external"].as_array().is_some_and(|o| o.iter().any(|x| x == GMAIL));
            assert_eq!(gmail, ["forms", "card-money"].contains(&id), "{id}: open.external");
        }
    }

    #[test]
    fn the_permissions_digest_is_over_jcs() {
        let app = json!({"permissions": {"tools": ["bir_x"], "copy": true}});
        let direct = jcs::sha256_hex(&json!({"copy": true, "tools": ["bir_x"]})).unwrap();
        assert_eq!(permissions_digest(&app).unwrap(), direct);
        assert_eq!(
            permissions_digest(&json!({})).unwrap(),
            jcs::sha256_hex(&json!({})).unwrap()
        );
    }
}
