//! Payments of BIR returns, and the printable filing record.
//!
//! A return is paid in one of three ways, all kept here:
//! - a payment email the agent read (a routine searches Gmail with
//!   `bir_payment_search`'s query; `bir_payment_parse` reads the channels bir
//!   knows, such as MyEG, and the agent reads any other itself), recorded
//!   with `bir_record_payment`;
//! - a receipt: a bank's scan or a file (image or PDF) the user gave, passed
//!   as `proof_path` and copied into bir's own `receipts` folder beside the
//!   database (where the desktop app keeps its 2551Q payment receipts);
//! - "mark as paid" with no proof (`bir_mark_paid`).
//!
//! Proof is recommended, never required. A payment is matched to its return
//! by TIN (with branch), form and period, and is a fact of its own: it never
//! changes the return's filing status, so it neither confirms a return nor
//! stands in for BIR's confirmation (and a paid return can still be
//! amended). The payments are kept in the database's settings, as the other
//! mini-app records are.
//!
//! `bir_filing_record` writes one printable document: the filled form, BIR's
//! confirmation (its receipt email, else the filing's state) and the proof
//! of payment, for Rox's viewer (`rox://view?path=`). `bir_payment_receipt`
//! makes the proof of payment alone, in memory, for Rox's `rox.preview`.

use std::path::{Path, PathBuf};

use base64::Engine;
use bir_core::db::Database;
use bir_core::forms::FilingStatus;
use bir_core::forms::agent::AgentFormKind;
use bir_core::forms::taxform::{self, TaxPeriod};
use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::confirm::{check_keys, text, watched_branches};
use super::watch::Watch;
use super::resolve::{self, Branch};

/// Settings key of every recorded payment (a JSON list).
const PAYMENTS_KEY: &str = "mini_app.payments";
/// How far back a payment search looks.
const SEARCH_DAYS: i64 = 30;
/// Receipt files bir keeps as proof.
const PROOF_EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "webp", "heic", "pdf"];
/// The largest receipt file bir copies.
const PROOF_MAX_BYTES: u64 = 25 * 1024 * 1024;
/// The channel of a payment marked paid with no details.
const MANUAL: &str = "manual";
/// The most of a payment email's text kept with its record.
const RAW_MAX_BYTES: usize = 16 * 1024;

/// A payment channel bir knows by its emails. Others still work: the agent
/// reads their email and calls `bir_record_payment` with what it read.
struct Channel {
    name: &'static str,
    /// Its sender addresses, for the Gmail search.
    senders: &'static [&'static str],
    /// Reads one of its emails, or `None` when the text is not one.
    parse: fn(&str, NaiveDate) -> Option<Value>,
}

const CHANNELS: [Channel; 1] = [Channel {
    name: "MyEG",
    senders: &["no-reply@myeg.ph"],
    parse: parse_myeg,
}];

/// One payment of one return.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Payment {
    id: String,
    /// The return token's value: `<tin>/<FORM>-<period>`.
    key: String,
    /// The branch's full TIN, digits only.
    tin: String,
    form: String,
    period: String,
    channel: String,
    reference: String,
    #[serde(default)]
    transaction_number: Option<String>,
    amount: Option<f64>,
    fee: Option<f64>,
    /// When it was paid, RFC 3339 (Manila time).
    paid_at: Option<String>,
    /// The payment email's message id.
    message_id: Option<String>,
    proof: Proof,
    #[serde(default)]
    note: Option<String>,
    /// The payment email's text, as the agent read it.
    #[serde(default)]
    raw: Option<String>,
    recorded_at: String,
}

/// What proves a payment: its email, a receipt file in bir's storage, or
/// nothing (marked paid).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Proof {
    /// `email`, `file` or `none`.
    kind: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

impl Payment {
    fn total(&self) -> Option<f64> {
        self.amount.map(|amount| cents(amount + self.fee.unwrap_or(0.0)))
    }

    /// How it was recorded: `email`, `receipt` or `manual`.
    fn source(&self) -> &'static str {
        if self.message_id.is_some() {
            "email"
        } else if self.proof.kind == "file" {
            "receipt"
        } else {
            "manual"
        }
    }

    fn json(&self, taxpayer: Option<&str>) -> Value {
        let mut out = serde_json::to_value(self).expect("a payment serializes");
        let period = period_label(&self.form, &self.period);
        out["label"] = json!(format!("{} · {period}", self.form));
        out["token"] = json!(format!("{}{}", taxform::TOKEN_PREFIX, self.key));
        out["total"] = json!(self.total());
        out["source"] = json!(self.source());
        if let Some(taxpayer) = taxpayer {
            out["taxpayer"] = json!(taxpayer);
        }
        out
    }
}

fn cents(amount: f64) -> f64 {
    (amount * 100.0).round() / 100.0
}

/// `Q3 2026` from `2026-Q3` (the canonical period as stored).
fn period_label(form: &str, canonical: &str) -> String {
    taxform::parse(&format!("{form} {canonical}"), taxform::today())
        .period
        .map(TaxPeriod::label)
        .unwrap_or_else(|| canonical.to_string())
}

fn load(db: &Database) -> Result<Vec<Payment>, String> {
    let Some(stored) = db.get_setting(PAYMENTS_KEY).map_err(|error| error.to_string())? else {
        return Ok(Vec::new());
    };
    serde_json::from_str(&stored).map_err(|error| format!("the stored payments are unreadable: {error}"))
}

/// Whether a payment is recorded for the return `tin` (9 or 14 digits)
/// files as `form` for `period` (its canonical text, `2026-Q3`).
pub(crate) fn recorded_for(db: &Database, tin: &str, form: &str, period: &str) -> bool {
    let branch = |tin: &str| {
        let digits: String = tin.chars().filter(char::is_ascii_digit).collect();
        if digits.len() == 9 { format!("{digits}00000") } else { digits }
    };
    let tin = branch(tin);
    load(db).is_ok_and(|payments| {
        payments.iter().any(|payment| branch(&payment.tin) == tin && payment.form.eq_ignore_ascii_case(form) && payment.period == period)
    })
}

/// `bir_card_remove_payment {tin, form, period, id}`: payment `id` taken off
/// that return (recorded on the wrong return, or a test), with the receipt
/// file BIR kept for it. Only the user removes one, in an app.
pub(crate) fn remove(
    db: &Database,
    arguments: &Map<String, Value>,
    today: NaiveDate,
    receipts: &Path,
) -> Result<Value, String> {
    check_keys(arguments, &["tin", "form", "period", "id"])?;
    let target = target(
        db,
        text(arguments, "tin")?,
        text(arguments, "form")?,
        text(arguments, "period")?,
        today,
    )?;
    let id = text(arguments, "id")?;
    let key = target.key();
    let mut payments = load(db)?;
    let at = payments
        .iter()
        .position(|payment| payment.id == id && payment.key == key)
        .ok_or_else(|| format!("no payment `{id}` is recorded on {}", target.label()))?;
    let removed = payments.remove(at);
    save(db, &payments)?;
    if removed.proof.kind == "file"
        && let Some(path) = removed.proof.path.as_deref().map(Path::new)
        && path.starts_with(receipts)
    {
        let _ = std::fs::remove_file(path);
    }
    Ok(json!({
        "removed": true,
        "payment": removed.json(Some(&target.branch.name)),
        "return": target.filing(db, today),
    }))
}

fn save(db: &Database, payments: &[Payment]) -> Result<(), String> {
    let json = serde_json::to_string(payments).map_err(|error| error.to_string())?;
    db.set_setting(PAYMENTS_KEY, &json)
        .map_err(|error| error.to_string())
}

/// One return a payment is for.
struct Target {
    branch: Branch,
    code: &'static str,
    period: TaxPeriod,
}

impl Target {
    fn key(&self) -> String {
        taxform::canonical_value(&self.branch.tin, self.code, self.period)
    }

    fn label(&self) -> String {
        format!("{} · {}", self.code, self.period.label())
    }

    /// The stored return, its status and id; `null` when BIR holds none.
    fn filing(&self, db: &Database, today: NaiveDate) -> Value {
        let returns = resolve::stored_returns(db, &self.branch.tin, self.code, today);
        match resolve::furthest(&returns, self.code, self.period) {
            Some(filing) => json!({
                "id": filing.id,
                "status": format!("{:?}", filing.status),
                "status_detail": resolve::status_detail(db, Some(filing)),
            }),
            None => Value::Null,
        }
    }
}

/// A period as a payment names it: the canonical forms (`2026-Q3`,
/// `2026-09`, `2025`), what a token or `/taxform` takes, or the return
/// period's last day as payment channels print it (`09/30/2026`, the
/// quarter's end for a quarterly form).
fn period_of(code: &'static str, raw: &str, today: NaiveDate) -> Result<TaxPeriod, String> {
    let raw = raw.trim();
    for format in ["%m/%d/%Y", "%Y-%m-%d"] {
        if let Ok(end) = NaiveDate::parse_from_str(raw, format) {
            return TaxPeriod::Day(end).for_form(code, today);
        }
    }
    taxform::parse(&format!("{code} {raw}"), today)
        .period
        .ok_or_else(|| {
            format!("`period` `{raw}` is not a period (e.g. 2026-Q3, 2026-09, 2025, 09/30/2026)")
        })?
        .for_form(code, today)
}

