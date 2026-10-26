//! The form calendar: each taxpayer's returns and when they are due, which
//! forms are on the calendar, and what changed for a Google Calendar sync.
//!
//! Built on what BIR already keeps:
//! - the dues are bir-core's official calendar rules
//!   ([`DeadlineResolver::deadlines_for_forms`]) for the forms in each
//!   year's Forms Set, with the same overrides and profile filter the
//!   desktop's Google Calendar sync uses;
//! - which forms are on the calendar is the desktop's per-taxpayer calendar
//!   form selection ([`google_calendar::calendar_form_selection`]), so a
//!   form turned off here is off there too;
//! - a return's status is its stored return's (draft, queued, filed,
//!   confirmed, paid).
//!
//! Added here: reminder days per taxpayer, and the sync record behind
//! `bir_calendar_changes` / `bir_calendar_link` (each event's last synced
//! content and its Google event id), kept in the database's settings. The
//! sync itself is the agent's, through the Google Workspace server.

use std::collections::BTreeMap;

use bir_core::calendar_rules::{DeadlineKind, DeadlinePeriod, DeadlineResolver, DeadlineStatus};
use bir_core::db::Database;
use bir_core::forms::FilingStatus;
use bir_core::forms::taxform::{self, TaxPeriod};
use bir_core::google_calendar::{calendar_form_selection, save_calendar_form_selection};
use chrono::{Datelike, NaiveDate};
use serde_json::{Map, Value, json};

use super::resolve::{self, Branch};

/// Settings key of the sync record: event key -> what was last reported.
const SYNC_KEY: &str = "mini_app.calendar.sync";
/// Settings key prefix of a branch's reminder days.
const REMINDERS_PREFIX: &str = "mini_app.calendar.reminders.";
/// The desktop calendar's reminders: a week and a day before.
const DEFAULT_REMINDER_DAYS: [u32; 2] = [7, 1];

/// One return on the calendar.
struct Event {
    /// `<tin>/<FORM>-<period>`: the return token's value, stable per
    /// (taxpayer, form, period).
    key: String,
    tin: String,
    taxpayer: String,
    form: String,
    period: TaxPeriod,
    due: NaiveDate,
    status: &'static str,
    status_detail: String,
    enabled: bool,
    draft_id: Option<i64>,
    reminder_days: Vec<u32>,
    /// The rule's own date, and what a deadline override that moved it
    /// cites (`bir_deadline_override_set`'s reason, a BIR advisory).
    rule_due: NaiveDate,
    moved_reason: Option<String>,
}

