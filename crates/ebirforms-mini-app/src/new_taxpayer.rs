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

// The field rules, the same as the form's (`apps/ebirforms/tax-profile.tsx`:
// `TIN_RE`, `ZIP_RE`, `EMAIL_RE`, `phoneOk`): keep the two in step. The
// tests below hold the same examples.

/// An email: `local@domain.tld`, no spaces. The local part is letters,
/// digits and `. _ % + -`; the domain is labels of letters, digits and inner
/// hyphens joined by dots, ending in a top-level domain of 2 or more letters.
fn email_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^[A-Za-z0-9._%+-]+@[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?(?:\.[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?)*\.[A-Za-z]{2,}$",
        )
        .expect("regex")
    })
}
/// The longest email kept.
const EMAIL_MAX: usize = 254;

fn email_ok(email: &str) -> bool {
    email.len() <= EMAIL_MAX && email_re().is_match(email)
}

/// A Philippine phone number. Spaces, dashes and brackets are allowed and
/// only the digits count, 12 at most (the form stops typing there):
/// - a mobile: `09` and 9 more digits (11), or `639` and 9 more (12), with
///   or without a leading `+`;
/// - a landline: 7 or 8 digits not starting with 0, or with its area code
///   9 or 10 digits starting with `0` but not `09`.
///
/// A `+` only leads, and only in `+639…`; nothing else is allowed.
fn phone_ok(phone: &str) -> bool {
    let body = phone.strip_prefix('+').unwrap_or(phone);
    if body.is_empty()
        || !body
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ' ' | '(' | ')' | '-'))
    {
        return false;
    }
    let digits: String = body.chars().filter(char::is_ascii_digit).collect();
    let d = digits.as_bytes();
    if phone.starts_with('+') {
        return d.len() == 12 && digits.starts_with("639");
    }
    match d.len() {
        11 => digits.starts_with("09"),
        12 => digits.starts_with("639"),
        7 | 8 => d[0] != b'0',
        9 | 10 => d[0] == b'0' && (b'1'..=b'8').contains(&d[1]),
        _ => false,
    }
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
    let zips = zip_codes(app)?;
    let new = check(input, &rdos, &zips).map_err(|problems| problems.join("\n"))?;
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

/// The ZIP codes of the table the form picks from (`list_zip_codes`, each
/// "1000 - Manila CPO - Ermita": the code, then the place).
fn zip_codes(app: &BirApp) -> Result<Vec<String>, String> {
    let listed = app
        .call("list_zip_codes", Vec::new())
        .map_err(|error| error.to_string())?;
    Ok(listed
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|entry| entry.split(" - ").next())
        .map(|code| code.trim().to_string())
        .collect())
}

