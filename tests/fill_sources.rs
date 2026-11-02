//! Each filled box keeps its source, and Books' hand-offs fill as they are.
//!
//! - `ebirforms_form_fill` keeps a `source` (and a `source_ref`) per filled
//!   box; `ebirforms_form_fields` shows it; it survives a save and a new
//!   process reopening the return; a box the person types over loses it.
//! - Books' hand-off contract: `tests/fixtures/books-handoff/` holds one
//!   `books_handoff_get` payload per form ({form_open, form_fill}), the same
//!   files as Books' `tests/fixtures/handoff/`, and the two tools' input
//!   schemas. Each payload opens and fills a real draft with nothing
//!   rejected. Both repos pin [`CONTRACT_SHA256`] over the four files; with
//!   `BOOKS_DIR` set, the files are also compared with the Books checkout's.
//!   `EBIRFORMS_WRITE_HANDOFF_SCHEMA=1` rewrites the schema file.
//!
//! One test: the database handle and the key are process-wide.

#![allow(unsafe_code)] // the test sets the process env, as main does

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bir_app_core::{BirApp, BranchFacts, FieldValue, NewTaxpayer, OpenReturn};
use ebirforms_mini_app::{ManifestSource, Server, same_value, shown_amount};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TEST_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const ROOT: &str = "000123453";

/// The hand-offs, in Books' order.
const FIXTURES: [&str; 3] = ["2551q.json", "1601c.json", "1701ms.json"];
const SCHEMA_FILE: &str = "form-tools.schema.json";

/// SHA-256 over the four files (names sorted, each `name\0bytes\0`). The same
/// constant is in Books' `tests/handoff_contract.rs`.
const CONTRACT_SHA256: &str = "101d25a3f8197376de3368122ce23de1accfb394df444356317da7cbda74be77";

struct Bot {
    server: Server,
    next: i64,
}

impl Bot {
    /// A fresh process over the database: initialized and provisioned.
    fn start(app: &Arc<BirApp>, path: &Path) -> Self {
        let mut bot = Self {
            server: Server::with_app(
                app.clone(),
                path.to_path_buf(),
                ManifestSource::Unsigned,
                true,
            ),
            next: 0,
        };
        bot.send("initialize", json!({ "protocolVersion": "2025-06-18" }));
        let seed = [7u8; 32];
        let public =
            buwiz_plugin::b64::encode(SigningKey::from_bytes(&seed).verifying_key().as_bytes());
        let provisioned = bot.call(
            json!({}),
            "buwiz.provision",
            json!({ "protocol": "1.0.0", "plugin": "app.buwiz.ebirforms", "key": { "alg": "ed25519", "seed": buwiz_plugin::b64::encode(&seed) }, "pub": public }),
        );
        assert_eq!(
            provisioned["structuredContent"]["ok"], true,
            "{provisioned}"
        );
        bot
    }

    fn send(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let line = json!({ "jsonrpc": "2.0", "id": self.next, "method": method, "params": params })
            .to_string();
        serde_json::from_str(&self.server.handle_line(&line).expect("a reply")).unwrap()
    }

    fn call(&mut self, caller: Value, name: &str, arguments: Value) -> Value {
        let reply = self.send(
            "tools/call",
            json!({ "name": name, "arguments": arguments, "_meta": { "buwiz/caller": caller } }),
        );
        reply
            .get("result")
            .cloned()
            .unwrap_or_else(|| panic!("{name}: {reply}"))
    }

    /// The assistant's call, which must succeed.
    fn agent(&mut self, name: &str, arguments: Value) -> Value {
        let answer = self.call(json!({ "origin": "agent" }), name, arguments);
        assert_eq!(answer["isError"], false, "{name}: {answer}");
        answer["structuredContent"]["result"].clone()
    }

    /// The assistant's call, which must be refused: the refusal's text.
    fn refused(&mut self, name: &str, arguments: Value) -> String {
        let answer = self.call(json!({ "origin": "agent" }), name, arguments);
        assert_eq!(answer["isError"], true, "{name}: {answer}");
        answer["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    /// The taxpayer's ticket in this process.
    fn taxpayer(&mut self) -> String {
        let listed = self.agent("ebirforms_profiles_list", json!({}));
        listed[0]["tin_root"].as_str().unwrap().to_string()
    }

    fn tool_schema(&mut self, name: &str) -> Value {
        let tools = self.send("tools/list", json!({}))["result"]["tools"].clone();
        tools
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("no {name}"))["inputSchema"]
            .clone()
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/books-handoff")
}

fn fixture(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixtures().join(name)).unwrap()).unwrap()
}

fn contract_sha256(dir: &Path) -> String {
    let mut names: Vec<&str> = FIXTURES.to_vec();
    names.push(SCHEMA_FILE);
    names.sort_unstable();
    let mut hash = Sha256::new();
    for name in names {
        hash.update(name.as_bytes());
        hash.update([0]);
        hash.update(std::fs::read(dir.join(name)).unwrap());
        hash.update([0]);
    }
    hex::encode(hash.finalize())
}

