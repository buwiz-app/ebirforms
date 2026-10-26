//! BIR's confirmation emails, read by the agent instead of IMAP.
//!
//! A routine on the Rox server searches the user's Gmail with the query
//! `bir_confirmation_search` builds and wakes the agent on new mail; the
//! agent reads each email and calls `bir_confirm_filing`. That confirms the
//! return the same way the IMAP poller does
//! ([`Database::save_submission_receipt_with_email_date`] then
//! [`Database::confirm_submission_from_receipt`]): BIR's received time must
//! follow this filing's queueing, and only a filed (Submitted) return moves
//! to Confirmed.

use bir_core::db::{Database, ReceiptConfirmationOutcome};
use bir_core::forms::FilingStatus;
use bir_core::forms::taxform::{self, TaxPeriod};
use bir_core::receipt::{BirReceiptConfirmation, split_bir_filename};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use serde_json::{Map, Value, json};

use super::resolve;

/// The sender of eBIRForms confirmations (as the IMAP poller searches).
const BIR_SENDER: &str = "ebirforms-noreply@bir.gov.ph";
/// How far back a search looks, as the IMAP poller does.
const SEARCH_DAYS: i64 = 30;
/// Settings key prefix: a confirmed email's answer, by its message id.
const CONFIRMED_PREFIX: &str = "mini_app.confirmed_email.";

pub(crate) fn check_keys(arguments: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    match arguments
        .keys()
        .find(|key| !allowed.contains(&key.as_str()))
    {
        Some(key) => Err(format!(
            "unknown argument `{key}` (allowed: {})",
            allowed.join(", ")
        )),
        None => Ok(()),
    }
}

pub(crate) fn text<'a>(arguments: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    match arguments.get(key) {
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(text.trim()),
        _ => Err(format!("`{key}` is required (a string)")),
    }
}

/// The ways BIR's file name carries a branch's TIN: all 14 digits, and the
/// older 12 (root and the last three of the branch code).
fn tin_spellings(tin: &str) -> Vec<String> {
    let mut out = vec![tin.to_string()];
    if tin.len() == 14 && tin[9..11] == *"00" {
        out.push(format!("{}{}", &tin[..9], &tin[11..]));
    }
    out
}

/// Returns of these branches sent to BIR and not confirmed yet.
fn awaiting(db: &Database, branches: &[resolve::Branch], today: NaiveDate) -> Vec<Value> {
    let mut out = Vec::new();
    for branch in branches {
        for kind in bir_core::forms::agent::AgentFormKind::all() {
            let code = kind.form_code();
            for (period, summary) in resolve::stored_returns(db, &branch.tin, code, today) {
                if !matches!(summary.status, FilingStatus::Queued | FilingStatus::Submitted) {
                    continue;
                }
                out.push(json!({
                    "tin": branch.tin,
                    "taxpayer": branch.name,
                    "form": code,
                    "period": period.canonical(),
                    "status": if summary.status == FilingStatus::Queued { "queued" } else { "filed" },
                    "token": format!("{}{}", taxform::TOKEN_PREFIX, taxform::canonical_value(&branch.tin, code, period)),
                }));
            }
        }
    }
    out
}

/// The branches `tins` names (a 9-digit root is every branch of that
/// taxpayer), or every saved branch without it.
pub(crate) fn named_branches(
    db: &Database,
    arguments: &Map<String, Value>,
) -> Result<Vec<resolve::Branch>, String> {
    Ok(match arguments.get("tins") {
        None | Some(Value::Null) => resolve::branches(db, None)?,
        Some(Value::Array(tins)) if !tins.is_empty() => {
            let mut out = Vec::new();
            for tin in tins {
                let tin = tin
                    .as_str()
                    .ok_or_else(|| format!("`tins` must be TIN strings (got {tin})"))?;
                // A 9-digit root is every branch of that taxpayer.
                let digits = taxform::tin_digits(tin)
                    .ok_or_else(|| format!("`{tin}` is not a TIN (9 or 14 digits)"))?;
                if digits.len() == 9 {
                    let all = resolve::branches(db, None)?;
                    let mine: Vec<_> = all.into_iter().filter(|branch| branch.root == digits).collect();
                    if mine.is_empty() {
                        return Err(format!("no taxpayer with TIN {}", resolve::format_tin(&digits)));
                    }
                    out.extend(mine);
                } else {
                    out.extend(resolve::branches(db, Some(&digits))?);
                }
            }
            out
        }
        Some(other) => return Err(format!("`tins` must be a list of TINs (got {other})")),
    })
}

