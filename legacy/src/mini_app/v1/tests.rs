//! The edge between BIR's tools and v1: resolve shapes (9.2), tickets in
//! and out, links (9.4) and `_meta["rox/open"]` (9.6). Fake TINs only.

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
        name: "Dela Cruz Jane".into(),
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

fn ask(db: &Database, args: Value) -> Value {
    resolve(db, args.as_object().unwrap(), today())
}

/// No TIN, in any form, anywhere in `value`.
fn no_tin(value: &Value) {
    let text = value.to_string();
    assert!(!text.contains(ROOT), "a TIN leaked: {text}");
    assert!(!text.contains("123-456-788"), "a TIN leaked: {text}");
}

#[test]
fn a_typed_tin_resolves_to_a_token_with_a_ticket() {
    let db = db();
    let answer = ask(&db, json!({"kind": "scope", "name": "taxprofile", "value": ROOT}));
    assert_eq!(answer["result"], "token", "{answer}");
    let ticket = answer["ticket"].as_str().unwrap();
    assert!(tickets::is_ticket(ticket));
    assert_eq!(answer["title"], "Dela Cruz Jane");
    no_tin(&answer);
    // the same taxpayer again: the same ticket; and the ticket resolves back
    let again = ask(&db, json!({"kind": "scope", "name": "taxprofile", "value": ticket}));
    assert_eq!(again["ticket"], ticket);
    // a TIN no one has is an error, capped at 200
    let missing = ask(&db, json!({"kind": "scope", "name": "taxprofile", "value": "987654321"}));
    assert_eq!(missing["result"], "error");
    assert!(missing["message"].as_str().unwrap().chars().count() <= 200);
}

#[test]
fn completion_offers_choices_whose_values_are_tickets() {
    let db = db();
    let answer = ask(&db, json!({"kind": "scope", "name": "taxprofile", "value": "", "complete": true}));
    assert_eq!(answer["result"], "choices", "{answer}");
    let choices = answer["choices"].as_array().unwrap();
    assert_eq!(choices.len(), 1);
    assert!(tickets::is_ticket(choices[0]["value"].as_str().unwrap()));
    assert!(choices[0]["label"].as_str().unwrap().chars().count() <= 80);
    // the taxform steps keep their tail after the ticket
    let forms = ask(&db, json!({"kind": "scope", "name": "taxform", "value": format!("{ROOT}/"), "complete": true}));
    assert_eq!(forms["result"], "choices", "{forms}");
    for choice in forms["choices"].as_array().unwrap() {
        let value = choice["value"].as_str().unwrap();
        assert!(value.starts_with("tkt_") && !value.contains(ROOT), "{value}");
    }
}

#[test]
fn commands_open_same_plugin_apps_with_tickets_and_no_tins_in_props() {
    let db = db();
    let profile = ask(&db, json!({"kind": "command", "name": "taxprofile", "value": ROOT}));
    assert_eq!(profile["result"], "open", "{profile}");
    assert_eq!(profile["app"], "tax-profile");
    assert_eq!(profile["props"]["ticket"], profile["ticket"]);
    assert_eq!(profile["placement"], "full");
    no_tin(&profile);
    // /taxform for the current taxpayer (just set): a return's ticket
    let form = ask(&db, json!({"kind": "command", "name": "taxform", "value": "2551Q Q3 2026 view"}));
    assert_eq!(form["result"], "open", "{form}");
    assert_eq!(form["app"], "forms");
    assert_eq!(form["props"]["form"], "2551Q");
    assert_eq!(form["props"]["period"], "2026-Q3");
    assert_eq!(form["props"]["action"], "view");
    assert_eq!(form["title"], "2551Q · Q3 2026 · Dela Cruz Jane");
    let ticket = form["ticket"].as_str().unwrap();
    assert_eq!(form["props"]["ticket"], ticket);
    let subject = tickets::subject(&db, ticket).unwrap().unwrap();
    assert_eq!(subject.return_value(), Some("123456788/2551Q-2026-Q3"));
    no_tin(&form);
    // the return as a scope token resolves to the same ticket
    let token = ask(&db, json!({"kind": "scope", "name": "taxform", "value": format!("{ROOT}/2551Q-2026-Q3")}));
    assert_eq!(token["result"], "token");
    assert_eq!(token["ticket"], ticket);
}

#[test]
fn an_attached_ticket_is_the_default_taxpayer() {
    let db = db();
    let ticket = tickets::issue(&db, &Subject::Taxpayer(ROOT.into())).unwrap();
    let attached = json!([{ "scope": "taxprofile", "token": format!("@bir:taxprofile:{ticket}") }]);
    let form = ask(
        &db,
        json!({"kind": "command", "name": "taxform", "value": "1601C 2026-09", "context": {"attached": attached}}),
    );
    assert_eq!(form["result"], "open", "{form}");
    let subject = tickets::subject(&db, form["ticket"].as_str().unwrap()).unwrap().unwrap();
    assert_eq!(subject.root(), ROOT);
}

