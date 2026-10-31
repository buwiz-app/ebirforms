//! What one taxpayer files, for the assistant: `bir_profile_forms` (the
//! forms registered on the profile for a year, with each period's status,
//! and the as-needed forms) and `bir_returns_list` (the returns stored,
//! with status and payment).
//!
//! The apps read these through their own host functions; once those left
//! the model's surface (`rox/audience: ["app"]`), the assistant had no way
//! to answer "what forms is she registered for" short of opening an app.
//! Both take the taxpayer's `ticket` (from `@bir:taxprofile:`), never a
//! TIN, refuse a ticket BIR did not issue, and only read.

use bir_core::db::Database;
use bir_core::forms::FilingStatus;
use bir_core::forms::taxform::{self, Cadence};
use chrono::{Datelike, NaiveDate};
use serde_json::{Map, Value, json};

use super::v1::tickets;
use super::{calendar, payments, resolve};

pub(crate) const PROFILE_FORMS: &str = "bir_profile_forms";
pub(crate) const RETURNS_LIST: &str = "bir_returns_list";

fn check_keys(arguments: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    match arguments.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => Err(format!("unknown argument `{key}` (allowed: {})", allowed.join(", "))),
        None => Ok(()),
    }
}

/// The branch a `ticket` argument names, with its taxpayer's name.
fn branch_of(db: &Database, arguments: &Map<String, Value>) -> Result<resolve::Branch, String> {
    let ticket = arguments
        .get("ticket")
        .and_then(Value::as_str)
        .ok_or("`ticket` is required: the taxpayer's ticket (from an @bir:taxprofile token or a BIR answer)")?;
    if !tickets::is_ticket(ticket) {
        return Err("`ticket` must be the taxpayer's ticket (tkt_...), not a TIN or a name".into());
    }
    let subject = tickets::subject(db, ticket)?
        .ok_or("the ticket is not one BIR issued, or it was revoked")?;
    resolve::branches(db, Some(&subject.full_tin()))?
        .into_iter()
        .next()
        .ok_or_else(|| "this taxpayer is no longer saved in BIR".to_string())
}

fn year_of(arguments: &Map<String, Value>, today: NaiveDate) -> Result<Option<u16>, String> {
    match arguments.get("year") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|year| u16::try_from(year).ok())
            .filter(|year| (2000..=today.year() as u16 + 1).contains(year))
            .map(Some)
            .ok_or_else(|| format!("`year` must be a tax year such as {} (got {value})", today.year())),
    }
}

fn form_title(code: &str) -> &'static str {
    bir_core::forms::find_form(code).map(|form| form.title).unwrap_or("")
}

fn frequency(code: &str) -> &'static str {
    taxform::cadence(code).map(Cadence::as_str).unwrap_or("")
}

