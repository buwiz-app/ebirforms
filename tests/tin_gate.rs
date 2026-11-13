//! A TIN's state on the Buwiz account, end to end over a loopback account
//! server (never a real host): what the apps read (`ebirforms_account_tins`,
//! `ebirforms_account_tin_link`), what the Queue gate refuses and lets through,
//! and that nothing the assistant reads carries a TIN or staff's note. One
//! test: the database handle and the key are process-wide.
//!
//! The account list changes by signing out and in again (a new sign-in never
//! sees the last login's cached list): the complete list of the new server,
//! the list of today's server (no `include` marker), and a list that cannot
//! be read.

#![allow(unsafe_code)] // the test sets the process env, as main does

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use bir_app_core::{BirApp, BranchFacts, NewTaxpayer, OpenReturn, SendStage, TinRefusal};
use ebirforms_mini_app::{ManifestSource, Server};
use serde_json::{Value, json};

const TEST_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Synthetic taxpayers, one per state of the list (each with the official
/// check digit, so a return for it validates).
const UNVERIFIED: &str = "000123453";
const APPROVED: &str = "000987659";
const SENT_BACK: &str = "000111225";
const PAUSED: &str = "000333446";
const PENDING: &str = "000555664";
const ABSENT: &str = "000777887";
const FOREIGN_LINK: &str = "000999915";
/// The server says it can send although its status is pending.
const INFO_ONLY: &str = "000444444";

const VERIFY_URL: &str = "https://account.buwiz.com/tins/claims/c1/verify";
const FIX_URL: &str = "https://account.buwiz.com/tins/claims/c3/fix";
const NOTE: &str = "Retake it in daylight, said JUANA DELA CRUZ";
const SIGNED_IN_AS: &str = "owner@example.test";

const ROOTS: [&str; 8] = [
    UNVERIFIED,
    APPROVED,
    SENT_BACK,
    PAUSED,
    PENDING,
    ABSENT,
    FOREIGN_LINK,
    INFO_ONLY,
];

/// A TIN in `text`, whatever its form: a fixture's root bare (9 digits), with
/// its 3-digit or 5-digit branch (12 and 14 digits), dashed in groups, or any
/// run of 9 or more digits (a ticket's own characters are left out first).
fn tin_in(text: &str) -> Option<String> {
    let tickets = regex::Regex::new(r"tkt_[0-9A-HJKMNP-TV-Z]{26}").unwrap();
    let text = tickets.replace_all(text, "tkt");
    // a card's `as_of` is a clock in milliseconds, not a TIN
    let text = regex::Regex::new(r#""as_of":\s*[0-9]+"#)
        .unwrap()
        .replace_all(&text, "as_of");
    for root in ROOTS {
        let dashed = format!("{}-{}-{}", &root[..3], &root[3..6], &root[6..]);
        for form in [
            root.to_string(),
            format!("{root}000"),
            format!("{root}00000"),
            dashed,
        ] {
            if text.contains(&form) {
                return Some(form);
            }
        }
    }
    let run = regex::Regex::new(r"[0-9]{9,}|[0-9]{3}[-. ][0-9]{3}[-. ][0-9]{3}").unwrap();
    run.find(&text).map(|found| found.as_str().to_string())
}

/// What the mock account server answers for the list.
struct Mock {
    list: (u16, Value),
    requests: Vec<String>,
    /// The list route drops the connection: the server cannot be reached.
    down: bool,
}

fn row(tin_root: &str, status: &str, can_send: bool, extra: Value) -> Value {
    let mut row = json!({
        "id": format!("id-{tin_root}"),
        "tin": format!("{}-{}-{}-00000", &tin_root[..3], &tin_root[3..6], &tin_root[6..]),
        "tin_masked": "***-***-453-00000",
        "registered_name": "SYNTHETIC REGISTERED NAME",
        "activated_at": null,
        "permissions": ["profile.view", "filing.prepare"],
        "status": status, "can_send": can_send,
    });
    row.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    row
}

/// The new server's complete list: every state but ABSENT, which is not on it.
fn complete_list() -> Value {
    json!({ "include": "all", "profiles": [
        row(UNVERIFIED, "unverified", false, json!({ "action": { "label": "Verify to send", "url": VERIFY_URL } })),
        row(APPROVED, "approved", true, json!({})),
        row(SENT_BACK, "needs_fix", false, json!({
            "reasons": ["A photo is too blurry to read.", "Name does not match MARIA CLARA."], "note": NOTE,
            "action": { "label": "Fix and send again", "url": FIX_URL } })),
        row(PAUSED, "paused", false, json!({})),
        row(PENDING, "pending", false, json!({})),
        row(INFO_ONLY, "pending", true, json!({})),
        row(FOREIGN_LINK, "needs_fix", false, json!({
            "action": { "label": "Fix", "url": "https://evil.example/fix" } })),
    ] })
}

/// Today's server: no marker, so a TIN missing from it says nothing.
fn old_server_list() -> Value {
    json!({ "profiles": [
        row(APPROVED, "approved", true, json!({})),
        row(PENDING, "pending", false, json!({})),
    ] })
}

fn bind() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (listener, base)
}

fn serve(listener: TcpListener, mock: Arc<Mutex<Mock>>) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                let header = header.trim_end();
                if header.is_empty() {
                    break;
                }
                let (name, value) = header.split_once(':').unwrap();
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let route: Vec<&str> = line.split_whitespace().take(2).collect();
            let (status, answer) = match route[..] {
                ["POST", "/auth/device/start"] => (
                    200,
                    json!({
                        "device_code": "bwd_device", "user_code": "BCDF-GHJK",
                        "verification_uri": "https://account.buwiz.com/device",
                        "verification_uri_complete": "https://account.buwiz.com/device?user_code=BCDF-GHJK",
                        "expires_in": 900, "interval": 5,
                    }),
                ),
                ["POST", "/auth/device/poll"] => (
                    200,
                    json!({ "access_token": "bwa_one", "token_type": "Bearer", "expires_in": 900, "refresh_token": "bwr_one", "scope": "app" }),
                ),
                ["GET", "/me"] => (200, json!({ "user_id": "u1", "email": SIGNED_IN_AS })),
                ["POST", "/api/devices"] => (200, json!({ "id": "dev_1" })),
                ["POST", "/api/auth/logout"] => (200, json!({})),
                ["GET", path] if path.starts_with("/api/tax-profiles") => {
                    let mut mock = mock.lock().unwrap();
                    mock.requests.push(path.to_string());
                    if mock.down {
                        continue;
                    }
                    mock.list.clone()
                }
                _ => (404, json!({ "error": { "code": "not_found" } })),
            };
            let text = answer.to_string();
            write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                text.len()
            )
            .unwrap();
        }
    });
}

