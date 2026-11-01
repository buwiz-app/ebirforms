//! Buwiz sign-in for the Sign-in app (`register`): account.buwiz.com's
//! device flow (RFC 8628) under client id `buwiz-ebirforms`.
//!
//! Start asks for a code (`POST /oauth/device/code`), the person approves it
//! at `account.buwiz.com/device`, and polling the token endpoint
//! (`POST /oauth/token`, the device-code grant) answers the tokens. The
//! refresh token is kept in the data folder as a 0600 file
//! ([`ACCOUNT_FILE`]), never in a keychain; each refresh rotates it, so the
//! file is rewritten every time. The access token lives in memory only.
//!
//! What leaves this module is who is signed in and the code to type: no
//! token is ever in a tool result, a log line or the manifest. Sending
//! stays off ([`crate::owner::NO_SENDING`]); sign-in only prepares for it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Where the account API lives; a test points it at a mock server.
pub const ACCOUNT_URL_ENV: &str = "EBIRFORMS_ACCOUNT_URL";
const ACCOUNT_URL: &str = "https://api.buwiz.com";
/// This mini app's public client (no secret), with scope `app`.
pub(crate) const CLIENT_ID: &str = "buwiz-ebirforms";
/// The refresh token and the signed-in email, in the data folder (0600).
pub const ACCOUNT_FILE: &str = "buwiz-account.json";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const TIMEOUT: Duration = Duration::from_secs(20);

/// A code waiting for the person's approval.
struct Pending {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_at: Instant,
    interval: u64,
}

/// The sign-in state of this process.
pub(crate) struct Account {
    base: String,
    file: PathBuf,
    pending: Option<Pending>,
    /// The access token and when it stops working.
    access: Option<(String, Instant)>,
}

/// What the token endpoint answered.
enum Grant {
    Tokens {
        access: String,
        refresh: String,
        expires_in: u64,
    },
    /// An RFC 6749 error code (`authorization_pending`, `slow_down`, ...).
    Refused(String),
}

impl Account {
    /// The account kept in `dir` (the data folder).
    pub(crate) fn in_dir(dir: PathBuf) -> Self {
        let base = std::env::var(ACCOUNT_URL_ENV)
            .ok()
            .filter(|url| !url.trim().is_empty())
            .unwrap_or_else(|| ACCOUNT_URL.to_string());
        Self::at(dir, &base)
    }