/// `bir_profile_forms {ticket, year?}`.
pub(crate) fn profile_forms(
    db: &Database,
    arguments: &Map<String, Value>,
    today: NaiveDate,
) -> Result<Value, String> {
    check_keys(arguments, &["ticket", "year"])?;
    let branch = branch_of(db, arguments)?;
    let year = year_of(arguments, today)?.unwrap_or(today.year() as u16);
    let mut codes: Vec<&'static str> = Vec::new();
    for raw in db
        .active_form_codes_for_year(&branch.tin, year)
        .map_err(|error| error.to_string())?
    {
        if let Some(code) = taxform::form_code_of(&raw)
            && !codes.contains(&code)
        {
            codes.push(code);
        }
    }
    // Each period of the year, as the calendar has it (due dates run into
    // the next year for the last quarter and the annual return).
    let mut window = Map::new();
    window.insert("tin".into(), json!(branch.tin));
    window.insert("from".into(), json!(format!("{year}-01-01")));
    window.insert("to".into(), json!(format!("{}-12-31", year + 1)));
    let events = calendar::get(db, &window, today)
        .map(|got| got["events"].as_array().cloned().unwrap_or_default())
        .unwrap_or_default();
    let forms: Vec<Value> = codes
        .iter()
        .map(|code| {
            let periods: Vec<Value> = events
                .iter()
                .filter(|event| event["form"] == *code)
                .filter(|event| {
                    event["period"]
                        .as_str()
                        .is_some_and(|period| period.starts_with(&year.to_string()))
                })
                .map(|event| {
                    json!({
                        "period": event["period"],
                        "label": event["label"],
                        "due": event["due"],
                        "status": event["status"],
                        "overdue": event["overdue"],
                    })
                })
                .collect();
            json!({
                "code": code,
                "title": form_title(code),
                "frequency": frequency(code),
                "periods": periods,
            })
        })
        .collect();
    let as_needed: Vec<Value> = bir_core::forms::agent::AgentFormKind::all()
        .into_iter()
        .map(|kind| kind.form_code())
        .filter(|code| matches!(taxform::cadence(code), Some(Cadence::AsNeeded)))
        .map(|code| json!({ "code": code, "title": form_title(code) }))
        .collect();
    let years = db.list_forms_set_years(&branch.tin).unwrap_or_default();
    Ok(json!({
        "taxpayer": branch.name,
        "year": year,
        "years_with_forms": years,
        "registered": forms,
        "as_needed": as_needed,
        "note": if codes.is_empty() {
            "No forms are registered on this profile for that year. They are chosen in the BIR Tax Profile app (Forms tab)."
        } else {
            "registered: the forms chosen on the profile for the year, each period with its status (not_started, draft, queued, filed, confirmed, paid). as_needed: forms filed only when something happens."
        },
    }))
}

