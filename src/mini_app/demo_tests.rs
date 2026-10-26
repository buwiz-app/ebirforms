//! The Excel demo (`crates/bir-shell/demo/`): each workbook fills its
//! return through the agent's own tools and the return validates.
//!
//! Every test here runs on this thread's scratch database (never the
//! taxpayer database) for the made-up test taxpayer 123-456-789-00000.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::server::MiniServer;

const ROOT: &str = "123456789";
const TIN: &str = "12345678900000";

/// One sheet's cells: row number -> column letters -> text.
type Sheet = BTreeMap<u32, BTreeMap<String, String>>;

fn demo(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("demo")
        .join(name)
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The first sheet of an .xlsx: each cell's cached value or inline text.
fn read_sheet(name: &str) -> Sheet {
    let file = std::fs::File::open(demo(name)).unwrap_or_else(|error| panic!("{name}: {error}"));
    let mut book = zip::ZipArchive::new(file).expect("an xlsx is a zip");
    let mut xml = String::new();
    book.by_name("xl/worksheets/sheet1.xml")
        .expect("the first sheet")
        .read_to_string(&mut xml)
        .unwrap();
    let mut sheet = Sheet::new();
    for chunk in xml.split("<c r=\"").skip(1) {
        let Some((reference, rest)) = chunk.split_once('"') else {
            continue;
        };
        let body = rest.split("</c>").next().unwrap_or("");
        let value = if let Some((_, after)) = body.split_once("<v>") {
            after.split("</v>").next().unwrap_or("").to_string()
        } else if let Some((_, after)) = body.split_once("<t") {
            let after = after.split_once('>').map(|(_, text)| text).unwrap_or("");
            after.split("</t>").next().unwrap_or("").to_string()
        } else {
            continue;
        };
        let column: String = reference.chars().take_while(char::is_ascii_alphabetic).collect();
        let row: u32 = reference[column.len()..].parse().expect("a cell row");
        sheet.entry(row).or_default().insert(column, unescape(&value));
    }
    sheet
}

/// The `Total` row's value under `header` (the row whose first cell is `first`).
fn total(sheet: &Sheet, first: &str, header: &str) -> f64 {
    let headers = sheet
        .values()
        .find(|row| row.get("A").map(String::as_str) == Some(first))
        .expect("the header row");
    let column = headers
        .iter()
        .find(|(_, text)| text.as_str() == header)
        .map(|(column, _)| column.clone())
        .unwrap_or_else(|| panic!("no column `{header}`"));
    let totals = sheet
        .values()
        .find(|row| row.get("A").map(String::as_str) == Some("Total"))
        .expect("the Total row");
    totals[&column].parse().expect("a number")
}

/// Every data row's text under `header`.
fn column(sheet: &Sheet, first: &str, header: &str) -> Vec<String> {
    let (head, headers) = sheet
        .iter()
        .find(|(_, row)| row.get("A").map(String::as_str) == Some(first))
        .expect("the header row");
    let letter = headers
        .iter()
        .find(|(_, text)| text.as_str() == header)
        .map(|(column, _)| column.clone())
        .unwrap();
    sheet
        .range(head + 1..)
        .take_while(|(_, row)| row.get("A").map(String::as_str) != Some("Total"))
        .filter_map(|(_, row)| row.get(&letter).cloned())
        .collect()
}

fn amount(value: f64) -> String {
    format!("{value:.2}")
}

/// A tool call's answer, as the agent reads it.
fn tool(server: &mut MiniServer, name: &str, arguments: Value) -> Value {
    let reply = server
        .handle(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        }))
        .wait()
        .expect("answered");
    let result = &reply["result"];
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert_eq!(result["isError"], false, "{name}: {text}");
    serde_json::from_str(text).unwrap_or(Value::String(text.to_string()))
}

/// Dela Cruz Jane, the made-up test taxpayer, on a scratch database.
fn jane() -> MiniServer {
    let mut server = MiniServer::for_tests();
    tool(
        &mut server,
        "bir_call",
        json!({"fn": "create_taxpayer", "args": [{
            "tin_root": ROOT,
            "name": "Dela Cruz Jane",
            "taxpayer_type": "Individual",
            "head_office": {
                "rdo_code": "018",
                "registered_address": "12 Sampaguita Street, Barangay Barretto, Olongapo City",
                "zip_code": "2200",
                "phone": "09170000000",
                "email": "jane.delacruz@example.com"
            }
        }]}),
    );
    server
}

/// Validate, and on failure name every error.
fn validates(server: &mut MiniServer) {
    let checked = tool(server, "bir_form_validate", json!({}));
    assert_eq!(checked["ok"], true, "{checked:#}");
}