fn target(
    db: &Database,
    tin: &str,
    form: &str,
    period: &str,
    today: NaiveDate,
) -> Result<Target, String> {
    let branch = resolve::branches(db, Some(tin))?
        .into_iter()
        .next()
        .ok_or_else(|| format!("no taxpayer with TIN {tin}"))?;
    let code = taxform::form_code_of(form)
        .ok_or_else(|| format!("`form` `{form}` is not a BIR form code"))?;
    let period = period_of(code, period, today)?;
    Ok(Target {
        branch,
        code,
        period,
    })
}

fn optional_text(arguments: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.trim().is_empty() => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.trim().to_string())),
        Some(other) => Err(format!("`{key}` must be a string (got {other})")),
    }
}

/// An amount in pesos: a number, or text such as `PHP 1,234.50`.
fn amount(raw: &str) -> Option<f64> {
    let start = raw.find(|c: char| c.is_ascii_digit())?;
    let number: String = raw[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.')
        .filter(|c| *c != ',')
        .collect();
    number.trim_end_matches('.').parse::<f64>().ok().map(cents)
}

fn amount_argument(arguments: &Map<String, Value>, key: &str) -> Result<Option<f64>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_f64()
            .filter(|amount| *amount >= 0.0)
            .map(|amount| Some(cents(amount)))
            .ok_or_else(|| format!("`{key}` must be an amount in pesos (got {number})")),
        Some(Value::String(text)) => amount(text)
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be an amount in pesos, e.g. 629.94 (got `{text}`)")),
        Some(other) => Err(format!("`{key}` must be an amount in pesos (got {other})")),
    }
}

/// When a payment was made, as RFC 3339 in Manila time: RFC 3339, a local
/// `YYYY-MM-DD HH:MM[:SS]` or `MM/DD/YYYY HH:MM[:SS]` (as payment emails
/// print it), or a date.
fn paid_time(raw: &str) -> Result<String, String> {
    let manila = chrono::FixedOffset::east_opt(8 * 3600).expect("UTC+08:00");
    let raw = raw.trim();
    if let Ok(at) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Ok(at.with_timezone(&manila).to_rfc3339());
    }
    let local = [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
        "%m/%d/%Y %H:%M:%S",
        "%m/%d/%Y %H:%M",
    ]
    .iter()
    .find_map(|format| NaiveDateTime::parse_from_str(raw, format).ok())
    .or_else(|| {
        ["%Y-%m-%d", "%m/%d/%Y"]
            .iter()
            .find_map(|format| NaiveDate::parse_from_str(raw, format).ok())
            .and_then(|date| date.and_hms_opt(0, 0, 0))
    });
    local
        .and_then(|local| local.and_local_timezone(manila).single())
        .map(|at| at.to_rfc3339())
        .ok_or_else(|| {
            format!("`paid_at` must be when it was paid, e.g. 2026-10-26T18:57:10+08:00 (got `{raw}`)")
        })
}

/// Where bir keeps payment receipts: `receipts` beside the database.
pub(crate) fn receipts_dir() -> PathBuf {
    bir_core::db::app_database_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("receipts")
}

/// Copy a receipt file into bir's storage.
fn store_proof(source: &str, dir: &Path, target: &Target) -> Result<Proof, String> {
    let source = Path::new(source.strip_prefix("file://").unwrap_or(source));
    if !source.is_absolute() {
        return Err(format!(
            "`proof_path` must be the receipt file's absolute path (got `{}`)",
            source.display()
        ));
    }
    let metadata = std::fs::metadata(source)
        .map_err(|error| format!("cannot read the receipt `{}`: {error}", source.display()))?;
    if !metadata.is_file() {
        return Err(format!("`{}` is not a file", source.display()));
    }
    if metadata.len() > PROOF_MAX_BYTES {
        return Err("the receipt is over 25 MB; attach a smaller scan".into());
    }
    let ext = source
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|ext| PROOF_EXTENSIONS.contains(&ext.as_str()))
        .ok_or_else(|| {
            format!(
                "a receipt must be an image or a PDF ({}): `{}`",
                PROOF_EXTENSIONS.join(", "),
                source.display()
            )
        })?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let name = format!(
        "payment-{}-{}-{}-{stamp}.{ext}",
        target.branch.tin,
        target.code,
        target.period.canonical()
    );
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("cannot create bir's receipts folder: {error}"))?;
    let stored = dir.join(name);
    std::fs::copy(source, &stored).map_err(|error| format!("cannot store the receipt: {error}"))?;
    Ok(Proof {
        kind: "file".into(),
        path: Some(stored.display().to_string()),
        name: source
            .file_name()
            .map(|name| name.to_string_lossy().to_string()),
    })
}

