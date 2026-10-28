//! Answer cards (spec 9.8): which card an assistant's answer carries.
//!
//! The tool picks the card, never the model: after a BIR tool answers the
//! assistant, [`decorate`] looks at what it answered and adds
//! `_meta["rox/open"]` for one of BIR's own signed inline card apps, with
//! the answer's data as `props.snapshot` (and `as_of` when it can be
//! re-read), the tool and
//! arguments the card's Refresh re-reads live with, and up to three
//! next-step chips (`_meta["rox/suggest"]`). The server then turns every
//! TIN in the props into a ticket and [`fit`]s them under the 8 KiB props
//! limit.
//!
//! The cards are eight apps, each drawing a family of cards of about one
//! height (an app declares one inline size), so the manifest stays within
//! its 16 apps and each app's permissions stay narrow: `card-people` (who),
//! `card-forms` (returns and the queue), `card-dates` (deadlines and
//! penalties), `card-money` (payments), `card-help` (BIR's health),
//! `card-return` (one return read closely), `card-quick` (short cards with
//! one decision) and `card-calc` (lookups). `props.card` says which card.

use chrono::NaiveDate;
use serde_json::{Map, Value, json};

use super::{answers, calendar, lookup, return_view, taxpayer};

/// The inline size a card app declares (the host clamps it): each app
/// draws cards of about one height, so its card fits what it shows. The
/// short ones (a question, a few facts and a button or two) are their own
/// apps rather than leaving a tall frame half empty.
pub(crate) fn inline(app: &str) -> Value {
    let height = match app {
        "card-quick" => 230,
        "card-calc" => 215,
        "card-help" => 240,
        "card-money" | "card-people" | "card-forms" => 320,
        _ => 360,
    };
    json!({ "width": 560, "height": height, "min_width": 320, "resizable": true })
}

/// Card kinds: id, app, small.
pub(crate) const KINDS: &[(&str, &str, bool)] = &[
    ("pick", "card-people", false),
    ("taxpayer", "card-people", false),
    ("board", "card-people", false),
    ("branches", "card-people", false),
    ("quickadd", "card-people", false),
    ("streak", "card-people", false),
    ("forms", "card-forms", false),
    ("matrix", "card-forms", false),
    ("status", "card-forms", false),
    ("needs", "card-forms", false),
    ("explain", "card-forms", false),
    ("drafts", "card-forms", false),
    ("outbox", "card-forms", false),
    ("bulk", "card-forms", false),
    ("dues", "card-dates", false),
    ("month", "card-dates", false),
    ("overdue", "card-dates", false),
    ("penalty", "card-dates", false),
    ("week", "card-dates", false),
    ("unpaid", "card-money", false),
    ("paidsum", "card-money", false),
    ("digest", "card-money", false),
    ("health", "card-help", false),
    ("summary", "card-return", false),
    ("preview", "card-return", false),
    ("amend", "card-return", false),
    ("attach", "card-return", false),
    ("import", "card-return", false),
    ("approve", "card-quick", true),
    ("match", "card-quick", true),
    ("markpaid", "card-quick", true),
    ("confirm", "card-quick", true),
    ("signin", "card-quick", true),
    ("shift", "card-quick", true),
    ("remind", "card-quick", true),
    ("atc", "card-calc", true),
    ("rdo", "card-calc", true),
    ("rate", "card-calc", true),
];

/// The card apps, in manifest order.
#[cfg(test)]
pub(crate) const APPS: [&str; 8] =
    ["card-people", "card-forms", "card-dates", "card-money", "card-help", "card-return", "card-quick", "card-calc"];

/// Props a card's snapshot may fill: a little under 8 KiB, so the host's
/// limit holds after encoding.
const PROPS_MAX: usize = 7_600;

fn kind(id: &str) -> (&'static str, &'static str, bool) {
    *KINDS.iter().find(|(kind, ..)| *kind == id).expect("a known card")
}