impl Event {
    fn form_name(&self) -> &'static str {
        bir_core::forms::find_form(&self.form)
            .map(|form| form.title)
            .unwrap_or("")
    }

    fn label(&self) -> String {
        format!("{} · {}", self.form, self.period.label())
    }

    fn filed(&self) -> bool {
        matches!(self.status, "filed" | "confirmed" | "paid")
    }

    /// What `bir_calendar_get` lists.
    fn json(&self, today: NaiveDate) -> Value {
        let mut event = json!({
            "key": self.key,
            "tin": self.tin,
            "taxpayer": self.taxpayer,
            "form": self.form,
            "form_name": self.form_name(),
            "period": self.period.canonical(),
            "label": self.label(),
            "due": self.due.format("%Y-%m-%d").to_string(),
            "rule_due": self.rule_due.format("%Y-%m-%d").to_string(),
            "moved": self.moved_reason.is_some(),
            "moved_reason": self.moved_reason,
            "status": self.status,
            "status_detail": self.status_detail,
            "overdue": !self.filed() && self.due < today,
            "enabled": self.enabled,
            "reminder_days": self.reminder_days,
            "token": format!("{}{}", taxform::TOKEN_PREFIX, self.key),
        });
        if taxform::form_code_of(&self.form).is_some() {
            let mut props = json!({
                "tin": self.tin,
                "form": self.form,
                "period": self.period.canonical(),
                "action": "file",
            });
            if let Some(id) = self.draft_id {
                props["draft_id"] = json!(id);
            }
            event["open"] = json!(resolve::app_link("forms", &props));
        }
        event
    }

    /// What a Google Calendar event is made from: an all-day event on
    /// `due`, its title, description and reminders. Only what should move
    /// the Google event is here (not the day-to-day `overdue`).
    fn sync_json(&self) -> Value {
        let filed = if self.filed() { "[Filed] " } else { "" };
        let title = format!("{filed}[BIR] {} · {}", self.label(), self.taxpayer);
        let last4: String = self.tin.chars().skip(self.tin.len().saturating_sub(4)).collect();
        let description = format!(
            "Taxpayer: {} (TIN ending {last4})\nForm: {} {}\nPeriod: {}\nDue: {}\nStatus: {}\nIn Rox: {}{}",
            self.taxpayer,
            self.form,
            self.form_name(),
            self.period.label(),
            TaxPeriod::Day(self.due).label(),
            self.status_detail_for_sync(),
            taxform::TOKEN_PREFIX,
            self.key,
        );
        json!({
            "key": self.key,
            "tin": self.tin,
            "taxpayer": self.taxpayer,
            "form": self.form,
            "period": self.period.canonical(),
            "due": self.due.format("%Y-%m-%d").to_string(),
            "status": self.status,
            "title": title,
            "description": description,
            "reminder_days": self.reminder_days,
        })
    }

    /// The status without a draft's save date, which changes on every save.
    fn status_detail_for_sync(&self) -> String {
        match self.status {
            "draft" => "Draft".to_string(),
            _ => self.status_detail.clone(),
        }
    }
}

/// The period a calendar rule's deadline is for.
fn period_of(period: &DeadlinePeriod) -> Option<TaxPeriod> {
    Some(match *period {
        DeadlinePeriod::Monthly {
            taxable_year,
            month,
        } => TaxPeriod::Month {
            year: u16::try_from(taxable_year).ok()?,
            month,
        },
        DeadlinePeriod::Quarterly {
            taxable_year,
            quarter,
        } => TaxPeriod::Quarter {
            year: u16::try_from(taxable_year).ok()?,
            quarter,
        },
        DeadlinePeriod::Annual { taxable_year } => TaxPeriod::Year(u16::try_from(taxable_year).ok()?),
        DeadlinePeriod::EventBased => return None,
    })
}

fn status_word(status: Option<&FilingStatus>) -> &'static str {
    match status {
        None => "not_started",
        Some(FilingStatus::Draft) => "draft",
        Some(FilingStatus::Queued) => "queued",
        Some(FilingStatus::Submitted) => "filed",
        Some(FilingStatus::Confirmed) => "confirmed",
        Some(FilingStatus::Paid) => "paid",
    }
}

/// A branch's reminder days, the desktop's week and day by default.
fn reminder_days(db: &Database, tin: &str) -> Vec<u32> {
    db.get_setting(&format!("{REMINDERS_PREFIX}{tin}"))
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_else(|| DEFAULT_REMINDER_DAYS.to_vec())
}

/// The due date of `code`'s return for `period` (its final deadline, after
/// any official extension), when the calendar rules date it.
pub(crate) fn due_date(db: &Database, tin: &str, code: &str, period: TaxPeriod) -> Option<NaiveDate> {
    let global = db.get_deadline_overrides();
    let overrides = match db.get_profile(tin).ok().flatten() {
        Some(profile) => bir_core::forms::event_trigger::periodic_overrides(
            &global,
            &profile,
            Some(period.year()),
        ),
        None => global,
    };
    DeadlineResolver::deadlines_for_forms(&[code.to_string()], i32::from(period.year()), &overrides)
        .into_iter()
        .find(|deadline| period_of(&deadline.period) == Some(period))
        .and_then(|deadline| deadline.final_deadline_date())
}

