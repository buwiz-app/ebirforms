//! One stored return, read for the assistant's cards (spec 9.8):
//! `bir_return {ticket | form+period, view}` answers its computation
//! (summary), its filled boxes laid out like the paper form (paper, the
//! preview card: Rox has no PDF viewer), what an amendment changed (amend),
//! the attachments its form asks for (attachments) and what a spreadsheet
//! import would fill (import).
//!
//! The values are the return's own: its stored JSON read through the
//! form's official box map ([`agent::field_map`]), never recomputed here.
//! Three app-only tools change something, each only when the user presses
//! its button: `bir_card_attach` (keep an uploaded attachment),
//! `bir_card_import` (read a picked .xlsx or .csv; reads only) and
//! `bir_card_import_fill` (put the import's total on the return's draft).

use std::collections::BTreeMap;
use std::io::Read;

use bir_core::db::Database;
use bir_core::forms::agent::{self, AgentFormKind};
use bir_core::forms::taxform::{self, TaxPeriod};
use bir_core::forms::{FilingStatus, FormDraftSummary};
use chrono::{Duration, NaiveDate};
use serde_json::{Map, Value, json};

use super::v1::tickets;
use super::{lookup, resolve};

pub(crate) const RETURN: &str = "bir_return";
pub(crate) const ATTACH: &str = "bir_card_attach";
pub(crate) const IMPORT: &str = "bir_card_import";
pub(crate) const IMPORT_FILL: &str = "bir_card_import_fill";
/// Settings key prefix of a return's attachments: `<return value>` ->
/// `{attachment id: {name, file, uploaded_at}}`.
const ATTACHMENTS_PREFIX: &str = "mini_app.attachments.";
const VIEWS: [&str; 5] = ["summary", "paper", "amend", "attachments", "import"];

fn check_keys(arguments: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    match arguments.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => Err(format!("unknown argument `{key}` (allowed: {})", allowed.join(", "))),
        None => Ok(()),
    }
}

fn text<'a>(arguments: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    arguments.get(key).and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty())
}

/// The return a call names.
pub(crate) struct Target {
    /// The branch's full TIN, digits.
    pub tin: String,
    pub taxpayer: String,
    pub code: &'static str,
    pub kind: AgentFormKind,
    pub period: TaxPeriod,
}

impl Target {
    /// `<tin>/<FORM>-<period>`: the return token's value.
    pub(crate) fn value(&self) -> String {
        taxform::canonical_value(&self.tin, self.code, self.period)
    }

    fn label(&self) -> String {
        format!("{} · {}", self.code, self.period.label())
    }

    fn stored(&self, db: &Database, today: NaiveDate) -> Option<FormDraftSummary> {
        let returns = resolve::stored_returns(db, &self.tin, self.code, today);
        resolve::furthest(&returns, self.code, self.period).cloned()
    }
}

