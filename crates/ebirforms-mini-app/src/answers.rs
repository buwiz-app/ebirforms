//! The assistant's read-only reference answers (APP-8 round 2): an ATC
//! from eBIRForms' table, a late-filing penalty estimate from eBIRForms'
//! penalty engine, and the sending queue. None of them computes a tax: each
//! reads what eBIRForms answers and shapes it for a card.
//!
//! Money is whole centavos (`i64`) here. eBIRForms' penalty engine takes and
//! answers `f64` pesos, so [`penalty_estimate`] crosses that boundary once
//! each way and rounds to the centavo; every amount it answers is decimal
//! text made from centavos (`"1234.50"`).

use bir_app_core::bir_core::forms::taxform::{self, TaxPeriod};
use bir_app_core::{BirApp, QueueGate, SendStage, TinRefusal, TinStatement};
use bir_submit::TinsRead;
use chrono::NaiveDate;
use serde_json::{Map, Value, json};

use crate::tickets::{self, Subject};

/// The longest list a lookup answers.
const MATCHES_MAX: usize = 20;
/// How long a sent return stays on the queue card.
const SENT_DAYS: i64 = 30;
/// A sent return with no receipt: how it was sent is not known.
const NO_RECEIPT: &str = "no receipt, so not known whether it was a dry run";

fn reject_unknown(arguments: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    match arguments
        .keys()
        .find(|key| !allowed.contains(&key.as_str()))
    {
        Some(key) if allowed.is_empty() => {
            Err(format!("this tool takes no arguments (got `{key}`)"))
        }
        Some(key) => Err(format!(
            "unknown argument `{key}` (allowed: {})",
            allowed.join(", ")
        )),
        None => Ok(()),
    }
}

fn text_arg(arguments: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(Some(text.trim().to_string())),
        Some(Value::String(_)) => Ok(None),
        Some(other) => Err(format!("`{key}` must be a string (got {other})")),
    }
}

fn date_arg(arguments: &Map<String, Value>, key: &str) -> Result<Option<NaiveDate>, String> {
    text_arg(arguments, key)?
        .map(|raw| {
            NaiveDate::parse_from_str(&raw, "%Y-%m-%d")
                .map_err(|_| format!("`{key}` must be a date like 2026-10-31 (got `{raw}`)"))
        })
        .transpose()
}

// ------------------------------------------------------------------ ATC

/// One ATC table rate as a percentage: `"5.0"` is `5%`. A blank, an excise
/// comment (`<!--P10.41/Proof liter-->`) or any other text is `None`: the
/// card says to check the form, never the raw value.
pub(crate) fn atc_percent(raw: &str) -> Option<String> {
    let text = raw.trim();
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let number = !whole.is_empty()
        && whole.chars().all(|c| c.is_ascii_digit())
        && fraction.chars().all(|c| c.is_ascii_digit())
        && !(text.contains('.') && fraction.is_empty());
    if !number {
        return None;
    }
    let trimmed = if fraction.is_empty() {
        text
    } else {
        text.trim_end_matches('0').trim_end_matches('.')
    };
    Some(format!("{trimmed}%"))
}

/// What a card shows for a code's rates (every distinct one the table has):
/// one number as a percentage; otherwise `None`, and `varies` when the
/// table lists more than one.
pub(crate) fn atc_rate(rates: &[String]) -> (Option<String>, bool) {
    match rates {
        [one] => (atc_percent(one), false),
        [] => (None, false),
        _ => (None, true),
    }
}

