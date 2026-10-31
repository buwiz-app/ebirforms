//! The MCP contract, request to response, over a scratch database: what
//! the Bot sends (provision, session open, app and assistant calls) and the
//! shapes it gets back. One test: the database handle and the key are
//! process-wide.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bir_app_core::{BirApp, BranchFacts, NewTaxpayer};
use buwiz_mcp::{ManifestSource, Server};
use ed25519_dalek::{Signature, SigningKey, Verifier as _, VerifyingKey};
use serde_json::{Value, json};

const TEST_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const ROOT: &str = "000123453";
const OTHER: &str = "000987654";

struct Bot {
    server: Server,
    next: i64,
}

impl Bot {
    fn send(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let line = json!({ "jsonrpc": "2.0", "id": self.next, "method": method, "params": params }).to_string();
        let reply: Value = serde_json::from_str(&self.server.handle_line(&line).expect("a reply")).unwrap();
        assert_eq!(reply["id"], self.next, "{reply}");
        reply
    }

    fn call_as(&mut self, caller: Value, name: &str, arguments: Value) -> Value {
        let reply = self.send("tools/call", json!({ "name": name, "arguments": arguments, "_meta": { "rox/caller": caller } }));
        reply.get("result").cloned().unwrap_or_else(|| panic!("{name}: {reply}"))
    }