/// The return of `ticket` (a return's ticket; a taxpayer's with `form` and
/// `period`), or of `form` holding a return's value (an expanded ticket).
pub(crate) fn target(db: &Database, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Target, String> {
    let mut named = taxform::TaxformArgs::default();
    if let Some(ticket) = text(arguments, "ticket") {
        if !tickets::is_ticket(ticket) {
            return Err("`ticket` must be a ticket (tkt_...): the return's, or the taxpayer's with `form` and `period`".into());
        }
        let subject = tickets::subject(db, ticket)?.ok_or("the ticket is not one BIR issued, or it was revoked")?;
        named = match subject.return_value() {
            Some(value) => taxform::parse(value, today),
            None => taxform::TaxformArgs { tin: Some(subject.full_tin()), ..Default::default() },
        };
    }
    if let Some(form) = text(arguments, "form") {
        let read = taxform::parse(form, today);
        if !read.unknown.is_empty() || read.form.is_none() {
            return Err(format!("`form` `{form}` is not a BIR form code"));
        }
        named.form = read.form;
        named.period = read.period.or(named.period);
        named.tin = read.tin.or(named.tin);
    }
    if let Some(raw) = text(arguments, "period") {
        named.period = Some(
            taxform::parse(raw, today)
                .period
                .ok_or_else(|| format!("`period` `{raw}` is not a period (e.g. 2026-Q3, 2026-09, 2025)"))?,
        );
    }
    let tin = named.tin.ok_or("name the return: its ticket, or the taxpayer's `ticket` with `form` and `period`")?;
    let code = named.form.ok_or("`form` is required with a taxpayer's ticket")?;
    let period = named
        .period
        .ok_or("`period` is required with a taxpayer's ticket")?
        .for_form(code, today)
        .map_err(|ask| ask.to_string())?;
    let kind = AgentFormKind::from_code(code).ok_or_else(|| format!("BIR does not prepare {code} returns"))?;
    let branch = resolve::branches(db, Some(&tin))?
        .into_iter()
        .next()
        .ok_or("this taxpayer is no longer saved in BIR")?;
    Ok(Target { tin: branch.tin, taxpayer: branch.name, code, kind, period })
}

/// A number in a box (`13,600.00`, `-5`, `(1,000.00)`), if it is one.
fn number(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    let negative = trimmed.starts_with('(') && trimmed.ends_with(')');
    let clean: String = trimmed.chars().filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
    if clean.is_empty() || clean == "-" || clean == "." {
        return None;
    }
    clean.parse::<f64>().ok().map(|n| if negative { -n } else { n })
}

/// A box whose value names the taxpayer's identity or a code, not an
/// amount or an answer to show: never on a card.
fn identity_box(label: &str, value: &str) -> bool {
    let lower = label.to_lowercase();
    ["tin", "zip", "telephone", "contact", "email", "e-mail", "rdo", "branch code", "page 2", "page 3", "signature"]
        .iter()
        .any(|word| lower.contains(word))
        || value.chars().filter(char::is_ascii_digit).count() >= 9 && number(value).is_none_or(|n| n.fract() == 0.0 && !value.contains(','))
}

/// The item number a box is under, for ordering: `14`, `26`, `36A`.
fn item_order(item: &str) -> (u32, String) {
    let digits: String = item.chars().take_while(char::is_ascii_digit).collect();
    (digits.parse().unwrap_or(u32::MAX), item.to_string())
}

/// One box as a card shows it.
struct Shown {
    key: String,
    item: String,
    label: String,
    value: String,
    computed: bool,
}

/// An amount as BIR's forms write one: `186,000.00`, `(1,000.00)`.
fn money(value: &str) -> bool {
    let inner = value.trim().trim_start_matches('(').trim_end_matches(')').trim_start_matches('-');
    inner.rsplit_once('.').is_some_and(|(whole, cents)| {
        cents.len() == 2 && cents.chars().all(|c| c.is_ascii_digit()) && !whole.is_empty() && whole.chars().all(|c| c.is_ascii_digit() || c == ',')
    })
}

/// The boxes of `draft` that hold something, in item order. A box that
/// repeats the taxpayer's TIN (in parts) is left out; a choice shows its
/// option's words.
fn filled(kind: AgentFormKind, draft: &Value) -> Result<Vec<Shown>, String> {
    let def = kind.definition();
    let map = agent::field_map(kind, draft)?;
    let tin: String = agent::identity(kind, draft).map(|who| who.tin).unwrap_or_default().chars().filter(char::is_ascii_digit).collect();
    let part_of_tin = |value: &str| value.len() >= 3 && value.chars().all(|c| c.is_ascii_digit()) && tin.contains(value);
    let mut out: Vec<Shown> = Vec::new();
    for entry in &def.boxes {
        if entry.group.is_some() || entry.key.contains('{') || entry.kind.contains("hidden") || entry.required == "hidden" {
            continue;
        }
        let value = def.box_value(&entry.key, &map);
        let value = value.trim();
        if value.is_empty() || value == "false" || value == "0" || value == "0.00" || identity_box(&entry.label, value) || part_of_tin(value) {
            continue;
        }
        let value = match entry.options.iter().find(|option| option.value == value) {
            Some(option) if !option.label.trim().is_empty() => option.label.trim().to_string(),
            _ if value == "true" => "Yes".to_string(),
            _ => value.to_string(),
        };
        let label = entry.label.trim_start_matches(|c: char| c.is_ascii_digit() || c == ' ' || c == '.').trim().to_string();
        let label = if label.is_empty() { entry.label.clone() } else { label };
        out.push(Shown { key: entry.key.clone(), item: entry.item.clone(), label, value, computed: entry.computed });
    }
    out.sort_by_key(|shown| item_order(&shown.item));
    Ok(out)
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max - 1).collect::<String>())
    }
}

