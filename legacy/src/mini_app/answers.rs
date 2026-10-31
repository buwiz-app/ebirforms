//! The assistant's tools behind BIR's answer cards (spec 9.8) that no
//! existing tool covered: find a taxpayer by a misspelled name, one
//! taxpayer at a glance, late returns and a penalty estimate, returns not
//! paid, and every taxpayer's standing. They only read; [`super::cards`]
//! puts each answer on its card.
//!
//! Like [`super::taxpayer`], they take the taxpayer's `ticket`, refuse a
//! ticket BIR did not issue, and name taxpayers by ticket in what they
//! answer (the server's scrubber turns TIN fields into tickets).

use bir_core::calendar_rules::DeadlineResolver;
use bir_core::db::Database;
use bir_core::forms::FilingStatus;
use bir_core::forms::taxform::{self, TaxPeriod};
use bir_core::profile::TaxpayerType;
use bir_core::penalties::{PenaltyProfile, PenaltyRequest, compute_penalties};
use bir_core::tax_profile::BranchField;
use chrono::{Datelike, Duration, NaiveDate};
use serde_json::{Map, Value, json};

use super::v1::tickets::{self, Subject};
use super::{calendar, payments, resolve};

pub(crate) const FIND_TAXPAYER: &str = "bir_find_taxpayer";
pub(crate) const TAXPAYER: &str = "bir_taxpayer";
pub(crate) const PENALTY: &str = "bir_penalty";
pub(crate) const UNPAID: &str = "bir_unpaid";
pub(crate) const BOARD: &str = "bir_board";
pub(crate) const QUICK_ADD: &str = "bir_quick_add";
pub(crate) const OUTBOX: &str = "bir_outbox";
pub(crate) const DIGEST: &str = "bir_digest";
/// App-only: the quickadd card's Save.
pub(crate) const ADD_TAXPAYER: &str = "bir_card_add_taxpayer";
/// App-only: the outbox card's Cancel.
pub(crate) const CANCEL_SEND: &str = "bir_card_cancel_send";

/// The assistant's tools of this module, all read-only (`bir_quick_add`
/// only prefills a card; the user's Save adds the taxpayer).
pub(crate) const TOOLS: [&str; 8] = [FIND_TAXPAYER, TAXPAYER, PENALTY, UNPAID, BOARD, QUICK_ADD, OUTBOX, DIGEST];

fn check_keys(arguments: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    match arguments.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => Err(format!("unknown argument `{key}` (allowed: {})", allowed.join(", "))),
        None => Ok(()),
    }
}

fn text<'a>(arguments: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    arguments.get(key).and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty())
}

/// The subject of a `ticket` argument, if one is given.
fn subject(db: &Database, arguments: &Map<String, Value>) -> Result<Option<Subject>, String> {
    let Some(ticket) = text(arguments, "ticket") else {
        return Ok(None);
    };
    if !tickets::is_ticket(ticket) {
        return Err("`ticket` must be the taxpayer's ticket (tkt_...), not a TIN or a name".into());
    }
    tickets::subject(db, ticket)?
        .map(Some)
        .ok_or_else(|| "the ticket is not one BIR issued, or it was revoked".to_string())
}

fn date(arguments: &Map<String, Value>, key: &str) -> Result<Option<NaiveDate>, String> {
    text(arguments, key)
        .map(|raw| {
            NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                .map_err(|_| format!("`{key}` must be a date like 2026-11-03 (got `{raw}`)"))
        })
        .transpose()
}

fn amount(arguments: &Map<String, Value>, key: &str) -> Result<Option<f64>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => Ok(n.as_f64()),
        Some(Value::String(raw)) => {
            let clean: String = raw.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
            clean
                .parse::<f64>()
                .map(Some)
                .map_err(|_| format!("`{key}` must be an amount in pesos (got `{raw}`)"))
        }
        Some(other) => Err(format!("`{key}` must be an amount in pesos (got {other})")),
    }
}

// ------------------------------------------------------------ find

/// Edit distance of two short words (names), for matching a misspelling.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (row[j + 1] + 1).min(row[j] + 1).min(prev + usize::from(ca != cb));
            prev = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// How well `query` names `name`: the share of the query's words that
/// match a word of the name (exactly, as a prefix, or within one or two
/// letters for longer words).
pub(crate) fn name_score(query: &str, name: &str) -> f64 {
    let words = |text: &str| -> Vec<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_string)
            .collect()
    };
    let (asked, named) = (words(query), words(name));
    if asked.is_empty() || named.is_empty() {
        return 0.0;
    }
    let hit = |word: &str| {
        named.iter().any(|candidate| {
            candidate == word
                || (word.len() >= 2 && candidate.starts_with(word))
                || distance(word, candidate) <= if word.len() >= 6 { 2 } else { usize::from(word.len() >= 3) }
        })
    };
    asked.iter().filter(|word| hit(word)).count() as f64 / asked.len() as f64
}