/// BIR's all-caps descriptions in sentence case.
fn sentence(text: &str) -> String {
    let squeezed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !squeezed
        .chars()
        .filter(|c| c.is_alphabetic())
        .all(char::is_uppercase)
    {
        return squeezed;
    }
    let lower = squeezed.to_lowercase();
    let mut chars = lower.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

/// `ebirforms_atc_lookup {query}`: the ATCs whose code or description
/// matches, the exact code first, each with the rate eBIRForms' table gives.
pub(crate) fn atc_lookup(arguments: &Map<String, Value>) -> Result<Value, String> {
    reject_unknown(arguments, &["query"])?;
    let query = text_arg(arguments, "query")?
        .ok_or("`query` is required: an ATC (WV010, WC158) or part of its description")?;
    let exact = BirApp::atc(&query);
    let mut matches = match &exact {
        Some(found) => vec![found.clone()],
        None => BirApp::search_atcs(&query),
    };
    if matches.is_empty() {
        return Err(format!(
            "no ATC matches `{}`",
            query.chars().take(40).collect::<String>()
        ));
    }
    let more = matches.len() > MATCHES_MAX;
    matches.truncate(MATCHES_MAX);
    Ok(json!({
        "mode": "atc",
        "query": query,
        "matches": matches.iter().map(|atc| {
            let description = atc
                .title
                .split_once(" · ")
                .map_or(String::new(), |(_, description)| sentence(description));
            let (rate, varies) = atc_rate(&BirApp::atc_rates(&atc.value));
            json!({ "code": atc.value, "description": description, "rate": rate, "rate_varies": varies })
        }).collect::<Vec<_>>(),
        "more": more,
        "note": "Rates are eBIRForms' ATC table's, as a percentage. A null rate means check the form: the table has no plain rate for the code, or (rate_varies) lists more than one rate for it. Tell the user to check the rate on the form before using it.",
    }))
}

// -------------------------------------------------------------- penalty

/// Peso text (`12,500`, `₱1234.5`, `0.75`) as whole centavos: no floats,
/// at most two decimals, never negative.
pub(crate) fn centavos(raw: &str) -> Result<i64, String> {
    let bad = || format!("`tax_due` must be an amount in pesos like 12500.00 (got `{raw}`)");
    let clean: String = raw
        .trim()
        .trim_start_matches('₱')
        .trim_start_matches("PHP")
        .chars()
        .filter(|c| *c != ',' && !c.is_whitespace())
        .collect();
    let (whole, fraction) = clean.split_once('.').unwrap_or((&clean, ""));
    if whole.is_empty()
        || fraction.len() > 2
        || !whole.chars().all(|c| c.is_ascii_digit())
        || !fraction.chars().all(|c| c.is_ascii_digit())
    {
        return Err(bad());
    }
    let pesos: i64 = whole.parse().map_err(|_| bad())?;
    let cents: i64 = format!("{fraction:0<2}").parse().map_err(|_| bad())?;
    pesos
        .checked_mul(100)
        .and_then(|c| c.checked_add(cents))
        .ok_or_else(bad)
}

/// Whole centavos as decimal text: `123450` is `"1234.50"`.
pub(crate) fn pesos(centavos: i64) -> String {
    let sign = if centavos < 0 { "-" } else { "" };
    let abs = centavos.unsigned_abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

/// An amount eBIRForms' engine answered, in pesos, to the nearest centavo.
#[allow(clippy::cast_possible_truncation)]
fn to_centavos(pesos: f64) -> i64 {
    (pesos * 100.0).round() as i64
}

/// The form code in `form`: a code (2551Q), or a return's canonical value
/// (`<tin>/<FORM>-<period>`, what a return's ticket expands to).
fn form_code(raw: &str, today: NaiveDate) -> Option<&'static str> {
    let text = raw.rsplit_once('/').map_or(raw, |(_, rest)| rest);
    taxform::parse(text, today)
        .form
        .and_then(taxform::form_code_of)
        .or_else(|| taxform::form_code_of(text))
}

/// `ebirforms_penalty_estimate {form, due, tax_due?, filed_on?}`: what
/// filing a return late costs, from eBIRForms' penalty engine.
pub(crate) fn penalty_estimate(
    arguments: &Map<String, Value>,
    today: NaiveDate,
) -> Result<Value, String> {
    reject_unknown(arguments, &["form", "due", "tax_due", "filed_on"])?;
    let raw = text_arg(arguments, "form")?
        .ok_or("`form` is required: a form code (2551Q) or a return's ticket")?;
    let form = form_code(&raw, today).ok_or_else(|| {
        format!(
            "`{}` is not a BIR form",
            raw.chars().take(20).collect::<String>()
        )
    })?;
    let due = date_arg(arguments, "due")?
        .ok_or("`due` is required: the return's due date, like 2026-10-25")?;
    let filed_on = date_arg(arguments, "filed_on")?.unwrap_or(today);
    let tax_due = match arguments.get("tax_due") {
        None | Some(Value::Null) => 0,
        Some(Value::String(text)) if text.trim().is_empty() => 0,
        Some(Value::String(text)) => centavos(text)?,
        Some(Value::Number(number)) => number
            .as_i64()
            .filter(|pesos| *pesos >= 0)
            .and_then(|pesos| pesos.checked_mul(100))
            .ok_or("`tax_due` as a number must be whole pesos; send cents as text (12500.50)")?,
        Some(other) => {
            return Err(format!(
                "`tax_due` must be an amount in pesos (got {other})"
            ));
        }
    };
    let day = |date: NaiveDate| date.format("%Y-%m-%d").to_string();
    let mut out = json!({
        "mode": "penalty",
        "form": form,
        "due": day(due),
        "filed_on": day(filed_on),
        "tax_due": pesos(tax_due),
        // eBIRForms' estimate takes no taxpayer size: its engine uses the
        // regular rates, not the EOPT Act's lower ones for micro and small
        "rates": "regular",
        "basis": "eBIRForms' penalty engine at the regular rates (a medium or large taxpayer): surcharge on the tax due, interest on the unpaid tax and BIR's compromise. A micro or small taxpayer pays less under the EOPT Act (RA 11976). An estimate; BIR assesses the final amount.",
    });
    // f64 only at eBIRForms' boundary: centavos in, centavos out
    #[allow(clippy::cast_precision_loss)]
    let estimate = BirApp::penalty_estimate(form, due, filed_on, tax_due as f64 / 100.0);
    let Some(estimate) = estimate else {
        out["late"] = json!(false);
        out["days_late"] = json!(0);
        out["note"] = json!("Filed by the due date: no penalty.");
        return Ok(out);
    };
    let surcharge = to_centavos(estimate.surcharge);
    let interest = to_centavos(estimate.interest);
    let compromise = to_centavos(estimate.compromise);
    let penalties = surcharge + interest + compromise;
    out["late"] = json!(true);
    out["days_late"] = json!(estimate.days_late);
    out["surcharge"] = json!(pesos(surcharge));
    out["interest"] = json!(pesos(interest));
    out["compromise"] = json!(pesos(compromise));
    out["total_penalties"] = json!(pesos(penalties));
    out["total_to_pay"] = json!(pesos(tax_due + penalties));
    out["reply"] = json!(
        "The user sees every amount on the card. Reply in one or two sentences: how many days late, the total to pay, and that it is an estimate at the regular rates (less for a micro or small taxpayer). Do not list or tabulate the amounts."
    );
    Ok(out)
}

// ---------------------------------------------------------------- queue

fn queue_period(year: i64, quarter: Option<i64>, month: Option<i64>) -> Option<TaxPeriod> {
    let year = u16::try_from(year).ok()?;
    Some(match (month, quarter) {
        (Some(month), _) if (1..=12).contains(&month) => TaxPeriod::Month {
            year,
            month: u8::try_from(month).ok()?,
        },
        (_, Some(quarter)) if (1..=4).contains(&quarter) => TaxPeriod::Quarter {
            year,
            quarter: u8::try_from(quarter).ok()?,
        },
        _ => TaxPeriod::Year(year),
    })
}

/// The account's TIN list, read at most once and only when a return's row
/// needs it (`ebirforms_queue_list` of a plain queue asks Buwiz nothing).
pub(crate) struct Live<'a> {
    read: &'a dyn Fn() -> TinsRead,
    cached: std::cell::OnceCell<TinsRead>,
    now: i64,
}