fn shown_json(shown: &Shown) -> Value {
    json!({ "item": shown.item, "label": clip(&shown.label, 56), "value": clip(&shown.value, 40), "computed": shown.computed })
}

/// `bir_return {ticket?, form?, period?, view}`.
pub(crate) fn view(db: &Database, arguments: &Map<String, Value>, today: NaiveDate) -> Result<Value, String> {
    check_keys(arguments, &["ticket", "form", "period", "view"])?;
    let view = text(arguments, "view").unwrap_or("summary");
    if !VIEWS.contains(&view) {
        return Err(format!("`view` must be one of {} (got `{view}`)", VIEWS.join(", ")));
    }
    let target = target(db, arguments, today)?;
    let stored = target.stored(db, today);
    let draft = stored.as_ref().and_then(|filing| db.draft_data_json(filing.id).ok().flatten());
    let mut out = json!({
        "view": view,
        "form": target.code,
        "title": bir_core::forms::find_form(target.code).map(|form| form.title).unwrap_or(""),
        "period": target.period.canonical(),
        "label": target.label(),
        "taxpayer": target.taxpayer,
        "status": stored.as_ref().map(|filing| format!("{:?}", filing.status)),
        "key": target.value(),
    });
    let body = match view {
        "summary" => summary(target.kind, draft.as_ref())?,
        "paper" => paper(target.kind, draft.as_ref())?,
        "amend" => amend(db, &target, stored.as_ref(), draft.as_ref())?,
        "attachments" => attachments(db, &target)?,
        _ => import_view(&target, stored.as_ref()),
    };
    if let (Value::Object(out), Value::Object(body)) = (&mut out, body) {
        out.extend(body);
    }
    Ok(out)
}

fn not_saved() -> Value {
    json!({ "items": [], "note": "No return of this form and period is saved yet. Start it in BIR Forms." })
}

/// The money items of the return and what is left to pay.
fn summary(kind: AgentFormKind, draft: Option<&Value>) -> Result<Value, String> {
    let Some(draft) = draft else { return Ok(not_saved()) };
    let boxes = filled(kind, draft)?;
    let money: Vec<&Shown> = boxes
        .iter()
        .filter(|shown| money(&shown.value) && number(&shown.value).is_some_and(|n| n != 0.0))
        .collect();
    // One row per item (two-column forms repeat an item for A and B); the
    // schedules' rows (no item number) first, as the amounts they add up.
    let (rows, items): (Vec<&Shown>, Vec<&Shown>) = money.iter().partition(|shown| shown.item.is_empty());
    let money: Vec<&Shown> = rows.into_iter().chain(items).collect();
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut items = Vec::new();
    for shown in &money {
        let id = (shown.item.clone(), shown.label.clone());
        // A schedule's total repeats an item's amount: the item says it.
        let repeated = shown.item.is_empty() && money.iter().any(|other| !other.item.is_empty() && other.value == shown.value);
        if seen.contains(&id) || repeated {
            continue;
        }
        seen.push(id);
        items.push(shown_json(shown));
    }
    // What is left to pay: the last item that says payable.
    let payable = money
        .iter()
        .rev()
        .find(|shown| shown.label.to_lowercase().contains("payable"))
        .map(|shown| shown_json(shown));
    Ok(json!({
        "items": items.into_iter().take(18).collect::<Vec<_>>(),
        "payable": payable,
        "note": "The return's own amounts, as BIR's form rules computed them when it was last saved. computed: worked out by the form, not typed.",
    }))
}

/// Every filled box, for the paper view.
fn paper(kind: AgentFormKind, draft: Option<&Value>) -> Result<Value, String> {
    let Some(draft) = draft else { return Ok(not_saved()) };
    // A box with no item number is a schedule's row or the page's own
    // bookkeeping (page count, flags): only amounts are kept.
    let boxes: Vec<Shown> = filled(kind, draft)?.into_iter().filter(|shown| !shown.item.is_empty() || money(&shown.value)).collect();
    Ok(json!({
        "items": boxes.iter().map(shown_json).collect::<Vec<_>>(),
        "note": "The return's filled boxes in item order, as they print on the official form (taxpayer numbers left out). Open it in BIR Forms for the printable page.",
    }))
}