/// The new taxpayer as typed, or every problem with it, by field. `rdos`
/// and `zips` are the codes of the lists the form picks from: the RDO and
/// the ZIP code must be one of them, never free text.
pub(crate) fn check(
    input: &Map<String, Value>,
    rdos: &[String],
    zips: &[String],
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
    if !facts.zip_code.is_empty() {
        if !(facts.zip_code.len() == 4 && facts.zip_code.bytes().all(|b| b.is_ascii_digit())) {
            problems.push("zip_code: a ZIP code is 4 digits, like 1226".into());
        } else if !zips.contains(&facts.zip_code) {
            problems.push("zip_code: pick a ZIP code from the list".into());
        }
    }
    if !facts.phone.is_empty() && !phone_ok(&facts.phone) {
        problems.push(
            "phone: type a Philippine number: a mobile 09XXXXXXXXX, +639XXXXXXXXX or 639XXXXXXXXX, or a landline of 7 to 10 digits with an optional area code, like (02) 8123 4567".into(),
        );
    }
    if !facts.email.is_empty() && !email_ok(&facts.email) {
        problems.push("email: type an email like name@example.com, with no spaces".into());
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

    fn zips() -> Vec<String> {
        vec!["1101".into(), "1600".into()]
    }

    /// `check` with the lists above.
    fn checked(value: Value) -> Result<NewTaxpayer, Vec<String>> {
        check(&input(value), &rdos(), &zips())
    }

    /// The fields `check` refuses `good()` with `field` of the head office
    /// set to `value`.
    fn refused_head(field: &str, value: &str) -> Vec<String> {
        let mut form = good();
        form["head_office"][field] = json!(value);
        match checked(form) {
            Ok(_) => Vec::new(),
            Err(problems) => fields(&problems).iter().map(|f| f.to_string()).collect(),
        }
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
        let new = checked(good()).unwrap();
        assert_eq!(new.tin_root, "000111222");
        assert_eq!(new.head_office.rdo_code, "043");
        assert_eq!(new.birth_date, None);
        // only the TIN, name, type and RDO are required
        let bare = json!({ "tin_root": "000111222", "name": "Synthetic", "taxpayer_type": "Individual", "head_office": { "rdo_code": "050" } });
        assert!(checked(bare).is_ok());
    }

    #[test]
    fn each_bad_field_is_named() {
        let bad = json!({
            "tin_root": "77777",
            "name": "  ",
            "taxpayer_type": "Robot",
            "head_office": { "zip_code": "16", "phone": "12", "email": "nope" }
        });
        let problems = checked(bad).unwrap_err();
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
        for tin in [
            "00011122",
            "0001112223",
            "000-111-222",
            "00011122x",
            "000555666a",
            "00055566a",
            "abcdefghi",
            "",
        ] {
            let mut value = good();
            value["tin_root"] = json!(tin);
            assert_eq!(fields(&checked(value).unwrap_err()), ["tin_root"], "{tin}");
        }
    }

    #[test]
    fn the_rdo_is_required_and_from_the_table() {
        let mut missing = good();
        missing["head_office"]["rdo_code"] = json!("");
        let problems = checked(missing).unwrap_err();
        assert_eq!(fields(&problems), ["rdo_code"]);
        let mut gone = good();
        gone["head_office"]
            .as_object_mut()
            .unwrap()
            .remove("rdo_code");
        assert_eq!(fields(&checked(gone).unwrap_err()), ["rdo_code"]);
        let mut unknown = good();
        unknown["head_office"]["rdo_code"] = json!("999");
        assert_eq!(fields(&checked(unknown).unwrap_err()), ["rdo_code"]);
    }

    #[test]
    fn the_zip_is_a_code_from_the_table() {
        for zip in ["1101", "1600", ""] {
            assert!(refused_head("zip_code", zip).is_empty(), "{zip}");
        }
        // not in the table, not 4 digits, or the picker's label rather than its code
        for zip in ["9999", "16", "16000", "1600 - Pasig", "Pasig", "16a0"] {
            assert_eq!(refused_head("zip_code", zip), ["zip_code"], "{zip}");
        }
        // the RDO too: its code, never the label the input shows
        for rdo in ["043 - Pasig City", "Pasig City", "43"] {
            assert_eq!(refused_head("rdo_code", rdo), ["rdo_code"], "{rdo}");
        }
    }

    // The same examples as the form's rules (tax-profile.tsx `phoneOk`).
    #[test]
    fn a_phone_is_a_philippine_number() {
        for phone in [
            "",
            "09170000000",
            "0917 000 0000",
            "0917-000-0000",
            "+639170000000",
            "+63 917 000 0000",
            "+63 (917) 000-0000",
            "+639156837000",
            "09156837000",
            "639156837000",
            "639170000000",
            "63 917 000 0000",
            "8123 4567",
            "812 3456",
            "(02) 8123 4567",
            "02 8123-4567",
            "028123456",
            "(032) 234 5678",
        ] {
            assert!(refused_head("phone", phone).is_empty(), "{phone}");
        }
        for phone in [
            "12",
            "123456",
            "0917000000",
            "091700000000",
            "9170000000",
            "+63917000000",
            "+6391700000000",
            "+6328123 4567",
            "+1 415 555 0100",
            "63917000000",
            "6391700000000",
            "6391568370001",
            "+6391568370001",
            "091568370001",
            "638170000000",
            "0123 4567",
            "0912345678",
            "0012345678",
            "02.8123.4567",
            "0917000000a",
            "call 09170000000",
            "0917+0000000",
            "+",
            "()-",
        ] {
            assert_eq!(refused_head("phone", phone), ["phone"], "{phone}");
        }
    }

    // The same examples as the form's rules (tax-profile.tsx `EMAIL_RE`).
    #[test]
    fn an_email_is_plausible() {
        for email in [
            "",
            "sample@example.invalid",
            "first.last+tag@sub.example.ph",
            "a_b-c%d@ex-ample.co",
            "A@B.PH",
        ] {
            assert!(refused_head("email", email).is_empty(), "{email}");
        }
        let long = format!("{}@example.com", "a".repeat(250));
        for email in [
            "nope",
            "a@b",
            "a@b.c",
            "a@b.123",
            "@example.com",
            "name@",
            "na me@example.com",
            "name@exa mple.com",
            "name@@example.com",
            "name@-example.com",
            "name@example-.com",
            "name@example..com",
            "name@.example.com",
            "name@example.com.",
            "näme@example.com",
            long.as_str(),
        ] {
            assert_eq!(refused_head("email", email), ["email"], "{email}");
        }
    }

    #[test]
    fn nothing_but_the_form_s_fields() {
        let mut extra = good();
        extra["birth_date"] = json!("1990-01-01");
        assert_eq!(fields(&checked(extra).unwrap_err()), ["birth_date"]);
        let mut extra = good();
        extra["head_office"]["line_of_business"] = json!("x");
        assert_eq!(fields(&checked(extra).unwrap_err()), ["line_of_business"]);
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