/// `bir_find_taxpayer {name}`.
pub(crate) fn find_taxpayer(db: &Database, arguments: &Map<String, Value>, _today: NaiveDate) -> Result<Value, String> {
    check_keys(arguments, &["name"])?;
    let query = text(arguments, "name").ok_or("`name` is required: the name as the user wrote it")?;
    let mut scored: Vec<(f64, resolve::Branch)> = resolve::branches(db, None)?
        .into_iter()
        .filter(|branch| branch.is_head_office)
        .map(|branch| (name_score(query, &branch.name), branch))
        .filter(|(score, _)| *score >= 0.5)
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    let matches: Vec<Value> = scored
        .into_iter()
        .take(6)
        .map(|(score, branch)| {
            let rdo = resolve::who_rdo(db, &branch.tin).unwrap_or_default();
            json!({
                "tin": branch.root,
                "name": branch.name,
                "detail": if rdo.is_empty() { String::new() } else { format!("RDO {rdo}") },
                "exact": score >= 1.0,
            })
        })
        .collect();
    Ok(json!({
        "query": query,
        "matches": matches,
        "note": if matches.is_empty() {
            "No saved taxpayer has a name like that. Ask the user to check the spelling, or to add the taxpayer in Buwiz Sign-in."
        } else {
            "Each match's `tin` is its ticket. Ask the user which one they mean when it is not exact; never guess."
        },
    }))
}

// ------------------------------------------------------------ taxpayer