struct Bot {
    server: Server,
    next: i64,
}

impl Bot {
    fn call_as(&mut self, caller: Value, name: &str, arguments: Value) -> Value {
        self.next += 1;
        let line = json!({
            "jsonrpc": "2.0", "id": self.next, "method": "tools/call",
            "params": { "name": name, "arguments": arguments, "_meta": { "buwiz/caller": caller } },
        })
        .to_string();
        let reply: Value =
            serde_json::from_str(&self.server.handle_line(&line).expect("a reply")).unwrap();
        reply
            .get("result")
            .cloned()
            .unwrap_or_else(|| panic!("{name}: {reply}"))
    }

    fn app(&mut self, app: &str, name: &str, arguments: Value) -> Value {
        self.call_as(json!({ "origin": "app", "app": app }), name, arguments)
    }

    fn agent(&mut self, name: &str, arguments: Value) -> Value {
        self.call_as(json!({ "origin": "agent" }), name, arguments)
    }

    /// The Sign-in app's device flow, against the mock.
    fn sign_in(&mut self) {
        for tool in ["start", "poll"] {
            let name = format!("ebirforms_account_sign_in_{tool}");
            result_of(&self.app("register", &name, json!({})));
        }
        let status = self.app("register", "ebirforms_account_status", json!({}));
        assert_eq!(result_of(&status)["signed_in"], true, "{status}");
    }

