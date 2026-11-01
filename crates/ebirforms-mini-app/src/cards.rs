//! Answer cards (spec 9.8): which card an assistant's answer carries.
//!
//! The tool picks the card, never the model: after an eBIRForms tool answers
//! the assistant, [`decorate`] looks at what it answered and adds
//! `_meta["buwiz/open"]` for one of this plugin's inline card apps, with the
//! answer as `props.snapshot` and the tool and arguments the card's Refresh
//! re-reads it with. [`fit`] keeps the props under the Bot's 8 KiB limit.
//!
//! Round 1 (ported from `legacy/src/mini_app/cards.rs`): `card-dates` (what's
//! due, this week, this month, late returns) over the filing calendar, and
//! `card-calc` (an RDO). Round 2 adds the penalty estimate and the sending
//! queue to `card-dates`, and an ATC to `card-calc`. Every card only reads:
//! none can queue, send or cancel. `props.card` says which card an app draws.

use chrono::NaiveDate;
use serde_json::{Map, Value, json};

use crate::server::{ATC_LOOKUP, CALENDAR_GET, PENALTY_ESTIMATE, QUEUE_LIST, RDO_LOOKUP};
use crate::tickets;

/// Card kinds: id, app, small.
const KINDS: &[(&str, &str, bool)] = &[
    ("dues", "card-dates", false),
    ("week", "card-dates", false),
    ("month", "card-dates", false),
    ("overdue", "card-dates", false),
    ("penalty", "card-dates", false),
    ("queue", "card-dates", false),
    ("rdo", "card-calc", true),
    ("atc", "card-calc", true),
];

/// The read tool a card app's Refresh re-runs (named by the card's props,
/// so not written in its source): all the app may call.
pub(crate) fn refresh_tools(app: &str) -> &'static [&'static str] {
    match app {
        "card-dates" => &[CALENDAR_GET, PENALTY_ESTIMATE, QUEUE_LIST],
        "card-calc" => &[RDO_LOOKUP, ATC_LOOKUP],
        _ => &[],
    }
}

/// Props a card's snapshot may fill: a little under 8 KiB, so the Bot's
/// limit holds after encoding.
const PROPS_MAX: usize = 7_600;

/// The inline size a card app declares: each app draws cards of about one
/// height, and the card kit reports its content height (`buwiz.fit`) within
/// `min_height`..`max_height`. The Bot holds the placeholder until the kit's
/// `buwiz.ready()` (`wait_ready`).
pub(crate) fn inline(app: &str) -> Value {
    let height = if app == "card-calc" { 215 } else { 360 };
    json!({ "width": 560, "height": height, "min_width": 320, "min_height": 120, "max_height": 640, "resizable": true, "wait_ready": true })
}

/// The props every card app takes.
pub(crate) fn props_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "card": { "type": "string", "maxLength": 32 },
            "ticket": { "type": "string", "maxLength": 30 },
            "snapshot": { "type": "object" },
            "as_of": { "type": "integer" },
            "tool": { "type": "string", "maxLength": 64 },
            "args": { "type": "object" }
        }
    })
}

/// `_meta["buwiz/open"]` for card `id`: the answer's snapshot, and the read
/// tool and arguments Refresh uses.
fn open(
    id: &str,
    title: &str,
    ticket: Option<&str>,
    snapshot: Value,
    tool: &str,
    args: Value,
) -> Value {
    let (card, app, small) = *KINDS
        .iter()
        .find(|(kind, ..)| *kind == id)
        .expect("a known card");
    let mut props = json!({
        "card": card,
        "snapshot": snapshot,
        "args": args,
        "tool": tool,
        "as_of": chrono::Utc::now().timestamp_millis(),
    });
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

/// A `YYYY-MM-DD` argument.
fn day(args: &Map<String, Value>, key: &str) -> Option<NaiveDate> {
    args.get(key)
        .and_then(Value::as_str)
        .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok())
}

/// The card a calendar read shows: late returns, a week, one month, or
/// what's due.
fn calendar_card(args: &Map<String, Value>) -> (&'static str, &'static str) {
    if args.get("filter").and_then(Value::as_str) == Some("overdue") {
        return ("overdue", "Late returns");
    }
    match (day(args, "from"), day(args, "to")) {
        (Some(from), Some(to)) if (to - from).num_days() <= 7 => ("week", "This week"),
        (Some(from), Some(to))
            if from.format("%Y-%m").to_string() == to.format("%Y-%m").to_string() =>
        {
            ("month", "Calendar")
        }
        _ => ("dues", "What's due"),
    }
}

/// Add the card an answer to the assistant carries. `args` are the
/// arguments as the assistant sent them (tickets, never TINs).
pub(crate) fn decorate(tool: &str, args: &Map<String, Value>, result: &mut Value) {
    if result["isError"] == true {
        return;
    }
    let Some(data) = result.pointer("/structuredContent/result").cloned() else {
        return;
    };
    let ticket = args.get("tin").and_then(Value::as_str);
    let args_value = Value::Object(args.clone());
    let card = match tool {
        CALENDAR_GET => {
            let (id, title) = calendar_card(args);
            open(id, title, ticket, data, tool, args_value)
        }
        RDO_LOOKUP => open("rdo", "RDO", None, data, tool, args_value),
        ATC_LOOKUP => open("atc", "ATC", None, data, tool, args_value),
        PENALTY_ESTIMATE => {
            // a return's ticket in `form` opens that return in Forms
            let ticket = args
                .get("form")
                .and_then(Value::as_str)
                .filter(|form| tickets::is_ticket(form));
            open(
                "penalty",
                "Penalty estimate",
                ticket,
                data,
                tool,
                args_value,
            )
        }
        QUEUE_LIST => open("queue", "Sending queue", None, data, tool, args_value),
        _ => return,
    };
    result["_meta"]["buwiz/open"] = card;
    fit(result);
}