#[test]
fn the_2551q_sales_workbook_fills_a_return_that_validates() {
    let sheet = read_sheet("2551Q-2026-Q3-sales.xlsx");
    let gross = total(&sheet, "Month", "Gross sales");
    let tax = total(&sheet, "Month", "Percentage tax");
    assert!(column(&sheet, "Month", "ATC").iter().all(|atc| atc == "PT010"));
    assert_eq!(column(&sheet, "Month", "Month"), ["Jul 2026", "Aug 2026", "Sep 2026"]);

    let mut server = jane();
    // Item 13 is the filer's election, never the agent's. Jane's profile
    // records the graduated rates for 2026 (as filing her Q1 records them),
    // and she answered Q3's Item 13 "Not applicable" in the Forms app, which
    // saved the Q3 draft the agent then opens. Only the election is added
    // to the profile: no business start date, so no profile version is
    // confirmed by this save (that needs the app's reviewed plan).
    crate::bir_module::with_database(|db| {
        let mut profile = db.get_profile(TIN).unwrap().expect("Jane's profile");
        profile
            .tax_elections
            .push(bir_core::profile::TaxElectionHistory {
                taxable_year: 2026,
                election: bir_core::profile::IncomeTaxElection::GraduatedUnspecified,
                elected_at: chrono::NaiveDate::from_ymd_opt(2026, 4, 20)
                    .unwrap()
                    .and_hms_opt(9, 0, 0)
                    .unwrap(),
                source_form: "2551Q".into(),
            });
        let profile = db.save_profile(profile).unwrap();
        let mut draft = bir_core::forms::Form2551QDraft::new_from_profile(&profile, 2026, 3);
        draft.item_13_election = bir_core::forms::form_2551q::Item13Election::NotApplicable;
        db.save_2551q_draft(&draft).unwrap();
        Ok(())
    })
    .unwrap();

    tool(&mut server, "bir_form_open", json!({"code": "@bir:taxform:123456789/2551Q-2026-Q3"}));
    let fields = tool(&mut server, "bir_form_fields", json!({}));
    let keys: Vec<&str> = fields["boxes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry["key"].as_str())
        .collect();
    for key in ["txtATCAmt1", "frm2551Qv2018:txt15"] {
        assert!(keys.contains(&key), "{key} is not fillable: {keys:?}");
    }
    // The README's mapping: the quarter's gross sales on Schedule 1 row 1
    // (ATC PT010), no creditable tax withheld.
    let filled = tool(
        &mut server,
        "bir_form_fill",
        json!({"fields": {"txtATCAmt1": amount(gross), "frm2551Qv2018:txt15": "0.00"}, "source": "document"}),
    );
    assert!(filled.get("refused").is_none(), "{filled:#}");
    validates(&mut server);
    tool(&mut server, "bir_form_save_draft", json!({}));

    let draft = crate::bir_module::with_database(|db| Ok(db.get_2551q_draft(TIN, 2026, 3).unwrap()))
        .unwrap()
        .expect("saved");
    assert_eq!(draft.schedule_1[0].atc, "PT010");
    assert_eq!(amount(draft.schedule_1[0].taxable_amount), amount(gross));
    assert_eq!(amount(draft.total_tax_due), amount(tax), "the form's tax is the workbook's");
}

#[test]
fn the_1601c_payroll_workbook_fills_a_return_that_validates() {
    let sheet = read_sheet("1601C-2026-09-payroll.xlsx");
    let gross = total(&sheet, "Employee", "Gross compensation");
    let de_minimis = total(&sheet, "Employee", "De minimis benefits");
    let statutory = total(&sheet, "Employee", "Statutory contributions");
    let taxable = total(&sheet, "Employee", "Taxable compensation");
    let withheld = total(&sheet, "Employee", "Tax withheld");
    assert_eq!(column(&sheet, "Employee", "Employee").len(), 5);

    let mut server = jane();
    tool(&mut server, "bir_form_open", json!({"code": "123456789/1601C-2026-09"}));
    let filled = tool(
        &mut server,
        "bir_form_fill",
        json!({"fields": {
            "frm1601c:TaxWithheld": "1",
            "frm1601c:CatAgent": "P",
            "frm1601c:txtTax14": amount(gross),
            "frm1601c:txtTax15": "0.00",
            "frm1601c:txtTax16": "0.00",
            "frm1601c:txtTax17": "0.00",
            "frm1601c:txtTax18": amount(de_minimis),
            "frm1601c:txtTax19": amount(statutory),
            "frm1601c:txtTax20": "0.00",
            "frm1601c:txtTax23": "0.00",
            "frm1601c:txtTax25": amount(withheld),
        }, "source": "document"}),
    );
    assert!(filled.get("refused").is_none(), "{filled:#}");
    validates(&mut server);
    tool(&mut server, "bir_form_save_draft", json!({}));

    let draft = crate::bir_module::with_database(|db| Ok(db.get_1601c_draft(TIN, 2026, 9).unwrap()))
        .unwrap()
        .expect("saved");
    assert_eq!(
        amount(draft.tax_22_total_taxable),
        amount(taxable),
        "Item 22 is the workbook's taxable compensation"
    );
    assert_eq!(amount(draft.tax_27_taxes_withheld_for_remittance), amount(withheld));
}