fn new_id() -> String {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("pay-{stamp:x}")
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// `bir_record_payment {tin, form, period, channel, reference, amount, fee?,
/// paid_at, message_id?, proof_path?, transaction_number?, note?, raw?}`.
/// The same `message_id`, or the same channel and reference for the same
/// return, answers the payment already recorded (adding `proof_path` to it
/// when it had no receipt). A return marked paid with no details takes this
/// payment's details in place of the mark.
pub(crate) fn record(
    db: &Database,
    arguments: &Map<String, Value>,
    today: NaiveDate,
    receipts: &Path,
) -> Result<Value, String> {
    check_keys(
        arguments,
        &[
            "tin",
            "form",
            "period",
            "channel",
            "reference",
            "amount",
            "fee",
            "paid_at",
            "message_id",
            "proof_path",
            "transaction_number",
            "note",
            "raw",
        ],
    )?;
    let target = target(
        db,
        text(arguments, "tin")?,
        text(arguments, "form")?,
        text(arguments, "period")?,
        today,
    )?;
    let channel = text(arguments, "channel")?.to_string();
    let reference = optional_text(arguments, "reference")?.unwrap_or_default();
    let amount = amount_argument(arguments, "amount")?
        .ok_or("`amount` is required: the tax paid, in pesos (without the fees)")?;
    let fee = amount_argument(arguments, "fee")?;
    let paid_at = paid_time(text(arguments, "paid_at")?)?;
    let message_id = optional_text(arguments, "message_id")?;
    let proof_path = optional_text(arguments, "proof_path")?;
    let transaction_number = optional_text(arguments, "transaction_number")?;
    let note = optional_text(arguments, "note")?;
    let raw = optional_text(arguments, "raw")?;
    let key = target.key();

    let mut payments = load(db)?;
    let same = payments.iter().position(|payment| {
        message_id.is_some() && payment.message_id == message_id
            || payment.key == key
                && !reference.is_empty()
                && payment.reference.eq_ignore_ascii_case(&reference)
                && payment.channel.eq_ignore_ascii_case(&channel)
    });
    if let Some(at) = same {
        let mut added_proof = false;
        if let Some(path) = &proof_path
            && payments[at].proof.kind != "file"
        {
            payments[at].proof = store_proof(path, receipts, &target)?;
            added_proof = true;
            save(db, &payments)?;
        }
        return Ok(json!({
            "recorded": added_proof,
            "already": true,
            "payment": payments[at].json(Some(&target.branch.name)),
            "return": target.filing(db, today),
        }));
    }

    let proof = match &proof_path {
        Some(path) => store_proof(path, receipts, &target)?,
        None if message_id.is_some() => Proof {
            kind: "email".into(),
            path: None,
            name: None,
        },
        None => Proof {
            kind: "none".into(),
            path: None,
            name: None,
        },
    };
    let payment = Payment {
        id: new_id(),
        key: key.clone(),
        tin: target.branch.tin.clone(),
        form: target.code.to_string(),
        period: target.period.canonical(),
        channel,
        reference,
        transaction_number,
        amount: Some(amount),
        fee,
        paid_at: Some(paid_at),
        message_id,
        proof,
        note,
        raw,
        recorded_at: now_rfc3339(),
    };
    // "Mark as paid" held the place of this payment: it takes its details.
    let marked = payments
        .iter()
        .position(|payment| payment.key == key && payment.channel == MANUAL);
    let payment = match marked {
        Some(at) => {
            let mark = payments.remove(at);
            // A receipt attached to the mark stays its proof.
            let proof = if payment.proof.kind != "file" && mark.proof.kind == "file" {
                mark.proof
            } else {
                payment.proof
            };
            Payment {
                id: mark.id,
                note: payment.note.or(mark.note),
                proof,
                ..payment
            }
        }
        None => payment,
    };
    let answer = payment.json(Some(&target.branch.name));
    payments.push(payment);
    save(db, &payments)?;
    Ok(json!({
        "recorded": true,
        "already": false,
        "replaced_mark": marked.is_some(),
        "payment": answer,
        "return": target.filing(db, today),
    }))
}

/// `bir_mark_paid {tin, form, period, note?, proof_path?}`: paid, with no
/// details. `proof_path` is a receipt to keep as its proof (the Forms app's
/// "Attach receipt"). A return already paid answers its payment, with the
/// receipt attached to it (in place of an earlier receipt file, which stays
/// in bir's storage).
pub(crate) fn mark_paid(
    db: &Database,
    arguments: &Map<String, Value>,
    today: NaiveDate,
    receipts: &Path,
) -> Result<Value, String> {
    check_keys(arguments, &["tin", "form", "period", "note", "proof_path"])?;
    let target = target(
        db,
        text(arguments, "tin")?,
        text(arguments, "form")?,
        text(arguments, "period")?,
        today,
    )?;
    let note = optional_text(arguments, "note")?;
    let proof_path = optional_text(arguments, "proof_path")?;
    let key = target.key();
    let mut payments = load(db)?;
    if let Some(at) = payments.iter().position(|payment| payment.key == key) {
        if let Some(path) = &proof_path {
            payments[at].proof = store_proof(path, receipts, &target)?;
            save(db, &payments)?;
        }
        return Ok(json!({
            "recorded": proof_path.is_some(),
            "already": true,
            "payment": payments[at].json(Some(&target.branch.name)),
            "return": target.filing(db, today),
        }));
    }
    let proof = match &proof_path {
        Some(path) => store_proof(path, receipts, &target)?,
        None => Proof {
            kind: "none".into(),
            path: None,
            name: None,
        },
    };
    let payment = Payment {
        id: new_id(),
        key,
        tin: target.branch.tin.clone(),
        form: target.code.to_string(),
        period: target.period.canonical(),
        channel: MANUAL.into(),
        reference: String::new(),
        transaction_number: None,
        amount: None,
        fee: None,
        paid_at: None,
        message_id: None,
        proof,
        note,
        raw: None,
        recorded_at: now_rfc3339(),
    };
    let answer = payment.json(Some(&target.branch.name));
    payments.push(payment);
    save(db, &payments)?;
    Ok(json!({
        "recorded": true,
        "already": false,
        "payment": answer,
        "return": target.filing(db, today),
    }))
}

/// `bir_payments_list {tin?, form?, period?}`: recorded payments, newest
/// first. A 9-digit TIN is every branch of that taxpayer; `period` needs
/// `form`.
pub(crate) fn list(
    db: &Database,
    arguments: &Map<String, Value>,
    today: NaiveDate,
) -> Result<Value, String> {
    check_keys(arguments, &["tin", "form", "period", "year", "message_id"])?;
    let message_id = optional_text(arguments, "message_id")?;
    let year = match arguments.get("year") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|raw| raw.trim().parse().ok()))
                .filter(|year| (2000..=2200).contains(year))
                .ok_or_else(|| format!("`year` must be a year like 2026 (got {value})"))?
                .to_string(),
        ),
    };
    let tin = optional_text(arguments, "tin")?
        .map(|tin| {
            taxform::tin_digits(&tin).ok_or_else(|| format!("`{tin}` is not a TIN (9 or 14 digits)"))
        })
        .transpose()?;
    let form = optional_text(arguments, "form")?
        .map(|form| {
            taxform::form_code_of(&form).ok_or_else(|| format!("`form` `{form}` is not a BIR form code"))
        })
        .transpose()?;
    let period = match (optional_text(arguments, "period")?, form) {
        (None, _) => None,
        (Some(_), None) => return Err("`period` needs `form`".into()),
        (Some(period), Some(code)) => Some(period_of(code, &period, today)?.canonical()),
    };
    let names: Vec<Branch> = resolve::branches(db, None).unwrap_or_default();
    let name_of = |tin: &str| {
        names
            .iter()
            .find(|branch| branch.tin == tin)
            .map(|branch| branch.name.clone())
    };
    let mut payments: Vec<Payment> = load(db)?
        .into_iter()
        .filter(|payment| match &tin {
            Some(tin) if tin.len() == 9 => payment.tin.starts_with(tin.as_str()),
            Some(tin) => payment.tin == *tin,
            None => true,
        })
        .filter(|payment| message_id.is_none() || payment.message_id == message_id)
        .filter(|payment| form.is_none_or(|code| payment.form == code))
        .filter(|payment| period.as_ref().is_none_or(|period| payment.period == *period))
        .filter(|payment| {
            year.as_ref().is_none_or(|year| {
                payment.paid_at.as_deref().unwrap_or(&payment.recorded_at).starts_with(year.as_str())
            })
        })
        .collect();
    payments.sort_by(|a, b| {
        let when = |payment: &Payment| {
            payment
                .paid_at
                .clone()
                .unwrap_or_else(|| payment.recorded_at.clone())
        };
        when(b).cmp(&when(a))
    });
    // The sums of what was paid (the tax and the fees).
    let mut by_form: Vec<(String, f64)> = Vec::new();
    let mut by_month: Vec<(String, f64)> = Vec::new();
    let mut total = 0.0;
    for payment in &payments {
        let paid = payment.total().unwrap_or(0.0);
        total += paid;
        let month: String = payment.paid_at.as_deref().unwrap_or(&payment.recorded_at).chars().take(7).collect();
        for (list, key) in [(&mut by_form, payment.form.clone()), (&mut by_month, month)] {
            match list.iter_mut().find(|(known, _)| *known == key) {
                Some(entry) => entry.1 += paid,
                None => list.push((key, paid)),
            }
        }
    }
    by_form.sort_by(|a, b| b.1.total_cmp(&a.1));
    by_month.sort_by(|a, b| a.0.cmp(&b.0));
    let sums = |list: Vec<(String, f64)>, key: &str| -> Vec<Value> {
        list.into_iter().map(|(name, amount)| json!({ key: name, "amount": cents(amount) })).collect()
    };
    Ok(json!({
        "year": year,
        "taxpayer": tin.as_deref().and_then(|tin| {
            names.iter().find(|branch| branch.tin.starts_with(tin)).map(|branch| branch.name.clone())
        }),
        "totals": {
            "count": payments.len(),
            "all": cents(total),
            "by_form": sums(by_form, "form"),
            "by_month": sums(by_month, "month"),
        },
        "payments": payments
            .iter()
            .map(|payment| payment.json(name_of(&payment.tin).as_deref()))
            .collect::<Vec<_>>(),
    }))
}

/// The returns (`<tin>/<FORM>-<period>`) that have a payment recorded.
pub(crate) fn keys(db: &Database) -> Result<Vec<String>, String> {
    Ok(load(db)?.into_iter().map(|payment| payment.key).collect())
}

/// The payments recorded on or after `since` (YYYY-MM-DD), newest first.
pub(crate) fn recorded_since(db: &Database, since: &str) -> Result<Vec<Value>, String> {
    let mut found: Vec<Payment> = load(db)?.into_iter().filter(|payment| payment.recorded_at.as_str() >= since).collect();
    found.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
    Ok(found
        .iter()
        .map(|payment| {
            json!({
                "tin": payment.tin,
                "form": payment.form,
                "period": payment.period,
                "label": format!("{} · {}", payment.form, period_label(&payment.form, &payment.period)),
                "detail": format!(
                    "{} paid{}{}",
                    payment.channel,
                    payment.total().map(|total| format!(" ₱{total:.2}")).unwrap_or_default(),
                    if payment.message_id.is_some() { " · from a payment email" } else { "" }
                ),
                "paid": true,
                "key": payment.key,
            })
        })
        .collect())
}

/// `bir_payment_search {tins?}`: the Gmail search for payment emails about
/// these taxpayers (MyEG's, and any BIR payment email naming their TIN),
/// and the sent returns with no payment recorded.
pub(crate) fn search(
    db: &Database,
    arguments: &Map<String, Value>,
    today: NaiveDate,
) -> Result<Value, String> {
    check_keys(arguments, &["tins", "taxpayers", "forms"])?;
    let watch = Watch::of(arguments)?;
    let branches = watched_branches(db, arguments, &watch)?;
    let since = today - chrono::Duration::days(SEARCH_DAYS);
    // Payment channels print the 9-digit TIN, plain or dashed.
    let mut tins: Vec<String> = Vec::new();
    for branch in &branches {
        for spelling in [branch.root.clone(), resolve::format_tin(&branch.root)] {
            if !tins.contains(&spelling) {
                tins.push(spelling);
            }
        }
    }
    let senders: Vec<String> = CHANNELS
        .iter()
        .flat_map(|channel| channel.senders.iter().map(|sender| format!("from:{sender}")))
        .collect();
    let named = if tins.is_empty() {
        String::new()
    } else {
        format!(
            " ({})",
            tins.iter()
                .map(|tin| format!("\"{tin}\""))
                .collect::<Vec<_>>()
                .join(" OR ")
        )
    };
    let query = format!(
        "after:{} -from:ebirforms-noreply@bir.gov.ph ({} OR ((BIR OR \"Bureau of Internal Revenue\") (payment OR paid OR \"transaction success\" OR \"payment confirmation\"))){named}",
        since.format("%Y/%m/%d"),
        senders.join(" OR "),
    );
    let payments = load(db)?;
    let mut awaiting = Vec::new();
    for branch in &branches {
        for kind in AgentFormKind::all() {
            let code = kind.form_code();
            if !watch.has_form(code) {
                continue;
            }
            for (period, summary) in resolve::stored_returns(db, &branch.tin, code, today) {
                if !matches!(
                    summary.status,
                    FilingStatus::Queued | FilingStatus::Submitted | FilingStatus::Confirmed
                ) {
                    continue;
                }
                let key = taxform::canonical_value(&branch.tin, code, period);
                if payments.iter().any(|payment| payment.key == key) {
                    continue;
                }
                awaiting.push(json!({
                    "tin": branch.tin,
                    "taxpayer": branch.name,
                    "form": code,
                    "period": period.canonical(),
                    "status": format!("{:?}", summary.status),
                    "token": format!("{}{key}", taxform::TOKEN_PREFIX),
                }));
            }
        }
    }
    Ok(json!({
        "query": query,
        "since": since.format("%Y-%m-%d").to_string(),
        "channels": CHANNELS.iter().map(|channel| json!({"name": channel.name, "senders": channel.senders})).collect::<Vec<_>>(),
        "tins": tins,
        "forms": watch.forms(),
        "awaiting": awaiting,
    }))
}

