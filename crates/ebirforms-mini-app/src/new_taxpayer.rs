//! A new taxpayer from the Tax Profile app's New taxpayer form
//! (`ebirforms_app_create_taxpayer`, [`crate::scope::Rule::New`]).
//!
//! The form's fields are checked one by one here, then written by
//! eBIRForms' own create (`BirApp::create_taxpayer`: the taxpayer and its
//! `00000` head office, refusing a TIN already saved). What goes back is the
//! new taxpayer's ticket and title, so the app opens its profile on the
//! ticket; neither the answer nor an error names the TIN.
//!
//! Only an app reaches this (an app-only tool, written from the person's own
//! press of Create): the assistant cannot add a taxpayer.

use bir_app_core::{BirApp, BranchFacts, NewTaxpayer};
use chrono::NaiveDate;
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::tickets::{self, Subject};

/// The taxpayer types eBIRForms keeps, as the form offers them.
const TYPES: [&str; 6] = [
    "Individual",
    "Corporation",
    "Partnership",
    "Cooperative",
    "Estate",
    "Trust",
];
const TOP: [&str; 4] = ["tin_root", "name", "taxpayer_type", "head_office"];
const HEAD: [&str; 5] = [
    "rdo_code",
    "registered_address",
    "zip_code",
    "phone",
    "email",
];

fn email_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[^\s@]+@[^\s@]+\.[^\s@]+$").expect("regex"))
}

/// `create_taxpayer [{tin_root, name, taxpayer_type, head_office: {rdo_code,
/// registered_address?, zip_code?, phone?, email?}}]`: `{ticket, title}`.
///
/// A refusal is one line per field, `<field>: <what to fix>`, so the form
/// shows each next to its field.
pub(crate) fn create(app: &BirApp, args: &[Value], today: NaiveDate) -> Result<Value, String> {
    let [Value::Object(input)] = args else {
        return Err("takes one argument: the new taxpayer, as an object".into());
    };
    let rdos = rdo_codes(app)?;
    let new = check(input, &rdos).map_err(|problems| problems.join("\n"))?;
    let root = new.tin_root.clone();
    app.create_taxpayer(&new)
        .map_err(|error| refusal(&error.to_string(), &root))?;
    let subject = Subject::Taxpayer(root);
    let ticket = tickets::issue(app, &subject)?;
    let title = subject.title(app, today).unwrap_or(new.name);
    Ok(json!({ "ticket": ticket, "title": title }))
}

/// The RDO codes of the table the form picks from (`list_rdos`).
fn rdo_codes(app: &BirApp) -> Result<Vec<String>, String> {
    let listed = app
        .call("list_rdos", Vec::new())
        .map_err(|error| error.to_string())?;
    Ok(listed
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|rdo| rdo.get("code").and_then(Value::as_str))
        .map(str::to_string)
        .collect())
}

/// The new taxpayer as typed, or every problem with it, by field.
pub(crate) fn check(
    input: &Map<String, Value>,
    rdos: &[String],
) -> Result<NewTaxpayer, Vec<String>> {
    let mut problems = Vec::new();
    let empty = Map::new();
    let head = match input.get("head_office") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(head)) => head,
        Some(_) => return Err(vec!["head_office: must be an object".into()]),
    };
    for (object, allowed) in [(input, &TOP[..]), (head, &HEAD[..])] {
        if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
            problems.push(format!("{key}: not a field of a new taxpayer"));
        }
    }
    let mut text = |object: &Map<String, Value>, key: &str| match object.get(key) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(value)) => value.trim().to_string(),
        Some(_) => {
            problems.push(format!("{key}: must be text"));
            String::new()
        }
    };
    let tin_root = text(input, "tin_root");
    let name = text(input, "name");
    let taxpayer_type = text(input, "taxpayer_type");
    let facts = BranchFacts {
        rdo_code: text(head, "rdo_code"),
        registered_address: text(head, "registered_address"),
        zip_code: text(head, "zip_code"),
        phone: text(head, "phone"),
        email: text(head, "email"),
    };
    if !(tin_root.len() == 9 && tin_root.bytes().all(|b| b.is_ascii_digit())) {
        problems.push("tin_root: the TIN is 9 digits, like 123-456-789".into());
    }
    if name.is_empty() {
        problems.push("name: type the registered name".into());
    }
    if !TYPES.contains(&taxpayer_type.as_str()) {
        problems.push(format!("taxpayer_type: pick one of {}", TYPES.join(", ")));
    }
    if facts.rdo_code.is_empty() {
        problems.push("rdo_code: pick the RDO the taxpayer is registered with".into());
    } else if !rdos.contains(&facts.rdo_code) {
        problems.push("rdo_code: pick an RDO from the list".into());
    }
    if !facts.zip_code.is_empty()
        && !(facts.zip_code.len() == 4 && facts.zip_code.bytes().all(|b| b.is_ascii_digit()))
    {
        problems.push("zip_code: a ZIP code is 4 digits, like 1226".into());
    }
    let phone_digits = facts.phone.chars().filter(char::is_ascii_digit).count();
    if !facts.phone.is_empty() && !(7..=13).contains(&phone_digits) {
        problems.push("phone: type a phone number like 09171234567 or 028123 4567".into());
    }
    if !facts.email.is_empty() && !email_re().is_match(&facts.email) {
        problems.push("email: type an email like name@example.com".into());
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(NewTaxpayer {
        tin_root,
        name,
        taxpayer_type,
        birth_date: None,
        head_office: facts,
    })
}

