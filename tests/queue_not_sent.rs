//! A return whose send did not get through stays queued, over a loopback
//! account server (never a real host): the mini app's own queue worker
//! signs in through the Sign-in app's tools, tries the return against a
//! server that has no way to take it ("not sent": nothing reached the BIR),
//! and the return stays Queued, never a Draft, reading a waiting stage with
//! the time of its next try. Every request on the way (sign-in, the TIN
//! list, the send) names the client `mini` with the plugin's version. One
//! test: the database handle and the key are process-wide.

#![allow(unsafe_code)] // the test sets the process env, as main does

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bir_app_core::{BirApp, BranchFacts, NewTaxpayer, OpenReturn};
use ebirforms_mini_app::{ManifestSource, Server};
use serde_json::{Value, json};

const TEST_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// A synthetic taxpayer (the official check digit, so a return validates).
const ROOT: &str = "000987659";
/// The plugin's version, as the manifest and the client header say it.
const VERSION: &str = "0.4.1";

/// What the loopback server saw: each request's path and its `buwiz-client`.
type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

fn serve(listener: TcpListener, seen: Seen) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let (mut length, mut client) = (0, None);
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
                } else if name.eq_ignore_ascii_case(bir_submit::CLIENT_HEADER) {
                    client = Some(value.trim().to_string());
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let route: Vec<&str> = line.split_whitespace().take(2).collect();
            seen.lock()
                .unwrap()
                .push((route[1].to_string(), client.clone()));
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
                ["GET", "/me"] => (
                    200,
                    json!({ "user_id": "u1", "email": "owner@example.test" }),
                ),
                ["POST", "/api/devices"] => (200, json!({ "id": "dev_1" })),
                ["GET", path] if path.starts_with("/api/tax-profiles") => (
                    200,
                    json!({ "include": "all", "profiles": [{
                        "id": "id-1", "tin": "000-987-659-00000", "registered_name": "SYNTHETIC NAME",
                        "permissions": ["profile.view", "filing.prepare"],
                        "status": "approved", "can_send": true,
                    }] }),
                ),
                // no trust file, no sealing key, no relay: nothing can be sent
                _ => (404, json!({ "error": { "code": "not_found" } })),
            };
            let text = answer.to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                text.len()
            );
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
}

fn result_of(answer: &Value) -> &Value {
    assert_eq!(answer["isError"], false, "{answer}");
    &answer["structuredContent"]["result"]
}

#[test]
fn a_return_that_was_not_sent_stays_queued_and_reads_when_it_tries_again() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    // SAFETY: set before this test starts any thread (one test per process).
    unsafe {
        std::env::set_var("BUWIZ_DB_KEY", TEST_KEY);
        std::env::set_var("BUWIZ_NO_KEYCHAIN", "1");
        std::env::set_var("EBIRFORMS_ACCOUNT_URL", &base);
    }
    let seen: Seen = Arc::default();
    serve(listener, seen.clone());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bir_data.db");

    // the server over the database, with its queue worker running through
    // the account (the Bot's open tokens are not what this test is about)
    let app = Arc::new(BirApp::open_at(&path).unwrap());
    let mut bot = Bot {
        server: Server::with_app(app.clone(), path.clone(), ManifestSource::Unsigned, false),
        next: 0,
    };
    bot.server.start_queue();

    // a saved return for a taxpayer
    let taxpayer = app
        .create_taxpayer(&NewTaxpayer {
            tin_root: ROOT.into(),
            name: "Synthetic Trading".into(),
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
    let tin = taxpayer.branches[0].tin.clone();
    let opened = app
        .open_return(&OpenReturn {
            tin: tin.clone(),
            form_code: "0619E".into(),
            year: 2026,
            period: 1,
            key: None,
        })
        .expect("open 0619E");
    let checked = app.validate("0619E", &opened.draft).expect("validate");
    assert!(checked.errors.is_empty(), "{:?}", checked.errors);
    app.save("0619E", &checked.draft).expect("save");
    let listed = bot.agent("ebirforms_profiles_list", json!({}));
    let ticket = result_of(&listed)[0]["tin_root"]
        .as_str()
        .unwrap()
        .to_string();

    // signed in, the person queues it
    for tool in ["start", "poll"] {
        let name = format!("ebirforms_account_sign_in_{tool}");
        result_of(&bot.app("register", &name, json!({})));
    }
    let queued = bot.app(
        "forms",
        "ebirforms_app_queue_form_draft",
        json!({ "args": [tin, "0619E", 2026, 1], "ticket": ticket }),
    );
    assert_eq!(queued["isError"], false, "{queued}");

    // the worker tries it at once and nothing gets through: the return reads
    // when it tries again
    let began = Instant::now();
    let row = loop {
        let answer = bot.agent("ebirforms_queue_list", json!({}));
        let list = result_of(&answer);
        assert_eq!(list["back_to_draft"], json!([]), "{answer}");
        let row = list["waiting"][0].clone();
        if row["line"].is_string() {
            break row;
        }
        assert!(
            began.elapsed() < Duration::from_secs(60),
            "the worker never tried: {answer}"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    let stage = row["stage"].as_str().unwrap();
    assert!(
        ["queued", "failed", "behind", "slowed_down", "still_waiting"].contains(&stage),
        "{row}"
    );
    let line = row["line"].as_str().unwrap_or_default();
    assert!(line.contains("Tries again in"), "a time in the line: {row}");

    // still Queued in the Forms app, with the same words on its send state
    let returns = bot.app(
        "forms",
        "ebirforms_app_list_returns",
        json!({ "args_json": json!(["all"]).to_string(), "ticket": ticket }),
    );
    let status = result_of(&returns)
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["form_code"] == "0619E")
        .map(|row| row["status"].as_str().unwrap().to_string());
    assert_eq!(status.as_deref(), Some("Queued"), "{returns}");
    let state = bot.app(
        "forms",
        "ebirforms_app_queued_send_state",
        json!({ "args": [tin, "0619E", 2026, 1], "ticket": ticket }),
    );
    assert_eq!(result_of(&state)["stage"], "Failed", "{state}");

    // every request the worker and the sign-in made said who is asking
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter().any(|(path, _)| path.starts_with("/api/tickets")
            || path.starts_with("/.well-known/")
            || path.starts_with("/api/v2/")),
        "the send was tried: {seen:?}"
    );
    let want = format!("mini/{VERSION}");
    for (path, client) in seen.iter() {
        assert_eq!(client.as_deref(), Some(want.as_str()), "{path}");
    }
}