#[test]
fn tickets_in_tool_arguments_become_tins_and_apps_may_not_name_tins() {
    let db = db();
    let taxpayer = tickets::issue(&db, &Subject::Taxpayer(ROOT.into())).unwrap();
    let ret = tickets::issue(&db, &Subject::of_return(&format!("{ROOT}/2551Q-2026-Q3"), today()).unwrap()).unwrap();
    let mut args = json!({"tin": taxpayer, "tins": [taxpayer]}).as_object().unwrap().clone();
    expand(&db, "bir_payments_list", &mut args, Origin::Agent).unwrap();
    assert_eq!(args["tin"], TIN, "a taxpayer's ticket is its head office");
    assert_eq!(args["tins"], json!([ROOT]), "a root covers every branch");
    // a return's ticket as bir_form_open's code; as `tin` it names the code too
    let mut args = json!({"code": ret}).as_object().unwrap().clone();
    expand(&db, "bir_form_open", &mut args, Origin::Agent).unwrap();
    assert_eq!(args["code"], "123456788/2551Q-2026-Q3");
    let mut args = json!({"tin": ret}).as_object().unwrap().clone();
    expand(&db, "bir_form_open", &mut args, Origin::Agent).unwrap();
    assert_eq!(args["code"], "123456788/2551Q-2026-Q3");
    // the agent may still type a TIN; an app may not (P1)
    let mut args = json!({"tin": ROOT}).as_object().unwrap().clone();
    expand(&db, "bir_calendar_get", &mut args, Origin::Agent).unwrap();
    assert!(expand(&db, "bir_calendar_get", &mut args, Origin::App).is_err());
    // a ticket BIR did not issue is refused
    let mut args = json!({"tin": format!("tkt_{}", "0".repeat(26))}).as_object().unwrap().clone();
    assert!(expand(&db, "bir_calendar_get", &mut args, Origin::Agent).is_err());
}

#[test]
fn results_the_model_reads_name_taxpayers_by_ticket() {
    let db = db();
    let link = format!(
        "rox://app/bir/forms?props={}",
        resolve::encode_component(&json!({"tin": TIN, "form": "2551Q", "period": "2026-Q3", "action": "file"}).to_string())
    );
    let mut result = json!({
        "content": [{ "type": "text", "text": "x" }],
        "structuredContent": { "result": {
            "tin": TIN,
            "tins": [ROOT],
            "key": format!("{ROOT}/2551Q-2026-Q3"),
            "token": format!("@bir:taxform:{ROOT}/2551Q-2026-Q3"),
            "note": "Filed for 123-456-788-00000 and 987-654-321",
            "open": link,
            "amount": 98765,
        }},
    });
    let text = serde_json::to_string_pretty(&result["structuredContent"]).unwrap();
    result["content"][0]["text"] = json!(text);
    Scrubber::new(&db, today()).result(&mut result);
    let out = &result["structuredContent"]["result"];
    assert!(tickets::is_ticket(out["tin"].as_str().unwrap()), "{out}");
    assert_eq!(out["key"], out["token"].as_str().unwrap().trim_start_matches("@bir:taxform:"));
    assert!(out["note"].as_str().unwrap().contains("[TIN]"), "an unsaved TIN is masked");
    let open = out["open"].as_str().unwrap();
    assert!(open.starts_with("rox://app/dev.goldcoders.bir/forms?ticket=tkt_"), "{open}");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(!text.contains(ROOT) && !text.contains("123-456-788"), "{text}");
    // numbers are not text: an amount is left alone
    assert_eq!(out["amount"], 98765);
}

#[test]
fn links_carry_the_plugin_id_and_a_ticket_and_open_meta_is_the_v1_shape() {
    let db = db();
    let opened = v1_props(&db, "tax-profile", &json!({"tin": ROOT, "year": 2026}), today()).unwrap();
    let ticket = opened.ticket.clone().unwrap();
    assert_eq!(opened.props, json!({"ticket": ticket, "year": 2026}));
    let link = app_link("tax-profile", &opened);
    assert_eq!(
        link,
        format!("rox://app/dev.goldcoders.bir/tax-profile?ticket={ticket}&props=%7B%22year%22%3A2026%7D")
    );
    assert!(link.len() <= 2048);
    let meta = open_meta("tax-profile", "BIR Tax Profile", &opened, "full");
    assert_eq!(
        meta,
        json!({"app": "tax-profile", "props": opened.props, "ticket": ticket, "title": "BIR Tax Profile", "placement": "full"})
    );
    // no taxpayer: no ticket, plain link
    let none = v1_props(&db, "calendar", &json!({}), today()).unwrap();
    assert_eq!(app_link("calendar", &none), "rox://app/dev.goldcoders.bir/calendar");
}
