//! The Tax Profile app's New taxpayer form over a scratch database: the
//! create call (`ebirforms_app_create_taxpayer`), its checks, who may make
//! it, and the ticket it answers. One test: the database handle and the key
//! are process-wide. Every TIN and name here is synthetic.

#![allow(unsafe_code)] // the test sets the process env, as main does

use std::sync::Arc;

use bir_app_core::BirApp;
use ebirforms_mini_app::{ManifestSource, Server};
use serde_json::{Value, json};

const TEST_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// Synthetic roots, never a real taxpayer's.
const ROOT: &str = "000111222";
const OTHER: &str = "000333444";
const CREATE: &str = "ebirforms_app_create_taxpayer";
const TAXPAYERS: &str = "ebirforms_taxpayers_list";

struct Bot {
    server: Server,
    next: i64,
}

impl Bot {
    fn send(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let line = json!({ "jsonrpc": "2.0", "id": self.next, "method": method, "params": params })
            .to_string();
        let reply: Value =
            serde_json::from_str(&self.server.handle_line(&line).expect("a reply")).unwrap();
        assert_eq!(reply["id"], self.next, "{reply}");
        reply
    }

    fn call_as(&mut self, caller: &Value, name: &str, arguments: Value) -> Value {
        let reply = self.send(
            "tools/call",
            json!({ "name": name, "arguments": arguments, "_meta": { "buwiz/caller": caller } }),
        );
        reply
            .get("result")
            .cloned()
            .unwrap_or_else(|| panic!("{name}: {reply}"))
    }

    /// An app session certified for `app` (its manifest entry's index).
    fn open(&mut self, app: &str, index: usize, id: u8) -> Value {
        let perms =
            jcs_sha256(&ebirforms_mini_app::manifest::unsigned()["apps"][index]["permissions"]);
        let open_id = b64(&[id; 16]);
        let not_after = chrono::Utc::now().timestamp_millis() + 3_600_000;
        let session = self.call_as(&json!({}), "buwiz.session.open", json!({
            "app": app, "open_id": open_id, "ks_pub": b64(&[2u8; 32]), "perms_sha256": perms, "not_after": not_after,
        }));
        assert_eq!(
            session["structuredContent"]["cert"]["app"], app,
            "{session}"
        );
        json!({ "origin": "app", "app": app, "open_id": open_id })
    }
}

fn b64(bytes: &[u8]) -> String {
    buwiz_plugin::b64::encode(bytes)
}

fn result_of(answer: &Value) -> &Value {
    assert_eq!(answer["isError"], false, "{answer}");
    &answer["structuredContent"]["result"]
}

