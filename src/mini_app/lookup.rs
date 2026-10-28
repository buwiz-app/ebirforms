//! Reference answers for the assistant's cards (spec 9.8): what a form is
//! (`bir_form_info`, the explain card) and one lookup tool for an ATC, an
//! RDO or a quick tax on an amount (`bir_lookup`, the atc, rdo and rate
//! cards). Both only read: BIR's own form registry, the official form
//! packages' workflow notes (due rule, attachments; `form_notes.json`,
//! generated from `rules/forms/*/workflow.json`) and the ATC and RDO tables.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use bir_core::db::Database;
use bir_core::forms::taxform;
use chrono::{Datelike, NaiveDate};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::v1::tickets;
use super::{calendar, resolve};

pub(crate) const FORM_INFO: &str = "bir_form_info";
pub(crate) const LOOKUP: &str = "bir_lookup";
pub(crate) const TOOLS: [&str; 2] = [FORM_INFO, LOOKUP];

#[derive(Deserialize)]
pub(crate) struct Attachment {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub when: String,
}

#[derive(Deserialize)]
pub(crate) struct FormNotes {
    #[serde(default)]
    pub due: Vec<String>,
    #[serde(default)]
    pub attach: Vec<Attachment>,
}

fn key(code: &str) -> String {
    code.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_ascii_uppercase()
}

/// The official package's notes of form `code`.
pub(crate) fn notes(code: &str) -> Option<&'static FormNotes> {
    static NOTES: OnceLock<BTreeMap<String, FormNotes>> = OnceLock::new();
    NOTES
        .get_or_init(|| {
            let raw: BTreeMap<String, FormNotes> =
                serde_json::from_str(include_str!("form_notes.json")).expect("form_notes.json parses");
            raw.into_iter().map(|(code, notes)| (key(&code), notes)).collect()
        })
        .get(&key(code))
}

fn check_keys(arguments: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    match arguments.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => Err(format!("unknown argument `{key}` (allowed: {})", allowed.join(", "))),
        None => Ok(()),
    }
}

fn text<'a>(arguments: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    arguments.get(key).and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty())
}

/// What BIR's tables say the rate of a percentage-tax ATC is (2551Q).
fn pt_rate(code: &str) -> Option<f64> {
    bir_core::forms::atc::find_atc(code).map(|entry| entry.rate * 100.0)
}

// ------------------------------------------------------------ form info

/// `bir_form_info {form, ticket?}`.
pub(crate) fn form_info(db: &Database, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Value, String> {
    check_keys(arguments, &["form", "ticket"])?;
    let raw = text(arguments, "form").ok_or("`form` is required: a form code such as 2551Q")?;
    let code = taxform::form_code_of(raw).ok_or_else(|| format!("`form` `{raw}` is not a BIR form code"))?;
    let def = bir_core::forms::find_form(code).ok_or_else(|| format!("BIR has no form {code}"))?;
    let mut who: Vec<String> = def.taxpayer_types.iter().map(|kind| format!("{kind:?}").to_lowercase()).collect();
    if who.len() > 1 {
        let last = who.pop().unwrap_or_default();
        who = vec![format!("{} and {last}", who.join(", "))];
    }
    let mut who = format!("{} taxpayers", who.join(""));
    match def.requires_vat {
        Some(true) => who.push_str(" registered for VAT"),
        Some(false) => who.push_str(" not registered for VAT"),
        None => {}
    }
    if def.requires_employees {
        who.push_str(" who withhold tax (for example on employees' pay)");
    }
    let notes = notes(code);
    let rate = match code {
        "2551Q" => pt_rate("PT010").map(|rate| format!("{rate}% of gross sales or receipts for most non-VAT businesses (PT010); other ATCs have their own rates")),
        _ => None,
    };
    // For one taxpayer: is it on their forms this year, and the period to start.
    let mut taxpayer = Value::Null;
    if let Some(ticket) = text(arguments, "ticket") {
        let subject = tickets::subject(db, ticket)?.ok_or("the ticket is not one BIR issued, or it was revoked")?;
        let branch = resolve::branches(db, Some(&subject.full_tin()))?
            .into_iter()
            .next()
            .ok_or("this taxpayer is no longer saved in BIR")?;
        let files = db
            .active_form_codes_for_year(&branch.tin, today.year() as u16)
            .unwrap_or_default()
            .iter()
            .any(|raw| taxform::form_code_of(raw) == Some(code));
        let period = bir_core::calendar_rules::DeadlineResolver::last_closed_period(code, today)
            .and_then(|period| calendar::period_of(&period));
        taxpayer = json!({
            "name": branch.name,
            "files_it": files,
            "period": period.map(taxform::TaxPeriod::canonical),
            "period_label": period.map(taxform::TaxPeriod::label),
        });
    }
    Ok(json!({
        "form": code,
        "title": def.title,
        "category": def.category,
        "frequency": taxform::cadence(code).map(|cadence| cadence.as_str()).unwrap_or(""),
        "who": who,
        "rate": rate,
        "due": notes.map(|notes| notes.due.clone()).unwrap_or_default(),
        "attachments": notes.map(|notes| notes.attach.iter().map(|a| json!({ "label": a.label, "when": a.when })).collect::<Vec<_>>()).unwrap_or_default(),
        "deprecated": def.is_deprecated,
        "taxpayer": taxpayer,
        "note": "From BIR's form registry and the official form package (due rule, attachments). Not tax advice: the official instructions govern.",
    }))
}

// ------------------------------------------------------------ lookup

/// The forms whose ATC list has `code`.
fn atc_forms(code: &str) -> Vec<&'static str> {
    use bir_core::forms::*;
    let mut forms = Vec::new();
    if atc::find_atc(code).is_some() {
        forms.push("2551Q");
    }
    if form_1601eq::FORM_1601EQ_ATC_OPTIONS.iter().any(|o| o.code == code) {
        forms.extend(["0619E", "1601EQ"]);
    }
    if form_1601fq::FORM_1601FQ_PRIVATE_ATCS.iter().chain(form_1601fq::FORM_1601FQ_GOVERNMENT_ATCS).any(|o| o.code == code) {
        forms.extend(["0619F", "1601FQ"]);
    }
    if form_1600pt::FORM_1600PT_ATC_OPTIONS.iter().any(|o| o.code == code) {
        forms.push("1600PT");
    }
    if form_1600vt::FORM_1600VT_ATC_OPTIONS.iter().any(|o| o.code == code) {
        forms.push("1600VT");
    }
    if form_1600wp::FORM_1600WP_ATC_OPTIONS.iter().any(|o| o.code == code) {
        forms.push("1600WP");
    }
    if form_1602q::FORM_1602Q_SCHEDULE2_ATCS.contains(&code) {
        forms.push("1602Q");
    }
    if form_0605_official::FORM_0605_ATCS.iter().any(|(atc, _)| *atc == code) {
        forms.push("0605");
    }
    forms
}