/// What eBIRForms' create refused, by field, without the TIN.
fn refusal(message: &str, root: &str) -> String {
    if message.contains("already exists") {
        return "tin_root: a taxpayer with this TIN is already saved in eBIRForms".into();
    }
    let message = message.replace(root, "this TIN");
    if message.contains("TIN") {
        format!("tin_root: {message}")
    } else {
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rdos() -> Vec<String> {
        vec!["043".into(), "050".into()]
    }

    fn input(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    fn good() -> Value {
        json!({
            "tin_root": "000111222",
            "name": "Synthetic Sample Trading",
            "taxpayer_type": "Corporation",
            "head_office": {
                "rdo_code": "043",
                "registered_address": "1 Sample Street, Sample City",
                "zip_code": "1600",
                "phone": "09170000000",
                "email": "sample@example.invalid"
            }
        })
    }

    fn fields(problems: &[String]) -> Vec<&str> {
        problems
            .iter()
            .map(|line| line.split_once(':').unwrap().0)
            .collect()
    }

    #[test]
    fn a_complete_form_passes_as_typed() {
        let new = check(&input(good()), &rdos()).unwrap();
        assert_eq!(new.tin_root, "000111222");
        assert_eq!(new.head_office.rdo_code, "043");
        assert_eq!(new.birth_date, None);
        // only the TIN, name, type and RDO are required
        let bare = json!({ "tin_root": "000111222", "name": "Synthetic", "taxpayer_type": "Individual", "head_office": { "rdo_code": "050" } });
        assert!(check(&input(bare), &rdos()).is_ok());
    }

    #[test]
    fn each_bad_field_is_named() {
        let bad = json!({
            "tin_root": "77777",
            "name": "  ",
            "taxpayer_type": "Robot",
            "head_office": { "zip_code": "16", "phone": "12", "email": "nope" }
        });
        let problems = check(&input(bad), &rdos()).unwrap_err();
        assert_eq!(
            fields(&problems),
            [
                "tin_root",
                "name",
                "taxpayer_type",
                "rdo_code",
                "zip_code",
                "phone",
                "email"
            ]
        );
        assert!(
            problems.iter().all(|line| !line.contains("77777")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_bad_tin_is_refused() {
        for tin in ["00011122", "0001112223", "000-111-222", "00011122x", ""] {
            let mut value = good();
            value["tin_root"] = json!(tin);
            assert_eq!(
                fields(&check(&input(value), &rdos()).unwrap_err()),
                ["tin_root"],
                "{tin}"
            );
        }
    }

    #[test]
    fn the_rdo_is_required_and_from_the_table() {
        let mut missing = good();
        missing["head_office"]["rdo_code"] = json!("");
        let problems = check(&input(missing), &rdos()).unwrap_err();
        assert_eq!(fields(&problems), ["rdo_code"]);
        let mut gone = good();
        gone["head_office"]
            .as_object_mut()
            .unwrap()
            .remove("rdo_code");
        assert_eq!(
            fields(&check(&input(gone), &rdos()).unwrap_err()),
            ["rdo_code"]
        );
        let mut unknown = good();
        unknown["head_office"]["rdo_code"] = json!("999");
        assert_eq!(
            fields(&check(&input(unknown), &rdos()).unwrap_err()),
            ["rdo_code"]
        );
    }

    #[test]
    fn nothing_but_the_form_s_fields() {
        let mut extra = good();
        extra["birth_date"] = json!("1990-01-01");
        assert_eq!(
            fields(&check(&input(extra), &rdos()).unwrap_err()),
            ["birth_date"]
        );
        let mut extra = good();
        extra["head_office"]["line_of_business"] = json!("x");
        assert_eq!(
            fields(&check(&input(extra), &rdos()).unwrap_err()),
            ["line_of_business"]
        );
    }

    #[test]
    fn a_refusal_never_names_the_tin() {
        let duplicate = refusal(
            "A taxpayer with TIN root 000111222 already exists",
            "000111222",
        );
        assert!(duplicate.starts_with("tin_root: "), "{duplicate}");
        assert!(!duplicate.contains("000111222"));
        let other = refusal(
            "A TIN root is exactly 9 digits; got `000111222`",
            "000111222",
        );
        assert!(
            other.starts_with("tin_root: ") && !other.contains("000111222"),
            "{other}"
        );
    }
}