impl<'a> Live<'a> {
    pub(crate) fn new(read: &'a dyn Fn() -> TinsRead, now: i64) -> Self {
        Self {
            read,
            cached: std::cell::OnceCell::new(),
            now,
        }
    }

    /// The answer of a return that is no TIN refusal: no state, nothing read.
    fn none(&self) -> QueueGate {
        BirApp::tin_gate("", &[], self.now)
    }

    /// What the list says now about the return's TIN: unknown when it could
    /// not be read, and then nothing hides or replaces a stored refusal.
    fn gate(&self, tin: &str) -> QueueGate {
        let read = self.cached.get_or_init(|| (self.read)());
        BirApp::tin_gate(tin, std::slice::from_ref(read), self.now)
    }
}

/// Add a TIN statement's words to a card row: the chip, the one line and
/// the step's button. Never the staff note, and never the stored reason: an
/// assistant reads this, and only the fixed sentences may reach it.
fn tin_words(statement: &TinStatement, out: &mut Value) {
    crate::tins::assistant_fields(statement, out);
}

/// The words of a stored TIN refusal (a refused draft, or a queued return
/// that failed with a TIN code), without the server's text.
fn stored_refusal(stage: &SendStage) -> Option<TinStatement> {
    let code = match stage {
        SendStage::Refused { code, .. } => *code,
        SendStage::Failed { reason, .. } => TinRefusal::from_reason(reason)?,
        _ => return None,
    };
    Some(BirApp::tin_statement_for_refusal(code, None))
}