/// `_meta["rox/open"]` for card `id`.
pub(crate) fn open(id: &str, title: &str, ticket: Option<&str>, snapshot: Value, tool: &str, args: Value) -> Value {
    let (card, app, small) = kind(id);
    // The arguments are kept only for Refresh: an action card's (no tool)
    // could hold what the assistant read (an email's text, BIR's file name),
    // which embeds a TIN (spec 6.6).
    let args = if tool.is_empty() { json!({}) } else { args };
    let mut props = json!({
        "card": card,
        "snapshot": snapshot,
        "args": args,
    });
    // A card whose answer came from a tool that changes something (send,
    // record, confirm) or reads mail has no Refresh: re-running it is not
    // a read. Rox shows "as of" and Refresh only for a snapshot with
    // `as_of`, so such a card leaves it out.
    if !tool.is_empty() {
        props["tool"] = json!(tool);
        props["as_of"] = json!(chrono::Utc::now().timestamp_millis());
    }
    let mut open = json!({
        "app": app,
        "placement": "inline",
        "title": title.chars().take(60).collect::<String>(),
    });
    if let Some(ticket) = ticket {
        props["ticket"] = json!(ticket);
        open["ticket"] = json!(ticket);
    }
    open["props"] = props;
    if small {
        open["size"] = json!("small");
    }
    open
}

/// `data` without `keys`.
fn without(data: &Value, keys: &[&str]) -> Value {
    let mut data = data.clone();
    if let Some(map) = data.as_object_mut() {
        for key in keys {
            map.remove(*key);
        }
    }
    data
}

/// The last guard of spec 6.6 for a result that carries a card: any run of
/// digits in its props (and its text) that holds a saved taxpayer's TIN
/// root, plain or dashed and with or without a branch code (`123456788`,
/// `12345678800000-2551Qv2018-…`, `123-456-788-000`), becomes `[TIN]`. The
/// tools already answer with tickets; this catches a TIN embedded in other
/// text, such as an email the assistant passed in or a BIR file name.
pub(crate) fn redact(result: &mut Value, roots: &[String]) {
    if result.pointer("/_meta/rox~1open").is_none() || roots.is_empty() {
        return;
    }
    static RUN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let run = RUN.get_or_init(|| regex::Regex::new(r"\d(?:[\d-]*\d)?").expect("regex"));
    let clean = |text: &str| -> Option<String> {
        let mut hit = false;
        let out = run.replace_all(text, |caps: &regex::Captures<'_>| {
            let digits: String = caps[0].chars().filter(char::is_ascii_digit).collect();
            if digits.len() >= 9 && roots.iter().any(|root| digits.contains(root.as_str())) {
                hit = true;
                "[TIN]".to_string()
            } else {
                caps[0].to_string()
            }
        });
        hit.then(|| out.into_owned())
    };
    fn walk(value: &mut Value, clean: &dyn Fn(&str) -> Option<String>) {
        match value {
            Value::String(text) => {
                if let Some(out) = clean(text) {
                    *text = out;
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| walk(item, clean)),
            Value::Object(map) => map.values_mut().for_each(|item| walk(item, clean)),
            _ => {}
        }
    }
    if let Some(props) = result.pointer_mut("/_meta/rox~1open/props") {
        walk(props, &clean);
    }
    if let Some(content) = result.get_mut("content") {
        walk(content, &clean);
    }
    if let Some(structured) = result.get_mut("structuredContent") {
        walk(structured, &clean);
    }
}

fn suggest(chips: &[(&str, String)]) -> Value {
    Value::Array(
        chips
            .iter()
            .take(3)
            .map(|(label, prompt)| json!({ "label": label.chars().take(40).collect::<String>(), "prompt": prompt.chars().take(200).collect::<String>() }))
            .collect(),
    )
}

/// What a tool answered: its structured result, else its text as JSON.
fn data(result: &Value) -> Option<Value> {
    if let Some(value) = result.get("structuredContent").and_then(|structured| structured.get("result")) {
        return Some(value.clone());
    }
    result["content"][0]["text"].as_str().and_then(|text| serde_json::from_str(text).ok())
}

fn ticket_arg(args: &Map<String, Value>) -> Option<String> {
    args.get("ticket").or_else(|| args.get("tin")).and_then(Value::as_str).map(str::to_string)
}

/// Whether `from`..`to` is a week or less.
fn one_week(args: &Map<String, Value>) -> bool {
    let day = |key: &str| args.get(key).and_then(Value::as_str).and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok());
    matches!((day("from"), day("to")), (Some(from), Some(to)) if (to - from).num_days() <= 7)
}

/// Whether `from`..`to` is one calendar month.
fn one_month(args: &Map<String, Value>) -> bool {
    let day = |key: &str| args.get(key).and_then(Value::as_str).and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok());
    matches!((day("from"), day("to")), (Some(from), Some(to)) if from.format("%Y-%m").to_string() == to.format("%Y-%m").to_string())
}

/// The `@bir:taxprofile:` token of a ticket, for a chip's prompt.
fn token(ticket: &Option<String>) -> String {
    ticket.as_deref().map(|ticket| format!("@bir:taxprofile:{ticket}")).unwrap_or_else(|| "my taxpayers".into())
}