/// Every dated return of `branch`'s Forms Sets due between `from` and `to`.
fn branch_events(
    db: &Database,
    branch: &Branch,
    from: NaiveDate,
    to: NaiveDate,
    today: NaiveDate,
) -> Result<Vec<Event>, String> {
    let Some(profile) = db
        .get_profile(&branch.tin)
        .map_err(|error| error.to_string())?
    else {
        return Ok(Vec::new());
    };
    let selection = calendar_form_selection(db, &branch.tin);
    let reminders = reminder_days(db, &branch.tin);
    let global = db.get_deadline_overrides();
    let mut returns: BTreeMap<String, Vec<(TaxPeriod, bir_core::forms::FormDraftSummary)>> =
        BTreeMap::new();
    let mut out = Vec::new();
    for year in db
        .list_forms_set_years(&branch.tin)
        .map_err(|error| error.to_string())?
    {
        // A year's returns fall due that year or the next.
        if i32::from(year) > to.year() || i32::from(year) + 1 < from.year() {
            continue;
        }
        let codes = db
            .active_form_codes_for_year(&branch.tin, year)
            .map_err(|error| error.to_string())?;
        let overrides =
            bir_core::forms::event_trigger::periodic_overrides(&global, &profile, Some(year));
        for deadline in DeadlineResolver::deadlines_for_forms(&codes, i32::from(year), &overrides) {
            if !bir_core::integration::deadline_applies_to_profile(&profile, &deadline) {
                continue;
            }
            let DeadlineKind::Dated {
                final_deadline,
                original_deadline,
            } = deadline.deadline
            else {
                continue;
            };
            let moved_reason = (deadline.status == DeadlineStatus::Extended)
                .then(|| deadline.source_reference.clone().unwrap_or_default());
            if final_deadline < from || final_deadline > to {
                continue;
            }
            let Some(period) = period_of(&deadline.period) else {
                continue;
            };
            let code = deadline.form_code.clone();
            let stored = returns
                .entry(code.clone())
                .or_insert_with(|| resolve::stored_returns(db, &branch.tin, &code, today));
            let filing = resolve::furthest(stored, &code, period);
            out.push(Event {
                key: taxform::canonical_value(&branch.tin, &code, period),
                tin: branch.tin.clone(),
                taxpayer: branch.name.clone(),
                enabled: selection.allows(&code),
                status: status_word(filing.map(|filing| &filing.status)),
                status_detail: resolve::status_detail(db, filing),
                draft_id: filing.map(|filing| filing.id),
                form: code,
                period,
                due: final_deadline,
                reminder_days: reminders.clone(),
                rule_due: original_deadline,
                moved_reason,
            });
        }
    }
    Ok(out)
}

fn events(
    db: &Database,
    tin: Option<&str>,
    from: NaiveDate,
    to: NaiveDate,
    today: NaiveDate,
) -> Result<Vec<Event>, String> {
    let mut out = Vec::new();
    for branch in resolve::branches(db, tin)? {
        out.extend(branch_events(db, &branch, from, to, today)?);
    }
    out.sort_by(|a, b| {
        (a.due, &a.taxpayer, &a.tin, &a.form).cmp(&(b.due, &b.taxpayer, &b.tin, &b.form))
    });
    Ok(out)
}

/// One branch's calendar settings: its forms (every year's Forms Set,
/// newest first), each on or off, and its reminder days.
fn schedule(db: &Database, branch: &Branch) -> Value {
    let selection = calendar_form_selection(db, &branch.tin);
    let mut codes: Vec<String> = Vec::new();
    for year in db.list_forms_set_years(&branch.tin).unwrap_or_default() {
        for code in db
            .active_form_codes_for_year(&branch.tin, year)
            .unwrap_or_default()
        {
            if !codes.contains(&code) {
                codes.push(code);
            }
        }
    }
    codes.sort();
    let forms: Vec<Value> = codes
        .iter()
        .map(|code| {
            json!({
                "form": code,
                "name": bir_core::forms::find_form(code).map(|form| form.title).unwrap_or(""),
                "cadence": taxform::cadence(code).map(|cadence| cadence.as_str()).unwrap_or(""),
                "enabled": selection.allows(code),
            })
        })
        .collect();
    json!({
        "tin": branch.tin,
        "taxpayer": branch.name,
        "head_office": branch.is_head_office,
        "reminder_days": reminder_days(db, &branch.tin),
        "forms": forms,
    })
}