/// A return's stage as the card reads it. `gate`: what the account's list
/// says now about the return's TIN. `false`: the row is not shown (a
/// refusal the list says is fixed since).
fn stage(stage: Option<&SendStage>, gate: &QueueGate, out: &mut Value) -> bool {
    // A refusal for the TIN (stored), as the account's list says it stands
    // now. The stored words are the state's own sentence: the server's text
    // in the stored reason is not copied, so the fixed reasons of a sent-back
    // TIN come from the list's row or not at all.
    if let Some(stored) = stage.and_then(stored_refusal) {
        let shown = BirApp::effective_statement(Some(stored), gate);
        return match (stage, shown) {
            (Some(SendStage::Refused { .. }), Some(statement)) => {
                out["stage"] = json!("refused");
                out["draft_note"] = json!(BirApp::DRAFT_NOTE);
                tin_words(&statement, out);
                true
            }
            (Some(SendStage::Refused { .. }), None) => false,
            // a queued return that failed with a TIN code is asked again at once
            (_, Some(statement)) => {
                out["stage"] = json!("failed");
                tin_words(&statement, out);
                true
            }
            (_, None) => {
                out["stage"] = json!("queued");
                true
            }
        };
    }
    let name = match stage {
        None | Some(SendStage::Queued { .. }) => "queued",
        Some(SendStage::Sending) => "sending",
        Some(SendStage::Failed { .. }) => "failed",
        Some(SendStage::NeedsAttention { .. }) => "needs_attention",
        Some(SendStage::GaveUp { .. }) => "gave_up",
        Some(SendStage::Refused { .. }) => "refused",
        Some(SendStage::Sent { .. }) => "sent",
        Some(SendStage::Confirmed { .. }) => "confirmed",
        Some(SendStage::Paid) => "paid",
    };
    out["stage"] = json!(name);
    match stage {
        Some(SendStage::Queued { next_retry_at }) => {
            out["next_try"] = json!(next_retry_at);
        }
        Some(SendStage::Failed {
            reason,
            attempts,
            next_retry_at,
        }) => {
            out["reason"] = json!(reason);
            out["attempts"] = json!(attempts);
            out["next_try"] = json!(next_retry_at);
        }
        Some(SendStage::NeedsAttention { reason } | SendStage::GaveUp { reason }) => {
            out["reason"] = json!(reason);
        }
        Some(SendStage::Sent { at, receipt }) => {
            out["sent_at"] = json!(at);
            out["receipt"] = json!(receipt.is_some());
            // only a receipt says how it was sent: without one, `dry_run`
            // stays unset rather than reading as a live send
            match receipt {
                Some(receipt) => out["dry_run"] = json!(receipt.is_test()),
                None => out["receipt_note"] = json!(NO_RECEIPT),
            }
        }
        _ => {}
    }
    true
}

/// `ebirforms_queue_list {}`: the returns waiting to be sent to the BIR,
/// those sent in the last 30 days, and those Buwiz refused for their TIN
/// (back to a draft), each named by its ticket (`key`). A refused return
/// carries the state's chip and line and its step's button, from eBIRForms'
/// wording table and nothing else of the server's text: this answer is the
/// assistant's, and staff's free-text note never reaches it.
pub(crate) fn queue_list(
    app: &BirApp,
    arguments: &Map<String, Value>,
    today: NaiveDate,
    signed_in: bool,
    live: &Live,
) -> Result<Value, String> {
    reject_unknown(arguments, &[])?;
    let queue = app.list_queue().map_err(|error| error.to_string())?;
    let stages = app.send_states().map_err(|error| error.to_string())?;
    let since = (today - chrono::Duration::days(SENT_DAYS))
        .format("%Y-%m-%d")
        .to_string();
    let rows: Vec<_> = queue
        .into_iter()
        .filter(|row| row.status == "Queued" || row.updated_at.as_str() >= since.as_str())
        .filter_map(|row| {
            let form = taxform::form_code_of(&row.form_code)?;
            let period = queue_period(row.taxable_year, row.quarter, row.month)?;
            Some((row, form, period))
        })
        .collect();
    let subjects: Vec<Option<Subject>> = rows
        .iter()
        .map(|(row, form, period)| {
            Some(Subject::Return(taxform::canonical_value(
                &row.tin, form, *period,
            )))
        })
        .collect();
    let keys = tickets::issue_all(app, &subjects)?;
    let (mut waiting, mut sent) = (Vec::new(), Vec::new());
    for ((row, form, period), key) in rows.into_iter().zip(keys) {
        let taxpayer = Subject::of_tin(&row.tin)
            .and_then(|subject| Subject::Taxpayer(subject.root()).title(app, today))
            .unwrap_or_default();
        let mut out = json!({
            "key": key.unwrap_or_default(),
            "taxpayer": taxpayer,
            "form": form,
            "period": period.canonical(),
            "label": format!("{form} · {}", period.label()),
            "since": row.updated_at,
        });
        let sent_stage = stages.get(&row.id);
        let tin_refusal = sent_stage.is_some_and(|stage| stored_refusal(stage).is_some());
        let gate = if tin_refusal {
            live.gate(&row.tin)
        } else {
            live.none()
        };
        stage(sent_stage, &gate, &mut out);
        if row.status == "Queued" {
            waiting.push(out);
        } else {
            sent.push(out);
        }
    }
    let back_to_draft = back_to_draft(app, &since, today, live)?;
    Ok(json!({
        "mode": "queue",
        "sending": signed_in,
        "waiting": waiting,
        "back_to_draft": back_to_draft,
        "sent": sent,
        "note": if signed_in {
            "waiting: queued in Forms, sent to the BIR in the background. back_to_draft: Buwiz refused the return for its TIN, nothing was sent, and it is a draft again: say the row's line and what its step button does, and do not suggest sending it again or retrying. sent: sent in the last 30 days. dry_run comes from the return's receipt: true means the server only simulated the send. A sent return with no receipt has no dry_run: say it is not known whether it was a dry run, never that it was sent for real, and never suggest sending it again (the BIR may already have it; the user checks in the Forms app). The user queues and cancels in the Forms app."
        } else {
            "This computer is not signed in to Buwiz, so nothing is sent: the user signs in in the Sign-in app (ebirforms_app_open `register`). back_to_draft: Buwiz refused the return for its TIN, nothing was sent, and it is a draft again: say the row's line, and do not suggest retrying. The user queues and cancels in the Forms app."
        },
    }))
}

