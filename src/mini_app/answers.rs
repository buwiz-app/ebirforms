//! The assistant's tools behind BIR's answer cards (spec 9.8) that no
//! existing tool covered: find a taxpayer by a misspelled name, one
//! taxpayer at a glance, late returns and a penalty estimate, returns not
//! paid, and every taxpayer's standing. They only read; [`super::cards`]
//! puts each answer on its card.
//!
//! Like [`super::taxpayer`], they take the taxpayer's `ticket`, refuse a
//! ticket BIR did not issue, and name taxpayers by ticket in what they
//! answer (the server's scrubber turns TIN fields into tickets).

use bir_core::db::Database;
use bir_core::forms::taxform::{self, TaxPeriod};
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

/// The tools of this module, all read-only.
pub(crate) const TOOLS: [&str; 5] = [FIND_TAXPAYER, TAXPAYER, PENALTY, UNPAID, BOARD];

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

/// `bir_taxpayer {ticket}`.
pub(crate) fn taxpayer(db: &Database, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Value, String> {
    check_keys(arguments, &["ticket"])?;
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
/// late, this year.
pub(crate) fn board(db: &Database, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Value, String> {
    check_keys(arguments, &[])?;
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
            "One taxpayer at a glance: name, type, RDO, branches, the forms they file this year and the next deadline. Use it for \"who is ...\", \"show me her profile\". `ticket`: the taxpayer's ticket.",
            json!({ "ticket": { "type": "string" } }),
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
            "Every taxpayer's standing this year: returns done, due within two weeks and late, worst first. Use it for \"how are my clients doing?\".",
            json!({}),
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