/// An ATC's description and rate (percent).
fn atc(code: &str) -> Option<(String, Option<f64>)> {
    use bir_core::forms::*;
    if let Some(entry) = bir_core::reference::get_atc(code) {
        return Some((entry.description, entry.rate.trim().parse().ok()));
    }
    if let Some(entry) = atc::find_atc(code) {
        return Some((entry.description.to_string(), Some(entry.rate * 100.0)));
    }
    if let Some(option) = form_1601eq::FORM_1601EQ_ATC_OPTIONS.iter().find(|o| o.code == code) {
        return Some((option.description.to_string(), option.rate_text.parse().ok()));
    }
    form_1601fq::FORM_1601FQ_PRIVATE_ATCS
        .iter()
        .chain(form_1601fq::FORM_1601FQ_GOVERNMENT_ATCS)
        .find(|o| o.code == code)
        .map(|option| (option.description.to_string(), option.rate_text.parse().ok()))
}

/// Tidy BIR's all-caps descriptions for reading.
fn sentence(text: &str) -> String {
    let squeezed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if squeezed.chars().filter(|c| c.is_alphabetic()).all(|c| c.is_uppercase()) {
        let lower = squeezed.to_lowercase();
        let mut chars = lower.chars();
        chars.next().map(|first| first.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
    } else {
        squeezed
    }
}

fn amount(raw: &Value) -> Option<f64> {
    match raw {
        Value::Number(n) => n.as_f64(),
        Value::String(text) => {
            let clean: String = text.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
            clean.parse().ok()
        }
        _ => None,
    }
}

fn cents(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// `bir_lookup {atc? | rdo? | amount and rate (a percent or an ATC)}`.
pub(crate) fn lookup(db: &Database, arguments: &Map<String, Value>, _today: NaiveDate) -> Result<Value, String> {
    check_keys(arguments, &["atc", "rdo", "amount", "rate"])?;
    if let Some(raw) = arguments.get("amount") {
        let base = amount(raw).ok_or_else(|| format!("`amount` must be pesos (got {raw})"))?;
        let rate_raw = text(arguments, "rate").map(str::to_string).or_else(|| arguments.get("rate").and_then(Value::as_f64).map(|r| r.to_string()));
        let rate_raw = rate_raw.ok_or("`rate` is required with `amount`: a percent (3) or an ATC (PT010)")?;
        let upper = rate_raw.trim().to_ascii_uppercase();
        let (rate, rule, code) = if upper.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            let (description, rate) = atc(&upper).ok_or_else(|| format!("`{rate_raw}` is not an ATC BIR knows"))?;
            let rate = rate.ok_or_else(|| format!("{upper} has no single rate in BIR's tables"))?;
            (rate, format!("{upper}: {}", sentence(&description)), Some(upper.clone()))
        } else {
            let rate: f64 = upper.trim_end_matches('%').trim().parse().map_err(|_| format!("`rate` must be a percent such as 3 (got `{rate_raw}`)"))?;
            let rate = if rate > 0.0 && rate < 1.0 && upper.contains('.') && !upper.contains('%') { rate * 100.0 } else { rate };
            let rule = if (rate - 3.0).abs() < 1e-9 {
                "3% percentage tax on gross sales or receipts (PT010, Sec. 116), the rate for most non-VAT businesses. It was 1% from July 2020 to June 2023."
                    .to_string()
            } else if (rate - 12.0).abs() < 1e-9 {
                "12% VAT on the amount (exclusive of VAT).".to_string()
            } else {
                format!("{rate}% of the amount.")
            };
            (rate, rule, None)
        };
        if !(0.0..=100.0).contains(&rate) {
            return Err("`rate` must be between 0 and 100 percent".into());
        }
        return Ok(json!({
            "mode": "rate",
            "amount": cents(base),
            "rate": rate,
            "tax": cents(base * rate / 100.0),
            "atc": code,
            "rule": rule,
            "note": "A quick computation, not a return: the return's own rules (period, ATC, deductions) decide the tax.",
        }));
    }
    if let Some(raw) = text(arguments, "atc") {
        let code = raw.to_ascii_uppercase().replace([' ', '-'], "");
        let (description, rate) = atc(&code).ok_or_else(|| format!("`{raw}` is not an ATC in BIR's tables"))?;
        return Ok(json!({
            "mode": "atc",
            "code": code,
            "covers": sentence(&description),
            "rate": rate,
            "forms": atc_forms(&code),
        }));
    }
    if let Some(raw) = text(arguments, "rdo") {
        let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
        let matches: Vec<bir_core::reference::Rdo> = if !digits.is_empty() && digits.len() <= 3 {
            bir_core::reference::get_rdo(&format!("{digits:0>3}"))
                .or_else(|| bir_core::reference::get_rdo(&raw.to_ascii_uppercase()))
                .into_iter()
                .collect()
        } else {
            let wanted = raw.to_lowercase();
            bir_core::reference::get_all_rdos()
                .into_iter()
                .filter(|rdo| rdo.description.to_lowercase().contains(&wanted) || rdo.code.eq_ignore_ascii_case(raw))
                .take(8)
                .collect()
        };
        if matches.is_empty() {
            return Err(format!("no RDO matches `{raw}`"));
        }
        // How many saved taxpayers are registered there.
        let branches = resolve::branches(db, None).unwrap_or_default();
        let rdos: Vec<String> = branches
            .iter()
            .filter(|branch| branch.is_head_office)
            .map(|branch| resolve::who_rdo(db, &branch.tin).unwrap_or_default())
            .collect();
        return Ok(json!({
            "mode": "rdo",
            "matches": matches.iter().map(|rdo| json!({
                "code": rdo.code,
                "office": rdo.description,
                "taxpayers": rdos.iter().filter(|code| **code == rdo.code).count(),
            })).collect::<Vec<_>>(),
        }));
    }
    Err("name `atc`, `rdo`, or `amount` with `rate`".into())
}

/// The tools' `tools/list` entries.
pub(crate) fn descriptors() -> Vec<Value> {
    vec![
        json!({
            "name": FORM_INFO,
            "description": "What a BIR form is, in plain words: its title, who files it, how often, when it is due, the rate where BIR's tables have it, and what to attach, from BIR's form registry and the official form package. With the taxpayer's `ticket`, whether it is on their forms and the period to start. Use it for \"what's 2551Q?\", \"what do I attach to 1701Q?\".",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "form": { "type": "string", "description": "Form code, e.g. 2551Q." },
                    "ticket": { "type": "string", "description": "The taxpayer's ticket, to offer starting one." }
                },
                "required": ["form"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true },
        }),
        json!({
            "name": LOOKUP,
            "description": "Look up BIR reference data or do a quick tax sum. `atc` (WC158): what the code covers, its rate and the forms that use it. `rdo` (040, or a place name): the revenue district office and how many saved taxpayers are registered there. `amount` with `rate` (a percent, 3, or an ATC, PT010): the tax on it and the rule used (\"3% of 186,000?\"). Rox shows the answer as a card.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "atc": { "type": "string" },
                    "rdo": { "type": "string" },
                    "amount": { "type": ["number", "string"], "description": "Pesos." },
                    "rate": { "type": ["number", "string"], "description": "A percent (3) or an ATC (PT010)." }
                },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true },
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_form_has_its_package_notes_and_atcs_name_their_forms() {
        for kind in bir_core::forms::agent::AgentFormKind::all() {
            assert!(notes(kind.form_code()).is_some(), "{}", kind.form_code());
        }
        assert!(atc_forms("WC158").contains(&"1601EQ"));
        assert_eq!(atc("WC158").unwrap().1, Some(1.0));
        assert!(atc_forms("PT010").contains(&"2551Q"));
        assert_eq!(sentence("VAT WITHHOLDING  ON  PURCHASE OF GOODS"), "Vat withholding on purchase of goods");
    }
}