/// The returns Buwiz refused for their TIN, now drafts, from the last 30
/// days: each by ticket with the chip, line and step of the state the
/// account says now (a return whose TIN is active since is left out).
fn back_to_draft(
    app: &BirApp,
    since: &str,
    today: NaiveDate,
    live: &Live,
) -> Result<Vec<Value>, String> {
    let refused: Vec<_> = app
        .refused_returns()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|refused| refused.row.updated_at.as_str() >= since)
        .filter_map(|refused| {
            let form = taxform::form_code_of(&refused.row.form_code)?;
            let period = refused_period(&refused.row)?;
            Some((refused, form, period))
        })
        .collect();
    let subjects: Vec<Option<Subject>> = refused
        .iter()
        .map(|(refused, form, period)| {
            Some(Subject::Return(taxform::canonical_value(
                &refused.row.tin,
                form,
                *period,
            )))
        })
        .collect();
    let keys = tickets::issue_all(app, &subjects)?;
    let mut rows = Vec::new();
    for ((refused, form, period), key) in refused.into_iter().zip(keys) {
        let taxpayer = Subject::of_tin(&refused.row.tin)
            .and_then(|subject| Subject::Taxpayer(subject.root()).title(app, today))
            .unwrap_or_default();
        let mut out = json!({
            "key": key.unwrap_or_default(),
            "taxpayer": taxpayer,
            "form": form,
            "period": period.canonical(),
            "label": format!("{form} · {}", period.label()),
            "since": refused.row.updated_at,
        });
        if stage(Some(&refused.stage), &live.gate(&refused.row.tin), &mut out) {
            rows.push(out);
        }
    }
    Ok(rows)
}