/// Add the card (and next-step chips) a tool's answer to the assistant
/// carries. `args` are the arguments as the assistant sent them (tickets).
pub(crate) fn decorate(tool: &str, args: &Map<String, Value>, result: &mut Value) {
    if result["isError"] == true {
        return;
    }
    let Some(data) = data(result) else { return };
    let ticket = ticket_arg(args);
    let who = data["taxpayer"].as_str().or(data["name"].as_str()).unwrap_or("").to_string();
    let args_value = Value::Object(args.clone());
    let (card, chips): (Value, Vec<(&str, String)>) = match tool {
        taxpayer::PROFILE_FORMS => (
            open("forms", &format!("Forms · {who}"), ticket.as_deref(), data.clone(), tool, args_value),
            vec![
                ("What's due next", format!("What's due for {}?", token(&ticket))),
                ("This year's returns", format!("How did {} do this year?", token(&ticket))),
            ],
        ),
        taxpayer::RETURNS_LIST if args.contains_key("form") && args.contains_key("period") => (
            open("status", &format!("Return · {who}"), ticket.as_deref(), data.clone(), tool, args_value),
            vec![("Anything unpaid?", format!("What hasn't {} paid?", token(&ticket)))],
        ),
        taxpayer::RETURNS_LIST => (
            open("matrix", &format!("Returns · {who}"), ticket.as_deref(), data.clone(), tool, args_value),
            vec![
                ("Anything late?", format!("Is anything late for {}?", token(&ticket))),
                ("What's unpaid?", format!("What hasn't {} paid?", token(&ticket))),
            ],
        ),
        "bir_calendar_get" if one_week(args) => (
            open("week", "This week", ticket.as_deref(), data.clone(), tool, args_value),
            vec![("Anything late?", "Is anything late?".into())],
        ),
        "bir_calendar_get" if one_month(args) => (
            open("month", "Calendar", ticket.as_deref(), data.clone(), tool, args_value),
            vec![("Anything late?", "Is anything late?".into())],
        ),
        "bir_calendar_get" => (
            open("dues", "What's due", ticket.as_deref(), data.clone(), tool, args_value),
            vec![
                ("Anything late?", "Is anything late?".into()),
                ("How are my clients?", "How are my clients doing?".into()),
            ],
        ),
        answers::TAXPAYER if data["view"] == "branches" => (
            open("branches", &who, ticket.as_deref(), data.clone(), tool, args_value),
            vec![("Who is this?", format!("Who is {}?", token(&ticket)))],
        ),
        answers::TAXPAYER => (
            open("taxpayer", &who, ticket.as_deref(), data.clone(), tool, args_value),
            vec![
                ("Her forms", format!("What forms is {} registered for?", token(&ticket))),
                ("What's due", format!("What's due for {}?", token(&ticket))),
            ],
        ),
        answers::FIND_TAXPAYER => (
            open("pick", "Which taxpayer?", None, data.clone(), tool, args_value),
            vec![],
        ),
        answers::PENALTY if data["mode"] == "estimate" => (
            open("penalty", "Penalty estimate", ticket.as_deref(), data.clone(), tool, args_value),
            vec![],
        ),
        answers::PENALTY => (
            open("overdue", "Late returns", ticket.as_deref(), data.clone(), tool, args_value),
            vec![("What's due next", "What's due next?".into())],
        ),
        answers::UNPAID => (
            open("unpaid", "Not paid yet", ticket.as_deref(), data.clone(), tool, args_value),
            vec![],
        ),
        answers::BOARD if data["mode"] == "bulk" => (
            open("bulk", data["label"].as_str().unwrap_or("One form, every taxpayer"), None, data.clone(), tool, args_value),
            vec![],
        ),
        answers::BOARD if data["mode"] == "on_time" => (
            open("streak", &format!("On time · {}", data["year"]), None, data.clone(), tool, args_value),
            vec![("Who is late now?", "Which returns are late?".into())],
        ),
        answers::BOARD => (
            open("board", "My taxpayers", None, data.clone(), tool, args_value),
            vec![("Only the late ones", "Which returns are late?".into())],
        ),
        answers::QUICK_ADD => (
            open("quickadd", if data["exists"] == true { "Already in BIR" } else { "New taxpayer" }, None, data.clone(), "", args_value),
            vec![],
        ),
        answers::OUTBOX => (
            open("outbox", "Sending queue", None, data.clone(), tool, args_value),
            vec![],
        ),
        answers::DIGEST => (
            open("digest", "Since yesterday", None, data.clone(), tool, args_value),
            vec![("What's unpaid?", "What haven't I paid?".into())],
        ),
        lookup::FORM_INFO => (
            open("explain", &format!("{} · {}", data["form"].as_str().unwrap_or(""), data["title"].as_str().unwrap_or("")), ticket.as_deref(), data.clone(), tool, args_value),
            vec![("What's due for it?", format!("When is {} due?", data["form"].as_str().unwrap_or("it")))],
        ),
        lookup::LOOKUP => {
            let (card, title) = match data["mode"].as_str() {
                Some("atc") => ("atc", format!("ATC {}", data["code"].as_str().unwrap_or(""))),
                Some("rdo") => ("rdo", "Revenue district office".to_string()),
                _ => ("rate", "Quick tax".to_string()),
            };
            (open(card, &title, None, data.clone(), tool, args_value), vec![])
        }
        calendar::DEADLINE => (
            open("shift", &format!("{} deadline", data["label"].as_str().unwrap_or("The")), ticket.as_deref(), data.clone(), tool, args_value),
            vec![],
        ),
        return_view::RETURN => {
            let card = match data["view"].as_str() {
                Some("paper") => "preview",
                Some("amend") => "amend",
                Some("attachments") => "attach",
                Some("import") => "import",
                _ => "summary",
            };
            let label = data["label"].as_str().unwrap_or("Return");
            let chips = match card {
                "summary" => vec![("Show the form", format!("Show me {label} before I send it"))],
                "preview" => vec![("What does it need?", format!("Can I send {label}?"))],
                _ => vec![],
            };
            // The import card changes the draft only when the user presses
            // Fill; it has nothing to refresh.
            let read = if card == "import" { "" } else { tool };
            (open(card, &format!("{label} · {}", data["taxpayer"].as_str().unwrap_or("")), ticket.as_deref(), data.clone(), read, args_value), chips)
        }
        "bir_drafts_list" => (
            open("drafts", "Drafts", ticket.as_deref(), data.clone(), tool, args_value),
            vec![],
        ),
        "bir_payments_list" if !args.contains_key("form") => (
            open("paidsum", &format!("Paid{}", data["year"].as_str().map(|year| format!(" in {year}")).unwrap_or_default()), ticket.as_deref(), data.clone(), tool, args_value),
            vec![("What's unpaid?", format!("What hasn't {} paid?", token(&ticket)))],
        ),
        "bir_status" => (
            open("health", "BIR", None, data.clone(), tool, args_value),
            vec![],
        ),
        "bir_calendar_set" if args.contains_key("reminder_days") => (
            open("remind", &format!("Reminders · {}", data["taxpayer"].as_str().unwrap_or("")), ticket.as_deref(), data.clone(), "", args_value),
            vec![],
        ),
        "bir_form_needs_you" if data["ok"] != true => (
            open("needs", &format!("What {} needs", data["form"].as_str().unwrap_or("the return")), None, data.clone(), tool, args_value),
            vec![],
        ),
        "bir_form_send" if data["status"] == "approval_required" => (
            open("approve", &format!("Send {}?", data["form"].as_str().unwrap_or("this return")), None, data.clone(), "", args_value),
            vec![],
        ),
        "bir_payment_parse" if data.is_object() => (
            open("match", "Payment found", None, data.clone(), "", args_value),
            vec![],
        ),
        "bir_confirm_filing" => (
            // BIR's file name starts with the TIN: the card shows the return
            // and when BIR received it, never the file name.
            open("confirm", "BIR confirmation", None, without(&data, &["reference", "message_id", "filename"]), "", args_value),
            vec![],
        ),
        _ => return,
    };
    let meta = result
        .as_object_mut()
        .expect("a tool result is an object")
        .entry("_meta")
        .or_insert_with(|| json!({}));
    meta["rox/open"] = card;
    if !chips.is_empty() {
        meta["rox/suggest"] = suggest(&chips);
    }
}