fn check_keys(arguments: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
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

fn text<'a>(arguments: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(Some(text.trim())),
        Some(Value::String(_)) => Ok(None),
        Some(other) => Err(format!("`{key}` must be a string (got {other})")),
    }
}

fn date(arguments: &Map<String, Value>, key: &str) -> Result<Option<NaiveDate>, String> {
    text(arguments, key)?
        .map(|raw| {
            NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                .map_err(|_| format!("`{key}` must be a date like 2026-10-31 (got `{raw}`)"))
        })
        .transpose()
}

/// `bir_calendar_get {tin?, from?, to?}`.
pub(crate) fn get(
    db: &Database,
    arguments: &Map<String, Value>,
    today: NaiveDate,
) -> Result<Value, String> {
    check_keys(arguments, &["tin", "from", "to"])?;
    let tin = text(arguments, "tin")?;
    let from = date(arguments, "from")?
        .or_else(|| NaiveDate::from_ymd_opt(today.year(), 1, 1))
        .unwrap_or(today);
    let to = date(arguments, "to")?
        .or_else(|| NaiveDate::from_ymd_opt(today.year() + 1, 12, 31))
        .unwrap_or(today);
    if to < from {
        return Err("`to` is before `from`".into());
    }
    let listed = events(db, tin, from, to, today)?;
    let taxpayers: Vec<Value> = resolve::branches(db, tin)?
        .iter()
        .map(|branch| schedule(db, branch))
        .collect();
    Ok(json!({
        "from": from.format("%Y-%m-%d").to_string(),
        "to": to.format("%Y-%m-%d").to_string(),
        "today": today.format("%Y-%m-%d").to_string(),
        "events": listed.iter().map(|event| event.json(today)).collect::<Vec<_>>(),
        "taxpayers": taxpayers,
    }))
}

/// A form code as the Forms Set keeps it: `1601-C` -> `1601C`.
fn form_key(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase()
}