/// The period of a listed return, as its form is filed.
fn refused_period(row: &bir_app_core::ReturnRow) -> Option<TaxPeriod> {
    let year = u16::try_from(row.taxable_year).ok()?;
    let index = u8::try_from(row.period).ok()?;
    Some(match taxform::cadence(&row.form_code)? {
        taxform::Cadence::Monthly => TaxPeriod::Month { year, month: index },
        taxform::Cadence::Quarterly => TaxPeriod::Quarter {
            year,
            quarter: index,
        },
        taxform::Cadence::Annual | taxform::Cadence::AsNeeded => TaxPeriod::Year(year),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_number_reads_as_a_rate() {
        assert_eq!(atc_percent("5.0").as_deref(), Some("5%"));
        assert_eq!(atc_percent("0.50").as_deref(), Some("0.5%"));
        assert_eq!(atc_percent("15").as_deref(), Some("15%"));
        assert_eq!(atc_percent(" 10.25 ").as_deref(), Some("10.25%"));
        for text in [
            "",
            "  ",
            "<!--P10.41/Proof liter-->",
            "<!-- -->",
            "5.",
            ".5",
            "1.2.3",
            "P10",
        ] {
            assert_eq!(atc_percent(text), None, "{text:?}");
        }
        let rates = |list: &[&str]| list.iter().map(|r| (*r).to_string()).collect::<Vec<_>>();
        assert_eq!(atc_rate(&rates(&["5.0"])), (Some("5%".into()), false));
        assert_eq!(atc_rate(&rates(&["10.0", "5.0"])), (None, true));
        assert_eq!(atc_rate(&rates(&[""])), (None, false));
        assert_eq!(
            atc_rate(&rates(&["<!--P10.41/Proof liter-->"])),
            (None, false)
        );
        assert_eq!(atc_rate(&[]), (None, false));
    }

    #[test]
    fn no_raw_table_text_reaches_an_answer() {
        for atc in BirApp::atcs() {
            let found = atc_lookup(&args(json!({"query": atc.value}))).unwrap();
            let text = found.to_string();
            assert!(
                !text.contains("<!--") && !text.contains("-->"),
                "{}: {text}",
                atc.value
            );
            for found in found["matches"].as_array().unwrap() {
                let rate = &found["rate"];
                assert!(
                    rate.is_null() || rate.as_str().is_some_and(|r| r.ends_with('%')),
                    "{}",
                    atc.value
                );
            }
        }
    }

    #[test]
    fn peso_text_is_whole_centavos() {
        assert_eq!(centavos("12,500"), Ok(1_250_000));
        assert_eq!(centavos("₱1234.5"), Ok(123_450));
        assert_eq!(centavos("0.75"), Ok(75));
        assert_eq!(centavos("PHP 10.05"), Ok(1005));
        for bad in ["-5", "1.234", "abc", ".5", "1e3", ""] {
            assert!(centavos(bad).is_err(), "{bad}");
        }
        assert_eq!(pesos(123_450), "1234.50");
        assert_eq!(pesos(5), "0.05");
        assert_eq!(pesos(0), "0.00");
    }

    #[test]
    fn a_tax_due_round_trips_to_the_exact_centavo() {
        assert_eq!(centavos("1,234.56"), Ok(123_456));
        assert_eq!(pesos(123_456), "1234.56");
        let today = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        for (sent, echoed) in [
            ("1,234.56", "1234.56"),
            ("0.07", "0.07"),
            ("9,999,999.99", "9999999.99"),
        ] {
            let late = penalty_estimate(
                &args(json!({"form": "1601C", "due": "2026-08-10", "tax_due": sent})),
                today,
            )
            .unwrap();
            assert_eq!(late["tax_due"], echoed);
            let cents = |key: &str| centavos(late[key].as_str().unwrap()).unwrap();
            assert_eq!(
                cents("total_to_pay"),
                centavos(sent).unwrap() + cents("total_penalties")
            );
        }
    }

    #[test]
    fn a_form_code_or_a_returns_value_names_the_form() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        assert_eq!(form_code("2551q", today), Some("2551Q"));
        assert_eq!(form_code("000123453/1601C-2026-09", today), Some("1601C"));
        assert_eq!(form_code("hello", today), None);
    }

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn a_late_return_costs_what_the_engine_says_in_centavos() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        let late = penalty_estimate(
            &args(json!({"form": "2551Q", "due": "2026-07-25", "tax_due": "10,000"})),
            today,
        )
        .unwrap();
        assert_eq!(late["late"], true);
        assert_eq!(late["days_late"], 99);
        assert_eq!(late["rates"], "regular");
        assert!(late["basis"].as_str().unwrap().contains("micro or small"));
        let cents = |key: &str| centavos(late[key].as_str().unwrap()).unwrap();
        assert_eq!(
            cents("total_penalties"),
            cents("surcharge") + cents("interest") + cents("compromise")
        );
        assert_eq!(cents("total_to_pay"), 1_000_000 + cents("total_penalties"));
        assert!(cents("surcharge") > 0);

        let on_time =
            penalty_estimate(&args(json!({"form": "2551Q", "due": "2026-11-25"})), today).unwrap();
        assert_eq!(on_time["late"], false);
        assert!(on_time.get("surcharge").is_none());

        for bad in [
            json!({"due": "2026-07-25"}),
            json!({"form": "2551Q"}),
            json!({"form": "2551Q", "due": "25/07/2026"}),
            json!({"form": "2551Q", "due": "2026-07-25", "tax_due": 12.5}),
            json!({"form": "2551Q", "due": "2026-07-25", "rate": "1"}),
        ] {
            assert!(
                penalty_estimate(&args(bad.clone()), today).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_code_the_table_lists_at_two_rates_reads_as_varies() {
        let wi010 = atc_lookup(&args(json!({"query": "WI010"}))).unwrap();
        assert_eq!(wi010["matches"][0]["rate"], Value::Null, "{wi010}");
        assert_eq!(wi010["matches"][0]["rate_varies"], true);
        // listed twice at the same rate: one rate
        let wv010 = atc_lookup(&args(json!({"query": "WV010"}))).unwrap();
        assert_eq!(wv010["matches"][0]["rate"], "5%", "{wv010}");
        assert_eq!(wv010["matches"][0]["rate_varies"], false);
        let varies = BirApp::atcs()
            .iter()
            .filter(|atc| atc_rate(&BirApp::atc_rates(&atc.value)).1)
            .count();
        assert_eq!(varies, 96);
    }

    #[test]
    fn a_sent_return_without_a_receipt_has_no_dry_run() {
        let at = Some("2026-11-01T02:50:44Z".to_string());
        let mut bare = json!({});
        stage(
            Some(&SendStage::Sent {
                at: at.clone(),
                receipt: None,
            }),
            &no_state(),
            &mut bare,
        );
        assert_eq!(bare["stage"], "sent");
        assert_eq!(bare["receipt"], false);
        assert!(bare.get("dry_run").is_none(), "{bare}");
        assert_eq!(bare["receipt_note"], NO_RECEIPT);
        for (mode, dry_run) in [("dry_run", true), ("live", false)] {
            let receipt = bir_app_core::ReceiptInfo {
                jti: None,
                mode: mode.into(),
                iat: None,
            };
            let mut row = json!({});
            stage(
                Some(&SendStage::Sent {
                    at: at.clone(),
                    receipt: Some(receipt),
                }),
                &no_state(),
                &mut row,
            );
            assert_eq!(row["receipt"], true);
            assert_eq!(row["dry_run"], dry_run, "{mode}");
            assert!(row.get("receipt_note").is_none());
        }
    }

    // ------------------------------------------------ a TIN's state on a row

    use bir_submit::AccountTins;

    /// The synthetic TIN of the repo's fixtures.
    const TIN: &str = "000123453";

    /// A gate that knows nothing: the list could not be read.
    fn no_state() -> QueueGate {
        BirApp::tin_gate("", &[], 0)
    }

    /// The gate for [`TIN`] from an account list with this one row.
    fn gate_of(row: Value) -> QueueGate {
        let list = AccountTins::parse(&json!({ "include": "all", "profiles": [row] }), 1_000);
        BirApp::tin_gate(TIN, &[Ok(list)], 1_000)
    }

    fn account_row(status: &str, can_send: bool, extra: Value) -> Value {
        let mut row = json!({
            "tin": "000-123-453-00000", "registered_name": "SYNTHETIC TAXPAYER ONE",
            "status": status, "can_send": can_send,
        });
        row.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        row
    }

    fn refused(code: TinRefusal, message: &str) -> SendStage {
        SendStage::Refused {
            code,
            reason: format!(
                "Not sent (nothing reached BIR): {message} ({})",
                code.code()
            ),
        }
    }

    #[test]
    fn a_refused_return_reads_its_state_line_and_step() {
        let draft_note = "Back to a draft. Nothing was sent.";
        let cases = [
            (
                TinRefusal::NotActive,
                "not_on_account",
                "Not on your Buwiz account",
                "This TIN is not on your Buwiz account.",
                Some(("add", "Add it")),
            ),
            (
                TinRefusal::Unverified,
                "not_verified",
                "Not verified",
                "This TIN is on your Buwiz account but not verified yet.",
                Some(("verify", "Verify it")),
            ),
            (
                TinRefusal::Pending,
                "waiting_for_review",
                "Waiting for review",
                "Buwiz is reviewing this TIN. You can send it once it is approved.",
                None,
            ),
            (
                TinRefusal::NeedsFix,
                "sent_back",
                "Sent back",
                "Buwiz sent this TIN back for a fix.",
                Some(("fix", "Fix it")),
            ),
            (
                TinRefusal::Paused,
                "paused",
                "Paused",
                "Buwiz staff paused this TIN. It can't send until they turn it back on.",
                None,
            ),
            (
                TinRefusal::AccountBanned,
                "account_banned",
                "Account banned",
                "This Buwiz account is banned, so nothing can be sent.",
                None,
            ),
        ];
        for (code, state, chip, line, step) in cases {
            let message = if code == TinRefusal::NeedsFix {
                "Sent back for a fix: A photo is too blurry to read."
            } else {
                "the server's own sentence"
            };
            let mut row = json!({});
            assert!(stage(Some(&refused(code, message)), &no_state(), &mut row));
            assert_eq!(row["stage"], "refused", "{code:?}");
            assert_eq!(row["state"], state);
            assert_eq!(row["chip"], chip);
            assert_eq!(row["line"], line);
            assert_eq!(row["draft_note"], draft_note);
            match step {
                Some((kind, label)) => {
                    assert_eq!(row["step"], json!({ "kind": kind, "label": label }))
                }
                None => assert_eq!(row["step"], Value::Null),
            }
            assert!(row.get("reason").is_none(), "no stored text: {row}");
        }
        // the seventh state is the list's alone: an approved TIN this login may not send
        let gate = gate_of(account_row("approved", false, json!({})));
        let mut row = json!({});
        assert!(stage(
            Some(&refused(TinRefusal::NotActive, "x")),
            &gate,
            &mut row
        ));
        assert_eq!(row["state"], "not_allowed");
        assert_eq!(row["chip"], "Can't send");
        assert_eq!(
            row["line"],
            "This Buwiz account is not allowed to send this TIN."
        );
        assert_eq!(row["step"], Value::Null);
    }

    #[test]
    fn a_legacy_failed_row_with_a_tin_code_reads_the_line_not_the_retry_text() {
        let failed = |reason: &str| SendStage::Failed {
            reason: reason.into(),
            attempts: 2,
            next_retry_at: Some("2099-01-01T00:00:00Z".into()),
        };
        let mut row = json!({});
        stage(
            Some(&failed(
                "Not sent (nothing reached BIR): this TIN is not active on your account (tin_not_active)",
            )),
            &no_state(),
            &mut row,
        );
        assert_eq!(row["stage"], "failed");
        assert_eq!(row["chip"], "Not on your Buwiz account");
        assert_eq!(row["line"], "This TIN is not on your Buwiz account.");
        for key in ["reason", "attempts", "next_try", "draft_note"] {
            assert!(row.get(key).is_none(), "{key}: {row}");
        }
        // a failure that is no TIN refusal keeps today's reading
        let mut plain = json!({});
        stage(
            Some(&failed(
                "Not sent (nothing reached BIR): the network is down",
            )),
            &no_state(),
            &mut plain,
        );
        assert_eq!(plain["stage"], "failed");
        assert_eq!(
            plain["reason"],
            "Not sent (nothing reached BIR): the network is down"
        );
        assert_eq!(plain["attempts"], 2);
        assert_eq!(plain["next_try"], "2099-01-01T00:00:00Z");
        assert!(plain.get("chip").is_none(), "{plain}");
    }

    #[test]
    fn a_refused_row_is_hidden_when_the_live_state_is_active() {
        let active = gate_of(account_row("approved", true, json!({})));
        let mut row = json!({});
        assert!(!stage(
            Some(&refused(TinRefusal::Unverified, "x")),
            &active,
            &mut row
        ));
        // a queued return that failed with a TIN code is asked again at once
        let mut waiting = json!({});
        assert!(stage(
            Some(&SendStage::Failed {
                reason: "Not sent (nothing reached BIR): x (tin_unverified)".into(),
                attempts: 1,
                next_retry_at: Some("2099-01-01T00:00:00Z".into()),
            }),
            &active,
            &mut waiting
        ));
        assert_eq!(waiting["stage"], "queued");
        assert!(waiting.get("chip").is_none() && waiting.get("reason").is_none());
    }

    #[test]
    fn the_live_state_wins_over_the_stored_refusal() {
        let live = gate_of(account_row("paused", false, json!({})));
        let mut row = json!({});
        assert!(stage(
            Some(&refused(TinRefusal::NotActive, "x")),
            &live,
            &mut row
        ));
        assert_eq!(row["chip"], "Paused");
        assert_eq!(row["state"], "paused");
    }

    #[test]
    fn staff_text_never_reaches_a_row() {
        let note = "Retake it in daylight, JUANA DELA CRUZ";
        let live = gate_of(account_row(
            "needs_fix",
            false,
            json!({ "reasons": ["A photo is too blurry to read."], "note": note }),
        ));
        // an old stored message that carries a staff sentence
        let stored = refused(
            TinRefusal::NeedsFix,
            "Sent back for a fix: A photo is too blurry to read. Staff: do not tell JUANA",
        );
        for gate in [&live, &no_state()] {
            let mut row = json!({});
            stage(Some(&stored), gate, &mut row);
            let text = row.to_string();
            assert!(
                !text.contains("JUANA") && !text.contains("daylight"),
                "{text}"
            );
            assert!(!text.contains("Staff:"), "{text}");
            assert!(
                row.get("note").is_none() && row.get("next").is_none(),
                "{text}"
            );
        }
        // with the list's row, the line holds its fixed reasons only
        let mut row = json!({});
        stage(Some(&stored), &live, &mut row);
        assert_eq!(
            row["line"],
            "Buwiz sent this TIN back for a fix: A photo is too blurry to read."
        );
    }

    #[test]
    fn an_atc_lookup_finds_the_code_first() {
        let exact = atc_lookup(&args(json!({"query": "wv010"}))).unwrap();
        let matches = exact["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["code"], "WV010");
        assert!(atc_lookup(&args(json!({"query": "zzzz-no-such"}))).is_err());
        assert!(atc_lookup(&args(json!({}))).is_err());
    }
}
