//! Buwiz sign-in and sending for the Sign-in app (`register`) and the
//! queue: eBIRForms' own submission client ([`bir_submit::Submission`])
//! under client id `buwiz-ebirforms`.
//!
//! Sign-in is account.buwiz.com's device flow (RFC 8628): start asks for a
//! code, the person approves it at `account.buwiz.com/device`, and polling
//! stores the tokens and registers this computer's device key. The same
//! session sends: a return the Forms app queues is sealed to Buwiz's Sender,
//! goes out through the relay, and comes back with a signed receipt
//! ([`Account::transport`], run by `BirApp::start_queue_worker`).
//!
//! The tokens and the device key are files in the data folder (0600,
//! `buwiz_submit_*.txt`), never a keychain. What leaves this module is who
//! is signed in and the code to type: no token or device code is ever in a
//! tool result, a log line or the manifest.

use std::path::PathBuf;
use std::sync::Arc;

use bir_submit::secrets::FileStore;
use bir_submit::{BuwizTransport, PollOutcome, SignInStatus, Submission, SubmitConfig};
use serde_json::{Value, json};

/// Where the Buwiz API lives: https, or http on this machine (a local
/// server in dry run).
pub const ACCOUNT_URL_ENV: &str = "EBIRFORMS_ACCOUNT_URL";
/// This mini app's public client (no secret), with scope `app`.
pub(crate) const CLIENT_ID: &str = "buwiz-ebirforms";
/// Seconds between polls when the server named none.
const INTERVAL: i64 = 5;

/// The sign-in state of this process.
pub(crate) struct Account {
    submission: Submission,
    /// Runs the client's calls for the tools (the queue has its own).
    runtime: tokio::runtime::Runtime,
    /// The server's poll interval for the code waiting, raised by `slow_down`.
    interval: i64,
}