/// `bir_payment_parse {text}`: the payment one email describes, when bir
/// knows its channel's format; `null` otherwise.
/// Its `record` carries the email's text (`raw`, trimmed, at most 16 KB)
/// and `message_id` when given, so the payment keeps its email.
pub(crate) fn parse(arguments: &Map<String, Value>, today: NaiveDate) -> Result<Value, String> {
    check_keys(arguments, &["text", "message_id"])?;
    let email = text(arguments, "text")?;
    let message_id = optional_text(arguments, "message_id")?;
    let Some(mut parsed) = CHANNELS.iter().find_map(|channel| (channel.parse)(email, today)) else {
        return Ok(Value::Null);
    };
    if let Some(record) = parsed.get_mut("record").and_then(Value::as_object_mut) {
        record.insert("raw".into(), json!(capped(email.trim(), RAW_MAX_BYTES)));
        if let Some(message_id) = message_id {
            record.insert("message_id".into(), json!(message_id));
        }
    }
    Ok(parsed)
}

/// A TIN as a payment names it, all 14 digits (a root is its head office).
fn full_tin(tin: &str) -> Option<String> {
    taxform::tin_digits(tin).map(|digits| if digits.len() == 9 { format!("{digits}00000") } else { digits })
}

/// A routine's `bir_record_payment` (spec 5.9, the routine's `writes`):
/// email-backed only. It carries the email (`message_id`, and its text as
/// `raw`), bir reads the payment from that text itself, and the arguments
/// must say the same: TIN, form, period and amount, and the reference when
/// the email has one. The model read `raw` in bir's answer with tickets for
/// TINs: they are read back first, and `raw` is kept that way.
pub(crate) fn email_backed(
    arguments: &mut Map<String, Value>,
    today: NaiveDate,
) -> Result<(), String> {
    const RULE: &str = "a routine records only what the email says";
    let Some(message_id) = optional_text(arguments, "message_id")? else {
        return Err(format!(
            "{RULE}: pass the email's `message_id` (bir_payment_parse's `record` carries it)"
        ));
    };
    if optional_text(arguments, "proof_path")?.is_some() {
        return Err(format!("{RULE}: the email is the proof, so a routine passes no `proof_path`"));
    }
    // the email as bir_payment_parse read it, not the copy the model sends
    // back (that one has its TIN hidden, and could be changed)
    let Some(raw) = read_email(&message_id) else {
        return Err(format!(
            "{RULE}: read the email with bir_payment_parse first, with the same `message_id`"
        ));
    };
    let mut email = Map::new();
    email.insert("text".into(), json!(raw));
    let parsed = parse(&email, today)?;
    let Some(record) = parsed.get("record").filter(|record| record.is_object()) else {
        return Err(format!(
            "{RULE}: bir does not know this email's format, so a routine records nothing from it (record it from a chat)"
        ));
    };
    let said = |key: &str| record[key].as_str().unwrap_or("");
    let asked = |key: &str| optional_text(arguments, key).ok().flatten().unwrap_or_default();
    let code = taxform::form_code_of(&asked("form"));
    let period = |text: &str| code.and_then(|code| period_of(code, text, today).ok());
    let mut differs = Vec::new();
    if full_tin(&asked("tin")).is_none() || full_tin(&asked("tin")) != full_tin(said("tin")) {
        differs.push("TIN");
    }
    if code.is_none() || code != taxform::form_code_of(said("form")) {
        differs.push("form");
    }
    if period(&asked("period")).is_none() || period(&asked("period")) != period(said("period")) {
        differs.push("period");
    }
    let amount = amount_argument(arguments, "amount").ok().flatten();
    if amount.is_none() || amount != record["amount"].as_f64().map(cents) {
        differs.push("amount");
    }
    if !said("reference").is_empty() && !asked("reference").eq_ignore_ascii_case(said("reference")) {
        differs.push("reference");
    }
    if !differs.is_empty() {
        return Err(format!(
            "{RULE}: its {} differ from the arguments; pass bir_payment_parse's `record` as it is",
            differs.join(", ")
        ));
    }
    arguments.insert("raw".into(), json!(raw));
    Ok(())
}

/// The payment emails bir_payment_parse read lately, by message_id: what a
/// routine's record is checked against ([`email_backed`]). In memory only,
/// the last [`READ_KEPT`].
static READ: std::sync::Mutex<std::collections::VecDeque<(String, String)>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());
const READ_KEPT: usize = 64;

/// Keep `text` as the email `message_id` says, for [`email_backed`].
pub(crate) fn remember_email(message_id: &str, text: &str) {
    let mut read = READ.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    read.retain(|(id, _)| id != message_id);
    if read.len() == READ_KEPT {
        read.pop_front();
    }
    read.push_back((message_id.to_string(), text.to_string()));
}

fn read_email(message_id: &str) -> Option<String> {
    let read = READ.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    read.iter().find(|(id, _)| id == message_id).map(|(_, text)| text.clone())
}

/// `text` cut to at most `max` bytes, at a character boundary.
fn capped(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// `parsed` (a payment email, as [`parse`] reads it) with `recorded`:
/// whether its return already holds it, by the rule [`record`] uses for
/// the same payment (its reference through the same channel).
pub(crate) fn with_recorded(db: &Database, mut parsed: Value, today: NaiveDate) -> Value {
    let Some(record) = parsed.get("record") else { return parsed };
    let field = |name: &str| record[name].as_str().unwrap_or("");
    let (reference, channel) = (field("reference"), field("channel"));
    let message_id = field("message_id");
    let readable = readable(record);
    let same_email = !message_id.is_empty()
        && load(db).is_ok_and(|payments| payments.iter().any(|payment| payment.message_id.as_deref() == Some(message_id)));
    let recorded = same_email
        || readable && target(db, field("tin"), field("form"), field("period"), today).is_ok_and(|target| {
            let key = target.key();
            load(db).is_ok_and(|payments| {
                payments.iter().any(|payment| {
                    payment.key == key
                        && payment.reference.eq_ignore_ascii_case(reference)
                        && payment.channel.eq_ignore_ascii_case(channel)
                })
            })
        });
    parsed["recorded"] = json!(recorded);
    parsed["readable"] = json!(readable);
    parsed
}

/// Whether a parsed `record` names a payment: a BIR form code, a TIN, an
/// amount and a reference. A channel that masks its email (`*`) leaves
/// none, and a card offers nothing to record from it.
pub(crate) fn readable(record: &Value) -> bool {
    let field = |name: &str| record[name].as_str().unwrap_or("").trim().to_string();
    let amount = record["amount"]
        .as_f64()
        .or_else(|| record["amount"].as_str().and_then(|raw| raw.replace(',', "").parse().ok()))
        .unwrap_or(0.0);
    taxform::form_code_of(&field("form")).is_some()
        && taxform::tin_digits(&field("tin")).is_some()
        && amount > 0.0
        && field("reference").chars().any(|c| c.is_ascii_alphanumeric())
}

/// The labels of MyEG's "BIR Transaction Success" email, in its order.
const MYEG_LABELS: [&str; 15] = [
    "Agency Name",
    "TIN",
    "Branch Code",
    "RDO Code",
    "Form Type",
    "Tax Type",
    "Return Period",
    "Transaction Date",
    "Transaction Number",
    "Reference Number",
    "Payment Channel",
    "Transaction Amount",
    "Bank Fee",
    "Transaction Fee",
    "Total Amount",
];

/// Where `label` (lowercase ASCII) stands as whole words in `lower`.
fn find_label(lower: &str, label: &str) -> Option<usize> {
    let bytes = lower.as_bytes();
    let mut from = 0;
    while let Some(at) = lower[from..].find(label) {
        let start = from + at;
        let end = start + label.len();
        let before = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let after = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if before && after {
            return Some(start);
        }
        from = start + 1;
    }
    None
}

/// Each label's value: the text from the label to the next label, however
/// the email was flattened (one line with `|`, a table, label: value lines).
fn labelled<'a>(text: &str, labels: &[&'a str]) -> Vec<(&'a str, String)> {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = flat.to_ascii_lowercase();
    let mut found: Vec<(usize, usize, &str)> = labels
        .iter()
        .filter_map(|label| {
            find_label(&lower, &label.to_ascii_lowercase()).map(|at| (at, at + label.len(), *label))
        })
        .collect();
    found.sort_by_key(|(at, ..)| *at);
    // A label inside another's span (none in MyEG's) would cut it short.
    found.dedup_by_key(|(at, ..)| *at);
    let mut out = Vec::new();
    for (i, (_, end, label)) in found.iter().enumerate() {
        let stop = found.get(i + 1).map_or(flat.len(), |next| next.0);
        let value = flat[*end..stop.max(*end)]
            .trim_matches(|c: char| c.is_whitespace() || c == '|' || c == ':' || c == '*')
            .to_string();
        out.push((*label, value));
    }
    out
}