    fn sign_out(&mut self) {
        result_of(&self.app("register", "ebirforms_account_sign_out", json!({})));
    }
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

fn denied(answer: &Value) {
    assert_eq!(answer["isError"], true, "{answer}");
    assert_eq!(
        answer["structuredContent"]["code"], "permission_denied",
        "{answer}"
    );
}

/// The taxpayer's name: its state, so a name never holds a TIN.
fn name_of(root: &str) -> String {
    let state = match root {
        UNVERIFIED => "Unverified",
        APPROVED => "Approved",
        SENT_BACK => "Sent Back",
        PAUSED => "Paused",
        PENDING => "Pending",
        ABSENT => "Absent",
        INFO_ONLY => "Info Only",
        _ => "Foreign Link",
    };
    format!("Synthetic {state} Trading")
}

fn create(app: &BirApp, root: &str) -> String {
    let taxpayer = app
        .create_taxpayer(&NewTaxpayer {
            tin_root: root.into(),
            name: name_of(root),
            taxpayer_type: "Individual".into(),
            birth_date: Some("1990-01-15".into()),
            head_office: BranchFacts {
                rdo_code: "043".into(),
                registered_address: "1 Sample Street, Pasig City".into(),
                zip_code: "1600".into(),
                phone: "09171234567".into(),
                email: "taxpayer@example.test".into(),
            },
        })
        .expect("taxpayer");
    taxpayer.branches[0].tin.clone()
}

/// A saved 0619E for `tin` (month 1), as the Forms app would hold it.
fn saved_0619e(app: &BirApp, tin: &str) -> bir_app_core::Draft {
    let opened = app
        .open_return(&OpenReturn {
            tin: tin.into(),
            form_code: "0619E".into(),
            year: 2026,
            period: 1,
            key: None,
        })
        .expect("open 0619E");
    let checked = app.validate("0619E", &opened.draft).expect("validate");
    assert!(checked.errors.is_empty(), "{:?}", checked.errors);
    let stored = app.save("0619E", &checked.draft).expect("save");
    stored.draft.unwrap_or(checked.draft)
}

#[test]
fn the_account_states_reach_the_apps_and_the_gate() {
    let mock = Arc::new(Mutex::new(Mock {
        list: (200, complete_list()),
        requests: vec![],
        down: false,
    }));
    // The mock's port is bound first and the process environment is set before
    // the mock's thread starts (`owner::prepare` is not used: it names a data
    // folder, while this test opens a scratch database and also needs the
    // account URL, which it does not set).
    let (listener, base) = bind();
    // SAFETY: set before this test starts any thread (one test per process).
    unsafe {
        std::env::set_var("BUWIZ_DB_KEY", TEST_KEY);
        std::env::set_var("BUWIZ_NO_KEYCHAIN", "1");
        std::env::set_var("EBIRFORMS_ACCOUNT_URL", &base);
    }
    serve(listener, mock.clone());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bir_data.db");
    let app = Arc::new(BirApp::open_at(&path).unwrap());
    let tins: Vec<(&str, String)> = [
        UNVERIFIED,
        APPROVED,
        SENT_BACK,
        PAUSED,
        PENDING,
        ABSENT,
        FOREIGN_LINK,
        INFO_ONLY,
    ]
    .into_iter()
    .map(|root| (root, create(&app, root)))
    .collect();
    let tin_of = |root: &str| tins.iter().find(|(r, _)| *r == root).unwrap().1.clone();
    let mut bot = Bot {
        server: Server::with_app(app.clone(), path.clone(), ManifestSource::Unsigned, false),
        next: 0,
    };
    // each taxpayer's ticket, as the assistant is given them
    let listed = bot.agent("ebirforms_profiles_list", json!({}));
    let ticket_of = |root: &str| -> String {
        result_of(&listed)
            .as_array()
            .unwrap()
            .iter()
            .find(|profile| profile["name"] == name_of(root))
            .unwrap_or_else(|| panic!("{listed}"))["tin_root"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let clean = |answer: &Value| {
        let text = answer.to_string();
        assert_eq!(tin_in(&text), None, "a TIN in {text}");
        for private in ["SYNTHETIC REGISTERED", SIGNED_IN_AS] {
            assert!(!text.contains(private), "{private} in {text}");
        }
    };
    let row_of = |answer: &Value, ticket: &str| -> Value {
        result_of(answer)["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["ticket"] == ticket)
            .cloned()
            .unwrap_or_else(|| panic!("no row for {ticket} in {answer}"))
    };
    let queue = |bot: &mut Bot, root: &str| {
        bot.app(
            "forms",
            "ebirforms_app_queue_form_draft",
            json!({ "args": [tin_of(root), "0619E", 2026, 1], "ticket": ticket_of(root) }),
        )
    };
    let status_of = |bot: &mut Bot, root: &str| -> String {
        let rows = bot.app(
            "forms",
            "ebirforms_app_list_returns",
            json!({ "args_json": json!(["all"]).to_string(), "ticket": ticket_of(root) }),
        );
        result_of(&rows)
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["form_code"] == "0619E")
            .map_or("none".to_string(), |row| {
                row["status"].as_str().unwrap().to_string()
            })
    };

    // ---- signed out: states unknown, nothing blocked by the list; the
    // sign-in refusal is today's
    let signed_out = bot.app("tax-profile", "ebirforms_account_tins", json!({}));
    assert_eq!(result_of(&signed_out)["read"], "signed_out", "{signed_out}");
    for row in result_of(&signed_out)["rows"].as_array().unwrap() {
        assert_eq!(
            (row["state"].as_str(), row["can_queue"].as_bool()),
            (Some("unknown"), Some(true))
        );
        assert_eq!(row["chip"], Value::Null);
    }
    assert_eq!(result_of(&signed_out)["rows"].as_array().unwrap().len(), 8);
    assert!(
        mock.lock().unwrap().requests.is_empty(),
        "signed out asks nothing"
    );
    let refused = queue(&mut bot, UNVERIFIED);
    assert!(
        error_text(&refused).contains("Sign in to Buwiz first"),
        "{refused}"
    );

    // ---- the new server's complete list
    bot.sign_in();
    let answer = bot.app("tax-profile", "ebirforms_account_tins", json!({}));
    clean(&answer);
    let list = result_of(&answer);
    assert_eq!(
        (
            list["read"].as_str(),
            list["complete"].as_bool(),
            list["stale"].as_bool()
        ),
        (Some("ok"), Some(true), Some(false))
    );
    assert!(
        list["checked_at"]
            .as_str()
            .is_some_and(|at| at.ends_with('Z')),
        "{list}"
    );
    assert_eq!(
        mock.lock().unwrap().requests,
        ["/api/tax-profiles?include=all,pending"]
    );
    let expect = [
        (
            UNVERIFIED,
            "not_verified",
            "Not verified",
            "This TIN is on your Buwiz account but not verified yet.",
            Some(("verify", "Verify it")),
            false,
        ),
        (APPROVED, "active", "", "", None, true),
        (
            SENT_BACK,
            "sent_back",
            "Sent back",
            "Buwiz sent this TIN back for a fix: A photo is too blurry to read. Name does not match MARIA CLARA.",
            Some(("fix", "Fix it")),
            false,
        ),
        (
            PAUSED,
            "paused",
            "Paused",
            "Buwiz staff paused this TIN. It can't send until they turn it back on.",
            None,
            false,
        ),
        (
            PENDING,
            "waiting_for_review",
            "Waiting for review",
            "Buwiz is reviewing this TIN. You can send it once it is approved.",
            None,
            false,
        ),
        (
            ABSENT,
            "not_on_account",
            "Not on your Buwiz account",
            "This TIN is not on your Buwiz account.",
            Some(("add", "Add it")),
            false,
        ),
    ];
    for (root, state, chip, line, step, can_queue) in expect {
        let row = row_of(&answer, &ticket_of(root));
        assert_eq!(row["state"], state, "{root}: {row}");
        assert_eq!(row["can_queue"], can_queue, "{root}");
        assert_eq!(row["branch"], "00000");
        if chip.is_empty() {
            assert_eq!(
                (&row["chip"], &row["line"], &row["step"]),
                (&Value::Null, &Value::Null, &Value::Null)
            );
            continue;
        }
        assert_eq!(row["chip"], chip);
        assert!(
            ["danger", "warning", "info"].contains(&row["tone"].as_str().unwrap()),
            "{row}"
        );
        assert_eq!(row["line"], line);
        assert!(
            row["next"]
                .as_str()
                .unwrap()
                .ends_with("Your answers are kept."),
            "{row}"
        );
        match step {
            Some((kind, label)) => assert_eq!(row["step"], json!({ "kind": kind, "label": label })),
            None => assert_eq!(row["step"], Value::Null),
        }
        assert!(
            row.get("note").is_none(),
            "a list carries no staff note: {row}"
        );
    }

    // one taxpayer's branches, and one TIN with staff's note (for a screen
    // that shows it); a TIN of another taxpayer is not this ticket's to read
    let own = bot.app(
        "forms",
        "ebirforms_account_tins",
        json!({ "ticket": ticket_of(UNVERIFIED) }),
    );
    clean(&own);
    assert_eq!(
        result_of(&own)["rows"].as_array().unwrap().len(),
        1,
        "{own}"
    );
    assert_eq!(result_of(&own)["rows"][0]["state"], "not_verified");
    let one = bot.app(
        "forms",
        "ebirforms_account_tins",
        json!({ "ticket": ticket_of(SENT_BACK), "tin": tin_of(SENT_BACK) }),
    );
    assert_eq!(
        result_of(&one)["rows"].as_array().unwrap().len(),
        1,
        "{one}"
    );
    assert_eq!(
        result_of(&one)["rows"][0]["note"],
        format!("{NOTE}."),
        "{one}"
    );
    let elsewhere = bot.app(
        "forms",
        "ebirforms_account_tins",
        json!({ "ticket": ticket_of(UNVERIFIED), "tin": tin_of(APPROVED) }),
    );
    assert!(
        error_text(&elsewhere).contains("another taxpayer"),
        "{elsewhere}"
    );
    for bad in [
        json!({ "tin": tin_of(APPROVED) }),
        json!({ "nope": 1 }),
        json!({ "ticket": "tkt_00000000000000000000000000" }),
    ] {
        let refused = bot.app("forms", "ebirforms_account_tins", bad.clone());
        assert_eq!(refused["isError"], true, "{bad}: {refused}");
    }
    // a check again asks the server at most every 15 seconds: no second request yet
    let again = bot.app(
        "tax-profile",
        "ebirforms_account_tins",
        json!({ "refresh": true }),
    );
    assert_eq!(result_of(&again)["read"], "ok");
    assert_eq!(mock.lock().unwrap().requests.len(), 1);
    // every saved taxpayer is answered to the apps that list them, and to no other
    let everyone = bot.app("forms", "ebirforms_account_tins", json!({}));
    assert!(
        error_text(&everyone).contains("apps that list every taxpayer"),
        "{everyone}"
    );
    let people = bot.app("card-people", "ebirforms_account_tins", json!({}));
    assert_eq!(
        result_of(&people)["rows"].as_array().unwrap().len(),
        8,
        "{people}"
    );
    assert_eq!(tin_in(&people.to_string()), None, "{people}");
    // the card-people card has the list tool and not the link tool
    let link_refused = bot.app(
        "card-people",
        "ebirforms_account_tin_link",
        json!({ "ticket": ticket_of(ABSENT), "step": "add" }),
    );
    assert!(
        error_text(&link_refused).contains("permissions"),
        "{link_refused}"
    );
    // Tax Profile calls as itself, for one branch and for a link
    let header = bot.app(
        "tax-profile",
        "ebirforms_account_tins",
        json!({ "ticket": ticket_of(SENT_BACK), "tin": tin_of(SENT_BACK) }),
    );
    assert_eq!(
        result_of(&header)["rows"][0]["state"],
        "sent_back",
        "{header}"
    );
    let tp_link = bot.app(
        "tax-profile",
        "ebirforms_account_tin_link",
        json!({ "ticket": ticket_of(UNVERIFIED), "tin": tin_of(UNVERIFIED), "step": "verify" }),
    );
    assert_eq!(result_of(&tp_link)["url"], VERIFY_URL, "{tp_link}");
    // a branch that is not saved is refused, and no ticket is made for it
    let unsaved = bot.app(
        "forms",
        "ebirforms_account_tins",
        json!({ "ticket": ticket_of(UNVERIFIED), "tin": format!("{UNVERIFIED}00007") }),
    );
    assert!(error_text(&unsaved).contains("no such branch"), "{unsaved}");

    // ---- who may call them: apps that list them; never the assistant,
    // a resolve or a routine
    for name in ["ebirforms_account_tins", "ebirforms_account_tin_link"] {
        let args = json!({ "ticket": ticket_of(UNVERIFIED), "step": "verify" });
        denied(&bot.agent(name, args.clone()));
        denied(&bot.call_as(json!({ "origin": "resolve" }), name, args.clone()));
        denied(&bot.call_as(json!({ "origin": "routine" }), name, args.clone()));
        let calendar = bot.app("calendar", name, args);
        assert_eq!(calendar["isError"], true, "{calendar}");
        assert!(error_text(&calendar).contains("permissions"), "{calendar}");
    }

    // ---- the links: only "Add it" carries a TIN; Verify and Fix open the
    // row's own page; nothing off the account site is ever answered
    let link = |bot: &mut Bot, app: &str, root: &str, step: &str| {
        bot.app(
            app,
            "ebirforms_account_tin_link",
            json!({ "ticket": ticket_of(root), "step": step }),
        )
    };
    let add = link(&mut bot, "forms", ABSENT, "add");
    assert_eq!(
        result_of(&add)["url"],
        format!("https://account.buwiz.com/tins/new?tin={ABSENT}&branch=00000"),
        "{add}"
    );
    assert_eq!(
        result_of(&link(&mut bot, "forms", UNVERIFIED, "verify"))["url"],
        VERIFY_URL
    );
    assert_eq!(
        result_of(&link(&mut bot, "forms", SENT_BACK, "fix"))["url"],
        FIX_URL
    );
    let verify = link(&mut bot, "forms", UNVERIFIED, "verify");
    assert!(
        tin_in(&verify.to_string()).is_none(),
        "Verify carries no TIN: {verify}"
    );
    // a link the server supplied off the site is not followed
    let foreign = link(&mut bot, "forms", FOREIGN_LINK, "fix");
    assert_eq!(
        result_of(&foreign)["url"],
        "https://account.buwiz.com/tins",
        "{foreign}"
    );
    // a step it does not know, a TIN of another taxpayer
    assert_eq!(link(&mut bot, "forms", ABSENT, "open")["isError"], true);
    let other = bot.app(
        "forms",
        "ebirforms_account_tin_link",
        json!({ "ticket": ticket_of(ABSENT), "tin": tin_of(APPROVED), "step": "add" }),
    );
    assert!(error_text(&other).contains("another taxpayer"), "{other}");

    // ---- the Queue gate: a TIN known not sendable is refused with the line,
    // and nothing is queued
    for (root, line) in [
        (
            UNVERIFIED,
            "This TIN is on your Buwiz account but not verified yet.",
        ),
        (
            SENT_BACK,
            "Buwiz sent this TIN back for a fix: A photo is too blurry to read. Name does not match MARIA CLARA.",
        ),
        (
            PAUSED,
            "Buwiz staff paused this TIN. It can't send until they turn it back on.",
        ),
        (
            PENDING,
            "Buwiz is reviewing this TIN. You can send it once it is approved.",
        ),
        (ABSENT, "This TIN is not on your Buwiz account."),
    ] {
        let refused = queue(&mut bot, root);
        let text = error_text(&refused);
        assert!(
            text.contains(&format!("{line} Nothing was queued.")),
            "{root}: {text}"
        );
        assert!(
            !text.contains("JUANA"),
            "staff's note stays out of a refusal: {text}"
        );
    }
    // the same gate holds the other queue call, by the draft's own TIN
    let draft = saved_0619e(&app, &tin_of(UNVERIFIED));
    let by_draft = bot.app(
        "forms",
        "ebirforms_app_screen_form_queue",
        json!({ "args": ["0619E", draft.0], "ticket": ticket_of(UNVERIFIED) }),
    );
    assert!(
        error_text(&by_draft).contains("not verified yet. Nothing was queued."),
        "{by_draft}"
    );
    assert_eq!(
        status_of(&mut bot, UNVERIFIED),
        "Draft",
        "nothing was queued"
    );
    // an active TIN queues
    saved_0619e(&app, &tin_of(APPROVED));
    let queued = queue(&mut bot, APPROVED);
    assert_eq!(queued["isError"], false, "{queued}");
    assert_eq!(status_of(&mut bot, APPROVED), "Queued");

    // a queue call whose TIN is not one the gate can check is refused, not waved through
    let odd = bot.app(
        "forms",
        "ebirforms_app_queue_form_draft",
        json!({ "args": [format!("{APPROVED}1"), "0619E", 2026, 2], "ticket": ticket_of(APPROVED) }),
    );
    assert!(
        error_text(&odd).contains("names no TIN eBIRForms can check"),
        "{odd}"
    );

    // ---- a state shown as information only: the server says it can send, so
    // Queue is never off and the gate never refuses, whatever the status says
    let info = row_of(
        &bot.app("tax-profile", "ebirforms_account_tins", json!({})),
        &ticket_of(INFO_ONLY),
    );
    assert_eq!(info["can_queue"], true, "{info}");
    assert_eq!(
        info["state"], "waiting_for_review",
        "shown as information: {info}"
    );
    saved_0619e(&app, &tin_of(INFO_ONLY));
    let sent = queue(&mut bot, INFO_ONLY);
    assert_eq!(
        sent["isError"], false,
        "a TIN that can send is queued: {sent}"
    );
    assert_eq!(status_of(&mut bot, INFO_ONLY), "Queued");

    // ---- what the assistant reads of a refusal: the chip and the line,
    // and never staff's note or a TIN
    let id = app
        .queue_return("0619E", &saved_0619e(&app, &tin_of(SENT_BACK)))
        .expect("queue");
    app.demo_set_send_stage(
        id,
        &SendStage::Refused {
            code: TinRefusal::NeedsFix,
            reason: format!(
                "Not sent (nothing reached BIR): Sent back for a fix: A photo is too blurry to read. {NOTE} (tin_needs_fix)"
            ),
        },
    )
    .expect("a refused draft");
    // a queued return that failed with a TIN code (an older build's row), waiting
    saved_0619e(&app, &tin_of(PENDING));
    let waiting_id = app
        .queue_return("0619E", &saved_0619e(&app, &tin_of(PENDING)))
        .expect("queue");
    app.demo_set_send_stage(
        waiting_id,
        &SendStage::Failed {
            reason: "Not sent (nothing reached BIR): this TIN is not active on your account (tin_not_active)".into(),
            attempts: 2,
            next_retry_at: Some("2099-01-01T00:00:00Z".into()),
        },
    )
    .expect("a failed waiting row");
    let queue_list = bot.agent("ebirforms_queue_list", json!({}));
    let text = queue_list.to_string();
    for private in [
        "JUANA",
        "daylight",
        "Retake",
        "SYNTHETIC REGISTERED",
        SIGNED_IN_AS,
    ] {
        assert!(
            !text.contains(private),
            "{private} reached the assistant: {text}"
        );
    }
    assert!(
        tin_in(&text).is_none(),
        "a TIN reached the assistant: {text}"
    );
    let drafts = result_of(&queue_list)["back_to_draft"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(drafts.len(), 1, "{queue_list}");
    assert_eq!(drafts[0]["chip"], "Sent back");
    // the waiting row reads BIR's chip and line, not the retry text; the list was
    // read before its failure, so it cannot replace it
    let waiting = result_of(&queue_list)["waiting"]
        .as_array()
        .unwrap()
        .clone();
    let failed = waiting
        .iter()
        .find(|row| row["stage"] == "failed")
        .unwrap_or_else(|| panic!("a failed waiting row: {queue_list}"));
    assert_eq!(failed["chip"], "Not on your Buwiz account", "{failed}");
    assert_eq!(failed["line"], "This TIN is not on your Buwiz account.");
    for key in ["reason", "attempts", "next_try"] {
        assert!(failed.get(key).is_none(), "{key}: {failed}");
    }
    assert!(
        !text.contains("MARIA"),
        "a reason of the list reached the assistant: {text}"
    );
    // the list was read before the refusal, so it only confirms the state: the
    // stored refusal stands, in its plain sentence (no reasons, no note)
    assert_eq!(drafts[0]["line"], "Buwiz sent this TIN back for a fix.");
    // the Sending queue card's button: the link of the return's own ticket (the card app
    // may call only the link tool), the row's own page for the fix
    let card_link = bot.app(
        "card-dates",
        "ebirforms_account_tin_link",
        json!({ "ticket": drafts[0]["key"], "step": "fix" }),
    );
    assert_eq!(result_of(&card_link)["url"], FIX_URL, "{card_link}");
    let card_tins = bot.app("card-dates", "ebirforms_account_tins", json!({}));
    assert!(
        error_text(&card_tins).contains("permissions"),
        "{card_tins}"
    );
    // and the card the assistant's answer carries holds the same
    assert!(
        queue_list["_meta"]["buwiz/open"]["props"]["snapshot"]["back_to_draft"][0]["line"]
            .is_string()
    );

    // ---- the server cannot be reached: the last list is answered as stale, which
    // shows the state and never closes Queue (the 15 s age passes first)
    saved_0619e(&app, &tin_of(PAUSED));
    mock.lock().unwrap().down = true;
    std::thread::sleep(std::time::Duration::from_secs(16));
    let stale = bot.app(
        "tax-profile",
        "ebirforms_account_tins",
        json!({ "refresh": true }),
    );
    assert_eq!(result_of(&stale)["stale"], true, "{stale}");
    let paused = row_of(&stale, &ticket_of(PAUSED));
    assert_eq!(
        (paused["state"].as_str(), paused["can_queue"].as_bool()),
        (Some("paused"), Some(true)),
        "{paused}"
    );
    let through = queue(&mut bot, PAUSED);
    assert_eq!(
        through["isError"], false,
        "a stale list never blocks: {through}"
    );
    assert_eq!(status_of(&mut bot, PAUSED), "Queued");
    mock.lock().unwrap().down = false;

    // ---- today's server: no marker, so a TIN missing from its list is
    // unknown and not blocked; a pending row still blocks
    bot.sign_out();
    mock.lock().unwrap().list = (200, old_server_list());
    bot.sign_in();
    let old = bot.app("tax-profile", "ebirforms_account_tins", json!({}));
    assert_eq!(result_of(&old)["complete"], false, "{old}");
    assert_eq!(row_of(&old, &ticket_of(ABSENT))["state"], "unknown");
    assert_eq!(row_of(&old, &ticket_of(ABSENT))["can_queue"], true);
    assert_eq!(
        row_of(&old, &ticket_of(PENDING))["state"],
        "waiting_for_review"
    );
    let pending = queue(&mut bot, PENDING);
    assert!(
        error_text(&pending).contains("Buwiz is reviewing this TIN."),
        "{pending}"
    );
    saved_0619e(&app, &tin_of(ABSENT));
    let unknown = queue(&mut bot, ABSENT);
    assert_eq!(
        unknown["isError"], false,
        "an unknown state never blocks: {unknown}"
    );
    assert_eq!(status_of(&mut bot, ABSENT), "Queued");

    // ---- a banned account: every TIN reads as the account's state, Queue is
    // refused with the line, and the assistant still reads nothing private
    bot.sign_out();
    mock.lock().unwrap().list = (
        403,
        json!({ "error": { "code": "account_banned", "message": "banned" } }),
    );
    bot.sign_in();
    let banned = bot.app("tax-profile", "ebirforms_account_tins", json!({}));
    assert_eq!(result_of(&banned)["read"], "banned", "{banned}");
    let banned_row = row_of(&banned, &ticket_of(APPROVED));
    assert_eq!(
        (
            banned_row["state"].as_str(),
            banned_row["can_queue"].as_bool()
        ),
        (Some("account_banned"), Some(false))
    );
    let refused = queue(&mut bot, APPROVED);
    assert!(
        error_text(&refused)
            .contains("This Buwiz account is banned, so nothing can be sent. Nothing was queued."),
        "{refused}"
    );
    let banned_queue = bot.agent("ebirforms_queue_list", json!({}));
    assert_eq!(banned_queue["isError"], false, "{banned_queue}");
    assert_eq!(tin_in(&banned_queue.to_string()), None, "{banned_queue}");

    // ---- a list that cannot be read blocks nothing either
    bot.sign_out();
    mock.lock().unwrap().list = (500, json!({ "error": { "code": "boom" } }));
    bot.sign_in();
    let broken = bot.app("tax-profile", "ebirforms_account_tins", json!({}));
    assert_eq!(result_of(&broken)["read"], "unavailable", "{broken}");
    assert_eq!(row_of(&broken, &ticket_of(UNVERIFIED))["can_queue"], true);
    let through = queue(&mut bot, UNVERIFIED);
    assert_eq!(through["isError"], false, "{through}");
    assert_eq!(status_of(&mut bot, UNVERIFIED), "Queued");
    // the assistant's queue answer still reads, without the list
    assert_eq!(
        bot.agent("ebirforms_queue_list", json!({}))["isError"],
        false
    );
}