    /// The account kept in `dir`, against the account API at `base`.
    fn at(dir: PathBuf, base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            file: dir.join(ACCOUNT_FILE),
            pending: None,
            access: None,
        }
    }

    /// The saved sign-in: `(refresh_token, email)`.
    fn saved(&self) -> Option<(String, String)> {
        let text = std::fs::read_to_string(&self.file).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        let refresh = value["refresh_token"].as_str()?.to_string();
        Some((refresh, value["email"].as_str().unwrap_or("").to_string()))
    }

    fn save(&self, refresh: &str, email: &str) -> Result<(), String> {
        let text = json!({ "refresh_token": refresh, "email": email }).to_string();
        write_private(&self.file, &text)
    }

    /// Whether someone is signed in (a saved refresh token).
    pub(crate) fn signed_in(&self) -> bool {
        self.saved().is_some()
    }

    /// `ebirforms_account_status`: who is signed in, or the code waiting.
    pub(crate) fn status(&self) -> Value {
        if let Some((_, email)) = self.saved() {
            return json!({ "signed_in": true, "email": email });
        }
        match &self.pending {
            Some(pending) if pending.expires_at > Instant::now() => {
                let mut out = json!({ "signed_in": false, "state": "pending" });
                pending.describe(&mut out);
                out
            }
            _ => json!({ "signed_in": false }),
        }
    }

    /// `ebirforms_account_sign_in_start`: a new code to approve.
    pub(crate) fn start(&mut self) -> Result<Value, String> {
        if let Some((_, email)) = self.saved() {
            return Ok(json!({ "signed_in": true, "email": email }));
        }
        let (status, body) = post_json(
            &format!("{}/oauth/device/code", self.base),
            &json!({ "client_id": CLIENT_ID }),
        )?;
        if status != 200 {
            return Err(refusal(&body, "Buwiz could not start a sign-in"));
        }
        let text = |key: &str| {
            body[key]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("Buwiz's sign-in answer has no {key}"))
        };
        let expires_in = body["expires_in"].as_u64().unwrap_or(900);
        let pending = Pending {
            device_code: text("device_code")?,
            user_code: text("user_code")?,
            verification_uri: text("verification_uri")?,
            verification_uri_complete: body["verification_uri_complete"]
                .as_str()
                .map(str::to_string),
            expires_at: Instant::now() + Duration::from_secs(expires_in),
            interval: body["interval"].as_u64().unwrap_or(5).max(1),
        };
        let mut out = json!({ "signed_in": false, "state": "pending" });
        pending.describe(&mut out);
        self.pending = Some(pending);
        Ok(out)
    }

    /// `ebirforms_account_sign_in_poll`: once, whether the code was
    /// approved. `state`: pending, signed_in, expired or denied.
    pub(crate) fn poll(&mut self) -> Result<Value, String> {
        if let Some((_, email)) = self.saved() {
            return Ok(json!({ "signed_in": true, "state": "signed_in", "email": email }));
        }
        let Some(pending) = &mut self.pending else {
            return Ok(json!({ "signed_in": false, "state": "signed_out" }));
        };
        if pending.expires_at <= Instant::now() {
            self.pending = None;
            return Ok(json!({ "signed_in": false, "state": "expired" }));
        }
        let grant = token(
            &self.base,
            &[
                ("grant_type", DEVICE_GRANT),
                ("client_id", CLIENT_ID),
                ("device_code", &pending.device_code),
            ],
        )?;
        match grant {
            Grant::Refused(code) => match code.as_str() {
                "authorization_pending" | "slow_down" => {
                    if code == "slow_down" {
                        pending.interval += 5;
                    }
                    let mut out = json!({ "signed_in": false, "state": "pending" });
                    pending.describe(&mut out);
                    Ok(out)
                }
                "expired_token" | "invalid_grant" => {
                    self.pending = None;
                    Ok(json!({ "signed_in": false, "state": "expired" }))
                }
                "access_denied" => {
                    self.pending = None;
                    Ok(json!({ "signed_in": false, "state": "denied" }))
                }
                _ => Err("Buwiz refused the sign-in".into()),
            },
            Grant::Tokens {
                access,
                refresh,
                expires_in,
            } => {
                self.pending = None;
                let email = me(&self.base, &access).unwrap_or_default();
                self.save(&refresh, &email)?;
                self.access = Some((access, Instant::now() + Duration::from_secs(expires_in)));
                Ok(json!({ "signed_in": true, "state": "signed_in", "email": email }))
            }
        }
    }

    /// A working access token: the one held, else a refresh (which rotates
    /// the saved refresh token). `None` when no one is signed in or Buwiz
    /// no longer accepts the sign-in.
    pub(crate) fn access_token(&mut self) -> Result<Option<String>, String> {
        if let Some((token, until)) = &self.access
            && *until > Instant::now() + Duration::from_secs(30)
        {
            return Ok(Some(token.clone()));
        }
        let Some((refresh, email)) = self.saved() else {
            return Ok(None);
        };
        match token(
            &self.base,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", CLIENT_ID),
                ("refresh_token", &refresh),
            ],
        )? {
            Grant::Tokens {
                access,
                refresh,
                expires_in,
            } => {
                self.save(&refresh, &email)?;
                self.access = Some((
                    access.clone(),
                    Instant::now() + Duration::from_secs(expires_in),
                ));
                Ok(Some(access))
            }
            // revoked or expired on Buwiz's side: this sign-in is over
            Grant::Refused(_) => {
                self.forget();
                Ok(None)
            }
        }
    }

    /// `ebirforms_account_sign_out`: ends the sign-in on Buwiz (best
    /// effort) and forgets it here either way.
    pub(crate) fn sign_out(&mut self) -> Result<Value, String> {
        self.pending = None;
        let mut ended = true;
        if let Ok(Some(access)) = self.access_token() {
            ended = agent()
                .post(format!("{}/api/auth/logout", self.base))
                .bearer_auth(access)
                .send()
                .is_ok_and(|response| response.status().is_success());
        }
        self.forget();
        let mut out = json!({ "signed_in": false, "state": "signed_out" });
        if !ended {
            out["note"] = json!(
                "Signed out on this computer; Buwiz could not be reached to end the sign-in there too."
            );
        }
        Ok(out)
    }

    fn forget(&mut self) {
        self.access = None;
        let _ = std::fs::remove_file(&self.file);
    }
}