/// MyEG's "BIR Transaction Success" email (from `no-reply@myeg.ph`).
fn parse_myeg(email: &str, today: NaiveDate) -> Option<Value> {
    let lower = email.to_ascii_lowercase();
    if !lower.contains("bureau of internal revenue") && !lower.contains("myeg") {
        return None;
    }
    let fields = labelled(email, &MYEG_LABELS);
    let get = |label: &str| {
        fields
            .iter()
            .find(|(name, _)| *name == label)
            .map(|(_, value)| value.as_str())
            .filter(|value| !value.is_empty())
    };
    let first_word = |label: &str| get(label).and_then(|value| value.split_whitespace().next());
    let root: String = get("TIN")?.chars().filter(char::is_ascii_digit).collect();
    let branch_code = first_word("Branch Code")
        .map(|code| code.chars().filter(char::is_ascii_digit).collect::<String>())
        .filter(|code| !code.is_empty() && code.len() <= 5)
        .map(|code| format!("{code:0>5}"));
    let tin = match root.len() {
        9 => format!("{root}{}", branch_code.as_deref().unwrap_or("00000")),
        _ => taxform::tin_digits(&root)?,
    };
    let form_raw = first_word("Form Type")?;
    let form = taxform::form_code_of(form_raw);
    let return_period = first_word("Return Period")?;
    let period = form.and_then(|code| period_of(code, return_period, today).ok());
    let amount_paid = get("Transaction Amount").and_then(amount);
    let total = get("Total Amount").and_then(amount);
    if amount_paid.is_none() && total.is_none() {
        return None;
    }
    let bank_fee = get("Bank Fee").and_then(amount);
    let transaction_fee = get("Transaction Fee").and_then(amount);
    let fee = match (bank_fee, transaction_fee) {
        (None, None) => None,
        (bank, transaction) => Some(cents(bank.unwrap_or(0.0) + transaction.unwrap_or(0.0))),
    };
    // The tax itself: the transaction amount, else the total less the fees.
    let amount_paid = amount_paid.or_else(|| total.map(|total| cents(total - fee.unwrap_or(0.0))));
    let paid_at = get("Transaction Date").and_then(|date| {
        let mut words = date.split_whitespace();
        let stamp = match (words.next(), words.next()) {
            (Some(day), Some(time)) if time.contains(':') => format!("{day} {time}"),
            (Some(day), _) => day.to_string(),
            _ => return None,
        };
        paid_time(&stamp).ok()
    });
    let reference = first_word("Reference Number").unwrap_or("").to_string();
    let transaction_number = first_word("Transaction Number").map(str::to_string);
    let channel = "MyEG";
    let token = form
        .zip(period)
        .map(|(code, period)| format!("{}{}", taxform::TOKEN_PREFIX, taxform::canonical_value(&tin, code, period)));
    Some(json!({
        "channel": channel,
        "method": get("Payment Channel"),
        "tin": tin,
        "branch": branch_code.unwrap_or_else(|| "00000".into()),
        "rdo": get("RDO Code"),
        "form": form.unwrap_or(form_raw),
        "tax_type": get("Tax Type"),
        "return_period": return_period,
        "period": period.map(TaxPeriod::canonical),
        "paid_at": paid_at,
        "transaction_number": transaction_number,
        "reference": reference,
        "amount": amount_paid,
        "bank_fee": bank_fee,
        "transaction_fee": transaction_fee,
        "fee": fee,
        "total": total,
        "token": token,
        // What to pass to bir_record_payment, with the email's message_id.
        "record": {
            "tin": tin,
            "form": form.unwrap_or(form_raw),
            "period": period.map(TaxPeriod::canonical).unwrap_or_else(|| return_period.to_string()),
            "channel": channel,
            "reference": reference,
            "transaction_number": transaction_number,
            "amount": amount_paid,
            "fee": fee,
            "paid_at": paid_at,
        },
    }))
}

// ----- the printable filing record

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// The form sheet's `@page { size: W H }`, so the record's own pages match
/// it; a long bond page when the sheet names none.
fn page_size(document: &str) -> (String, String) {
    let fallback = ("612pt".to_string(), "936pt".to_string());
    let Some(start) = document.find("@page") else {
        return fallback;
    };
    let Some(size_at) = document[start..].find("size:") else {
        return fallback;
    };
    let rest = &document[start + size_at + "size:".len()..];
    let end = rest.find([';', '}']).unwrap_or(rest.len());
    let mut parts = rest[..end].split_whitespace();
    match (parts.next(), parts.next()) {
        (Some(width), Some(height)) => (width.to_string(), height.to_string()),
        _ => fallback,
    }
}

fn page_open(id: &str, width: &str, height: &str) -> String {
    format!(
        "<div class=\"page page-record\" id=\"{id}\" style=\"width:{width};min-height:{height};height:auto;overflow:visible;padding:28pt 36pt;box-sizing:border-box;font-family:'eBIRForms Arimo',Arial,Helvetica,sans-serif;font-size:10.5pt;line-height:1.4;color:#000;background:#fff\">"
    )
}

fn row(label: &str, value: &str) -> String {
    format!(
        "<tr><td style=\"padding:3pt 12pt 3pt 0;color:#555;vertical-align:top;white-space:nowrap\">{}</td><td style=\"padding:3pt 0;vertical-align:top\">{}</td></tr>",
        escape(label),
        escape(value)
    )
}

fn pesos(amount: Option<f64>) -> String {
    amount.map_or_else(|| "—".to_string(), |amount| format!("PHP {amount:.2}"))
}

/// When BIR has no receipt email on file: what the filing's state is.
fn confirmation_page(status: &FilingStatus, marks: &Value, width: &str, height: &str) -> String {
    let mark = |key: &str| {
        marks[key]
            .as_str()
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string)
    };
    let reference = mark("submission_filename").map(|name| resolve::filing_reference(&name));
    let state = match status {
        FilingStatus::Draft => "Not filed: this return is still a draft.".to_string(),
        FilingStatus::Queued => "Queued: this computer has not sent it to BIR yet.".to_string(),
        FilingStatus::Submitted => {
            "Sent to BIR. BIR's confirmation email has not been matched to it yet.".to_string()
        }
        FilingStatus::Confirmed | FilingStatus::Paid => {
            "Confirmed by BIR. Its confirmation email is not on file here.".to_string()
        }
    };
    let mut html = page_open("page-record-confirmation", width, height);
    html.push_str("<h1 style=\"font-size:17pt;font-weight:700;margin:0 0 10pt 0\">BIR filing confirmation</h1>");
    html.push_str(&format!("<p style=\"margin:0 0 10pt 0\">{}</p>", escape(&state)));
    html.push_str("<table style=\"border-collapse:collapse\">");
    if let Some(reference) = &reference {
        html.push_str(&row("File name", reference));
    }
    if let Some(at) = mark("submitted_at") {
        html.push_str(&row("Sent to BIR", &at));
    }
    if let Some(at) = mark("confirmed_at") {
        html.push_str(&row("Confirmed", &at));
    }
    html.push_str("</table></div>");
    html
}

/// A receipt file shown inline: an image, or a PDF as an embedded document.
fn proof_html(proof: &Proof) -> String {
    let Some(path) = proof.path.as_deref() else {
        return String::new();
    };
    let name = proof.name.clone().unwrap_or_else(|| {
        Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default()
    });
    let ext = Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "pdf" => "application/pdf",
        _ => "",
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) if !mime.is_empty() => bytes,
        _ => {
            return format!(
                "<p style=\"color:#a00\">The receipt {} could not be read from bir's storage.</p>",
                escape(&name)
            );
        }
    };
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    let caption = format!(
        "<p style=\"margin:10pt 0 4pt 0;color:#555\">Receipt: {}</p>",
        escape(&name)
    );
    if mime == "application/pdf" {
        format!(
            "{caption}<object data=\"data:application/pdf;base64,{data}\" type=\"application/pdf\" style=\"width:100%;height:640pt;border:1px solid #ccc\"><p>The PDF receipt is kept at {}.</p></object>",
            escape(path)
        )
    } else {
        format!(
            "{caption}<img src=\"data:{mime};base64,{data}\" alt=\"{}\" style=\"max-width:100%;max-height:640pt;border:1px solid #ccc\">",
            escape(&name)
        )
    }
}