/// Only https, or http to this machine: codes and tokens never cross a
/// network in plain text.
fn private_url(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', ':']).next().unwrap_or_default();
    matches!(host, "localhost" | "127.0.0.1")
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

impl Account {
    /// The account kept in `dir` (the data folder), against
    /// [`ACCOUNT_URL_ENV`] or api.buwiz.com.
    pub(crate) fn in_dir(dir: PathBuf) -> Self {
        let base = std::env::var(ACCOUNT_URL_ENV)
            .ok()
            .map(|url| url.trim().to_string())
            .filter(|url| private_url(url))
            .unwrap_or_else(|| bir_submit::DEFAULT_SERVER_URL.to_string());
        Self::at(dir, &base)
    }

    /// The account kept in `dir`, against the Buwiz API at `base`.
    fn at(dir: PathBuf, base: &str) -> Self {
        let config = SubmitConfig {
            server_url: base.trim_end_matches('/').to_string(),
            client_id: CLIENT_ID.to_string(),
            device_name: format!("eBIRForms mini app on {}", std::env::consts::OS),
            // the shipped keys; a lab build also honours BUWIZ_TRUST_KEYS
            trust_keys: bir_submit::trust::pinned_keys(),
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("ebirforms-account")
            .enable_all()
            .build()
            .expect("a runtime for the account client");
        Self {
            submission: Submission::with(config, Box::new(FileStore::new(dir))),
            runtime,
            interval: INTERVAL,
        }
    }

    /// The queue's way out, for `BirApp::start_queue_worker`.
    pub(crate) fn transport(&self) -> Arc<BuwizTransport> {
        self.submission.transport()
    }

    /// Runs the queue now instead of at its next minute.
    pub(crate) fn send_now(&self) {
        self.submission.send_now();
    }

    /// Whether someone is signed in.
    pub(crate) fn signed_in(&self) -> bool {
        matches!(
            self.submission.sign_in_status(),
            SignInStatus::SignedIn { .. }
        )
    }

    /// `ebirforms_account_status`: who is signed in, or the code waiting.
    pub(crate) fn status(&self) -> Value {
        match self.submission.sign_in_status() {
            SignInStatus::SignedIn { email } => {
                json!({ "signed_in": true, "email": email.unwrap_or_default() })
            }
            SignInStatus::Pending {
                user_code,
                verification_uri,
                verification_uri_complete,
                expires_at,
            } => json!({
                "signed_in": false,
                "state": "pending",
                "user_code": user_code,
                "verification_uri": verification_uri,
                "verification_uri_complete": verification_uri_complete,
                "expires_in": (expires_at - now()).max(0),
                "interval": self.interval,
            }),
            SignInStatus::SignedOut => json!({ "signed_in": false }),
        }
    }

    /// `ebirforms_account_sign_in_start`: a new code to approve.
    pub(crate) fn start(&mut self) -> Result<Value, String> {
        if self.signed_in() {
            return Ok(self.status());
        }
        let code = self
            .runtime
            .block_on(self.submission.start_sign_in())
            .map_err(|error| format!("Buwiz could not start a sign-in: {error}"))?;
        self.interval = code.interval.max(1);
        Ok(self.status())
    }

    /// `ebirforms_account_sign_in_poll`: once, whether the code was
    /// approved. `state`: pending, signed_in, expired or denied.
    pub(crate) fn poll(&mut self) -> Result<Value, String> {
        if self.signed_in() {
            let mut out = self.status();
            out["state"] = json!("signed_in");
            return Ok(out);
        }
        let outcome = self
            .runtime
            .block_on(self.submission.poll_sign_in())
            .map_err(|error| format!("Buwiz could not finish the sign-in: {error}"))?;
        Ok(match outcome {
            PollOutcome::Pending { interval } => {
                self.interval = interval.max(1);
                self.status()
            }
            PollOutcome::SignedIn => {
                let mut out = self.status();
                out["state"] = json!("signed_in");
                out
            }
            PollOutcome::Denied => json!({ "signed_in": false, "state": "denied" }),
            PollOutcome::Expired => json!({ "signed_in": false, "state": "expired" }),
        })
    }

    /// `ebirforms_account_sign_out`: ends the sign-in on Buwiz (when it can
    /// be reached) and forgets it here.
    pub(crate) fn sign_out(&mut self) -> Result<Value, String> {
        self.runtime
            .block_on(self.submission.sign_out())
            .map_err(|error| format!("Buwiz could not sign this computer out: {error}"))?;
        Ok(json!({ "signed_in": false, "state": "signed_out" }))
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;

    use super::*;

    /// What the mock account server saw.
    #[derive(Default)]
    struct Seen {
        polls: usize,
        devices: usize,
        logouts: Vec<String>,
    }

    /// A one-thread HTTP server with account.buwiz.com's sign-in routes and
    /// answers (buwiz-com src/oauth.rs): the first poll is pending, the
    /// second approved.
    fn mock() -> (String, Arc<Mutex<Seen>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Seen::default()));
        let state = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut length = 0;
                let mut bearer = String::new();
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    let header = header.trim_end();
                    if header.is_empty() {
                        break;
                    }
                    let (name, value) = header.split_once(':').unwrap();
                    match name.to_ascii_lowercase().as_str() {
                        "content-length" => length = value.trim().parse().unwrap(),
                        "authorization" => {
                            bearer = value.trim().trim_start_matches("Bearer ").to_string()
                        }
                        _ => {}
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let body = String::from_utf8(body).unwrap();
                let mut seen = state.lock().unwrap();
                let route: Vec<&str> = line.split_whitespace().take(2).collect();
                let (status, answer) = match route[..] {
                    ["POST", "/auth/device/start"] => {
                        assert!(body.contains("buwiz-ebirforms"), "{body}");
                        (
                            200,
                            json!({
                                "device_code": "bwd_device", "user_code": "BCDF-GHJK",
                                "verification_uri": "https://account.buwiz.com/device",
                                "verification_uri_complete": "https://account.buwiz.com/device?user_code=BCDF-GHJK",
                                "expires_in": 900, "interval": 5,
                            }),
                        )
                    }
                    ["POST", "/auth/device/poll"] => {
                        assert!(body.contains("bwd_device"), "{body}");
                        seen.polls += 1;
                        if seen.polls == 1 {
                            (
                                400,
                                json!({ "error": "authorization_pending", "error_description": "waiting" }),
                            )
                        } else {
                            (
                                200,
                                json!({ "access_token": "bwa_one", "token_type": "Bearer", "expires_in": 900, "refresh_token": "bwr_one", "scope": "app" }),
                            )
                        }
                    }
                    ["GET", "/me"] if bearer.starts_with("bwa_") => (
                        200,
                        json!({ "user_id": "u1", "email": "owner@example.com" }),
                    ),
                    ["POST", "/api/devices"] => {
                        seen.devices += 1;
                        (200, json!({ "id": "dev_1" }))
                    }
                    ["POST", "/api/auth/logout"] => {
                        seen.logouts.push(bearer.clone());
                        (200, json!({}))
                    }
                    _ => (
                        404,
                        json!({ "error": { "code": "not_found", "message": "not found" } }),
                    ),
                };
                drop(seen);
                let text = answer.to_string();
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                    text.len()
                )
                .unwrap();
            }
        });
        (base, seen)
    }

    /// No token, and no device code, in what a tool answers.
    fn clean(answer: &Value) -> &Value {
        let text = answer.to_string();
        for secret in ["bwa_", "bwr_", "bwd_"] {
            assert!(
                !text.contains(secret),
                "{secret} reached a tool result: {text}"
            );
        }
        answer
    }

    #[test]
    fn device_sign_in_registers_the_device_and_signs_out() {
        let dir = tempfile::tempdir().unwrap();
        let (base, seen) = mock();
        let mut account = Account::at(dir.path().to_path_buf(), &base);
        assert_eq!(clean(&account.status()), &json!({ "signed_in": false }));

        let started = account.start().unwrap();
        clean(&started);
        assert_eq!(started["state"], "pending");
        assert_eq!(started["user_code"], "BCDF-GHJK");
        assert_eq!(
            started["verification_uri_complete"],
            "https://account.buwiz.com/device?user_code=BCDF-GHJK"
        );
        assert_eq!(started["interval"], 5);
        assert!(started["expires_in"].as_i64().unwrap() > 890);

        assert_eq!(clean(&account.poll().unwrap())["state"], "pending");
        assert!(!account.signed_in());

        let approved = account.poll().unwrap();
        assert_eq!(
            clean(&approved),
            &json!({ "signed_in": true, "state": "signed_in", "email": "owner@example.com" })
        );
        assert_eq!(
            seen.lock().unwrap().devices,
            1,
            "the device key is registered"
        );
        // the session and the device key: files only this user can read
        let mut files = 0;
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let path = entry.unwrap().path();
            files += 1;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "{} is {mode:o}", path.display());
            }
        }
        assert!(files >= 2, "{files} secret files");
        assert_eq!(
            clean(&account.status()),
            &json!({ "signed_in": true, "email": "owner@example.com" })
        );

        let out = account.sign_out().unwrap();
        assert_eq!(
            clean(&out),
            &json!({ "signed_in": false, "state": "signed_out" })
        );
        assert_eq!(seen.lock().unwrap().logouts, ["bwa_one"]);
        assert!(!account.signed_in());
    }

    #[test]
    fn only_https_or_this_machine_is_a_buwiz_server() {
        assert!(private_url("https://api.buwiz.com"));
        assert!(private_url("http://127.0.0.1:3019"));
        assert!(private_url("http://localhost:3019/"));
        assert!(!private_url("http://api.buwiz.com"));
        assert!(!private_url("http://127.0.0.1.evil.example"));
    }

    #[test]
    fn an_unreachable_server_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let mut account = Account::at(dir.path().to_path_buf(), "http://127.0.0.1:9");
        let error = account.start().unwrap_err();
        assert!(error.contains("could not reach"), "{error}");
    }
}