impl Pending {
    /// The fields the Sign-in app shows: never the device code.
    fn describe(&self, out: &mut Value) {
        out["user_code"] = json!(self.user_code);
        out["verification_uri"] = json!(self.verification_uri);
        if let Some(complete) = &self.verification_uri_complete {
            out["verification_uri_complete"] = json!(complete);
        }
        out["expires_in"] = json!(
            self.expires_at
                .saturating_duration_since(Instant::now())
                .as_secs()
        );
        out["interval"] = json!(self.interval);
    }
}

fn agent() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .unwrap_or_default()
}

/// Network failures say where, never what was sent.
fn unreachable(_: reqwest::Error) -> String {
    "Buwiz could not be reached; check the connection and try again".into()
}

fn post_json(url: &str, body: &Value) -> Result<(u16, Value), String> {
    let response = agent().post(url).json(body).send().map_err(unreachable)?;
    let status = response.status().as_u16();
    Ok((status, response.json().unwrap_or(Value::Null)))
}

/// The token endpoint (form-encoded).
fn token(base: &str, form: &[(&str, &str)]) -> Result<Grant, String> {
    let response = agent()
        .post(format!("{base}/oauth/token"))
        .form(form)
        .send()
        .map_err(unreachable)?;
    let ok = response.status().is_success();
    let body: Value = response.json().unwrap_or(Value::Null);
    if !ok {
        return Ok(Grant::Refused(
            body["error"].as_str().unwrap_or("server_error").to_string(),
        ));
    }
    let text = |key: &str| {
        body[key]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("Buwiz's token answer has no {key}"))
    };
    Ok(Grant::Tokens {
        access: text("access_token")?,
        refresh: text("refresh_token")?,
        expires_in: body["expires_in"].as_u64().unwrap_or(900),
    })
}

/// The signed-in account's email (`GET /me`).
fn me(base: &str, access: &str) -> Option<String> {
    let response = agent()
        .get(format!("{base}/me"))
        .bearer_auth(access)
        .send()
        .ok()?;
    let body: Value = response.json().ok()?;
    body["email"].as_str().map(str::to_string)
}

/// An OAuth refusal's description, or `fallback`.
fn refusal(body: &Value, fallback: &str) -> String {
    body["error_description"]
        .as_str()
        .map(|text| format!("{fallback}: {text}"))
        .unwrap_or_else(|| fallback.to_string())
}