/// `bir_confirmation_search {tins?}`: the Gmail search for BIR's
/// confirmation emails about these taxpayers (all of them by default), and
/// the returns still waiting for one.
pub(crate) fn search(
    db: &Database,
    arguments: &Map<String, Value>,
    today: NaiveDate,
) -> Result<Value, String> {
    check_keys(arguments, &["tins"])?;
    let branches = named_branches(db, arguments)?;
    let since = today - chrono::Duration::days(SEARCH_DAYS);
    let mut tins: Vec<String> = Vec::new();
    for branch in &branches {
        for spelling in tin_spellings(&branch.tin) {
            if !tins.contains(&spelling) {
                tins.push(spelling);
            }
        }
    }
    let names = if tins.is_empty() {
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
    Ok(json!({
        "query": format!("from:{BIR_SENDER} after:{}{names}", since.format("%Y/%m/%d")),
        "since": since.format("%Y-%m-%d").to_string(),
        "tins": tins,
        "awaiting": awaiting(db, &branches, today),
    }))
}

/// BIR's received time, in Manila: RFC 3339, or a local `YYYY-MM-DD HH:MM[:SS]`.
fn received(raw: &str) -> Result<(NaiveDate, NaiveTime, Option<String>), String> {
    let manila = chrono::FixedOffset::east_opt(8 * 3600).expect("UTC+08:00");
    if let Ok(at) = chrono::DateTime::parse_from_rfc3339(raw) {
        let local = at.with_timezone(&manila).naive_local();
        return Ok((local.date(), local.time(), Some(at.to_rfc3339())));
    }
    for format in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"] {
        if let Ok(local) = NaiveDateTime::parse_from_str(raw, format) {
            let at = local
                .and_local_timezone(manila)
                .single()
                .map(|at| at.to_rfc3339());
            return Ok((local.date(), local.time(), at));
        }
    }
    Err(format!(
        "`received_at` must be BIR's received time, e.g. 2026-10-12T14:52:00+08:00 (got `{raw}`)"
    ))
}