/// The filed version (kept when the return was reverted for amendment)
/// against the amended draft, the items that differ.
fn amend(db: &Database, target: &Target, stored: Option<&FormDraftSummary>, draft: Option<&Value>) -> Result<Value, String> {
    let (Some(filing), Some(draft)) = (stored, draft) else { return Ok(not_saved()) };
    let snapshots = db.list_amendment_snapshots_for_tin(&filing.tin).map_err(|error| error.to_string())?;
    let original = snapshots.iter().find(|snapshot| {
        snapshot.form_code == target.code
            && serde_json::from_str::<Value>(&snapshot.draft_json).is_ok_and(|json| {
                json["id"].as_i64() == Some(filing.id)
                    || agent::identity(target.kind, &json).ok().zip(agent::identity(target.kind, draft).ok()).is_some_and(
                        |(old, new)| old.taxable_year == new.taxable_year && old.period == new.period,
                    )
            })
    });
    let Some(original) = original else {
        let state = match filing.status {
            FilingStatus::Submitted | FilingStatus::Confirmed => "not_started",
            FilingStatus::Draft | FilingStatus::Queued => "never_filed",
            FilingStatus::Paid => "paid",
        };
        return Ok(json!({
            "state": state,
            "changes": [],
            "note": match state {
                "not_started" => "BIR has this return; no amendment is started. Amending it in BIR Forms (Revert to Draft) keeps the filed copy, then this shows what changed.",
                "paid" => "This return is marked paid; amend it in BIR Forms if BIR allows.",
                _ => "This return has not reached BIR yet: edit it directly instead of amending.",
            },
        }));
    };
    let before: Value = serde_json::from_str(&original.draft_json).map_err(|error| error.to_string())?;
    let old: BTreeMap<String, String> = filled(target.kind, &before)?.into_iter().map(|shown| (shown.key, shown.value)).collect();
    let now = filled(target.kind, draft)?;
    let def = target.kind.definition();
    let mut changes = Vec::new();
    let mut keys: Vec<&String> = old.keys().chain(now.iter().map(|shown| &shown.key)).collect();
    keys.sort();
    keys.dedup();
    let new: BTreeMap<&String, &String> = now.iter().map(|shown| (&shown.key, &shown.value)).collect();
    for key in keys {
        let (was, is) = (old.get(key).map(String::as_str).unwrap_or(""), new.get(key).map(|v| v.as_str()).unwrap_or(""));
        let entry = def.box_def(key);
        let label = entry.map(|entry| entry.label.as_str()).unwrap_or(key);
        if was == is || label.to_lowercase().contains("amended") {
            continue;
        }
        changes.push((
            item_order(entry.map(|entry| entry.item.as_str()).unwrap_or("")),
            json!({ "item": entry.map(|entry| entry.item.clone()), "label": clip(label, 56), "before": clip(was, 30), "after": clip(is, 30) }),
        ));
    }
    changes.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(json!({
        "state": "amending",
        "filed_at": original.submitted_at,
        "changes": changes.into_iter().map(|change| change.1).take(30).collect::<Vec<_>>(),
        "note": "before: the return as BIR received it; after: the amended draft as last saved.",
    }))
}

fn attachment_store(db: &Database, target: &Target) -> Map<String, Value> {
    db.get_setting(&format!("{ATTACHMENTS_PREFIX}{}", target.value()))
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

/// The attachments the official package lists for the form, each kept or
/// missing for this return.
fn attachments(db: &Database, target: &Target) -> Result<Value, String> {
    let kept = attachment_store(db, target);
    let listed = lookup::notes(target.code).map(|notes| notes.attach.as_slice()).unwrap_or_default();
    Ok(json!({
        "items": listed.iter().map(|attachment| json!({
            "id": attachment.id,
            "label": clip(&attachment.label, 90),
            "when": clip(&attachment.when, 120),
            "file": kept.get(&attachment.id).and_then(|kept| kept["name"].as_str()),
        })).collect::<Vec<_>>(),
        "note": "From the official form package's required attachments; each applies only when its condition does. file: what the user uploaded for this return in BIR. eBIRForms does not take attachments: keep them for BIR's eAFS or the RDO.",
    }))
}

/// The box an import's total goes in, per form.
pub(crate) fn import_box(code: &str) -> Option<&'static str> {
    match code {
        "2551Q" => Some("txtATCAmt1"),
        "1701Q" => Some("frm1701q:txt36A"),
        "2550Q" => Some("frm2550qv2024:vatableSales"),
        _ => None,
    }
}