/// Shrink a card's props under the limit: the longest list in the
/// snapshot loses its tail (noted as `more`) until it fits; failing that,
/// the card reads live (Refresh) with no snapshot.
pub(crate) fn fit(result: &mut Value) {
    let Some(props) = result.pointer_mut("/_meta/buwiz~1open/props") else {
        return;
    };
    for _ in 0..64 {
        if props.to_string().len() <= PROPS_MAX {
            return;
        }
        let Some(list) = props["snapshot"]
            .as_object_mut()
            .and_then(|snapshot| {
                snapshot
                    .values_mut()
                    .filter_map(Value::as_array_mut)
                    .max_by_key(|list| list.len())
            })
            .filter(|list| list.len() > 1)
        else {
            break;
        };
        let keep = list.len() * 3 / 4;
        list.truncate(keep);
        props["snapshot"]["more"] = json!(true);
    }
    if props.to_string().len() > PROPS_MAX {
        props["snapshot"] = json!({ "more": true });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn a_calendar_read_picks_its_card_by_range_and_filter() {
        assert_eq!(calendar_card(&args(json!({}))).0, "dues");
        assert_eq!(
            calendar_card(&args(json!({"from": "2026-11-02", "to": "2026-11-08"}))).0,
            "week"
        );
        assert_eq!(
            calendar_card(&args(json!({"from": "2026-11-01", "to": "2026-11-30"}))).0,
            "month"
        );
        assert_eq!(
            calendar_card(&args(json!({"from": "2026-01-01", "to": "2026-12-31"}))).0,
            "dues"
        );
        assert_eq!(
            calendar_card(&args(json!({"filter": "overdue"}))).0,
            "overdue"
        );
    }

    #[test]
    fn a_card_is_inline_with_its_snapshot_and_fits_8_kib() {
        let events: Vec<Value> = (0..400)
            .map(|i| json!({"key": format!("tkt_{i:026}"), "taxpayer": "Sample Trading (synthetic)", "label": "2551Q · Q3 2026", "due": "2026-10-26"}))
            .collect();
        let mut result = json!({
            "content": [], "isError": false,
            "structuredContent": { "result": { "events": events } },
        });
        decorate(
            CALENDAR_GET,
            &args(json!({"tin": "tkt_00000000000000000000000000"})),
            &mut result,
        );
        let open = &result["_meta"]["buwiz/open"];
        assert_eq!(open["app"], "card-dates");
        assert_eq!(open["placement"], "inline");
        assert_eq!(open["props"]["card"], "dues");
        assert_eq!(open["props"]["tool"], CALENDAR_GET);
        assert_eq!(open["ticket"], "tkt_00000000000000000000000000");
        assert!(open["props"].to_string().len() <= PROPS_MAX);
        assert_eq!(open["props"]["snapshot"]["more"], true);
        assert!(
            !open["props"]["snapshot"]["events"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn every_card_app_carries_the_kit_verbatim_and_ships() {
        let kit = include_str!("../../../apps/ebirforms/card-kit.tsx.in").trim_end();
        for (app, source) in [
            ("card-dates", crate::manifest::CARD_DATES_SOURCE),
            ("card-calc", crate::manifest::CARD_CALC_SOURCE),
        ] {
            let begin = source.find("// card-kit:begin").expect("a kit");
            let end = source.find("// card-kit:end").expect("a kit end") + "// card-kit:end".len();
            assert_eq!(&source[begin..end], kit, "{app}'s card kit drifted");
            assert!(!refresh_tools(app).is_empty(), "{app} has no Refresh tool");
        }
        for (kind, app, _) in KINDS {
            assert!(
                crate::manifest::app_source(app).is_some(),
                "{kind}: {app} is not shipped"
            );
        }
    }

    #[test]
    fn round_two_answers_pick_their_cards() {
        let result =
            || json!({ "isError": false, "structuredContent": { "result": { "mode": "x" } } });
        for (tool, sent, app, card) in [
            (ATC_LOOKUP, json!({"query": "WV010"}), "card-calc", "atc"),
            (
                PENALTY_ESTIMATE,
                json!({"form": "2551Q", "due": "2026-07-25"}),
                "card-dates",
                "penalty",
            ),
            (QUEUE_LIST, json!({}), "card-dates", "queue"),
        ] {
            let mut answer = result();
            decorate(tool, &args(sent), &mut answer);
            let open = &answer["_meta"]["buwiz/open"];
            assert_eq!(open["app"], app, "{tool}");
            assert_eq!(open["props"]["card"], card, "{tool}");
            assert!(
                refresh_tools(app).contains(&tool),
                "{tool} is not {app}'s to call"
            );
            assert!(open.get("ticket").is_none(), "{tool}");
        }
        let mut answer = result();
        let ticket = "tkt_0000000000000000000000000A";
        decorate(
            PENALTY_ESTIMATE,
            &args(json!({"form": ticket, "due": "2026-07-25"})),
            &mut answer,
        );
        assert_eq!(answer["_meta"]["buwiz/open"]["ticket"], ticket);
    }

    #[test]
    fn errors_and_other_tools_carry_no_card() {
        let mut failed = json!({ "isError": true, "content": [] });
        decorate(CALENDAR_GET, &Map::new(), &mut failed);
        assert!(failed.get("_meta").is_none());
        let mut other = json!({ "isError": false, "structuredContent": { "result": {} } });
        decorate("ebirforms_status", &Map::new(), &mut other);
        assert!(other.get("_meta").is_none());
    }
}