/// Write `text` to a 0600 file, made private before anything is in it.
fn write_private(path: &PathBuf, text: &str) -> Result<(), String> {
    use std::io::Write;
    let partial = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&partial)
        .map_err(|error| format!("the sign-in could not be saved: {error}"))?;
    crate::owner::restrict(&partial, 0o600)?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("the sign-in could not be saved: {error}"))?;
    std::fs::rename(&partial, path)
        .map_err(|error| format!("the sign-in could not be saved: {error}"))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// What the mock account server saw.
    #[derive(Default)]
    struct Seen {
        polls: usize,
        logouts: Vec<String>,
    }

    /// A one-thread HTTP server with account.buwiz.com's routes and answers
    /// (buwiz-com src/oauth.rs): the first device-code poll is pending, the
    /// second approved; each refresh rotates the refresh token.
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
                let (status, answer) = match line.split_whitespace().take(2).collect::<Vec<_>>()[..]
                {
                    ["POST", "/oauth/device/code"] => {
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
                    ["POST", "/oauth/token"] if body.contains("device_code=bwd_device") => {
                        assert!(body.contains("grant-type%3Adevice_code"), "{body}");
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
                    ["POST", "/oauth/token"] if body.contains("refresh_token=bwr_one") => (
                        200,
                        json!({ "access_token": "bwa_two", "token_type": "Bearer", "expires_in": 900, "refresh_token": "bwr_two", "scope": "app" }),
                    ),
                    ["POST", "/oauth/token"] => (
                        400,
                        json!({ "error": "invalid_grant", "error_description": "no" }),
                    ),
                    ["GET", "/me"] if bearer.starts_with("bwa_") => (
                        200,
                        json!({ "user_id": "u1", "email": "owner@example.com" }),
                    ),
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
    fn device_sign_in_refresh_and_sign_out() {
        let dir = tempfile::tempdir().unwrap();
        let (base, seen) = mock();
        let mut account = Account::at(dir.path().to_path_buf(), &base);
        assert_eq!(clean(&account.status()), &json!({ "signed_in": false }));

        let started = account.start().unwrap();
        clean(&started);
        assert_eq!(started["state"], "pending");
        assert_eq!(started["user_code"], "BCDF-GHJK");
        assert_eq!(
            started["verification_uri"],
            "https://account.buwiz.com/device"
        );
        assert_eq!(started["interval"], 5);
        assert!(started["expires_in"].as_u64().unwrap() > 890);
        assert_eq!(clean(&account.status())["user_code"], "BCDF-GHJK");

        let pending = account.poll().unwrap();
        assert_eq!(clean(&pending)["state"], "pending");
        assert!(!account.signed_in());

        let approved = account.poll().unwrap();
        assert_eq!(
            clean(&approved),
            &json!({ "signed_in": true, "state": "signed_in", "email": "owner@example.com" })
        );
        let file = dir.path().join(ACCOUNT_FILE);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the token file is {mode:o}");
        }
        assert!(std::fs::read_to_string(&file).unwrap().contains("bwr_one"));
        assert_eq!(
            clean(&account.status()),
            &json!({ "signed_in": true, "email": "owner@example.com" })
        );

        // the access token ran out: a refresh rotates the saved refresh token
        account.access = None;
        assert_eq!(account.access_token().unwrap().as_deref(), Some("bwa_two"));
        let saved = std::fs::read_to_string(&file).unwrap();
        assert!(
            saved.contains("bwr_two") && !saved.contains("bwr_one"),
            "{saved}"
        );

        let out = account.sign_out().unwrap();
        assert_eq!(
            clean(&out),
            &json!({ "signed_in": false, "state": "signed_out" })
        );
        assert_eq!(seen.lock().unwrap().logouts, ["bwa_two"]);
        assert!(!file.exists());
        assert!(!account.signed_in());
    }

    #[test]
    fn a_refused_refresh_signs_out_here() {
        let dir = tempfile::tempdir().unwrap();
        let (base, _) = mock();
        let account_file = dir.path().join(ACCOUNT_FILE);
        write_private(
            &account_file,
            &json!({ "refresh_token": "bwr_revoked", "email": "a@example.com" }).to_string(),
        )
        .unwrap();
        let mut account = Account::at(dir.path().to_path_buf(), &base);
        assert!(account.signed_in());
        assert_eq!(account.access_token().unwrap(), None);
        assert!(!account.signed_in());
    }

    #[test]
    fn an_unreachable_server_says_so_without_details() {
        let dir = tempfile::tempdir().unwrap();
        let mut account = Account::at(dir.path().to_path_buf(), "http://127.0.0.1:9");
        let error = account.start().unwrap_err();
        assert!(error.contains("could not be reached"), "{error}");
    }
}