fn import_view(target: &Target, stored: Option<&FormDraftSummary>) -> Value {
    let label = import_box(target.code)
        .and_then(|key| target.kind.definition().box_def(key))
        .map(|entry| entry.label.clone());
    let draft = stored.is_none_or(|filing| filing.status == FilingStatus::Draft);
    json!({
        "box": label,
        "draft": draft,
        "forms": ["2551Q", "1701Q", "2550Q"],
        "note": if label.is_none() {
            "BIR imports a sales sheet into 2551Q, 1701Q and 2550Q only."
        } else if !draft {
            "This return has left Draft: it cannot be filled any more."
        } else {
            "The user picks an .xlsx or .csv on the card. BIR reads the rows (a date and an amount on each), lists the ones it skips, and fills nothing until they press Fill."
        },
    })
}

/// `bir_card_attach {ticket, attachment, upload, name}` (the attach card's
/// Upload): keep `bytes` as the return's attachment.
pub(crate) fn attach(db: &Database, arguments: &Map<String, Value>, bytes: &[u8], dir: &std::path::Path, today: NaiveDate) -> Result<Value, String> {
    let target = target(db, arguments, today)?;
    let id = text(arguments, "attachment").ok_or("`attachment` is required: the attachment's id")?;
    let listed = lookup::notes(target.code).map(|notes| notes.attach.as_slice()).unwrap_or_default();
    let attachment = listed.iter().find(|attachment| attachment.id == id).ok_or_else(|| format!("{} lists no attachment `{id}`", target.code))?;
    let name = text(arguments, "name").ok_or("`name` is required: the picked file's name")?;
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|ext| ext.chars().all(|c| c.is_ascii_alphanumeric()) && ext.len() <= 5)
        .ok_or("`name` must be the picked file's name, with its extension")?;
    let folder = dir.join(target.value().replace(['/', ' '], "_"));
    std::fs::create_dir_all(&folder).map_err(|error| format!("cannot keep the attachment: {error}"))?;
    let file = folder.join(format!("{id}.{ext}"));
    std::fs::write(&file, bytes).map_err(|error| format!("cannot keep the attachment: {error}"))?;
    let mut kept = attachment_store(db, &target);
    kept.insert(id.to_string(), json!({ "name": name, "file": file.display().to_string(), "uploaded_at": chrono::Utc::now().to_rfc3339() }));
    db.set_setting(&format!("{ATTACHMENTS_PREFIX}{}", target.value()), &Value::Object(kept).to_string())
        .map_err(|error| error.to_string())?;
    Ok(json!({ "kept": true, "attachment": attachment.label, "name": name }))
}

// ------------------------------------------------------------ import

/// A spreadsheet read for an import: its rows as text cells.
fn sheet_rows(name: &str, bytes: &[u8]) -> Result<Vec<Vec<String>>, String> {
    let lower = name.to_lowercase();
    if lower.ends_with(".csv") {
        let text = String::from_utf8_lossy(bytes);
        return Ok(text.lines().map(csv_line).collect());
    }
    if lower.ends_with(".xlsx") {
        return xlsx_rows(bytes);
    }
    Err("pick an .xlsx or .csv file".into())
}

fn csv_line(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cell.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => cells.push(std::mem::take(&mut cell)),
            _ => cell.push(c),
        }
    }
    cells.push(cell);
    cells.into_iter().map(|cell| cell.trim().to_string()).collect()
}