fn payment_page(payments: &[Payment], width: &str, height: &str) -> String {
    let mut html = page_open("page-record-payment", width, height);
    html.push_str("<h1 style=\"font-size:17pt;font-weight:700;margin:0 0 10pt 0\">Proof of payment</h1>");
    if payments.is_empty() {
        html.push_str("<p>No payment is recorded for this return.</p></div>");
        return html;
    }
    for payment in payments {
        let channel = if payment.channel == MANUAL {
            "Marked paid (no details)".to_string()
        } else {
            payment.channel.clone()
        };
        let proof = match payment.source() {
            "email" => "Payment email",
            "receipt" => "Receipt file",
            _ => "None (marked paid)",
        };
        html.push_str("<table style=\"border-collapse:collapse;margin:0 0 8pt 0\">");
        html.push_str(&row("Channel", &channel));
        if !payment.reference.is_empty() {
            html.push_str(&row("Reference number", &payment.reference));
        }
        if let Some(number) = &payment.transaction_number {
            html.push_str(&row("Transaction number", number));
        }
        html.push_str(&row("Amount", &pesos(payment.amount)));
        if payment.fee.is_some() {
            html.push_str(&row("Fees", &pesos(payment.fee)));
            html.push_str(&row("Total paid", &pesos(payment.total())));
        }
        html.push_str(&row(
            "Paid",
            payment.paid_at.as_deref().unwrap_or("— (not stated)"),
        ));
        html.push_str(&row("Proof", proof));
        if let Some(message_id) = &payment.message_id {
            html.push_str(&row("Email message id", message_id));
        }
        if let Some(note) = &payment.note {
            html.push_str(&row("Note", note));
        }
        html.push_str(&row("Recorded", &payment.recorded_at));
        html.push_str("</table>");
        if let Some(raw) = &payment.raw {
            html.push_str(&format!(
                "<pre style=\"white-space:pre-wrap;word-break:break-word;margin:0 0 8pt 0;padding:6pt;border:1px solid #ddd;font-family:inherit;font-size:9.5pt\">{}</pre>",
                escape(raw)
            ));
        }
        html.push_str(&proof_html(&payment.proof));
    }
    html.push_str("<p style=\"margin-top:14pt;color:#555;font-size:9pt\">Payment and BIR's filing confirmation are separate records.</p></div>");
    html
}

/// Gmail's page for the message `message_id`.
fn gmail_url(message_id: &str) -> String {
    format!("https://mail.google.com/mail/u/0/#all/{}", resolve::encode_component(message_id))
}

/// One return's receipt, made in memory.
pub(crate) struct Receipt {
    /// The static page: every payment's details and proof.
    pub html: String,
    /// "Receipt · 1601C · Jan 2026 · <taxpayer>".
    pub title: String,
    /// The return, its taxpayer, its Gmail link and its payments' details
    /// (no email text, no image), for the assistant and the receipt card.
    pub summary: Value,
}