/// `bir_returns_list {ticket, year?, form?}`.
pub(crate) fn returns_list(
    db: &Database,
    arguments: &Map<String, Value>,
    today: NaiveDate,
) -> Result<Value, String> {
    check_keys(arguments, &["ticket", "year", "form", "period"])?;
    let branch = branch_of(db, arguments)?;
    let year = year_of(arguments, today)?;
    let form = arguments.get("form").and_then(Value::as_str);
    // One return: its form and period.
    let period = match (form, arguments.get("period").and_then(Value::as_str)) {
        (Some(form), Some(raw)) => {
            let code = taxform::form_code_of(form).ok_or_else(|| format!("`form` `{form}` is not a BIR form code"))?;
            let named = taxform::parse(raw, today)
                .period
                .ok_or_else(|| format!("`period` `{raw}` is not a period (e.g. 2026-Q3, 2026-09, 2025)"))?;
            Some(named.for_form(code, today).map_err(|ask| ask.to_string())?.canonical())
        }
        (None, Some(_)) => return Err("`period` needs `form`".into()),
        _ => None,
    };
    let stored = resolve::drafts_list(db, Some(&branch.tin), form, true, today)?;
    let mut paid_args = Map::new();
    paid_args.insert("tin".into(), json!(branch.tin));
    let paid: Vec<String> = payments::list(db, &paid_args, today)
        .map(|listed| {
            listed["payments"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|payment| payment["token"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let returns: Vec<Value> = stored["drafts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| period.as_ref().is_none_or(|period| row["period"] == *period))
        .filter(|row| {
            year.is_none_or(|year| {
                row["period"]
                    .as_str()
                    .is_some_and(|period| period.starts_with(&year.to_string()))
            })
        })
        .map(|row| {
            let status = row["status"].as_str().unwrap_or("");
            let filed = status != format!("{:?}", FilingStatus::Draft)
                && status != format!("{:?}", FilingStatus::Queued);
            json!({
                "form": row["form"],
                "title": form_title(row["form"].as_str().unwrap_or("")),
                "period": row["period"],
                "label": row["label"],
                "status": status,
                "filed": filed,
                "paid": status == format!("{:?}", FilingStatus::Paid)
                    || row["token"].as_str().is_some_and(|token| paid.iter().any(|p| p == token)),
                "last_saved": row["updated_at"],
                "token": row["token"],
            })
        })
        .collect();
    Ok(json!({
        "taxpayer": branch.name,
        "year": year,
        "returns": returns,
    }))
}

/// The two tools' `tools/list` entries.
pub(crate) fn descriptors() -> Vec<Value> {
    vec![
        json!({
            "name": PROFILE_FORMS,
            "description": "What forms a taxpayer is registered for: the BIR forms chosen on their tax profile for a year (code, title, monthly/quarterly/annual), each period's due date and status (not_started, draft, queued, filed, confirmed, paid), and the as-needed forms they can file when something happens. Use it to answer \"what forms is she registered for\", \"what does he file\", \"what is due for her this year\". `ticket`: the taxpayer's ticket (from @bir:taxprofile). `year`: the tax year (default this year). Read-only.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ticket": { "type": "string", "description": "The taxpayer's ticket." },
                    "year": { "type": "integer", "description": "Tax year, e.g. 2026." }
                },
                "required": ["ticket"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": RETURNS_LIST,
            "description": "The returns a taxpayer has in BIR: each form and period with its status (Draft, Queued, Submitted = sent to BIR, Confirmed by BIR's email, Paid), whether it was filed and whether payment is recorded, newest first. Use it to answer \"what returns has she filed\", \"is the Q3 2551Q filed\", \"what is still a draft\". `ticket`: the taxpayer's ticket (from @bir:taxprofile); `year` and `form` (e.g. 2551Q) narrow the list. Read-only.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ticket": { "type": "string", "description": "The taxpayer's ticket." },
                    "year": { "type": "integer", "description": "Tax year, e.g. 2026." },
                    "form": { "type": "string", "description": "A form code, e.g. 2551Q." },
                    "period": { "type": "string", "description": "With `form`: one return's period (2026-Q3, 2026-09), for \"did her 2551Q go through?\"." }
                },
                "required": ["ticket"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true },
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use bir_core::profile::TaxpayerType;
    use bir_core::tax_profile::{BranchFacts, NewTaxpayer};

    const ROOT: &str = "123456788";

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 26).unwrap()
    }

    fn db() -> Database {
        let db = Database::open_ephemeral().expect("ephemeral database");
        db.create_taxpayer(NewTaxpayer {
            tin_root: ROOT.into(),
            name: "Dela Cruz Jane".into(),
            taxpayer_type: TaxpayerType::Individual,
            birth_date: None,
            head_office: BranchFacts { rdo_code: "047".into(), ..BranchFacts::default() },
        })
        .unwrap();
        db.set_yearly_forms(&format!("{ROOT}00000"), 2026, &["2551Q", "1701Q"]).unwrap();
        db
    }

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn profile_forms_lists_the_registered_forms_with_their_periods() {
        let db = db();
        let ticket = tickets::issue(&db, &tickets::Subject::Taxpayer(ROOT.into())).unwrap();
        let got = profile_forms(&db, &args(json!({"ticket": ticket, "year": 2026})), today()).unwrap();
        assert_eq!(got["taxpayer"], "Dela Cruz Jane");
        let codes: Vec<&str> = got["registered"].as_array().unwrap().iter().map(|f| f["code"].as_str().unwrap()).collect();
        assert!(codes.contains(&"2551Q") && codes.contains(&"1701Q"), "{got}");
        let q = got["registered"].as_array().unwrap().iter().find(|f| f["code"] == "2551Q").unwrap();
        assert_eq!(q["frequency"], "quarterly");
        assert!(!q["periods"].as_array().unwrap().is_empty(), "{q}");
        assert!(!got["as_needed"].as_array().unwrap().is_empty());
        assert!(!got.to_string().contains(ROOT));
    }

    #[test]
    fn both_refuse_a_tin_or_an_unissued_ticket() {
        let db = db();
        for bad in [json!({"ticket": ROOT}), json!({"ticket": format!("tkt_{}", "0".repeat(26))}), json!({})] {
            assert!(profile_forms(&db, &args(bad.clone()), today()).is_err(), "{bad}");
            assert!(returns_list(&db, &args(bad.clone()), today()).is_err(), "{bad}");
        }
    }

    #[test]
    fn returns_list_answers_for_the_ticket() {
        let db = db();
        let ticket = tickets::issue(&db, &tickets::Subject::Taxpayer(ROOT.into())).unwrap();
        let got = returns_list(&db, &args(json!({"ticket": ticket, "year": 2026})), today()).unwrap();
        assert_eq!(got["taxpayer"], "Dela Cruz Jane");
        assert!(got["returns"].is_array());
    }
}