/// `bir_taxpayer {ticket, view?}`: `view: "branches"` answers the same
/// facts on the branches card.
pub(crate) fn taxpayer(db: &Database, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Value, String> {
    check_keys(arguments, &["ticket", "view"])?;
    let view = match text(arguments, "view") {
        None | Some("glance") => "glance",
        Some("branches") => "branches",
        Some(other) => return Err(format!("`view` must be glance or branches (got `{other}`)")),
    };
    let subject = subject(db, arguments)?.ok_or("`ticket` is required: the taxpayer's ticket")?;
    let who = db
        .get_taxpayer(&subject.root())
        .map_err(|error| error.to_string())?
        .ok_or("this taxpayer is no longer saved in BIR")?;
    let branches: Vec<Value> = who
        .branches
        .iter()
        .map(|branch| {
            json!({
                "tin": branch.tin,
                "code": branch.branch_code,
                "name": branch.name,
                "head_office": branch.is_head_office,
                "rdo": branch.value(BranchField::RdoCode),
                "address": branch.value(BranchField::RegisteredAddress),
            })
        })
        .collect();
    let head = who.branches.iter().find(|branch| branch.is_head_office).or(who.branches.first());
    let year = today.year() as u16;
    let mut forms: Vec<String> = Vec::new();
    if let Some(head) = head {
        for code in db.active_form_codes_for_year(&head.tin, year).unwrap_or_default() {
            if let Some(code) = taxform::form_code_of(&code)
                && !forms.iter().any(|known| known == code)
            {
                forms.push(code.to_string());
            }
        }
    }
    let mut window = Map::new();
    window.insert("tin".into(), json!(subject.full_tin()));
    window.insert("from".into(), json!(today.format("%Y-%m-%d").to_string()));
    window.insert("to".into(), json!((today + Duration::days(370)).format("%Y-%m-%d").to_string()));
    let next = calendar::get(db, &window, today)
        .ok()
        .and_then(|got| {
            got["events"].as_array()?.iter().find(|event| {
                event["enabled"] == true && !matches!(event["status"].as_str(), Some("filed" | "confirmed" | "paid"))
            }).cloned()
        })
        .map(|event| json!({ "form": event["form"], "label": event["label"], "due": event["due"], "key": event["key"] }));
    Ok(json!({
        "view": view,
        "tin_root": who.taxpayer.tin_root,
        "name": who.taxpayer.name,
        "type": format!("{:?}", who.taxpayer.taxpayer_type),
        "rdo": head.map(|branch| branch.value(BranchField::RdoCode)).unwrap_or(""),
        "branches": branches,
        "forms": forms,
        "year": year,
        "next_due": next,
    }))
}

// ------------------------------------------------------------ penalties

/// The penalty schedule a form files under: withholding returns, else the
/// standard one.
fn schedule(code: &str) -> PenaltyProfile {
    if code.starts_with("06") || code.starts_with("16") {
        PenaltyProfile::Withholding
    } else {
        PenaltyProfile::StandardFiling
    }
}

fn money(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// `bir_penalty {form, period?, due?, tax_due?, filed_on?, ticket?}` (an
/// estimate), or `{ticket?}` alone (the returns already late).
pub(crate) fn penalty(db: &Database, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Value, String> {
    check_keys(arguments, &["ticket", "form", "period", "due", "tax_due", "filed_on"])?;
    let subject = subject(db, arguments)?;
    let Some(form) = text(arguments, "form") else {
        return overdue(db, subject, today);
    };
    let code = taxform::form_code_of(form).ok_or_else(|| format!("`form` `{form}` is not a BIR form code"))?;
    let period = text(arguments, "period")
        .map(|raw| {
            taxform::parse(raw, today)
                .period
                .ok_or_else(|| format!("`period` `{raw}` is not a period (e.g. 2026-Q3, 2026-09, 2025)"))
        })
        .transpose()?;
    let due = match (date(arguments, "due")?, &subject, period) {
        (Some(due), _, _) => due,
        (None, Some(subject), Some(period)) => calendar::due_date(db, &subject.full_tin(), code, period)
            .ok_or("BIR has no deadline for that form and period")?,
        _ => return Err("give the return's `due` date, or the taxpayer's `ticket` with the `period`".into()),
    };
    let filed_on = date(arguments, "filed_on")?.unwrap_or(today);
    let tax_due = amount(arguments, "tax_due")?.unwrap_or(0.0);
    let computed = compute_penalties(&PenaltyRequest {
        form_code: code,
        taxable_period: period.map(TaxPeriod::canonical).unwrap_or_default(),
        schedule: schedule(code),
        taxpayer_class: None,
        due_date: due,
        filing_date: filed_on,
        payment_date: None,
        basic_tax_due: tax_due,
        gross_sales_or_receipts: None,
        is_amended_return: false,
    });
    let days_late = (filed_on - due).num_days().max(0);
    Ok(json!({
        "mode": "estimate",
        "form": code,
        "period": period.map(TaxPeriod::canonical),
        "due": due.format("%Y-%m-%d").to_string(),
        "filed_on": filed_on.format("%Y-%m-%d").to_string(),
        "days_late": days_late,
        "tax_due": money(tax_due),
        "surcharge": money(computed.surcharge),
        "interest": money(computed.interest),
        "compromise": money(computed.compromise),
        "total_penalties": money(computed.surcharge + computed.interest + computed.compromise),
        "total_to_pay": money(tax_due + computed.surcharge + computed.interest + computed.compromise),
        "basis": "Surcharge on the tax due, interest a year on the unpaid tax (12% under TRAIN), and the compromise from BIR's table, as BIR's own engine computes them for this computer's returns. An estimate: BIR assesses the final amount.",
    }))
}

/// The returns past their due date and not filed.
fn overdue(db: &Database, subject: Option<Subject>, today: NaiveDate) -> Result<Value, String> {
    let mut window = Map::new();
    if let Some(subject) = &subject {
        window.insert("tin".into(), json!(subject.full_tin()));
    }
    window.insert("from".into(), json!((today - Duration::days(400)).format("%Y-%m-%d").to_string()));
    window.insert("to".into(), json!(today.format("%Y-%m-%d").to_string()));
    let events = calendar::get(db, &window, today)?["events"].as_array().cloned().unwrap_or_default();
    let items: Vec<Value> = events
        .iter()
        .filter(|event| event["overdue"] == true && event["enabled"] == true)
        .map(|event| {
            let due = event["due"].as_str().and_then(|due| NaiveDate::parse_from_str(due, "%Y-%m-%d").ok());
            json!({
                "key": event["key"],
                "tin": event["tin"],
                "taxpayer": event["taxpayer"],
                "form": event["form"],
                "period": event["period"],
                "label": event["label"],
                "due": event["due"],
                "status": event["status"],
                "days_late": due.map(|due| (today - due).num_days()).unwrap_or(0),
            })
        })
        .collect();
    Ok(json!({
        "mode": "overdue",
        "today": today.format("%Y-%m-%d").to_string(),
        "items": items,
        "note": "Filing today adds a surcharge, interest and a compromise. Call bir_penalty with `form`, `period`, the taxpayer's `ticket` and `tax_due` for an amount.",
    }))
}

// ------------------------------------------------------------ unpaid

/// `bir_unpaid {ticket?}`: returns sent or confirmed with no payment.
pub(crate) fn unpaid(db: &Database, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Value, String> {
    check_keys(arguments, &["ticket"])?;
    let mut search = Map::new();
    if let Some(subject) = subject(db, arguments)? {
        search.insert("tins".into(), json!([subject.root()]));
    }
    let found = payments::search(db, &search, today)?;
    let items: Vec<Value> = found["awaiting"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| matches!(row["status"].as_str(), Some("Submitted" | "Confirmed")))
        .cloned()
        .collect();
    Ok(json!({
        "items": items,
        "note": "Sent or confirmed returns with no payment recorded. Mark one paid (the user presses Save), attach a receipt, or let the payments routine find it in mail.",
    }))
}

// ------------------------------------------------------------ board

/// `bir_board {}`: each taxpayer's returns done, due within two weeks and
/// late, this year. `{form, period?}`: one return across every taxpayer
/// who files it (bulk); `{view: "on_time"}`: each taxpayer's on-time rate
/// this year (streak).
pub(crate) fn board(db: &Database, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Value, String> {
    check_keys(arguments, &["form", "period", "view"])?;
    if let Some(form) = text(arguments, "form") {
        return bulk(db, form, text(arguments, "period"), today);
    }
    if text(arguments, "period").is_some() {
        return Err("`period` needs `form`".into());
    }
    match text(arguments, "view") {
        None | Some("standing") => {}
        Some("on_time") => return on_time(db, today),
        Some(other) => return Err(format!("`view` must be standing or on_time (got `{other}`)")),
    }
    let mut window = Map::new();
    window.insert("from".into(), json!(format!("{}-01-01", today.year())));
    window.insert("to".into(), json!((today + Duration::days(60)).format("%Y-%m-%d").to_string()));
    let events = calendar::get(db, &window, today)?["events"].as_array().cloned().unwrap_or_default();
    let soon = today + Duration::days(14);
    let mut rows: Vec<(String, String, u32, u32, u32)> = Vec::new();
    for event in events.iter().filter(|event| event["enabled"] == true) {
        let tin: String = event["tin"].as_str().unwrap_or("").chars().take(9).collect();
        let name = event["taxpayer"].as_str().unwrap_or("").to_string();
        let at = match rows.iter().position(|row| row.0 == tin) {
            Some(at) => at,
            None => {
                rows.push((tin, name, 0, 0, 0));
                rows.len() - 1
            }
        };
        let done = matches!(event["status"].as_str(), Some("filed" | "confirmed" | "paid"));
        let due = event["due"].as_str().and_then(|due| NaiveDate::parse_from_str(due, "%Y-%m-%d").ok());
        if done {
            rows[at].2 += 1;
        } else if event["overdue"] == true {
            rows[at].4 += 1;
        } else if due.is_some_and(|due| due <= soon) {
            rows[at].3 += 1;
        }
    }
    rows.sort_by(|a, b| b.4.cmp(&a.4).then(b.3.cmp(&a.3)).then(a.1.cmp(&b.1)));
    Ok(json!({
        "year": today.year(),
        "taxpayers": rows.into_iter().map(|(tin, name, done, soon, late)| json!({
            "tin_root": tin, "name": name, "done": done, "due_soon": soon, "late": late,
        })).collect::<Vec<_>>(),
        "note": "done: filed this year; due_soon: due within 14 days; late: past due and not filed.",
    }))
}

/// Whether a return in this state reached BIR.
fn done(status: &str) -> bool {
    matches!(status, "filed" | "confirmed" | "paid")
}

/// The name a branch goes by on a card: the taxpayer's, and the branch
/// code for a branch.
fn branch_name(branch: &resolve::Branch) -> String {
    if branch.is_head_office {
        branch.name.clone()
    } else {
        format!("{} · branch {}", branch.name, branch.tin.get(9..).unwrap_or(""))
    }
}

/// One form and period across every taxpayer who files it.
fn bulk(db: &Database, form: &str, period: Option<&str>, today: NaiveDate) -> Result<Value, String> {
    let code = taxform::form_code_of(form).ok_or_else(|| format!("`form` `{form}` is not a BIR form code"))?;
    let period = match period {
        Some(raw) => taxform::parse(raw, today)
            .period
            .ok_or_else(|| format!("`period` `{raw}` is not a period (e.g. 2026-10, 2026-Q3, 2025)"))?
            .for_form(code, today)
            .map_err(|ask| ask.to_string())?,
        None => DeadlineResolver::last_closed_period(code, today)
            .and_then(|period| calendar::period_of(&period))
            .ok_or_else(|| format!("{code} is filed as needed: name the `period`"))?,
    };
    let year = period.year();
    let mut rows: Vec<(bool, bool, String, Value)> = Vec::new();
    for branch in resolve::branches(db, None)? {
        let files = db
            .active_form_codes_for_year(&branch.tin, year)
            .unwrap_or_default()
            .iter()
            .any(|raw| taxform::form_code_of(raw) == Some(code));
        if !files {
            continue;
        }
        let stored = resolve::stored_returns(db, &branch.tin, code, today);
        let status = calendar::status_word(resolve::furthest(&stored, code, period).map(|filing| &filing.status));
        let due = calendar::due_date(db, &branch.tin, code, period);
        let late = !done(status) && due.is_some_and(|due| due < today);
        let name = branch_name(&branch);
        rows.push((
            done(status),
            late,
            name.clone(),
            json!({
                "tin": branch.tin,
                "taxpayer": name,
                "status": status,
                "due": due.map(|due| due.format("%Y-%m-%d").to_string()),
                "late": late,
                "key": taxform::canonical_value(&branch.tin, code, period),
            }),
        ));
    }
    // Still to do first, the late ones first among them.
    rows.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    let left = rows.iter().filter(|row| !row.0).count();
    Ok(json!({
        "mode": "bulk",
        "form": code,
        "period": period.canonical(),
        "label": format!("{code} · {}", period.label()),
        "total": rows.len(),
        "left": left,
        "rows": rows.into_iter().map(|row| row.3).collect::<Vec<_>>(),
        "note": "Every taxpayer whose forms this year include the form, with that period's return: not_started, draft, queued, filed, confirmed, paid. `key` opens the return.",
    }))
}

/// Each taxpayer's returns due so far this year, filed on time or not.
fn on_time(db: &Database, today: NaiveDate) -> Result<Value, String> {
    let mut window = Map::new();
    window.insert("from".into(), json!(format!("{}-01-01", today.year())));
    window.insert("to".into(), json!(today.format("%Y-%m-%d").to_string()));
    let events = calendar::get(db, &window, today)?["events"].as_array().cloned().unwrap_or_default();
    let mut stored: std::collections::HashMap<(String, String), Vec<(TaxPeriod, bir_core::forms::FormDraftSummary)>> =
        std::collections::HashMap::new();
    // root -> (name, due, on time, late)
    let mut rows: Vec<(String, String, u32, u32, u32)> = Vec::new();
    for event in events.iter().filter(|event| event["enabled"] == true) {
        let tin = event["tin"].as_str().unwrap_or("").to_string();
        let form = event["form"].as_str().unwrap_or("").to_string();
        let Some(due) = event["due"].as_str().map(str::to_string) else { continue };
        let root: String = tin.chars().take(9).collect();
        let at = match rows.iter().position(|row| row.0 == root) {
            Some(at) => at,
            None => {
                rows.push((root.clone(), event["taxpayer"].as_str().unwrap_or("").to_string(), 0, 0, 0));
                rows.len() - 1
            }
        };
        rows[at].2 += 1;
        if !done(event["status"].as_str().unwrap_or("")) {
            rows[at].4 += 1;
            continue;
        }
        // When it left: the return's own filing mark, else its last save.
        let period = event["period"]
            .as_str()
            .and_then(|period| taxform::parse(&format!("{form} {period}"), today).period);
        let returns = stored
            .entry((tin.clone(), form.clone()))
            .or_insert_with(|| resolve::stored_returns(db, &tin, &form, today));
        let filed = period
            .and_then(|period| resolve::furthest(returns, &form, period))
            .map(|filing| {
                db.draft_data_json(filing.id)
                    .ok()
                    .flatten()
                    .and_then(|data| data["submitted_at"].as_str().map(str::to_string))
                    .unwrap_or_else(|| filing.updated_at.clone())
            })
            .unwrap_or_default();
        if !filed.is_empty() && filed.get(..10).unwrap_or(&filed) <= due.as_str() {
            rows[at].3 += 1;
        } else {
            rows[at].4 += 1;
        }
    }
    let mut out: Vec<(Option<u32>, String, Value)> = rows
        .into_iter()
        .map(|(root, name, due, on_time, late)| {
            let rate = (due > 0).then(|| (on_time * 100 + due / 2) / due);
            (rate, name.clone(), json!({ "tin_root": root, "name": name, "due": due, "on_time": on_time, "late": late, "rate": rate }))
        })
        .collect();
    // The ones who file late most come first.
    out.sort_by(|a, b| a.0.unwrap_or(101).cmp(&b.0.unwrap_or(101)).then(a.1.cmp(&b.1)));
    Ok(json!({
        "mode": "on_time",
        "year": today.year(),
        "taxpayers": out.into_iter().map(|row| row.2).collect::<Vec<_>>(),
        "note": "Returns due from January 1 to today on each taxpayer's calendar. on_time: filed by its due date (the date it was sent, else its last save); late: filed after it, or not filed yet. rate: on_time as a percent of due.",
    }))
}

// ------------------------------------------------------------ quick add

fn taxpayer_type(raw: Option<&str>, name: &str) -> Result<TaxpayerType, String> {
    let guess = |name: &str| {
        let lower = name.to_lowercase();
        if [" corp", " inc", "corporation", "incorporated", " co.", " ltd"].iter().any(|word| lower.contains(word)) {
            TaxpayerType::Corporation
        } else {
            TaxpayerType::Individual
        }
    };
    Ok(match raw.map(str::to_lowercase).as_deref() {
        None => guess(name),
        Some("individual" | "person") => TaxpayerType::Individual,
        Some("corporation" | "corp" | "company") => TaxpayerType::Corporation,
        Some("partnership") => TaxpayerType::Partnership,
        Some("cooperative") => TaxpayerType::Cooperative,
        Some("estate") => TaxpayerType::Estate,
        Some("trust") => TaxpayerType::Trust,
        Some(other) => return Err(format!("`type` must be individual, corporation, partnership, cooperative, estate or trust (got `{other}`)")),
    })
}

fn rdo_code(raw: &str) -> Result<(String, String), String> {
    let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
    let code = format!("{digits:0>3}");
    let rdo = bir_core::reference::get_rdo(&code)
        .or_else(|| bir_core::reference::get_rdo(raw.trim()))
        .ok_or_else(|| format!("`rdo` `{raw}` is not an RDO code (e.g. 040)"))?;
    Ok((rdo.code, rdo.description))
}

/// `bir_quick_add {name, tin?, type?, rdo?}`: a new taxpayer's card,
/// prefilled. Nothing is saved: the user checks it and presses Save
/// (`bir_card_add_taxpayer`).
pub(crate) fn quick_add(db: &Database, arguments: &Map<String, Value>, _today: NaiveDate) -> Result<Value, String> {
    check_keys(arguments, &["name", "tin", "type", "rdo"])?;
    let name = text(arguments, "name").ok_or("`name` is required: the registered name as the user wrote it")?;
    let root = text(arguments, "tin")
        .map(|raw| {
            taxform::tin_digits(raw)
                .map(|digits| digits[..9].to_string())
                .ok_or_else(|| "`tin` must be the TIN the user typed (9 or 14 digits)".to_string())
        })
        .transpose()?;
    if let Some(root) = &root
        && let Ok(saved) = resolve::branches(db, Some(root))
        && let Some(saved) = saved.first()
    {
        return Ok(json!({
            "exists": true,
            "tin_root": saved.root,
            "name": saved.name,
            "note": "This TIN is already saved in BIR, under the name shown; its tin_root is its ticket. Nothing was added.",
        }));
    }
    let kind = taxpayer_type(text(arguments, "type"), name)?;
    let rdo = text(arguments, "rdo").map(rdo_code).transpose()?;
    let ticket = root
        .as_ref()
        .map(|root| tickets::issue(db, &Subject::Taxpayer(root.clone())))
        .transpose()?;
    Ok(json!({
        "saved": false,
        "pending_user": true,
        "name": name,
        "ticket": ticket,
        "tin_end": root.as_ref().map(|root| root[6..].to_string()),
        "type": format!("{kind:?}"),
        "rdo": rdo.as_ref().map(|rdo| rdo.0.clone()),
        "rdo_office": rdo.map(|rdo| rdo.1),
        "note": "Nothing is saved yet: Rox shows the user a card to check the name, type and RDO, and the taxpayer is added only when they press Save.",
    }))
}

/// `bir_card_add_taxpayer {ticket, name, type, rdo}` (the quickadd card's
/// Save, apps only): add the taxpayer the ticket's TIN names.
pub(crate) fn add_taxpayer(db: &Database, arguments: &Map<String, Value>) -> Result<Value, String> {
    check_keys(arguments, &["ticket", "name", "type", "rdo"])?;
    let ticket = text(arguments, "ticket").ok_or("`ticket` is required: the card's ticket for the TIN")?;
    let root = match tickets::subject(db, ticket)? {
        Some(Subject::Taxpayer(root)) => root,
        Some(_) => return Err("the ticket names a branch or a return, not a new taxpayer".into()),
        None => return Err("the ticket is not one BIR issued, or it was revoked".into()),
    };
    if resolve::branches(db, Some(&root)).is_ok_and(|saved| !saved.is_empty()) {
        return Err("this TIN is already saved in BIR".into());
    }
    let name = text(arguments, "name").ok_or("`name` is required")?;
    let kind = taxpayer_type(text(arguments, "type"), name)?;
    let rdo = match text(arguments, "rdo") {
        Some(raw) => rdo_code(raw)?.0,
        None => String::new(),
    };
    db.create_taxpayer(bir_core::tax_profile::NewTaxpayer {
        tin_root: root,
        name: name.to_string(),
        taxpayer_type: kind,
        birth_date: None,
        head_office: bir_core::tax_profile::BranchFacts { rdo_code: rdo, ..Default::default() },
    })
    .map_err(|error| error.to_string())?;
    Ok(json!({ "saved": true, "ticket": ticket, "name": name }))
}

// ------------------------------------------------------------ outbox

/// A stored return's period from its summary row.
fn summary_period(summary: &bir_core::forms::FormDraftSummary) -> TaxPeriod {
    let year = summary.taxable_year;
    match (summary.month, summary.quarter) {
        (Some(month), _) if (1..=12).contains(&month) => TaxPeriod::Month { year, month },
        (_, Some(quarter)) if (1..=4).contains(&quarter) => TaxPeriod::Quarter { year, quarter },
        _ => TaxPeriod::Year(year),
    }
}

fn names(db: &Database) -> Vec<resolve::Branch> {
    resolve::branches(db, None).unwrap_or_default()
}

fn name_of(names: &[resolve::Branch], tin: &str) -> String {
    let digits: String = tin.chars().filter(char::is_ascii_digit).collect();
    names
        .iter()
        .find(|branch| branch.tin == digits || (digits.len() == 9 && branch.root == digits && branch.is_head_office))
        .map(branch_name)
        .unwrap_or_default()
}

/// `bir_outbox {}`: returns waiting to be sent to eBIRForms, and the ones
/// sent in the last 30 days that BIR has not confirmed yet.
pub(crate) fn outbox(db: &Database, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Value, String> {
    check_keys(arguments, &[])?;
    let names = names(db);
    let since = (today - Duration::days(30)).format("%Y-%m-%d").to_string();
    let (mut queued, mut sent) = (Vec::new(), Vec::new());
    for summary in db.list_all_queued_submissions().map_err(|error| error.to_string())? {
        let period = summary_period(&summary);
        let row = json!({
            "tin": summary.tin,
            "taxpayer": name_of(&names, &summary.tin),
            "form": summary.form_code,
            "period": period.canonical(),
            "label": format!("{} · {}", summary.form_code, period.label()),
            "since": summary.updated_at,
            "key": taxform::canonical_value(&summary.tin, &summary.form_code, period),
        });
        match summary.status {
            FilingStatus::Queued => queued.push(row),
            FilingStatus::Submitted if summary.updated_at.as_str() >= since.as_str() => sent.push(row),
            _ => {}
        }
    }
    Ok(json!({
        "queued": queued,
        "sent": sent,
        "note": "queued: waiting for BIR's sending queue to send it to eBIRForms (the user can cancel one before it is sent). sent: sent in the last 30 days, waiting for BIR's confirmation email.",
    }))
}

/// `bir_card_cancel_send {ticket}` (the outbox card's Cancel, apps only):
/// the queued return the ticket names, as its stored row has it. The
/// caller cancels it through the `cancel_queued` host function.
pub(crate) fn queued_row(db: &Database, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Vec<Value>, String> {
    check_keys(arguments, &["ticket"])?;
    let ticket = text(arguments, "ticket").ok_or("`ticket` is required: the return's ticket")?;
    let value = match tickets::subject(db, ticket)? {
        Some(subject) => subject.return_value().map(str::to_string).ok_or("the ticket names a taxpayer, not a return")?,
        None => return Err("the ticket is not one BIR issued, or it was revoked".into()),
    };
    let named = taxform::parse(&value, today);
    let (Some(tin), Some(code), Some(period)) = (named.tin, named.form, named.period) else {
        return Err("the ticket's return is incomplete".into());
    };
    let summary = db
        .list_all_queued_submissions()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|summary| {
            summary.status == FilingStatus::Queued
                && summary.form_code == code
                && summary.tin.chars().filter(char::is_ascii_digit).collect::<String>().starts_with(&tin)
                && summary_period(summary) == period
        })
        .ok_or("this return is not waiting in the sending queue any more (it was sent or cancelled)")?;
    let slot = summary.month.or(summary.quarter).map_or(Value::Null, |slot| json!(slot));
    Ok(vec![json!(summary.tin), json!(summary.form_code), json!(summary.taxable_year), slot])
}

// ------------------------------------------------------------ digest

/// `bir_digest {since?}`: what BIR recorded since yesterday (or `since`):
/// returns BIR confirmed and payments recorded (the routines' work), and
/// returns sent more than three days ago still without a confirmation.
pub(crate) fn digest(db: &Database, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Value, String> {
    check_keys(arguments, &["since"])?;
    let since = date(arguments, "since")?.unwrap_or(today - Duration::days(1));
    let since_text = since.format("%Y-%m-%d").to_string();
    let stale = (today - Duration::days(3)).format("%Y-%m-%d").to_string();
    let names = names(db);
    let paid_keys = payments::keys(db)?;
    let mut items = Vec::new();
    for summary in db.list_all_filed_submissions().map_err(|error| error.to_string())? {
        let period = summary_period(&summary);
        let key = taxform::canonical_value(&summary.tin, &summary.form_code, period);
        let row = |kind: &str, detail: String| {
            json!({
                "kind": kind,
                "tin": summary.tin,
                "taxpayer": name_of(&names, &summary.tin),
                "form": summary.form_code,
                "period": period.canonical(),
                "label": format!("{} · {}", summary.form_code, period.label()),
                "detail": detail,
                "paid": summary.status == FilingStatus::Paid || paid_keys.contains(&key),
                "key": key,
            })
        };
        let at = summary.updated_at.get(..10).unwrap_or(&summary.updated_at).to_string();
        match summary.status {
            FilingStatus::Confirmed | FilingStatus::Paid if at >= since_text => {
                let received = db
                    .draft_data_json(summary.id)
                    .ok()
                    .flatten()
                    .and_then(|data| data["confirmed_at"].as_str().map(str::to_string))
                    .unwrap_or_else(|| summary.updated_at.clone());
                items.push(row("confirmed", format!("BIR confirmation received {}", received.get(..10).unwrap_or(&received))));
            }
            FilingStatus::Submitted if at < stale => {
                items.push(row("waiting", format!("Sent {at}; no confirmation from BIR yet")));
            }
            _ => {}
        }
    }
    for payment in payments::recorded_since(db, &since_text)? {
        let mut payment = payment;
        payment["kind"] = json!("payment");
        payment["taxpayer"] = json!(name_of(&names, payment["tin"].as_str().unwrap_or("")));
        items.push(payment);
    }
    Ok(json!({
        "since": since_text,
        "items": items,
        "note": "confirmed: BIR's confirmation email was matched to the return (paid tells whether a payment is recorded). payment: a payment recorded on a return (from a payment email, or by hand). waiting: sent more than three days ago and not confirmed yet.",
    }))
}

/// The tools' `tools/list` entries.
pub(crate) fn descriptors() -> Vec<Value> {
    let tool = |name: &str, description: &str, properties: Value, required: &[&str]| {
        json!({
            "name": name,
            "description": description,
            "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
            "annotations": { "readOnlyHint": true },
        })
    };
    vec![
        tool(
            FIND_TAXPAYER,
            "Find a saved taxpayer by name, even misspelled or in another order (\"jane deli cruz\" finds \"Dela Cruz Jane\"). Answers the closest matches, each with its ticket; Rox shows them as a card the user picks from. Use it whenever the user names a taxpayer without an @bir:taxprofile token. Never search the web for a taxpayer.",
            json!({ "name": { "type": "string", "description": "The name as the user wrote it." } }),
            &["name"],
        ),
        tool(
            TAXPAYER,
            "One taxpayer at a glance: name, type, RDO, branches, the forms they file this year and the next deadline. Use it for \"who is ...\", \"show me her profile\"; with `view: \"branches\"` for \"what branches does ... have?\". `ticket`: the taxpayer's ticket.",
            json!({ "ticket": { "type": "string" }, "view": { "type": "string", "enum": ["glance", "branches"] } }),
            &["ticket"],
        ),
        tool(
            PENALTY,
            "Late returns and what filing late costs. With `form` (and `due`, or the taxpayer's `ticket` and `period`), `tax_due` and `filed_on` (default today): an estimate of surcharge, interest and compromise from BIR's own penalty engine. Without `form`: every return past its due date and not filed (one taxpayer with `ticket`, else all). Use it for \"anything late?\", \"how much if I file 2 months late?\".",
            json!({
                "ticket": { "type": "string", "description": "The taxpayer's ticket." },
                "form": { "type": "string", "description": "Form code, e.g. 0619E." },
                "period": { "type": "string", "description": "The return's period, e.g. 2026-09." },
                "due": { "type": "string", "description": "The due date, YYYY-MM-DD, when there is no ticket." },
                "tax_due": { "type": ["number", "string"], "description": "The tax due in pesos." },
                "filed_on": { "type": "string", "description": "When it would be filed and paid, YYYY-MM-DD (default today)." }
            }),
            &[],
        ),
        tool(
            UNPAID,
            "Returns sent to BIR or confirmed that have no payment recorded (one taxpayer with `ticket`, else all). Use it for \"what haven't I paid?\". Rox shows a card where the user marks each paid.",
            json!({ "ticket": { "type": "string" } }),
            &[],
        ),
        tool(
            BOARD,
            "Every taxpayer's standing this year: returns done, due within two weeks and late, worst first (\"how are my clients doing?\"). With `form` (and `period`, default the latest one closed): that one return for every taxpayer who files it, still to do first (\"who still needs 0619E for October?\"). With `view: \"on_time\"`: each taxpayer's on-time filing rate this year, latest filers first (\"who files late most?\").",
            json!({
                "form": { "type": "string", "description": "Form code, e.g. 0619E." },
                "period": { "type": "string", "description": "With `form`: the period, e.g. 2026-10, 2026-Q3." },
                "view": { "type": "string", "enum": ["standing", "on_time"] }
            }),
            &[],
        ),
        tool(
            QUICK_ADD,
            "Offer to add a taxpayer who is not in BIR yet (\"add Mario Reyes, TIN 123-456-789\"): Rox shows a short form prefilled with the name, TIN, type and RDO; nothing is saved until the user presses Save on it. Use bir_find_taxpayer first when they might already be saved. `tin` is the TIN exactly as the user typed it; never repeat it in your answer.",
            json!({
                "name": { "type": "string", "description": "Registered name, as the user wrote it." },
                "tin": { "type": "string", "description": "The TIN the user typed." },
                "type": { "type": "string", "enum": ["individual", "corporation", "partnership", "cooperative", "estate", "trust"] },
                "rdo": { "type": "string", "description": "RDO code, e.g. 040." }
            }),
            &["name"],
        ),
        tool(
            OUTBOX,
            "The sending queue: returns waiting to be sent to eBIRForms, and those sent in the last 30 days still waiting for BIR's confirmation. Use it for \"is anything still sending?\". The user can cancel a queued return on the card.",
            json!({}),
            &[],
        ),
        tool(
            DIGEST,
            "What BIR recorded since yesterday (or `since`, YYYY-MM-DD): returns BIR confirmed and payments recorded (by the confirmation and payment routines or by hand), and returns sent over three days ago with no confirmation yet. Use it for a morning summary or \"what happened overnight?\".",
            json!({ "since": { "type": "string", "description": "YYYY-MM-DD; default yesterday." } }),
            &[],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_misspelled_name_in_another_order_still_scores() {
        assert!(name_score("jane deli cruz", "Dela Cruz Jane") >= 0.99);
        assert!(name_score("dela cruz", "Dela Cruz Jane") >= 0.99);
        assert!(name_score("mario", "Dela Cruz Jane") < 0.5);
        assert_eq!(distance("deli", "dela"), 1);
    }
}