/// `bir_calendar_set {tin, forms, enabled, reminder_days?}`.
pub(crate) fn set(db: &Database, arguments: &Map<String, Value>) -> Result<Value, String> {
    check_keys(arguments, &["tin", "forms", "enabled", "reminder_days"])?;
    let tin = text(arguments, "tin")?.ok_or("`tin` is required: the taxpayer's TIN")?;
    let branch = resolve::branches(db, Some(tin))?
        .into_iter()
        .next()
        .ok_or_else(|| format!("no taxpayer with TIN {tin}"))?;
    let forms: Vec<String> = match arguments.get("forms") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(forms)) => forms
            .iter()
            .map(|form| {
                form.as_str()
                    .map(form_key)
                    .filter(|code| !code.is_empty())
                    .ok_or_else(|| format!("`forms` must be form codes such as 2551Q (got {form})"))
            })
            .collect::<Result<_, _>>()?,
        Some(other) => return Err(format!("`forms` must be a list of form codes (got {other})")),
    };
    let enabled = match arguments.get("enabled") {
        None | Some(Value::Null) if forms.is_empty() => None,
        Some(Value::Bool(enabled)) => Some(*enabled),
        _ => return Err("`enabled` must be true or false".into()),
    };
    let days = match arguments.get("reminder_days") {
        None | Some(Value::Null) => None,
        Some(Value::Array(days)) => Some(
            days.iter()
                .map(|day| {
                    day.as_u64()
                        .filter(|day| *day <= 60)
                        .map(|day| day as u32)
                        .ok_or_else(|| {
                            format!("`reminder_days` must be whole days from 0 to 60 (got {day})")
                        })
                })
                .collect::<Result<Vec<u32>, String>>()?,
        ),
        Some(other) => return Err(format!("`reminder_days` must be a list of days (got {other})")),
    };
    if forms.is_empty() && days.is_none() {
        return Err("name `forms` with `enabled`, or `reminder_days`".into());
    }
    if let Some(enabled) = enabled {
        let mut selection = calendar_form_selection(db, &branch.tin);
        for code in &forms {
            if selection.allows(code) != enabled {
                selection.toggle(code);
            }
        }
        save_calendar_form_selection(db, &branch.tin, &selection)
            .map_err(|error| error.to_string())?;
    }
    if let Some(mut days) = days {
        days.sort_unstable_by(|a, b| b.cmp(a));
        days.dedup();
        days.truncate(5);
        db.set_setting(
            &format!("{REMINDERS_PREFIX}{}", branch.tin),
            &serde_json::to_string(&days).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(schedule(db, &branch))
}

/// Seconds-precision UTC, the sync record's clock: sorts as text.
fn stamp(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn load_sync(db: &Database) -> Result<Map<String, Value>, String> {
    Ok(db
        .get_setting(SYNC_KEY)
        .map_err(|error| error.to_string())?
        .and_then(|json| serde_json::from_str::<Value>(&json).ok())
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default())
}

fn save_sync(db: &Database, record: &Map<String, Value>) -> Result<(), String> {
    db.set_setting(SYNC_KEY, &Value::Object(record.clone()).to_string())
        .map_err(|error| error.to_string())
}

/// `since` as the sync clock: RFC 3339, a date, or Unix seconds.
fn since_stamp(arguments: &Map<String, Value>) -> Result<Option<String>, String> {
    let raw = match arguments.get("since") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Number(seconds)) => {
            let seconds = seconds.as_i64().ok_or("`since` must be whole Unix seconds")?;
            let at = chrono::DateTime::from_timestamp(seconds, 0)
                .ok_or("`since` is out of range")?;
            return Ok(Some(stamp(at)));
        }
        Some(Value::String(raw)) if raw.trim().is_empty() => return Ok(None),
        Some(Value::String(raw)) => raw.trim(),
        Some(other) => return Err(format!("`since` must be a time (got {other})")),
    };
    if let Ok(at) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Ok(Some(stamp(at.with_timezone(&chrono::Utc))));
    }
    if let Ok(day) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Ok(Some(stamp(day.and_time(chrono::NaiveTime::MIN).and_utc())));
    }
    if let Ok(seconds) = raw.parse::<i64>()
        && let Some(at) = chrono::DateTime::from_timestamp(seconds, 0)
    {
        return Ok(Some(stamp(at)));
    }
    Err(format!(
        "`since` must be an RFC 3339 time, a date or Unix seconds (got `{raw}`)"
    ))
}