fn error_text(answer: &Value) -> String {
    assert_eq!(answer["isError"], true, "{answer}");
    answer["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The form as the app sends it: one argument, as JSON text.
fn form(root: &str, rdo: &str) -> Value {
    form_with(root, rdo, "", "")
}

/// The form with the head office's `field` set to `value` (none: as `form`).
fn form_with(root: &str, rdo: &str, field: &str, value: &str) -> Value {
    let mut new = json!({
        "tin_root": root,
        "name": "Synthetic Sample Trading",
        "taxpayer_type": "Corporation",
        "head_office": {
            "rdo_code": rdo,
            "registered_address": "1 Sample Street, Sample City",
            "zip_code": "1600",
            "phone": "09170000000",
            "email": "sample@example.invalid"
        }
    });
    if !field.is_empty() {
        new["head_office"][field] = json!(value);
    }
    json!({ "args_json": json!([new]).to_string() })
}

#[test]
fn the_new_taxpayer_form_over_a_scratch_database() {
    // SAFETY: set before any thread of this test reads them (one test per process).
    unsafe {
        std::env::set_var("BUWIZ_DB_KEY", TEST_KEY);
        std::env::set_var("BUWIZ_NO_KEYCHAIN", "1");
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bir_data.db");
    let app = Arc::new(BirApp::open_at(&path).unwrap());
    let mut bot = Bot {
        server: Server::with_app(app.clone(), path.clone(), ManifestSource::Unsigned, true),
        next: 0,
    };
    let seed = [7u8; 32];
    let public = b64(ed25519_dalek::SigningKey::from_bytes(&seed)
        .verifying_key()
        .as_bytes());
    let provisioned = bot.call_as(
        &json!({}),
        "buwiz.provision",
        json!({ "protocol": "1.0.0", "plugin": "app.buwiz.ebirforms", "key": { "alg": "ed25519", "seed": b64(&seed) }, "pub": public }),
    );
    assert_eq!(
        provisioned["structuredContent"]["ok"], true,
        "{provisioned}"
    );

    // the tool: the Tax Profile app's, a write, never the assistant's
    let tools = bot.send("tools/list", json!({}))["result"]["tools"].clone();
    let tool = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == CREATE)
        .cloned()
        .expect("the create tool is listed");
    assert_eq!(tool["_meta"]["buwiz/audience"], json!(["app"]));
    assert_eq!(tool["annotations"]["readOnlyHint"], false);
    let manifest = ebirforms_mini_app::manifest::unsigned();
    let granted = |index: usize| {
        manifest["apps"][index]["permissions"]["tools"]
            .as_array()
            .unwrap()
            .contains(&json!(CREATE))
    };
    assert_eq!(manifest["apps"][1]["id"], "tax-profile");
    assert!(granted(1), "Tax Profile may create");
    assert!(
        (0..manifest["apps"].as_array().unwrap().len())
            .filter(|i| *i != 1)
            .all(|i| !granted(i))
    );
    assert_eq!(
        manifest["apps"][1]["permissions"]["open"]["apps"],
        json!(["forms", "tax-profile"]),
        "Tax Profile opens the new taxpayer's own profile"
    );

    // /taxprofile and /taxprofile list: the Tax profiles card inline in the
    // chat, which reads the taxpayers itself; nothing about them is in the
    // answer, saved or not
    let card = json!({
        "result": "open", "app": "card-people", "placement": "inline", "title": "Tax profiles",
        "props": { "card": "people", "tool": TAXPAYERS, "args": {} }
    });
    for value in ["", "list", "LIST"] {
        let none = resolve(&mut bot, value);
        assert_eq!(none["structuredContent"], card, "{value}: {none}");
    }
    // the card's read, none saved: an empty list
    let people = bot.open("card-people", 7, 4);
    let empty = bot.call_as(&people, TAXPAYERS, json!({}));
    assert_eq!(result_of(&empty), &json!({ "taxpayers": [] }));
    let none = bot.call_as(
        &json!({ "origin": "resolve" }),
        "ebirforms_resolve",
        json!({ "kind": "command", "name": "taxform", "value": "" }),
    );
    assert_eq!(none["structuredContent"]["result"], "error", "{none}");
    let message = none["structuredContent"]["message"].as_str().unwrap();
    assert!(
        message.contains("New taxpayer")
            && message.contains("BIR Tax Profile")
            && message.contains("/taxprofile create"),
        "{message}"
    );
    // /taxprofile create: the form, a name prefilled; a TIN not saved
    // yet opens it empty and is named nowhere
    let named = resolve(&mut bot, "create Synthetic Sample Trading");
    assert_eq!(
        named["structuredContent"]["props"],
        json!({ "start": "new", "name": "Synthetic Sample Trading" }),
        "{named}"
    );
    for value in [
        "create 000-111-222",
        "create 00011122200000",
        "create Juan 000111222",
    ] {
        let unsaved = resolve(&mut bot, value);
        assert_eq!(
            unsaved["structuredContent"]["props"],
            json!({ "start": "new" }),
            "{value}: {unsaved}"
        );
        assert!(!unsaved.to_string().contains(ROOT), "{unsaved}");
    }
    // edit and delete with nothing to find: refused, the TIN not echoed
    for value in ["edit 000111222", "delete 000-111-222"] {
        let refused = resolve(&mut bot, value);
        assert_eq!(refused["structuredContent"]["result"], "error", "{refused}");
        assert!(!refused.to_string().contains("111"), "{refused}");
    }
    // and the assistant can open Tax Profile on New taxpayer, with no ticket
    let opened = bot.call_as(
        &json!({ "origin": "agent" }),
        "ebirforms_app_open",
        json!({ "app": "tax-profile" }),
    );
    let open = &opened["_meta"]["buwiz/open"];
    assert_eq!(open["app"], "tax-profile", "{opened}");
    // (its picker, or its New taxpayer form when none is saved)
    assert_eq!(open["props"], json!({}));
    assert!(open.get("ticket").is_none());

    // only the Tax Profile app creates: not the assistant, a resolve or
    // routine call, nor the Forms app
    for origin in ["agent", "resolve", "routine"] {
        let refused = bot.call_as(&json!({ "origin": origin }), CREATE, form(ROOT, "043"));
        assert_eq!(
            refused["structuredContent"]["code"], "permission_denied",
            "{origin}: {refused}"
        );
    }
    let forms = bot.open("forms", 0, 1);
    let refused = bot.call_as(&forms, CREATE, form(ROOT, "043"));
    assert_eq!(
        refused["structuredContent"]["code"], "permission_denied",
        "{refused}"
    );
    let stranger = json!({ "origin": "app", "app": "tax-profile", "open_id": b64(&[9u8; 16]) });
    let refused = bot.call_as(&stranger, CREATE, form(ROOT, "043"));
    assert_eq!(
        refused["structuredContent"]["code"], "permission_denied",
        "{refused}"
    );
    assert!(
        app.list_taxpayers().unwrap().is_empty(),
        "nothing refused was saved"
    );

    // the Tax Profile app, opened on no taxpayer (its empty state)
    let profile = bot.open("tax-profile", 1, 3);
    // a bad TIN, a missing RDO, an RDO not in the table or sent as its
    // label: refused by field
    for (root, rdo, field) in [
        ("00011122", "043", "tin_root"),
        ("0001112223", "043", "tin_root"),
        ("00011122a", "043", "tin_root"),
        ("000111222a", "043", "tin_root"),
        (ROOT, "", "rdo_code"),
        (ROOT, "999", "rdo_code"),
        (ROOT, "043 - Pasig City", "rdo_code"),
    ] {
        let refused = error_text(&bot.call_as(&profile, CREATE, form(root, rdo)));
        assert!(
            refused.contains(&format!("{field}: ")),
            "{root}/{rdo}: {refused}"
        );
        assert!(!refused.contains(root) || root.is_empty(), "{refused}");
        assert!(!refused.contains(ROOT), "{refused}");
    }
    // a bad phone, email or ZIP code (a ZIP is a code from the table, never
    // free text): refused by field, the TIN named nowhere
    for (field, value) in [
        ("phone", "12"),
        ("phone", "0917000000"),
        ("phone", "+1 415 555 0100"),
        ("phone", "0917000000a"),
        ("email", "nope"),
        ("email", "a@b"),
        ("email", "na me@example.com"),
        ("zip_code", "9999"),
        ("zip_code", "1600 - Pasig"),
        ("zip_code", "Pasig"),
    ] {
        let refused =
            error_text(&bot.call_as(&profile, CREATE, form_with(ROOT, "043", field, value)));
        assert!(
            refused.contains(&format!("{field}: ")),
            "{field}={value}: {refused}"
        );
        assert!(!refused.contains(ROOT), "{refused}");
    }
    let no_form = error_text(&bot.call_as(&profile, CREATE, json!({ "args": [] })));
    assert!(no_form.contains("one argument"), "{no_form}");
    assert!(
        app.list_taxpayers().unwrap().is_empty(),
        "nothing refused was saved"
    );

    // a good one: saved with its 00000 head office, answered by ticket only
    let created = bot.call_as(&profile, CREATE, form(ROOT, "043"));
    let answer = result_of(&created).clone();
    assert_eq!(answer.as_object().unwrap().len(), 2, "{answer}");
    assert_eq!(answer["title"], "Synthetic Sample Trading");
    let ticket = answer["ticket"].as_str().unwrap().to_string();
    assert!(ticket.starts_with("tkt_"));
    assert!(
        !created.to_string().contains(ROOT),
        "the TIN came back: {created}"
    );
    let saved = app.get_taxpayer(ROOT).unwrap().expect("saved");
    assert_eq!(saved.name, "Synthetic Sample Trading");
    assert_eq!(saved.taxpayer_type, "Corporation");
    assert_eq!(saved.branches.len(), 1);
    assert!(saved.branches[0].is_head_office);
    assert_eq!(saved.branches[0].code, "00000");
    assert_eq!(saved.branches[0].facts.rdo_code, "043");
    assert_eq!(saved.branches[0].facts.zip_code, "1600");

    // the ticket opens the new taxpayer's profile: the app learns its TIN
    // from the ticket and every call is held to it
    let subject = bot.call_as(
        &profile,
        "ebirforms_ticket_subject",
        json!({ "ticket": ticket }),
    );
    assert_eq!(
        result_of(&subject)["tin"],
        format!("{ROOT}00000"),
        "{subject}"
    );
    let read = bot.call_as(
        &profile,
        "ebirforms_app_get_taxpayer",
        json!({ "args": [ROOT], "ticket": ticket }),
    );
    assert_eq!(result_of(&read)["name"], "Synthetic Sample Trading");

    // the same TIN again: refused, without the TIN, and nothing changes
    let duplicate = error_text(&bot.call_as(&profile, CREATE, form(ROOT, "050")));
    assert!(
        duplicate.contains("tin_root: a taxpayer with this TIN is already saved"),
        "{duplicate}"
    );
    assert!(!duplicate.contains(ROOT), "{duplicate}");
    assert_eq!(app.list_taxpayers().unwrap().len(), 1);
    assert_eq!(
        app.get_taxpayer(ROOT).unwrap().unwrap().branches[0]
            .facts
            .rdo_code,
        "043"
    );

    // from a profile open on one taxpayer, another can be added: a new
    // taxpayer is held to no ticket
    let second = bot.call_as(
        &profile,
        CREATE,
        json!({ "args_json": form(OTHER, "050")["args_json"], "ticket": ticket }),
    );
    let other_ticket = result_of(&second)["ticket"].as_str().unwrap().to_string();
    assert_ne!(other_ticket, ticket);
    assert_eq!(app.list_taxpayers().unwrap().len(), 2);
    // but the first one's ticket still reaches only the first
    let outside = bot.call_as(
        &profile,
        "ebirforms_app_get_taxpayer",
        json!({ "args": [OTHER], "ticket": ticket }),
    );
    assert!(error_text(&outside).contains("outside this app's scope"));

    // the chat now finds the new taxpayer by name
    let found = bot.call_as(
        &json!({ "origin": "resolve" }),
        "ebirforms_resolve",
        json!({ "kind": "command", "name": "taxprofile", "value": "synthetic" }),
    );
    assert_eq!(found["structuredContent"]["result"], "choices", "{found}");
    assert!(!found.to_string().contains(ROOT) && !found.to_string().contains(OTHER));

    // /taxprofile list with two saved: the same card, still naming no one
    for value in ["", "list"] {
        let listed = resolve(&mut bot, value);
        assert_eq!(listed["structuredContent"], card, "{value}: {listed}");
    }
    // the card's read: what each row shows and nothing else, the TIN only
    // as the screen shows it, never its digits run together
    let read = bot.call_as(&people, TAXPAYERS, json!({}));
    let rows = result_of(&read)["taxpayers"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 2, "{read}");
    let first = rows
        .iter()
        .find(|row| row["ticket"] == ticket.as_str())
        .expect("the first taxpayer");
    assert_eq!(
        first,
        &json!({
            "ticket": ticket, "name": "Synthetic Sample Trading", "tin_display": "000-111-222",
            "taxpayer_type": "Corporation", "rdo_code": "043", "rdo_office": first["rdo_office"]
        })
    );
    assert!(!first["rdo_office"].as_str().unwrap().is_empty(), "{first}");
    assert!(
        !read.to_string().contains(ROOT) && !read.to_string().contains(OTHER),
        "{read}"
    );
    // Tax Profile's and Forms' pickers read the same; the assistant, a
    // resolve or routine call may not
    for picker_app in [&profile, &forms] {
        let picker = bot.call_as(picker_app, TAXPAYERS, json!({}));
        assert_eq!(result_of(&picker), result_of(&read));
    }
    for origin in ["agent", "resolve", "routine"] {
        let refused = bot.call_as(&json!({ "origin": origin }), TAXPAYERS, json!({}));
        assert_eq!(
            refused["structuredContent"]["code"], "permission_denied",
            "{origin}: {refused}"
        );
    }
    let granted: Vec<&str> = manifest["apps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|app| {
            app["permissions"]["tools"]
                .as_array()
                .unwrap()
                .contains(&json!(TAXPAYERS))
        })
        .map(|app| app["id"].as_str().unwrap())
        .collect();
    assert_eq!(granted, ["forms", "tax-profile", "card-people"]);
    assert_eq!(manifest["apps"][7]["placement"], json!(["inline"]));
    assert_eq!(
        manifest["apps"][7]["permissions"]["open"]["apps"],
        json!(["tax-profile"])
    );

    // /taxprofile <name or TIN>: one match opens it, several are choices,
    // none an error that echoes no digits
    let one = resolve(&mut bot, ROOT);
    assert_eq!(one["structuredContent"]["result"], "open", "{one}");
    assert_eq!(
        one["structuredContent"]["props"],
        json!({ "ticket": ticket })
    );
    assert!(!one.to_string().contains(ROOT), "{one}");
    let several = resolve(&mut bot, "synthetic");
    assert_eq!(
        several["structuredContent"]["choices"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "{several}"
    );
    for value in ["nobody", "000999888"] {
        let none = resolve(&mut bot, value);
        assert_eq!(none["structuredContent"]["result"], "error", "{none}");
        assert!(!none.to_string().contains("999"), "{none}");
    }

    // /taxprofile create with a saved TIN: that taxpayer's profile
    for value in ["create 000111222", "create 000-111-222-00000"] {
        let saved = resolve(&mut bot, value);
        assert_eq!(saved["structuredContent"]["result"], "open", "{saved}");
        assert_eq!(saved["structuredContent"]["ticket"], ticket.as_str());
        assert_eq!(
            saved["structuredContent"]["props"],
            json!({ "ticket": ticket })
        );
        assert!(!saved.to_string().contains(ROOT), "{saved}");
    }

    // /taxprofile edit and delete: by TIN or ticket straight to the
    // section; by a name two share, a choice that keeps the verb
    for (verb, section) in [("edit", "edit"), ("delete", "delete")] {
        let by_tin = resolve(&mut bot, &format!("{verb} {ROOT}"));
        let open = &by_tin["structuredContent"];
        assert_eq!(open["result"], "open", "{by_tin}");
        assert_eq!(
            open["props"],
            json!({ "ticket": ticket, "section": section })
        );
        assert!(!by_tin.to_string().contains(ROOT), "{by_tin}");
        let by_name = resolve(&mut bot, &format!("{verb} synthetic"));
        let choices = by_name["structuredContent"]["choices"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(choices.len(), 2, "{by_name}");
        assert!(!by_name.to_string().contains(ROOT) && !by_name.to_string().contains(OTHER));
        let picked = choices
            .iter()
            .find(|c| c["value"] == format!("{verb} {other_ticket}"))
            .unwrap_or_else(|| panic!("{by_name}"));
        let again = resolve(&mut bot, picked["value"].as_str().unwrap());
        assert_eq!(
            again["structuredContent"]["props"],
            json!({ "ticket": other_ticket, "section": section }),
            "{again}"
        );
    }
    // delete only opens: both are still saved
    assert_eq!(app.list_taxpayers().unwrap().len(), 2);
}

/// `/taxprofile <value>`, as the Bot's resolve asks it.
fn resolve(bot: &mut Bot, value: &str) -> Value {
    bot.call_as(
        &json!({ "origin": "resolve" }),
        "ebirforms_resolve",
        json!({ "kind": "command", "name": "taxprofile", "value": value }),
    )
}

/// RFC 8785 for the test's integer-only objects: keys sorted, compact.
fn jcs(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            let body: Vec<String> = keys
                .iter()
                .map(|key| format!("{}:{}", Value::String((*key).clone()), jcs(&map[*key])))
                .collect();
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