fn xml_text(raw: &str) -> String {
    raw.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// The first worksheet of an .xlsx, as text cells (shared strings resolved).
fn xlsx_rows(bytes: &[u8]) -> Result<Vec<Vec<String>>, String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|_| "this .xlsx file is not readable".to_string())?;
    let mut read = |name: &str| -> Option<String> {
        let mut file = zip.by_name(name).ok()?;
        let mut text = String::new();
        file.read_to_string(&mut text).ok()?;
        Some(text)
    };
    let shared_xml = read("xl/sharedStrings.xml").unwrap_or_default();
    let sheet = read("xl/worksheets/sheet1.xml").ok_or("this .xlsx has no first worksheet")?;
    let si = regex::Regex::new(r"(?s)<si>(.*?)</si>").expect("regex");
    let t = regex::Regex::new(r"(?s)<t[^>]*>(.*?)</t>").expect("regex");
    let shared: Vec<String> = si
        .captures_iter(&shared_xml)
        .map(|cap| t.captures_iter(&cap[1]).map(|part| xml_text(&part[1])).collect::<String>())
        .collect();
    let row_re = regex::Regex::new(r"(?s)<row[^>]*>(.*?)</row>").expect("regex");
    let cell_re = regex::Regex::new(r#"(?s)<c\s+r="([A-Z]+)\d+"([^>]*?)(?:/>|>(.*?)</c>)"#).expect("regex");
    let value_re = regex::Regex::new(r"(?s)<v>(.*?)</v>").expect("regex");
    let mut rows = Vec::new();
    for row in row_re.captures_iter(&sheet) {
        let mut cells: Vec<String> = Vec::new();
        for cell in cell_re.captures_iter(&row[1]) {
            let column = cell[1].bytes().fold(0usize, |n, b| n * 26 + usize::from(b - b'A' + 1)) - 1;
            let attrs = &cell[2];
            let inner = cell.get(3).map(|m| m.as_str()).unwrap_or("");
            let raw = value_re.captures(inner).map(|v| xml_text(&v[1])).unwrap_or_default();
            let value = if attrs.contains(r#"t="s""#) {
                raw.parse::<usize>().ok().and_then(|i| shared.get(i).cloned()).unwrap_or_default()
            } else if attrs.contains(r#"t="inlineStr""#) {
                t.captures_iter(inner).map(|part| xml_text(&part[1])).collect()
            } else {
                raw
            };
            if cells.len() <= column {
                cells.resize(column + 1, String::new());
            }
            cells[column] = value.trim().to_string();
        }
        rows.push(cells);
    }
    Ok(rows)
}

/// A date cell: 2026-07-15, 07/15/2026, or an Excel day number.
fn cell_date(cell: &str) -> Option<NaiveDate> {
    let cell = cell.trim();
    for format in ["%Y-%m-%d", "%m/%d/%Y", "%m/%d/%y", "%Y/%m/%d", "%b %d, %Y", "%d %b %Y"] {
        if let Ok(day) = NaiveDate::parse_from_str(cell, format) {
            return Some(day);
        }
    }
    let serial: f64 = cell.parse().ok()?;
    (20_000.0..80_000.0).contains(&serial).then(|| NaiveDate::from_ymd_opt(1899, 12, 30).expect("epoch") + Duration::days(serial as i64))
}

/// An amount cell: `12,400.00`, `₱ 990`; not `12,4OO.00`.
fn cell_amount(cell: &str) -> Option<f64> {
    let trimmed = cell.trim();
    let trimmed = trimmed.strip_prefix("PHP").or_else(|| trimmed.strip_prefix("Php")).unwrap_or(trimmed);
    let clean: String = trimmed.chars().filter(|c| !matches!(c, '₱' | ',' | ' ' | '\u{a0}')).collect();
    if clean.is_empty() || clean.chars().any(|c| !(c.is_ascii_digit() || c == '.' || c == '-')) {
        return None;
    }
    clean.parse().ok()
}

fn period_bounds(period: TaxPeriod) -> (NaiveDate, NaiveDate) {
    let first = |year: u16, month: u32| NaiveDate::from_ymd_opt(i32::from(year), month, 1).expect("a month");
    let last = |year: u16, month: u32| {
        let next = if month == 12 { first(year + 1, 1) } else { first(year, month + 1) };
        next - Duration::days(1)
    };
    match period {
        TaxPeriod::Month { year, month } => (first(year, u32::from(month)), last(year, u32::from(month))),
        TaxPeriod::Quarter { year, quarter } => (first(year, u32::from(quarter) * 3 - 2), last(year, u32::from(quarter) * 3)),
        TaxPeriod::Year(year) => (first(year, 1), last(year, 12)),
        TaxPeriod::Day(day) => (day, day),
    }
}