/// `bir_calendar_changes {since?}`: the events of enabled forms added,
/// changed or removed that Google does not have yet. An added or changed
/// event stays listed until `bir_calendar_link` records its Google event;
/// a removed one until it is linked with no event id (deleted). `since`
/// narrows the list to changes after it. Each change's `id` is new for
/// every change, so a routine can tell the ones it has seen.
pub(crate) fn changes(
    db: &Database,
    arguments: &Map<String, Value>,
    today: NaiveDate,
) -> Result<Value, String> {
    check_keys(arguments, &["since"])?;
    let since = since_stamp(arguments)?;
    let now = stamp(chrono::Utc::now());
    let current: BTreeMap<String, Value> = events(db, None, NaiveDate::MIN, NaiveDate::MAX, today)?
        .into_iter()
        .filter(|event| event.enabled)
        .map(|event| (event.key.clone(), event.sync_json()))
        .collect();
    let mut record = load_sync(db)?;
    let before = record.clone();
    for (key, event) in &current {
        let entry = record
            .entry(key.clone())
            .or_insert_with(|| json!({ "event_id": null, "linked_at": null, "change": "", "event": null }));
        let removed = entry["change"] == "removed";
        if removed || entry["event"] != *event {
            entry["change"] = json!(if entry["event_id"].is_string() { "changed" } else { "added" });
            entry["event"] = event.clone();
            entry["changed_at"] = json!(now);
        }
    }
    let gone: Vec<String> = record
        .iter()
        .filter(|(key, entry)| !current.contains_key(*key) && entry["change"] != "removed")
        .map(|(key, _)| key.clone())
        .collect();
    for key in gone {
        if record[&key]["event_id"].is_string() {
            record[&key]["change"] = json!("removed");
            record[&key]["changed_at"] = json!(now);
        } else {
            record.remove(&key);
        }
    }
    if record != before {
        save_sync(db, &record)?;
    }
    let mut pending: Vec<Value> = record
        .iter()
        .filter(|(_, entry)| {
            let changed_at = entry["changed_at"].as_str().unwrap_or("");
            let open = match entry["change"].as_str() {
                Some("removed") => entry["event_id"].is_string(),
                _ => entry["linked_at"]
                    .as_str()
                    .is_none_or(|linked| linked < changed_at),
            };
            open && since.as_deref().is_none_or(|since| changed_at > since)
        })
        .map(|(key, entry)| {
            let change = entry["change"].as_str().unwrap_or("added");
            let changed_at = entry["changed_at"].as_str().unwrap_or("");
            json!({
                "id": format!("{change}:{key}@{changed_at}"),
                "change": change,
                "key": key,
                "event_id": entry["event_id"],
                "changed_at": changed_at,
                "event": if change == "removed" { Value::Null } else { entry["event"].clone() },
            })
        })
        .collect();
    pending.sort_by(|a, b| {
        (a["changed_at"].as_str(), a["key"].as_str()).cmp(&(b["changed_at"].as_str(), b["key"].as_str()))
    });
    Ok(json!({
        "since": since,
        "now": now,
        "changes": pending,
    }))
}