/// The markpaid card for `bir_mark_paid` from the assistant: nothing is
/// recorded until the user presses Save on it (P5).
pub(crate) fn mark_paid_card(args: &Map<String, Value>) -> Value {
    let form = args.get("form").and_then(Value::as_str).unwrap_or("");
    let period = args.get("period").and_then(Value::as_str).unwrap_or("");
    let text = format!(
        "Ready to mark {form} {period} paid. Nothing is recorded until the user presses Save on the card (they can add the date, amount, channel and reference, and attach a receipt)."
    );
    let snapshot = json!({
        "tin": args.get("tin"),
        "form": form,
        "period": period,
        "note": args.get("note"),
    });
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": { "result": { "recorded": false, "pending_user": true, "form": form, "period": period } },
        "isError": false,
        "_meta": { "rox/open": open("markpaid", &format!("Mark {form} paid"), None, snapshot, "", json!({})) },
    })
}

/// Shrink a card's props under the limit: the longest list in the
/// snapshot loses its tail (noted as `more`) until it fits.
pub(crate) fn fit(result: &mut Value) {
    let Some(props) = result.pointer_mut("/_meta/rox~1open/props") else { return };
    // Only answer cards carry a snapshot (an app's own open does not).
    if props.get("snapshot").is_none() {
        return;
    }
    // Props hold at most 100 items in a list (6.6).
    fn cap(value: &mut Value) -> bool {
        match value {
            Value::Array(items) => {
                let cut = items.len() > 100;
                items.truncate(100);
                items.iter_mut().fold(cut, |cut, item| cap(item) || cut)
            }
            Value::Object(map) => map.values_mut().fold(false, |cut, item| cap(item) || cut),
            _ => false,
        }
    }
    if cap(&mut props["snapshot"]) && props["snapshot"].is_object() {
        props["snapshot"]["more"] = json!(true);
    }
    for _ in 0..64 {
        if props.to_string().len() <= PROPS_MAX {
            return;
        }
        let Some(list) = longest_list(&mut props["snapshot"]) else { break };
        let keep = list.len() * 3 / 4;
        list.truncate(keep);
        props["snapshot"]["more"] = json!(true);
    }
    // Still too big: Refresh reads it live instead.
    props["snapshot"] = json!({ "more": true });
}