/// What an import read.
pub(crate) struct Imported {
    pub value: String,
    pub total: f64,
    pub rows: usize,
}

/// `bir_card_import {ticket, upload, name}`: read the sheet's rows for the
/// return (a date and an amount each, within the return's period). Reads
/// only; the total waits for `bir_card_import_fill`.
pub(crate) fn import_read(db: &Database, arguments: &Map<String, Value>, bytes: &[u8], today: NaiveDate) -> Result<(Value, Imported), String> {
    let target = target(db, arguments, today)?;
    let key = import_box(target.code).ok_or_else(|| format!("BIR imports a sales sheet into 2551Q, 1701Q and 2550Q only, not {}", target.code))?;
    let name = text(arguments, "name").ok_or("`name` is required: the picked file's name")?;
    let rows = sheet_rows(name, bytes)?;
    // The header row: a cell about the date and one about the amount.
    let header = rows.iter().take(10).position(|row| {
        row.iter().any(|cell| cell.to_lowercase().contains("date"))
            && row.iter().any(|cell| {
                let lower = cell.to_lowercase();
                ["amount", "sales", "total", "gross", "receipt"].iter().any(|word| lower.contains(word))
            })
    });
    let (date_col, amount_col, start) = match header {
        Some(at) => {
            let row = &rows[at];
            let date = row.iter().position(|cell| cell.to_lowercase().contains("date")).unwrap_or(0);
            let amount = row
                .iter()
                .position(|cell| {
                    let lower = cell.to_lowercase();
                    ["amount", "sales", "total", "gross", "receipt"].iter().any(|word| lower.contains(word))
                })
                .unwrap_or(1);
            (date, amount, at + 1)
        }
        None => (0, 1, 0),
    };
    let (from, to) = period_bounds(target.period);
    let (mut read, mut total, mut skipped) = (0usize, 0.0f64, Vec::new());
    for (index, row) in rows.iter().enumerate().skip(start) {
        if row.iter().all(|cell| cell.trim().is_empty()) {
            continue;
        }
        let number = index + 1;
        let cell = |col: usize| row.get(col).map(String::as_str).unwrap_or("");
        let why = match (cell_date(cell(date_col)), cell_amount(cell(amount_col))) {
            (None, _) => Some(("no date BIR can read", cell(date_col).to_string())),
            (_, None) => Some(("amount isn't a number", cell(amount_col).to_string())),
            (Some(day), _) if day < from || day > to => Some(("date outside the period", day.format("%Y-%m-%d").to_string())),
            (Some(_), Some(amount)) => {
                read += 1;
                total += amount;
                None
            }
        };
        if let Some((why, cell)) = why {
            skipped.push(json!({ "row": number, "why": why, "cell": clip(&cell, 24) }));
        }
    }
    let total = (total * 100.0).round() / 100.0;
    let label = target.kind.definition().box_def(key).map(|entry| entry.label.clone()).unwrap_or_default();
    let imported = Imported { value: target.value(), total, rows: read };
    Ok((
        json!({
            "file": name,
            "rows": read,
            "total": total,
            "skipped": skipped.into_iter().take(20).collect::<Vec<_>>(),
            "box": label,
            "period": target.period.label(),
            "filled": false,
        }),
        imported,
    ))
}

/// `bir_card_import_fill {ticket, import}`: put an import's total in the
/// return's box and save the draft.
pub(crate) fn import_fill(db: &Database, imported: &Imported, today: NaiveDate) -> Result<Value, String> {
    let named = taxform::parse(&imported.value, today);
    let (Some(tin), Some(code), Some(period)) = (named.tin, named.form, named.period) else {
        return Err("the import's return is incomplete".into());
    };
    let kind = AgentFormKind::from_code(code).ok_or("BIR does not prepare this form")?;
    let key = import_box(code).ok_or("this form takes no import")?;
    let tin = resolve::branches(db, Some(&tin))?
        .into_iter()
        .next()
        .map(|branch| branch.tin)
        .ok_or("this taxpayer is no longer saved in BIR")?;
    let profile = crate::agent::load_profile(db, &tin)?;
    let (year, slot) = period.open_args();
    let mut draft = crate::screen_form_module::load_draft(db, kind, &profile, year, slot, false, None)?;
    if agent::identity(kind, &draft)?.status != FilingStatus::Draft {
        return Err("this return is no longer a draft: it cannot be filled".into());
    }
    let from_profile = crate::agent::profile_context(db, kind, &profile, year, slot)?;
    agent::fill(kind, &mut draft, key, &json!(format!("{:.2}", imported.total)), Some(&from_profile))?;
    agent::save_draft(kind, db, &mut draft)?;
    let label = kind.definition().box_def(key).map(|entry| entry.label.clone()).unwrap_or_default();
    Ok(json!({ "filled": true, "box": label, "total": imported.total, "rows": imported.rows }))
}