/// The person fills boxes in the Forms app and saves the return.
fn person_fills(
    app: &BirApp,
    tin: &str,
    form: &str,
    year: i64,
    period: i64,
    boxes: &[(&str, &str)],
) {
    let opened = app
        .open_return(&OpenReturn {
            tin: tin.to_string(),
            form_code: form.into(),
            year,
            period,
            key: None,
        })
        .unwrap();
    let edits: BTreeMap<String, FieldValue> = boxes
        .iter()
        .map(|(key, value)| (key.to_string(), FieldValue::Text(value.to_string())))
        .collect();
    let typed = app.fill(form, &opened.draft, &edits, &[]).unwrap();
    assert!(typed.rejected.is_empty(), "{form}: {:?}", typed.rejected);
    let saved = app.save(form, &typed.draft).unwrap();
    assert_ne!(saved.saved, Some(false), "{form}: {saved:?}");
}

/// `form_open` with the taxpayer's ticket as `tin`.
fn open_args(handoff: &Value, ticket: &str) -> Value {
    let mut args = handoff["form_open"].clone();
    args["tin"] = json!(ticket);
    args
}

#[test]
fn filled_boxes_keep_their_source_and_books_hand_offs_fill_as_they_are() {
    // SAFETY: set before any thread of this test reads them (one test per process).
    unsafe {
        std::env::set_var("BUWIZ_DB_KEY", TEST_KEY);
        std::env::set_var("BUWIZ_NO_KEYCHAIN", "1");
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ebirforms.db");
    let app = Arc::new(BirApp::open_at(&path).unwrap());
    let tin = app
        .create_taxpayer(&NewTaxpayer {
            tin_root: ROOT.into(),
            name: "Juana Santos".into(),
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
        .expect("taxpayer")
        .branches[0]
        .tin
        .clone();

    // the contract files: the live schemas, the pinned hash, Books' copy
    let mut bot = Bot::start(&app, &path);
    let live = json!({
        "ebirforms_form_open": bot.tool_schema("ebirforms_form_open"),
        "ebirforms_form_fill": bot.tool_schema("ebirforms_form_fill"),
    });
    let schema_path = fixtures().join(SCHEMA_FILE);
    if std::env::var_os("EBIRFORMS_WRITE_HANDOFF_SCHEMA").is_some() {
        let mut text = serde_json::to_string_pretty(&live).unwrap();
        text.push('\n');
        std::fs::write(&schema_path, text).unwrap();
    }
    let kept: Value =
        serde_json::from_str(&std::fs::read_to_string(&schema_path).unwrap()).unwrap();
    assert_eq!(
        kept, live,
        "the form tools' schemas changed: rewrite {SCHEMA_FILE}, give Books the same file and update CONTRACT_SHA256 in both repos"
    );
    assert_eq!(
        contract_sha256(&fixtures()),
        CONTRACT_SHA256,
        "the hand-off contract files changed: update CONTRACT_SHA256 here and in Books"
    );
    if let Some(books) = std::env::var_os("BOOKS_DIR") {
        let theirs = Path::new(&books).join("tests/fixtures/handoff");
        assert_eq!(
            contract_sha256(&theirs),
            contract_sha256(&fixtures()),
            "Books' hand-off fixtures differ from this copy"
        );
    }

    // the person's own 1701-MS choices, made in the Forms app: business
    // income, graduated rates, OSD (Books' hand-off reads them; it never
    // sets them)
    person_fills(
        &app,
        &tin,
        "1701MS",
        2026,
        0,
        &[
            ("frm1701MS:txtTaxpayerNo11group", "a"),
            ("frm1701MS:txtTaxpayerNo12b", "true"),
            ("frm1701MS:txtTaxpayerNo17", "b"),
        ],
    );

    // each Books hand-off opens and fills a real draft as it is
    let ticket = bot.taxpayer();
    for name in FIXTURES {
        let handoff = fixture(name);
        let fill = &handoff["form_fill"];
        let keys: Vec<String> = fill["fields"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let opened = bot.agent("ebirforms_form_open", open_args(&handoff, &ticket));
        assert_eq!(
            opened["form"], handoff["form_open"]["code"],
            "{name}: {opened}"
        );
        let filled = bot.agent("ebirforms_form_fill", fill.clone());
        assert_eq!(filled["rejected"], json!({}), "{name}: {filled}");
        assert_eq!(filled["filled"], json!(keys), "{name}: {filled}");
        let fields = bot.agent("ebirforms_form_fields", json!({ "keys": keys }));
        for key in &keys {
            assert!(
                same_value(
                    fill["fields"][key].as_str().unwrap(),
                    fields["values"][key].as_str().unwrap_or_default()
                ),
                "{name} {key}: {fields}"
            );
            assert_eq!(
                fields["sources"][key],
                json!({ "source": "document", "source_ref": fill["source_ref"] }),
                "{name} {key}"
            );
        }
        let saved = bot.agent("ebirforms_form_save_draft", json!({}));
        assert_eq!(saved["sources_kept"], true, "{name}: {saved}");
        bot.agent("ebirforms_form_dismiss", json!({}));
    }

    // a new process reopens each saved return with its boxes' sources
    let mut bot = Bot::start(&app, &path);
    let ticket = bot.taxpayer();
    for name in FIXTURES {
        let handoff = fixture(name);
        let fill = &handoff["form_fill"];
        bot.agent("ebirforms_form_open", open_args(&handoff, &ticket));
        let fields = bot.agent("ebirforms_form_fields", json!({}));
        let expected: BTreeMap<String, Value> = fill["fields"]
            .as_object()
            .unwrap()
            .keys()
            .map(|key| {
                (
                    key.clone(),
                    json!({ "source": "document", "source_ref": fill["source_ref"] }),
                )
            })
            .collect();
        assert_eq!(fields["sources"], json!(expected), "{name}");
        bot.agent("ebirforms_form_dismiss", json!({}));
    }

    // on the 2551Q: what each later fill does to a box's source
    let q3 = fixture("2551q.json");
    bot.agent("ebirforms_form_open", open_args(&q3, &ticket));
    // the person's answer only from a card
    let refusal = bot.refused(
        "ebirforms_form_fill",
        json!({ "fields": { "txtATCAmt1": "1.00" }, "source": "user" }),
    );
    assert!(refusal.contains("from_card"), "{refusal}");
    // a source_ref is exactly {plugin, handoff, inputs_hash}
    let mut forged = q3["form_fill"]["source_ref"].clone();
    forged["note"] = json!("x");
    let refusal = bot.refused(
        "ebirforms_form_fill",
        json!({ "fields": { "txtATCAmt1": "1.00" }, "source": "document", "source_ref": forged }),
    );
    assert!(refusal.contains("source_ref"), "{refusal}");
    // a refused box gets no source
    let filled = bot.agent(
        "ebirforms_form_fill",
        json!({ "fields": { "frm2551Qv2018:nope": "1.00" }, "source": "document" }),
    );
    assert!(
        filled["rejected"].get("frm2551Qv2018:nope").is_some(),
        "{filled}"
    );
    let fields = bot.agent("ebirforms_form_fields", json!({}));
    assert!(
        fields["sources"].get("frm2551Qv2018:nope").is_none(),
        "{fields}"
    );
    // a later fill replaces the source; the person's card answer says so
    bot.agent(
        "ebirforms_form_fill",
        json!({ "fields": { "frm2551Qv2018:txt15": "100.00" }, "source": "user", "from_card": true }),
    );
    bot.agent(
        "ebirforms_form_fill",
        json!({ "fields": { "txtATCAmt1": "291000.00" }, "source": "ai" }),
    );
    let fields = bot.agent(
        "ebirforms_form_fields",
        json!({ "keys": ["txtATCAmt1", "frm2551Qv2018:txt15"] }),
    );
    assert_eq!(
        fields["sources"],
        json!({
            "frm2551Qv2018:txt15": { "source": "user", "from_card": true },
            "txtATCAmt1": { "source": "ai" },
        })
    );
    // a refused edit leaves the box and its source as they were
    let cleared = bot.agent(
        "ebirforms_form_fill",
        json!({ "fields": { "frm2551Qv2018:txt15": null }, "source": "ai" }),
    );
    assert!(
        cleared["rejected"].get("frm2551Qv2018:txt15").is_some(),
        "{cleared}"
    );
    let fields = bot.agent(
        "ebirforms_form_fields",
        json!({ "keys": ["frm2551Qv2018:txt15"] }),
    );
    assert_eq!(
        fields["sources"],
        json!({ "frm2551Qv2018:txt15": { "source": "user", "from_card": true } }),
        "{fields}"
    );
    bot.agent("ebirforms_form_save_draft", json!({}));
    bot.agent("ebirforms_form_dismiss", json!({}));

    // the person types over a box in the Forms app: it loses its source
    person_fills(&app, &tin, "2551Q", 2026, 3, &[("txtATCAmt1", "292000.00")]);
    bot.agent("ebirforms_form_open", open_args(&q3, &ticket));
    let fields = bot.agent("ebirforms_form_fields", json!({ "keys": ["txtATCAmt1"] }));
    assert_eq!(
        shown_amount(fields["values"]["txtATCAmt1"].as_str().unwrap()),
        Some(29_200_000)
    );
    assert_eq!(fields["sources"], json!({}), "{fields}");

    // the sources file holds no TIN and no amount (its names are keyed by
    // fill-sources.key; sources.rs tests that unkeyed hashes don't match)
    assert!(dir.path().join("fill-sources.key").is_file());
    let text = std::fs::read_to_string(dir.path().join("fill-sources.json")).unwrap();
    assert!(
        !text.contains(ROOT) && !text.contains("290000") && !text.contains("290,000"),
        "{text}"
    );
}