/// The longest list (by its JSON size) anywhere in `value`.
fn longest_list(value: &mut Value) -> Option<&mut Vec<Value>> {
    // Find its path first, then walk there mutably.
    fn find(value: &Value, path: &mut Vec<Value>, best: &mut Option<(Vec<Value>, usize)>) {
        match value {
            Value::Array(items) => {
                if items.len() > 1 {
                    let len = value.to_string().len();
                    if best.as_ref().is_none_or(|(_, known)| len > *known) {
                        *best = Some((path.clone(), len));
                    }
                }
                for (i, item) in items.iter().enumerate() {
                    path.push(json!(i));
                    find(item, path, best);
                    path.pop();
                }
            }
            Value::Object(map) => {
                for (key, item) in map {
                    path.push(json!(key));
                    find(item, path, best);
                    path.pop();
                }
            }
            _ => {}
        }
    }
    let mut best = None;
    find(value, &mut Vec::new(), &mut best);
    let (path, _) = best?;
    let mut at = value;
    for step in path {
        at = match step {
            Value::Number(i) => at.get_mut(i.as_u64()? as usize)?,
            Value::String(key) => at.get_mut(key.as_str())?,
            _ => return None,
        };
    }
    at.as_array_mut()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_is_inline_with_a_snapshot_and_fits_8_kib() {
        let rows: Vec<Value> = (0..500).map(|i| json!({"form": "2551Q", "period": format!("2026-{i}"), "status": "draft"})).collect();
        let mut result = json!({
            "content": [{"type": "text", "text": "x"}],
            "structuredContent": {"result": {"taxpayer": "Dela Cruz Jane", "returns": rows}},
            "isError": false,
        });
        let args = json!({"ticket": "tkt_0000000000000000000000000Z"}).as_object().unwrap().clone();
        decorate(taxpayer::RETURNS_LIST, &args, &mut result);
        fit(&mut result);
        let open = &result["_meta"]["rox/open"];
        assert_eq!(open["app"], "card-forms");
        assert_eq!(open["placement"], "inline");
        assert_eq!(open["props"]["card"], "matrix");
        assert!(open["props"]["as_of"].is_i64());
        assert!(open["props"].to_string().len() <= 8 * 1024);
        assert_eq!(open["props"]["snapshot"]["more"], true);
        assert!(result["_meta"]["rox/suggest"].as_array().unwrap().len() <= 3);
        // an error carries no card
        let mut refused = json!({"content": [{"type": "text", "text": "no"}], "isError": true});
        decorate(taxpayer::RETURNS_LIST, &args, &mut refused);
        assert!(refused.get("_meta").is_none());
    }

    #[test]
    fn every_kind_belongs_to_a_card_app() {
        for (id, app, _) in KINDS {
            assert!(APPS.contains(app), "{id}");
        }
    }
}