/// `bir_confirm_filing {tin, form, period, reference, received_at,
/// message_id}`: confirm the filed return the email is about, and answer it.
/// The same `message_id` again answers what the first call did.
pub(crate) fn confirm(
    db: &Database,
    arguments: &Map<String, Value>,
    today: NaiveDate,
) -> Result<Value, String> {
    check_keys(
        arguments,
        &["tin", "form", "period", "reference", "received_at", "message_id"],
    )?;
    let message_id = text(arguments, "message_id")?;
    let remembered = format!("{CONFIRMED_PREFIX}{message_id}");
    if let Some(answer) = db
        .get_setting(&remembered)
        .map_err(|error| error.to_string())?
        .and_then(|json| serde_json::from_str::<Value>(&json).ok())
    {
        let mut answer = answer;
        answer["already"] = json!(true);
        return Ok(answer);
    }
    let tin = text(arguments, "tin")?;
    let form = text(arguments, "form")?;
    let period = text(arguments, "period")?;
    let reference = arguments
        .get("reference")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let (date, time, email_at) = received(text(arguments, "received_at")?)?;

    let branch = resolve::branches(db, Some(tin))?
        .into_iter()
        .next()
        .ok_or_else(|| format!("no taxpayer with TIN {tin}"))?;
    let named = taxform::parse(&format!("{form} {period}"), today);
    let code = named
        .form
        .ok_or_else(|| format!("`form` `{form}` is not a BIR form code"))?;
    let period: TaxPeriod = named
        .period
        .ok_or_else(|| format!("`period` `{period}` is not a period (e.g. 2026-Q3, 2026-09, 2025)"))?
        .for_form(code, today)?;
    let returns = resolve::stored_returns(db, &branch.tin, code, today);
    let filing = resolve::furthest(&returns, code, period).ok_or_else(|| {
        format!(
            "{} has no {code} return for {} in BIR",
            branch.name,
            period.label()
        )
    })?;
    let token = format!(
        "{}{}",
        taxform::TOKEN_PREFIX,
        taxform::canonical_value(&branch.tin, code, period)
    );
    let marks = db
        .draft_data_json(filing.id)
        .map_err(|error| error.to_string())?
        .unwrap_or(Value::Null);
    let filename = marks["submission_filename"].as_str().unwrap_or("").to_string();
    let answer = |confirmed: bool, status: &FilingStatus, reason: Option<String>| {
        let mut answer = json!({
            "confirmed": confirmed,
            "already": false,
            "id": filing.id,
            "tin": branch.tin,
            "taxpayer": branch.name,
            "form": code,
            "period": period.canonical(),
            "label": format!("{code} · {}", period.label()),
            "token": token,
            "status": format!("{status:?}"),
            "reference": resolve::filing_reference(if filename.is_empty() { reference } else { &filename }),
            "received_at": format!("{date} {time}"),
            "message_id": message_id,
        });
        if let Some(reason) = reason {
            answer["reason"] = json!(reason);
        }
        answer
    };
    match filing.status {
        FilingStatus::Confirmed | FilingStatus::Paid => {
            return Ok(answer(true, &filing.status, Some("This return was already confirmed.".into())));
        }
        FilingStatus::Draft => {
            return Ok(answer(
                false,
                &filing.status,
                Some("This return is still a draft in BIR: it was never sent, so the email is not about it.".into()),
            ));
        }
        FilingStatus::Queued => {
            return Ok(answer(
                false,
                &filing.status,
                Some("This return is still queued: BIR only confirms a return once it has been sent. Check again after it is sent.".into()),
            ));
        }
        FilingStatus::Submitted => {}
    }
    if filename.is_empty() {
        return Ok(answer(
            false,
            &filing.status,
            Some("This filed return has no file name to match BIR's email to.".into()),
        ));
    }
    // The email names the file it confirms; it must be this return's.
    if let (Some(named), Some(ours)) = (split_bir_filename(reference), split_bir_filename(&filename))
        && (named.1 != ours.1 || named.2 != ours.2)
    {
        return Ok(answer(
            false,
            &filing.status,
            Some(format!(
                "The email confirms {}, not this return's {}.",
                resolve::filing_reference(reference),
                resolve::filing_reference(&filename)
            )),
        ));
    }
    let receipt = BirReceiptConfirmation {
        filename: format!("{}.xml", resolve::filing_reference(&filename)),
        date_received: date,
        time_received: time,
        source_from: Some(BIR_SENDER.to_string()),
        raw_text: format!(
            "Confirmed from email {message_id} (bir_confirm_filing). File name: {}. Received by BIR: {date} {time}.",
            if reference.is_empty() { &filename } else { reference }
        ),
        raw_html: None,
    };
    let (saved, _) = db
        .save_submission_receipt_with_email_date(&receipt, email_at.as_deref())
        .map_err(|error| error.to_string())?;
    let outcome = db
        .confirm_submission_from_receipt(&saved)
        .map_err(|error| error.to_string())?;
    if outcome == ReceiptConfirmationOutcome::Ignored {
        return Ok(answer(
            false,
            &filing.status,
            Some("BIR's received time is before this return was queued: the email is about an earlier filing of it.".into()),
        ));
    }
    let done = answer(true, &FilingStatus::Confirmed, None);
    db.set_setting(&remembered, &done.to_string())
        .map_err(|error| error.to_string())?;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bir_core::profile::TaxpayerType;
    use bir_core::tax_profile::{BranchFacts, NewTaxpayer};

    const ROOT: &str = "123456788";
    const TIN: &str = "12345678800000";

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 26).unwrap()
    }

    fn db() -> Database {
        let db = Database::open_ephemeral().expect("ephemeral database");
        db.create_taxpayer(NewTaxpayer {
            tin_root: ROOT.into(),
            name: "Galang, Juan".into(),
            taxpayer_type: TaxpayerType::Individual,
            birth_date: None,
            head_office: BranchFacts {
                rdo_code: "040".into(),
                registered_address: "1 Example Street, Pasig".into(),
                zip_code: "1600".into(),
                phone: "0280000000".into(),
                email: "office@example.com".into(),
            },
        })
        .expect("taxpayer");
        db
    }

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn the_search_names_the_sender_and_every_spelling_of_the_tin() {
        let db = db();
        let found = search(&db, &args(json!({"tins": [ROOT]})), today()).unwrap();
        let query = found["query"].as_str().unwrap();
        assert!(query.starts_with("from:ebirforms-noreply@bir.gov.ph after:2026/09/26"), "{query}");
        assert!(query.contains("\"12345678800000\""), "{query}");
        assert!(query.contains("\"123456788000\""), "{query}");
        assert_eq!(found["since"], "2026-09-26");
        assert!(search(&db, &Map::new(), today()).is_ok());
        assert!(search(&db, &args(json!({"tins": ["999999999"]})), today()).is_err());
    }

    #[test]
    fn a_return_that_was_not_sent_is_not_confirmed() {
        let db = db();
        let profile = db.get_profile(TIN).unwrap().unwrap();
        db.save_2551q_draft(&bir_core::forms::Form2551QDraft::new_from_profile(&profile, 2026, 3))
            .unwrap();
        let answer = confirm(
            &db,
            &args(json!({
                "tin": ROOT, "form": "2551Q", "period": "2026-Q3",
                "reference": "12345678800000-2551Qv2018-122026Q3.xml",
                "received_at": "2026-10-20T10:00:00+08:00", "message_id": "m-1",
            })),
            today(),
        )
        .unwrap();
        assert_eq!(answer["confirmed"], false, "{answer}");
        assert_eq!(answer["status"], "Draft");
        let missing = confirm(
            &db,
            &args(json!({
                "tin": ROOT, "form": "1601C", "period": "2026-09", "reference": "",
                "received_at": "2026-10-20 10:00", "message_id": "m-2",
            })),
            today(),
        );
        assert!(missing.unwrap_err().contains("has no 1601C return"));
    }

    #[test]
    fn a_filed_return_is_confirmed_once_per_email() {
        use bir_core::db::Claim1601CSubmissionResult;
        let db = db();
        let profile = db.get_profile(TIN).unwrap().unwrap();
        // A zero return: no taxes withheld, a private employer.
        let mut queued = bir_core::forms::form_1601c::Form1601CDraft::new_from_profile(&profile, 2026, 9);
        queued.any_taxes_withheld = false;
        queued.category_of_agent = "P".to_string();
        queued
            .transition_to_queued()
            .unwrap_or_else(|errors| panic!("queue: {errors:?}"));
        db.save_queued_1601c_draft(&queued).unwrap();
        let claim = db
            .claim_queued_1601c_submission(
                &queued.tin,
                queued.taxable_year,
                queued.month,
                &queued.queued_submission_fingerprint.clone(),
                &queued.next_retry_at.clone(),
                0,
            )
            .unwrap();
        let Claim1601CSubmissionResult::Claimed { mut draft, token } = claim else {
            panic!("expected a claim");
        };
        let filename = queued.default_submission_filename();
        draft.transition_to_submitted(filename.clone());
        // What the submission worker writes once BIR took the file (its
        // finish step is the worker's own): this test sends nothing.
        let _ = token;
        let json = serde_json::to_string(&draft).unwrap().replace('\'', "''");
        db.conn_for_tests()
            .execute_batch(&format!(
                "UPDATE form_drafts SET status = 'Submitted', data_json = '{json}'
                 WHERE tin = '{TIN}' AND form_code = '1601C' AND taxable_year = 2026 AND quarter = 9"
            ))
            .unwrap();
        let email = json!({
            "tin": TIN, "form": "1601C", "period": "2026-09",
            "reference": format!("{}.xml", resolve::filing_reference(&filename)),
            "received_at": "2099-12-31T15:00:00+08:00", "message_id": "gmail-123",
        });
        let answer = confirm(&db, &args(email.clone()), today()).unwrap();
        assert_eq!(answer["confirmed"], true, "{answer}");
        assert_eq!(answer["status"], "Confirmed");
        assert_eq!(answer["token"], format!("@bir:taxform:{ROOT}/1601C-2026-09"));
        let stored = db.get_1601c_draft(TIN, 2026, 9).unwrap().unwrap();
        assert_eq!(stored.status, FilingStatus::Confirmed);
        let again = confirm(&db, &args(email), today()).unwrap();
        assert_eq!(again["already"], true);
        assert_eq!(again["confirmed"], true);
    }
}