/// `bir_payment_receipt {tin, form, period}`: the proof of payment of one
/// return as a static page, every payment with its details and its proof
/// (an uploaded receipt embedded, or the payment email's text), and the
/// Gmail link of its email when it has one. Writes nothing.
pub(crate) fn receipt(db: &Database, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Receipt, String> {
    check_keys(arguments, &["tin", "form", "period"])?;
    let target = target(
        db,
        text(arguments, "tin")?,
        text(arguments, "form")?,
        text(arguments, "period")?,
        today,
    )?;
    let key = target.key();
    let payments: Vec<Payment> = load(db)?.into_iter().filter(|payment| payment.key == key).collect();
    if payments.is_empty() {
        return Err(format!(
            "no payment is recorded for {} of {}, so there is no receipt to show",
            target.label(),
            target.branch.name
        ));
    }
    let title = format!("Receipt · {} · {}", target.label(), target.branch.name);
    // A4, as the desktop's print preview lays a page
    let html = format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>{}</title><style>@page{{size:595pt 842pt;margin:0}}body{{margin:0;background:#fff}}</style></head><body>{}</body></html>",
        escape(&title),
        payment_page(&payments, "595pt", "842pt")
    );
    let gmail = payments.iter().find_map(|payment| payment.message_id.as_deref()).map(gmail_url);
    let details: Vec<Value> = payments
        .iter()
        .map(|payment| {
            json!({
                "channel": payment.channel,
                "reference": payment.reference,
                "transaction_number": payment.transaction_number,
                "amount": payment.amount,
                "fee": payment.fee,
                "total": payment.total(),
                "paid_at": payment.paid_at,
                "recorded_at": payment.recorded_at,
                "proof": payment.source(),
                "gmail_url": payment.message_id.as_deref().map(gmail_url),
            })
        })
        .collect();
    let summary = json!({
        "title": title,
        "form": target.code,
        "period": target.period.canonical(),
        "label": target.label(),
        "taxpayer": target.branch.name,
        "gmail_url": gmail,
        "payments": details,
    });
    Ok(Receipt { html, title, summary })
}

/// `bir_filing_record {tin, form, period}`: the filled return, BIR's
/// confirmation and the proof of payment as one printable HTML file in
/// `out_dir`, and the `rox://view` link that shows it.
pub(crate) fn filing_record(
    db: &Database,
    arguments: &Map<String, Value>,
    today: NaiveDate,
    out_dir: &Path,
) -> Result<Value, String> {
    let record = filing_record_page(db, arguments, today)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    std::fs::create_dir_all(out_dir).map_err(|error| error.to_string())?;
    let path = out_dir.join(format!("{}-{stamp}.html", record.file));
    std::fs::write(&path, &record.html).map_err(|error| error.to_string())?;
    let title = record.title;
    let path = path.display().to_string();
    let mut out = json!({
        "link": format!(
            "rox://view?path={}&title={}",
            resolve::encode_component(&path),
            resolve::encode_component(&title)
        ),
        "path": path,
        "title": title,
    });
    if let (Value::Object(out), Value::Object(facts)) = (&mut out, record.facts) {
        out.extend(facts);
    }
    Ok(out)
}

/// A filing record, made in memory.
pub(crate) struct RecordPage {
    /// The printable page.
    pub html: String,
    /// The file name it is saved under, without the stamp and extension.
    pub file: String,
    /// "2551Q Q3 2026 · <branch> · filing record".
    pub title: String,
    pub form: &'static str,
    /// The branch's name.
    pub taxpayer: String,
    /// `status`, `confirmed` and `payments`, as `bir_filing_record` reports them.
    pub facts: Value,
}

/// The filing record of `{tin, form, period}` as one page: the filled
/// return, BIR's confirmation and the payments. Writes nothing.
pub(crate) fn filing_record_page(
    db: &Database,
    arguments: &Map<String, Value>,
    today: NaiveDate,
) -> Result<RecordPage, String> {
    check_keys(arguments, &["tin", "form", "period"])?;
    let target = target(
        db,
        text(arguments, "tin")?,
        text(arguments, "form")?,
        text(arguments, "period")?,
        today,
    )?;
    let code = target.code;
    let returns = resolve::stored_returns(db, &target.branch.tin, code, today);
    let filing = resolve::furthest(&returns, code, target.period).ok_or_else(|| {
        format!(
            "{} has no {code} return for {} in BIR",
            target.branch.name,
            target.period.label()
        )
    })?;
    let kind = AgentFormKind::from_code(code).ok_or_else(|| format!("BIR has no form `{code}`"))?;
    let slug = crate::bir_module::print_slug_for(code)
        .ok_or_else(|| format!("Form {code} has no print sheet"))?;
    let draft = db
        .draft_data_json(filing.id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("the stored {code} return is gone"))?;
    let (fields, receipt) = crate::screen_form_module::print_map(db, kind, &draft)
        .map_err(|error| error.message().to_string())?;
    let document =
        bir_print::frozen_html::filled_document_with_receipt(slug, &fields, receipt.as_ref())?;
    let (width, height) = page_size(&document);
    let mut pages = String::new();
    if receipt.is_none() {
        pages.push_str(&confirmation_page(&filing.status, &draft, &width, &height));
    }
    let key = target.key();
    let payments: Vec<Payment> = load(db)?
        .into_iter()
        .filter(|payment| payment.key == key)
        .collect();
    pages.push_str(&payment_page(&payments, &width, &height));
    let document = match document.rfind("</body>") {
        Some(at) => format!("{}{pages}{}", &document[..at], &document[at..]),
        None => format!("{document}{pages}"),
    };
    let html = crate::static_html::staticize(&crate::print_sheet::prepare_html_for(
        slug, &document, true,
    ));
    Ok(RecordPage {
        html,
        file: format!("bir-{code}-{}-record", target.period.canonical()),
        title: format!("{} · {} · filing record", target.label(), target.branch.name),
        form: code,
        taxpayer: target.branch.name.clone(),
        facts: json!({
            "status": format!("{:?}", filing.status),
            "confirmed": receipt.is_some() || matches!(filing.status, FilingStatus::Confirmed | FilingStatus::Paid),
            "payments": payments.len(),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bir_core::profile::TaxpayerType;
    use bir_core::tax_profile::{BranchFacts, NewTaxpayer};

    /// MyEG's "BIR Transaction Success" email, as the user received it.
    const MYEG: &str = "Agency Name BUREAU OF INTERNAL REVENUE (BIR) | TIN 123456788 | Branch Code 00000 | RDO Code 040 - PASIG | Form Type 2551Q | Tax Type PT | Return Period 09/30/2026 | Transaction Date 10/26/2026 18:57:10 | Transaction Number TEST2026102618575200000001 | Reference Number TESTREF001 | Payment Channel Mobile | Transaction Amount PHP 629.94 | Bank Fee PHP 0.00 | Transaction Fee PHP 20.00 | Total Amount PHP 649.94";
    const ROOT: &str = "123456788";
    const TIN: &str = "12345678800000";

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 26).unwrap()
    }

    fn db() -> Database {
        let db = Database::open_ephemeral().expect("ephemeral database");
        db.create_taxpayer(NewTaxpayer {
            tin_root: ROOT.into(),
            name: "Dela Cruz, Juan".into(),
            taxpayer_type: TaxpayerType::Individual,
            birth_date: None,
            head_office: BranchFacts {
                rdo_code: "040".into(),
                registered_address: "1 Example Street, Olongapo".into(),
                zip_code: "2200".into(),
                phone: "0470000000".into(),
                email: "office@example.com".into(),
            },
        })
        .expect("taxpayer");
        db
    }

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    /// A scratch folder for receipts, never bir's own.
    fn scratch(name: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("bir-payments-{name}-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn myeg_email_is_read_into_a_payment() {
        let parsed = parse(&args(json!({"text": MYEG})), today()).unwrap();
        assert_eq!(parsed["channel"], "MyEG");
        assert_eq!(parsed["tin"], TIN);
        assert_eq!(parsed["branch"], "00000");
        assert_eq!(parsed["rdo"], "040 - PASIG");
        assert_eq!(parsed["form"], "2551Q");
        assert_eq!(parsed["tax_type"], "PT");
        // The return period is the quarter's last day.
        assert_eq!(parsed["return_period"], "09/30/2026");
        assert_eq!(parsed["period"], "2026-Q3");
        assert_eq!(parsed["paid_at"], "2026-10-26T18:57:10+08:00");
        assert_eq!(parsed["transaction_number"], "TEST2026102618575200000001");
        assert_eq!(parsed["reference"], "TESTREF001");
        assert_eq!(parsed["method"], "Mobile");
        assert_eq!(parsed["amount"], 629.94);
        assert_eq!(parsed["bank_fee"], 0.0);
        assert_eq!(parsed["transaction_fee"], 20.0);
        assert_eq!(parsed["fee"], 20.0);
        assert_eq!(parsed["total"], 649.94);
        assert_eq!(parsed["token"], "@bir:taxform:123456788/2551Q-2026-Q3");
        assert_eq!(parsed["record"]["period"], "2026-Q3");
        assert_eq!(parsed["record"]["channel"], "MyEG");
    }

    #[test]
    fn myeg_email_reads_the_same_as_a_table_with_a_footer() {
        let table = MYEG
            .split(" | ")
            .map(|field| {
                let (label, value) = MYEG_LABELS
                    .iter()
                    .find_map(|label| field.strip_prefix(label).map(|value| (*label, value.trim())))
                    .unwrap();
                format!("{label}:\t{value}\n")
            })
            .collect::<String>();
        let email = format!("BIR Transaction Success\n\n{table}\nThank you for using MyEG. 2026 MyEG Philippines.");
        let parsed = parse(&args(json!({"text": email})), today()).unwrap();
        assert_eq!(parsed["tin"], TIN);
        assert_eq!(parsed["period"], "2026-Q3");
        assert_eq!(parsed["reference"], "TESTREF001");
        assert_eq!(parsed["total"], 649.94);
        assert_eq!(parsed["paid_at"], "2026-10-26T18:57:10+08:00");
    }

    #[test]
    fn an_email_bir_does_not_know_parses_to_null() {
        let other = "GCash: You have paid PHP 629.94 to BIR. Ref No. 1234567890123.";
        assert_eq!(parse(&args(json!({"text": other})), today()).unwrap(), Value::Null);
        assert!(parse(&args(json!({"text": MYEG, "x": 1})), today()).is_err());
    }

    #[test]
    fn the_search_names_myeg_and_the_tin_and_lists_returns_awaiting_payment() {
        let db = db();
        let found = search(&db, &args(json!({"tins": [ROOT]})), today()).unwrap();
        let query = found["query"].as_str().unwrap();
        assert!(query.starts_with("after:2026/09/26 "), "{query}");
        assert!(query.contains("from:no-reply@myeg.ph"), "{query}");
        assert!(query.contains("-from:ebirforms-noreply@bir.gov.ph"), "{query}");
        assert!(query.contains("\"123456788\" OR \"123-456-788\""), "{query}");
        assert_eq!(found["awaiting"], json!([]));
        assert!(search(&db, &args(json!({"tins": ["999999999"]})), today()).is_err());
    }

    #[test]
    fn a_masked_email_is_unreadable_and_found_by_its_email() {
        let db = db();
        let receipts = scratch("masked");
        let parsed = parse(&args(json!({"text": MYEG})), today()).unwrap();
        assert_eq!(parsed["record"]["form"], "2551Q");
        assert!(readable(&parsed["record"]));
        let mut call = parsed["record"].as_object().unwrap().clone();
        call.insert("message_id".into(), json!("gmail-masked"));
        record(&db, &call, today(), &receipts).unwrap();
        // what an older parse kept of MyEG's masked layout: `*` for each field
        let masked = json!({"record": {
            "tin": "*", "form": "*", "period": "", "channel": "MyEG", "reference": "*",
            "amount": "992.10", "message_id": "gmail-masked",
        }});
        assert!(!readable(&masked["record"]));
        let checked = with_recorded(&db, masked.clone(), today());
        assert_eq!(checked["readable"], false);
        // its email is on a return already: found by the message id
        assert_eq!(checked["recorded"], true);
        let mut unknown = masked;
        unknown["record"]["message_id"] = json!("gmail-other");
        assert_eq!(with_recorded(&db, unknown, today())["recorded"], false);
        let listed = list(&db, &args(json!({"message_id": "gmail-masked"})), today()).unwrap();
        assert_eq!(listed["payments"].as_array().map(Vec::len), Some(1), "{listed}");
        assert_eq!(listed["payments"][0]["form"], "2551Q");
        let none = list(&db, &args(json!({"message_id": "gmail-other"})), today()).unwrap();
        assert_eq!(none["payments"], json!([]));
    }

    #[test]
    fn a_payment_is_recorded_once_per_email_and_listed() {
        let db = db();
        let receipts = scratch("email");
        let parsed = parse(&args(json!({"text": MYEG})), today()).unwrap();
        assert_eq!(with_recorded(&db, parsed.clone(), today())["recorded"], false);
        let mut call = parsed["record"].as_object().unwrap().clone();
        call.insert("message_id".into(), json!("gmail-1"));
        let first = record(&db, &call, today(), &receipts).unwrap();
        // the Payment found card now opens as already recorded
        assert_eq!(with_recorded(&db, parsed.clone(), today())["recorded"], true);
        assert_eq!(first["recorded"], true, "{first}");
        assert_eq!(first["already"], false);
        let payment = &first["payment"];
        assert_eq!(payment["key"], "123456788/2551Q-2026-Q3");
        assert_eq!(payment["tin"], TIN);
        assert_eq!(payment["amount"], 629.94);
        assert_eq!(payment["fee"], 20.0);
        assert_eq!(payment["total"], 649.94);
        assert_eq!(payment["proof"]["kind"], "email");
        assert_eq!(payment["source"], "email");
        assert_eq!(payment["taxpayer"], "Dela Cruz, Juan");
        // No return of that period is stored: the payment is kept anyway.
        assert_eq!(first["return"], Value::Null);

        let again = record(&db, &call, today(), &receipts).unwrap();
        assert_eq!(again["already"], true);
        // The same reference through another email is the same payment.
        call.insert("message_id".into(), json!("gmail-2"));
        let same = record(&db, &call, today(), &receipts).unwrap();
        assert_eq!(same["already"], true);
        // as the card sends it: a signed call's pesos are text
        let mut card = parsed["record"].as_object().unwrap().clone();
        card.retain(|_, value| !value.is_null());
        for key in ["amount", "fee"] {
            card.insert(key.into(), json!(card[key].to_string()));
        }
        assert_eq!(record(&db, &card, today(), &receipts).unwrap()["already"], true);

        let listed = list(&db, &args(json!({"tin": ROOT, "form": "2551Q", "period": "Q3 2026"})), today()).unwrap();
        assert_eq!(listed["payments"].as_array().unwrap().len(), 1, "{listed}");
        // Remove takes it off only the return it is on
        let id = listed["payments"][0]["id"].as_str().unwrap().to_string();
        let other = json!({"tin": ROOT, "form": "2551Q", "period": "2026-Q2", "id": id});
        assert!(remove(&db, &args(other), today(), &receipts).is_err());
        let gone = remove(&db, &args(json!({"tin": ROOT, "form": "2551Q", "period": "2026-Q3", "id": id})), today(), &receipts).unwrap();
        assert_eq!(gone["removed"], true);
        assert!(!recorded_for(&db, TIN, "2551Q", "2026-Q3"));
        // recorded again, as the routine would after a remove
        assert_eq!(record(&db, &call, today(), &receipts).unwrap()["already"], false);
        let none = list(&db, &args(json!({"tin": ROOT, "form": "1601C"})), today()).unwrap();
        assert_eq!(none["payments"], json!([]));
        assert!(list(&db, &args(json!({"period": "2026-Q3"})), today()).is_err());
    }

    #[test]
    fn a_receipt_shows_each_payment_with_its_email_or_its_file() {
        let db = db();
        let receipts = scratch("receipt-page");
        let ask = args(json!({"tin": ROOT, "form": "2551Q", "period": "2026-Q3"}));
        let none = receipt(&db, &ask, today()).err().expect("no payment, no receipt");
        assert!(none.contains("no payment is recorded for 2551Q · Q3 2026 of Dela Cruz, Juan"), "{none}");
        // the payments routine: the email's text and id ride in the record
        let parsed = parse(&args(json!({"text": format!("  {MYEG}\n"), "message_id": "18f0a1b2c3d4e5f6"})), today()).unwrap();
        assert_eq!(parsed["record"]["raw"], MYEG, "trimmed");
        assert_eq!(parsed["record"]["message_id"], "18f0a1b2c3d4e5f6");
        assert_eq!(capped("ab₱", 3), "ab", "cut at a character");
        record(&db, parsed["record"].as_object().unwrap(), today(), &receipts).unwrap();
        let shown = receipt(&db, &ask, today()).unwrap();
        assert_eq!(shown.title, "Receipt · 2551Q · Q3 2026 · Dela Cruz, Juan");
        assert_eq!(shown.summary["gmail_url"], "https://mail.google.com/mail/u/0/#all/18f0a1b2c3d4e5f6");
        assert_eq!(shown.summary["payments"][0]["total"], 649.94);
        assert_eq!(shown.summary["payments"][0]["proof"], "email");
        assert!(!shown.summary.to_string().contains("BUREAU"), "no email text in the summary");
        assert!(shown.html.starts_with("<!DOCTYPE html>") && !shown.html.to_ascii_lowercase().contains("<script"));
        assert!(shown.html.contains("Reference Number TESTREF") || shown.html.contains("Transaction Amount PHP 629.94"), "the email's text");
        assert!(shown.html.contains("PHP 649.94") && shown.html.contains("Recorded"));
        // a receipt file is embedded in the page
        let scan = receipts.join("bank-receipt.png");
        std::fs::write(&scan, b"\x89PNG\r\n\x1a\nnot really").unwrap();
        mark_paid(
            &db,
            &args(json!({"tin": ROOT, "form": "1601C", "period": "2026-01", "proof_path": scan.display().to_string()})),
            today(),
            &receipts,
        )
        .unwrap();
        let filed = receipt(&db, &args(json!({"tin": ROOT, "form": "1601C", "period": "2026-01"})), today()).unwrap();
        assert_eq!(filed.title, "Receipt · 1601C · Jan 2026 · Dela Cruz, Juan");
        assert!(filed.html.contains("<img src=\"data:image/png;base64,"), "the scan, embedded");
        assert_eq!(filed.summary["gmail_url"], Value::Null);
        assert_eq!(filed.summary["payments"][0]["proof"], "receipt");
    }

    #[test]
    fn marked_paid_without_proof_then_a_receipt_takes_its_place() {
        let db = db();
        let receipts = scratch("receipt");
        let marked = mark_paid(&db, &args(json!({"tin": ROOT, "form": "2551Q", "period": "2026-Q3", "note": "Paid at the bank"})), today(), &receipts).unwrap();
        assert_eq!(marked["recorded"], true);
        assert_eq!(marked["payment"]["channel"], "manual");
        assert_eq!(marked["payment"]["proof"]["kind"], "none");
        assert_eq!(marked["payment"]["amount"], Value::Null);
        let again = mark_paid(&db, &args(json!({"tin": TIN, "form": "2551Q", "period": "09/30/2026"})), today(), &receipts).unwrap();
        assert_eq!(again["already"], true);

        // A bank receipt scan, attached later.
        let scan = receipts.join("bank-receipt.png");
        std::fs::write(&scan, b"\x89PNG\r\n\x1a\nnot really").unwrap();
        let recorded = record(
            &db,
            &args(json!({
                "tin": ROOT, "form": "2551Q", "period": "2026-Q3", "channel": "LandBank",
                "reference": "LB-0001", "amount": "PHP 1,629.94", "paid_at": "10/25/2026 09:30",
                "proof_path": scan.display().to_string(),
            })),
            today(),
            &receipts.join("stored"),
        )
        .unwrap();
        assert_eq!(recorded["replaced_mark"], true, "{recorded}");
        let payment = &recorded["payment"];
        assert_eq!(payment["id"], marked["payment"]["id"]);
        assert_eq!(payment["amount"], 1629.94);
        assert_eq!(payment["note"], "Paid at the bank");
        assert_eq!(payment["paid_at"], "2026-10-25T09:30:00+08:00");
        assert_eq!(payment["source"], "receipt");
        let stored = payment["proof"]["path"].as_str().unwrap();
        assert!(stored.starts_with(receipts.join("stored").to_str().unwrap()), "{stored}");
        assert!(Path::new(stored).exists());
        assert_eq!(payment["proof"]["name"], "bank-receipt.png");
        let listed = list(&db, &args(json!({"tin": ROOT})), today()).unwrap();
        assert_eq!(listed["payments"].as_array().unwrap().len(), 1, "{listed}");

        // The Forms app's "Attach receipt" on a return paid with no proof.
        let other = mark_paid(&db, &args(json!({"tin": ROOT, "form": "1601C", "period": "2026-09"})), today(), &receipts).unwrap();
        assert_eq!(other["payment"]["proof"]["kind"], "none");
        let pdf = receipts.join("receipt.pdf");
        std::fs::write(&pdf, b"%PDF-1.4 scan").unwrap();
        let attached = mark_paid(
            &db,
            &args(json!({"tin": ROOT, "form": "1601C", "period": "2026-09", "proof_path": pdf.display().to_string()})),
            today(),
            &receipts.join("stored"),
        )
        .unwrap();
        assert_eq!(attached["already"], true);
        assert_eq!(attached["recorded"], true);
        assert_eq!(attached["payment"]["id"], other["payment"]["id"]);
        assert_eq!(attached["payment"]["proof"]["kind"], "file");
        assert_eq!(attached["payment"]["source"], "receipt");
        assert!(mark_paid(&db, &args(json!({"tin": ROOT, "form": "1601C", "period": "2026-09", "proof_path": "/no/such/receipt.txt"})), today(), &receipts).is_err());

        for bad in [
            json!({"tin": ROOT, "form": "2551Q", "period": "2026-Q3", "channel": "x", "amount": 1, "paid_at": "2026-10-25", "proof_path": "relative.png"}),
            json!({"tin": ROOT, "form": "2551Q", "period": "2026-Q3", "channel": "x", "amount": 1, "paid_at": "2026-10-25", "proof_path": "/nonexistent/receipt.png"}),
            json!({"tin": ROOT, "form": "2551Q", "period": "2026-Q3", "channel": "x", "amount": 1, "paid_at": "yesterday"}),
            json!({"tin": ROOT, "form": "2551Q", "period": "2026-Q3", "channel": "x", "paid_at": "2026-10-25"}),
            json!({"tin": "999999999", "form": "2551Q", "period": "2026-Q3", "channel": "x", "amount": 1, "paid_at": "2026-10-25"}),
        ] {
            assert!(record(&db, &args(bad.clone()), today(), &receipts).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_filing_record_holds_the_form_the_confirmation_and_the_payment() {
        let db = db();
        let profile = db.get_profile(TIN).unwrap().unwrap();
        db.save_2551q_draft(&bir_core::forms::Form2551QDraft::new_from_profile(&profile, 2026, 3))
            .unwrap();
        let receipts = scratch("record");
        let scan = receipts.join("receipt.png");
        std::fs::write(&scan, b"\x89PNG\r\n\x1a\nscan").unwrap();
        let parsed = parse(&args(json!({"text": MYEG})), today()).unwrap();
        let mut call = parsed["record"].as_object().unwrap().clone();
        call.insert("message_id".into(), json!("gmail-1"));
        call.insert("proof_path".into(), json!(scan.display().to_string()));
        record(&db, &call, today(), &receipts.join("stored")).unwrap();

        let made = filing_record(
            &db,
            &args(json!({"tin": ROOT, "form": "2551Q", "period": "2026-Q3"})),
            today(),
            &receipts,
        )
        .unwrap();
        let path = made["path"].as_str().unwrap();
        assert!(made["link"].as_str().unwrap().starts_with("rox://view?path=%2F"), "{made}");
        assert_eq!(made["status"], "Draft");
        assert_eq!(made["payments"], 1);
        let html = std::fs::read_to_string(path).unwrap();
        assert!(html.contains("BIR filing confirmation"));
        assert!(html.contains("Not filed: this return is still a draft."));
        assert!(html.contains("Proof of payment"));
        assert!(html.contains("TESTREF001"));
        assert!(html.contains("PHP 649.94"));
        assert!(html.contains("data:image/png;base64,"));
        assert!(!html.contains("<script"));

        let missing = filing_record(
            &db,
            &args(json!({"tin": ROOT, "form": "1601C", "period": "2026-09"})),
            today(),
            &receipts,
        );
        assert!(missing.unwrap_err().contains("has no 1601C return"));
    }
}