/// `bir_calendar_link {key, event_id}`: record the Google event of `key`
/// (from bir_calendar_changes). An empty or null `event_id` forgets it: say
/// so after deleting a removed event's Google event.
pub(crate) fn link(db: &Database, arguments: &Map<String, Value>) -> Result<Value, String> {
    check_keys(arguments, &["key", "event_id"])?;
    let key = text(arguments, "key")?
        .ok_or("`key` is required: a change's `key` from bir_calendar_changes")?
        .trim_start_matches(taxform::TOKEN_PREFIX)
        .to_string();
    let event_id = text(arguments, "event_id")?.map(str::to_string);
    let mut record = load_sync(db)?;
    let Some(entry) = record.get_mut(&key) else {
        return Err(format!(
            "no calendar event `{key}` to link: take keys from bir_calendar_changes"
        ));
    };
    match &event_id {
        Some(id) => {
            entry["event_id"] = json!(id);
            entry["linked_at"] = json!(stamp(chrono::Utc::now()));
        }
        None if entry["change"] == "removed" => {
            record.remove(&key);
        }
        None => {
            entry["event_id"] = Value::Null;
            entry["linked_at"] = Value::Null;
        }
    }
    save_sync(db, &record)?;
    Ok(json!({ "key": key, "event_id": event_id, "linked": event_id.is_some() }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bir_core::forms::Form2551QDraft;
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
                rdo_code: "047".into(),
                ..BranchFacts::default()
            },
        })
        .expect("taxpayer");
        db
    }

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    /// Put `codes` in the 2026 Forms Set.
    fn forms_set(db: &Database, codes: &[&str]) {
        let set = bir_core::forms::PerYearFormsSet::from_codes(
            2026,
            codes.iter().copied(),
            bir_core::forms::FormSetSource::Manual,
        );
        db.save_per_year_forms(TIN, 2026, &set).unwrap();
    }

    #[test]
    fn the_calendar_lists_dues_with_status_and_turns_forms_off() {
        let db = db();
        forms_set(&db, &["2551Q", "1601C"]);
        let listed = get(
            &db,
            &args(json!({"tin": ROOT, "from": "2026-10-01", "to": "2026-10-31"})),
            today(),
        )
        .unwrap();
        let events = listed["events"].as_array().unwrap();
        let q3 = events
            .iter()
            .find(|event| event["key"] == format!("{ROOT}/2551Q-2026-Q3"))
            .expect("2551Q Q3 falls due in October");
        assert_eq!(q3["status"], "not_started");
        assert_eq!(q3["enabled"], true);
        assert_eq!(q3["tin"], TIN);
        assert!(q3["open"].as_str().unwrap().starts_with("rox://app/bir/forms?props="));
        assert!(
            events
                .iter()
                .any(|event| event["key"] == format!("{ROOT}/1601C-2026-09"))
        );
        // A saved draft shows on its period.
        let profile = db.get_profile(TIN).unwrap().unwrap();
        db.save_2551q_draft(&Form2551QDraft::new_from_profile(&profile, 2026, 3))
            .unwrap();
        let again = get(&db, &args(json!({"tin": ROOT, "from": "2026-10-01", "to": "2026-10-31"})), today()).unwrap();
        let q3 = again["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["key"] == format!("{ROOT}/2551Q-2026-Q3"))
            .unwrap()
            .clone();
        assert_eq!(q3["status"], "draft");
        assert!(q3["status_detail"].as_str().unwrap().starts_with("Draft · "));
        // Batch off, then one back on.
        let schedule = set(
            &db,
            &args(json!({"tin": ROOT, "forms": ["2551Q", "1601-C"], "enabled": false})),
        )
        .unwrap();
        assert!(
            schedule["forms"]
                .as_array()
                .unwrap()
                .iter()
                .all(|form| form["enabled"] == false),
            "{schedule}"
        );
        set(&db, &args(json!({"tin": ROOT, "forms": ["1601C"], "enabled": true, "reminder_days": [3]}))).unwrap();
        let after = get(&db, &args(json!({"tin": ROOT, "from": "2026-10-01", "to": "2026-10-31"})), today()).unwrap();
        for event in after["events"].as_array().unwrap() {
            assert_eq!(event["enabled"], event["form"] == "1601C", "{event}");
            assert_eq!(event["reminder_days"], json!([3]));
        }
        assert!(set(&db, &args(json!({"tin": ROOT, "forms": ["2551Q"]}))).is_err());
        assert!(get(&db, &args(json!({"tin": "999999999"})), today()).is_err());
    }

    #[test]
    fn changes_stay_listed_until_linked_and_removals_until_deleted() {
        let db = db();
        forms_set(&db, &["2551Q"]);
        let first = changes(&db, &Map::new(), today()).unwrap();
        let listed = first["changes"].as_array().unwrap().clone();
        assert!(!listed.is_empty());
        assert!(listed.iter().all(|change| change["change"] == "added"));
        let q3 = listed
            .iter()
            .find(|change| change["key"] == format!("{ROOT}/2551Q-2026-Q3"))
            .unwrap()
            .clone();
        assert!(q3["event"]["title"].as_str().unwrap().contains("2551Q · Q3 2026"));
        // Asked again, the same ids: nothing new for a routine.
        let again = changes(&db, &Map::new(), today()).unwrap();
        assert_eq!(again["changes"], first["changes"]);
        // Linking takes it off the list.
        for change in &listed {
            link(&db, &args(json!({"key": change["key"], "event_id": format!("g-{}", change["key"])}))).unwrap();
        }
        assert!(changes(&db, &Map::new(), today()).unwrap()["changes"].as_array().unwrap().is_empty());
        // Turned off, every event is a removal carrying its Google id.
        set(&db, &args(json!({"tin": TIN, "forms": ["2551Q"], "enabled": false}))).unwrap();
        let removed = changes(&db, &Map::new(), today()).unwrap();
        let removed = removed["changes"].as_array().unwrap();
        assert_eq!(removed.len(), listed.len());
        assert!(removed.iter().all(|change| change["change"] == "removed" && change["event_id"].is_string()));
        // Deleted in Google: link with no id forgets it.
        for change in removed {
            link(&db, &args(json!({"key": change["key"], "event_id": null}))).unwrap();
        }
        assert!(changes(&db, &Map::new(), today()).unwrap()["changes"].as_array().unwrap().is_empty());
        assert!(link(&db, &args(json!({"key": "nope", "event_id": "x"}))).is_err());
        // A `since` after everything lists nothing.
        set(&db, &args(json!({"tin": TIN, "forms": ["2551Q"], "enabled": true}))).unwrap();
        let later = changes(&db, &args(json!({"since": "2999-01-01"})), today()).unwrap();
        assert!(later["changes"].as_array().unwrap().is_empty());
        let all = changes(&db, &args(json!({"since": 0})), today()).unwrap();
        assert!(all["changes"].as_array().unwrap().iter().all(|change| change["change"] == "added"));
    }

    /// A deadline moved from chat moves the calendar for every taxpayer and
    /// the deadline 2551Q's penalties run from: Q3 2026 (Oct 25, a Sunday)
    /// is due Oct 26, and once moved to Nov 3 a return filed on Nov 3
    /// carries no penalty.
    #[test]
    fn a_moved_deadline_moves_the_calendar_and_the_penalty_clock() {
        let db = db();
        forms_set(&db, &["2551Q"]);
        let q3 = |db: &Database| {
            get(db, &args(json!({"tin": ROOT, "from": "2026-10-01", "to": "2026-11-30"})), today())
                .unwrap()["events"]
                .as_array()
                .unwrap()
                .iter()
                .find(|event| event["key"] == format!("{ROOT}/2551Q-2026-Q3"))
                .unwrap()
                .clone()
        };
        let before = q3(&db);
        assert_eq!(before["due"], "2026-10-26");
        assert_eq!(before["moved"], false);
        let profile = db.get_profile(TIN).unwrap().unwrap();
        let kind = bir_core::forms::agent::AgentFormKind::Form2551Q;
        let opened = crate::screen_form_module::load_draft(&db, kind, &profile, 2026, 3, false, None)
            .unwrap();
        assert_eq!(opened["penalty_due_date"], "2026-10-26");

        bir_core::deadline_overrides::set(
            &db,
            &args(json!({"form": "2551Q", "period": "2026-Q3", "new_deadline": "2026-11-03", "reason": "RMC 1-2026"})),
            today(),
        )
        .unwrap();
        let after = q3(&db);
        assert_eq!(after["due"], "2026-11-03");
        assert_eq!(after["rule_due"], "2026-10-25");
        assert_eq!(after["moved_reason"], "RMC 1-2026");
        let opened = crate::screen_form_module::load_draft(&db, kind, &profile, 2026, 3, false, None)
            .unwrap();
        assert_eq!(opened["penalty_due_date"], "2026-11-03");
        let mut draft: Form2551QDraft = serde_json::from_value(opened).unwrap();
        draft.item_13_election = bir_core::forms::form_2551q::Item13Election::Graduated;
        draft.schedule_1[0].taxable_amount = 500_000.0;
        let on = |draft: &Form2551QDraft, date: NaiveDate| {
            let mut copy = draft.clone();
            bir_core::forms::penalty_items::with_clock(date, || copy.recompute(None));
            copy.total_penalties
        };
        let day = |m, d| NaiveDate::from_ymd_opt(2026, m, d).unwrap();
        assert_eq!(on(&draft, day(11, 3)), 0.0, "filed on the moved deadline");
        assert!(on(&draft, day(11, 4)) > 0.0);
        // The calendar sync sees a changed event.
        assert!(
            changes(&db, &Map::new(), today()).unwrap()["changes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|change| change["key"] == format!("{ROOT}/2551Q-2026-Q3")
                    && change["event"]["due"] == "2026-11-03")
        );
    }

    #[test]
    fn a_due_date_comes_from_the_calendar_rules() {
        let db = db();
        let due = due_date(&db, TIN, "1601C", TaxPeriod::Month { year: 2026, month: 9 }).unwrap();
        assert_eq!((due.year(), due.month()), (2026, 10));
        assert!(due_date(&db, TIN, "1601C", TaxPeriod::Quarter { year: 2026, quarter: 3 }).is_none());
    }
}