    fn agent(&mut self, name: &str, arguments: Value) -> Value {
        self.call_as(json!({ "origin": "agent" }), name, arguments)
    }
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn result_of(answer: &Value) -> &Value {
    assert_eq!(answer["isError"], false, "{answer}");
    &answer["structuredContent"]["result"]
}

fn error_text(answer: &Value) -> String {
    assert_eq!(answer["isError"], true, "{answer}");
    answer["content"][0]["text"].as_str().unwrap_or_default().to_string()
}

fn create(app: &BirApp, root: &str, name: &str) -> String {
    let taxpayer = app
        .create_taxpayer(&NewTaxpayer {
            tin_root: root.into(),
            name: name.into(),
            taxpayer_type: "Individual".into(),
            birth_date: Some("1990-01-15".into()),
            head_office: BranchFacts {
                rdo_code: "043".into(),
                registered_address: "1 Sample Street, Pasig City".into(),
                zip_code: "1600".into(),
                phone: "09171234567".into(),
                email: "juana@example.com".into(),
            },
        })
        .expect("taxpayer");
    taxpayer.branches[0].tin.clone()
}

#[test]
fn the_bot_contract_over_a_scratch_database() {
    // SAFETY: set before any thread of this test reads them (one test per process).
    unsafe {
        std::env::set_var("BUWIZ_DB_KEY", TEST_KEY);
        std::env::set_var("BUWIZ_NO_KEYCHAIN", "1");
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bir_data.db");
    let app = Arc::new(BirApp::open_at(&path).unwrap());
    let tin = create(&app, ROOT, "Juana Santos");
    let other_tin = create(&app, OTHER, "Pedro Cruz");
    let mut bot = Bot { server: Server::with_app(app.clone(), path.clone(), ManifestSource::Unsigned, true), next: 0 };

    // initialize, and nothing before rox.provision
    let init = bot.send("initialize", json!({ "protocolVersion": "2025-06-18" }));
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "buwiz-mcp");
    assert!(init["result"]["capabilities"]["resources"].is_object());
    assert_eq!(bot.send("tools/call", json!({ "name": "bir_status" }))["error"]["code"], -32600);
    assert_eq!(bot.send("tools/call", json!({ "name": "rox.nope" }))["error"]["code"], -32601);
    assert_eq!(bot.send("nope/nope", json!({}))["error"]["code"], -32601);
    let parse: Value = serde_json::from_str(&bot.server.handle_line("{not json").unwrap()).unwrap();
    assert_eq!(parse["error"]["code"], -32700);
    assert!(bot.server.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());

    // rox.provision: the plugin id stays dev.goldcoders.bir
    let seed = [7u8; 32];
    let signing = SigningKey::from_bytes(&seed);
    let public = b64(signing.verifying_key().as_bytes());
    let provision = json!({ "protocol": "1.0.0", "plugin": "dev.goldcoders.bir", "key": { "alg": "ed25519", "seed": b64(&seed) }, "pub": public });
    let mut wrong = provision.clone();
    wrong["plugin"] = json!("dev.goldcoders.other");
    assert_eq!(bot.call_as(json!({}), "rox.provision", wrong)["isError"], true);
    let provisioned = bot.call_as(json!({}), "rox.provision", provision);
    assert_eq!(provisioned["structuredContent"]["ok"], true, "{provisioned}");
    assert_eq!(provisioned["structuredContent"]["manifest_sha256"].as_str().unwrap().len(), 64);

    // tools and resources
    let tools = bot.send("tools/list", json!({}))["result"]["tools"].as_array().unwrap().clone();
    let tool = |name: &str| tools.iter().find(|tool| tool["name"] == name).cloned().unwrap_or_else(|| panic!("no {name}"));
    for name in ["bir_status", "bir_profiles_list", "bir_form_open", "bir_form_send", "bir_app_open", "bir_resolve"] {
        assert!(tool(name)["_meta"].is_null(), "{name} is the assistant's");
    }
    for name in ["bir_app_screen_form_open", "bir_app_screen_form_save", "bir_ticket_subject", "bir_app_print_preview"] {
        assert_eq!(tool(name)["_meta"]["rox/audience"], json!(["app"]), "{name}");
    }
    assert_eq!(tool("bir_app_screen_form_validate")["annotations"]["readOnlyHint"], true);
    assert_eq!(tool("bir_app_screen_form_save")["annotations"]["readOnlyHint"], false);
    let resources = bot.send("resources/list", json!({}))["result"]["resources"].clone();
    let uris: Vec<&str> = resources.as_array().unwrap().iter().map(|r| r["uri"].as_str().unwrap()).collect();
    assert_eq!(uris, ["rox-app://forms", "rox://manifest"]);
    let manifest: Value = serde_json::from_str(
        bot.send("resources/read", json!({ "uri": "rox://manifest" }))["result"]["contents"][0]["text"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["id"], "dev.goldcoders.bir");
    let forms = bot.send("resources/read", json!({ "uri": "rox-app://forms" }))["result"]["contents"][0].clone();
    assert_eq!(forms["mimeType"], "text/x-nsh-tsx");
    assert!(forms["text"].as_str().unwrap().contains("bir_app_"));
    assert!(bot.send("resources/read", json!({ "uri": "file:///etc/passwd" }))["error"].is_object());
    // every function the Forms app may call exists in bir-app-core
    let names = app.function_names();
    for tool in manifest["apps"][0]["permissions"]["tools"].as_array().unwrap() {
        if let Some(function) = tool.as_str().unwrap().strip_prefix("bir_app_").filter(|f| *f != "print_preview") {
            assert!(names.contains(&function), "{function} is not a BirApp function");
        }
    }

    // the assistant: taxpayers as tickets, never TINs
    let status = bot.agent("bir_status", json!({}));
    let status = result_of(&status).clone();
    assert_eq!(status["taxpayers"], 2, "{status}");
    assert_eq!(status["writable"], true, "the app is not running");
    let listed = bot.agent("bir_profiles_list", json!({}));
    let text = listed["content"][0]["text"].as_str().unwrap();
    assert!(!text.contains(ROOT) && !text.contains(OTHER), "a TIN reached the model: {text}");
    let juana = result_of(&listed).as_array().unwrap().iter().find(|t| t["name"] == "Juana Santos").unwrap().clone();
    let ticket = juana["tin_root"].as_str().unwrap().to_string();
    assert!(ticket.starts_with("tkt_"), "{juana}");
    let branch_ticket = juana["branches"][0]["tin"].as_str().unwrap().to_string();
    assert_eq!(branch_ticket, ticket, "the head office is the taxpayer");

    // the Bot resolves a ticket's title and the taxprofile scope
    let title = bot.call_as(json!({}), "rox.ticket.resolve", json!({ "ticket": ticket }));
    assert_eq!(title["structuredContent"], json!({ "valid": true, "title": "Juana Santos" }));
    let resolved = bot.call_as(json!({ "origin": "resolve" }), "bir_resolve", json!({ "kind": "scope", "name": "taxprofile", "value": "juana" }));
    assert_eq!(resolved["structuredContent"], json!({ "result": "token", "ticket": ticket, "title": "Juana Santos" }));
    let choices = bot.call_as(json!({ "origin": "resolve" }), "bir_resolve", json!({ "kind": "scope", "name": "taxprofile", "value": "", "complete": true }));
    assert_eq!(choices["structuredContent"]["choices"].as_array().unwrap().len(), 2, "{choices}");
    // a resolve call never writes
    let refused = bot.call_as(json!({ "origin": "resolve" }), "bir_form_save_draft", json!({}));
    assert_eq!(refused["structuredContent"]["code"], "permission_denied");

    // the assistant cannot call the app's tools
    let refused = bot.agent("bir_app_list_tax_years", json!({ "args": [tin], "ticket": ticket }));
    assert_eq!(refused["structuredContent"]["code"], "permission_denied", "{refused}");

    // bir_app_open hands the Bot the Forms app on a return's ticket
    let opened = bot.agent("bir_app_open", json!({ "app": "forms", "tin": ticket, "form": "2551Q", "period": "2026-Q3", "action": "file" }));
    let open = &opened["_meta"]["rox/open"];
    assert_eq!(open["app"], "forms", "{opened}");
    assert_eq!(open["placement"], "full");
    assert_eq!(open["props"]["form"], "2551Q");
    assert_eq!(open["props"]["period"], "2026-Q3");
    let return_ticket = open["props"]["ticket"].as_str().unwrap().to_string();
    assert!(return_ticket.starts_with("tkt_") && return_ticket != ticket);
    assert!(!opened.to_string().contains(ROOT));

    // an app open certified by the plugin key
    let perms = {
        let digest = buwiz_mcp::manifest::unsigned()["apps"][0]["permissions"].clone();
        // sha256 of the JCS bytes, as the Bot computes it
        jcs_sha256(&digest)
    };
    let open_id = b64(&[1u8; 16]);
    let not_after = chrono::Utc::now().timestamp_millis() + 3_600_000;
    let session = bot.call_as(json!({}), "rox.session.open", json!({
        "app": "forms", "open_id": open_id, "ks_pub": b64(&[2u8; 32]), "perms_sha256": perms, "not_after": not_after,
    }));
    let cert = session["structuredContent"]["cert"].clone();
    assert_eq!(cert["plugin"], "dev.goldcoders.bir", "{session}");
    let signature: [u8; 64] = URL_SAFE_NO_PAD.decode(session["structuredContent"]["cert_sig"].as_str().unwrap()).unwrap().try_into().unwrap();
    let verifying = VerifyingKey::from_bytes(&signing.verifying_key().to_bytes()).unwrap();
    assert!(verifying.verify(jcs(&cert).as_bytes(), &Signature::from_bytes(&signature)).is_ok());
    let forms_app = json!({ "origin": "app", "app": "forms", "open_id": open_id });
    let stranger = json!({ "origin": "app", "app": "forms", "open_id": b64(&[9u8; 16]) });
    assert_eq!(bot.call_as(stranger, "bir_ticket_subject", json!({ "ticket": ticket }))["structuredContent"]["code"], "permission_denied");

    // the app learns its ticket's subject, and is held to it
    let subject = bot.call_as(forms_app.clone(), "bir_ticket_subject", json!({ "ticket": return_ticket }));
    let subject = result_of(&subject).clone();
    assert_eq!(subject["kind"], "return");
    assert_eq!(subject["tin"], tin);
    assert_eq!(subject["form"], "2551Q");
    let years = bot.call_as(forms_app.clone(), "bir_app_list_tax_years", json!({ "args": [tin], "ticket": ticket }));
    assert_eq!(years["isError"], false, "{years}");
    let other = bot.call_as(forms_app.clone(), "bir_app_list_tax_years", json!({ "args": [other_tin], "ticket": ticket }));
    assert!(error_text(&other).contains("outside this app's scope"), "{other}");
    let no_ticket = bot.call_as(forms_app.clone(), "bir_app_list_tax_years", json!({ "args": [tin] }));
    assert!(error_text(&no_ticket).contains("ticket"));
    let rows = bot.call_as(forms_app.clone(), "bir_app_list_taxpayers", json!({ "ticket": ticket }));
    let rows = result_of(&rows).as_array().unwrap().clone();
    assert_eq!(rows.len(), 1, "a list is held to the ticket's taxpayer");
    assert_eq!(rows[0]["ticket"], ticket);
    let payments = bot.call_as(forms_app.clone(), "bir_payments_list", json!({ "tin": ticket, "form": "2551Q", "period": "2026-Q3" }));
    assert_eq!(result_of(&payments), &json!({ "payments": [] }));
    let opened = bot.call_as(forms_app.clone(), "bir_app_screen_form_open", json!({ "args_json": json!([tin, "2551Q", 2026, 3, null]).to_string(), "ticket": ticket }));
    let draft = result_of(&opened)["draft"].clone();
    assert!(draft.is_object(), "{opened}");

    // the assistant drafts a return
    let form = bot.agent("bir_form_open", json!({ "code": "2551Q", "tin": ticket, "period": "2026-Q3" }));
    let summary = result_of(&form).clone();
    assert_eq!(summary["form"], "2551Q", "{form}");
    assert_eq!(summary["tin"], ticket, "the open return names its taxpayer by ticket");
    assert_eq!(summary["year"], 2026);
    assert_eq!(summary["period"], 3);
    let fields = bot.agent("bir_form_fields", json!({}));
    assert!(result_of(&fields)["values"].as_object().is_some_and(|values| !values.is_empty()), "{fields}");
    let checked = bot.agent("bir_form_validate", json!({}));
    assert!(result_of(&checked)["field_errors"].is_array(), "{checked}");
    assert!(error_text(&bot.agent("bir_form_fill", json!({ "fields": {} }))).contains("fields"));
    let filled = bot.agent("bir_form_fill", json!({ "fields": { "no-such-box": "1.00" }, "source": "document" }));
    assert_eq!(filled["isError"], false, "{filled}");
    // unsaved edits keep it open
    assert!(error_text(&bot.agent("bir_form_dismiss", json!({}))).contains("unsaved"));

    // the Buwiz app open: it owns the database, so saving is refused here
    let held = bir_app_core::bir_core::db::try_acquire_live_owner_lock(&path).unwrap();
    let status = bot.agent("bir_status", json!({}));
    assert_eq!(result_of(&status)["writable"], false);
    let refused = bot.agent("bir_form_save_draft", json!({}));
    assert!(error_text(&refused).contains("Buwiz is open"), "{refused}");
    let refused = bot.call_as(forms_app.clone(), "bir_app_grant_send", json!({ "args": [tin, "2551Q"], "ticket": ticket }));
    assert!(error_text(&refused).contains("Buwiz is open"));
    drop(held);

    // the Buwiz app closed: saving goes through
    let saved = bot.agent("bir_form_save_draft", json!({}));
    let saved = result_of(&saved).clone();
    assert_eq!(saved["saved"], true, "{saved}");
    let draft_id = saved["id"].as_i64().expect("a saved id");
    let drafts = bot.agent("bir_drafts_list", json!({ "tin": ticket, "status": "all" }));
    let drafts = result_of(&drafts).as_array().unwrap().clone();
    assert!(drafts.iter().any(|row| row["draft_id"] == draft_id && row["form"] == "2551Q"), "{drafts:?}");
    assert!(drafts.iter().all(|row| row["tin"] == ticket));

    // sending needs the user's standing OK
    let send = bot.agent("bir_form_send", json!({}));
    assert_eq!(result_of(&send)["status"], "approval_required", "{send}");
    let granted = bot.call_as(forms_app.clone(), "bir_app_grant_send", json!({ "args": [tin, "2551Q"], "ticket": ticket }));
    assert_eq!(result_of(&granted)["form"], "2551Q", "{granted}");
    let grants = bot.call_as(forms_app.clone(), "bir_app_list_send_grants", json!({ "args": [tin], "ticket": ticket }));
    assert_eq!(result_of(&grants).as_array().unwrap().len(), 1);

    // the print preview of the saved return, for the Bot's document view
    let preview = bot.call_as(forms_app.clone(), "bir_app_print_preview", json!({ "ticket": ticket, "form": "2551Q", "draft_id": draft_id }));
    assert_eq!(preview["isError"], false, "{preview}");
    assert_eq!(preview["content"][0]["resource"]["mimeType"], "text/html");
    assert!(preview["content"][0]["resource"]["text"].as_str().unwrap().contains("<"));

    // dismissed after saving; rox.uninstall forgets every ticket
    assert_eq!(result_of(&bot.agent("bir_form_dismiss", json!({})))["dismissed"], true);
    assert_eq!(bot.call_as(json!({}), "rox.uninstall", json!({}))["structuredContent"]["ok"], true);
    let gone = bot.call_as(json!({}), "rox.ticket.resolve", json!({ "ticket": ticket }));
    assert_eq!(gone["structuredContent"]["valid"], false);
    bot.server.finish();
}

/// RFC 8785 for the test's integer-only objects: keys sorted, compact.
fn jcs(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            let body: Vec<String> = keys.iter().map(|key| format!("{}:{}", Value::String((*key).clone()), jcs(&map[*key]))).collect();
            format!("{{{}}}", body.join(","))
        }
        Value::Array(items) => format!("[{}]", items.iter().map(jcs).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
    }
}

fn jcs_sha256(value: &Value) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(jcs(value).as_bytes()))
}