/// The three app-only tools' `tools/list` entries.
pub(crate) fn app_descriptors() -> Vec<Value> {
    let tool = |name: &str, description: &str, properties: Value, required: &[&str], read_only: bool| {
        json!({
            "name": name,
            "description": description,
            "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
            "annotations": { "readOnlyHint": read_only },
            "_meta": { "rox/audience": ["app"] },
        })
    };
    vec![
        tool(
            ATTACH,
            "Mini apps only (refused for the assistant): the attach card's Upload, keeping an uploaded file (bir_app_upload) as one of the return's attachments.",
            json!({ "ticket": { "type": "string" }, "attachment": { "type": "string" }, "upload": { "type": "string" }, "name": { "type": "string" } }),
            &["ticket", "attachment", "upload", "name"],
            false,
        ),
        tool(
            IMPORT,
            "Mini apps only (refused for the assistant): the import card reads a picked .xlsx or .csv (bir_app_upload) for a return: rows read, rows skipped and their total. Fills nothing.",
            json!({ "ticket": { "type": "string" }, "upload": { "type": "string" }, "name": { "type": "string" } }),
            &["ticket", "upload", "name"],
            true,
        ),
        tool(
            IMPORT_FILL,
            "Mini apps only (refused for the assistant): the import card's Fill, putting the import's total on the return's draft.",
            json!({ "ticket": { "type": "string" }, "import": { "type": "string" } }),
            &["ticket", "import"],
            false,
        ),
    ]
}

/// `bir_return`'s `tools/list` entry.
pub(crate) fn descriptor() -> Value {
    json!({
        "name": RETURN,
        "description": "Read one saved return for the user, as a card. `ticket`: the return's ticket (@bir:taxform), or the taxpayer's with `form` and `period`. `view`: summary (the computation: sales, tax due, credits, what is left to pay; \"how much is her 1701Q?\"), paper (its filled boxes laid out like the printed form; \"show me the 2551Q before I send it\"), amend (what an amendment changed against the filed copy; \"what changes if I amend Q2?\"), attachments (what the form asks to attach, with what is uploaded; the user uploads on the card), import (a card where the user picks a sales .xlsx or .csv and BIR fills the return from it only when they press Fill).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "ticket": { "type": "string" },
                "form": { "type": "string", "description": "Form code, e.g. 2551Q." },
                "period": { "type": "string", "description": "e.g. 2026-Q3, 2026-09." },
                "view": { "type": "string", "enum": VIEWS }
            },
            "required": ["view"],
            "additionalProperties": false
        },
        "annotations": { "readOnlyHint": true },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    #[test]
    fn a_sheet_reads_its_rows_and_cells() {
        assert_eq!(csv_line(r#"2026-07-01,"12,400.00",x"#), ["2026-07-01", "12,400.00", "x"]);
        assert_eq!(cell_amount("12,400.00"), Some(12400.0));
        assert_eq!(cell_amount("₱ 990"), Some(990.0));
        assert_eq!(cell_amount("12,4OO.00"), None);
        assert_eq!(cell_date("07/15/2026"), NaiveDate::from_ymd_opt(2026, 7, 15));
        assert_eq!(cell_date("46218"), NaiveDate::from_ymd_opt(2026, 7, 15));
        let (from, to) = period_bounds(TaxPeriod::Quarter { year: 2026, quarter: 3 });
        assert_eq!((from.month(), to.day()), (7, 30));
        assert!(identity_box("Page 2 TIN 1", "123"));
        assert!(!identity_box("Total Tax Due", "13,600.00"));
        assert!(money("186,000.00") && money("(1,000.00)") && !money("2026") && !money("1"));
    }
}
