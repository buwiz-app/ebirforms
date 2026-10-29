//! What one instance of a mail routine watches (spec 5.9, "Routine
//! instances"): its `params`, `taxpayers` (TINs or names) and `forms`
//! (form codes). An empty list, or no params at all, watches everything,
//! so an instance made before params existed keeps working.
//!
//! The broker hands the params to the routine's search as arguments and to
//! every call of the routine's turn as `_meta["rox/caller"].params`; the
//! search narrows the Gmail query and the returns awaiting, and the turn's
//! writes are refused outside them.

use bir_core::db::Database;
use bir_core::forms::taxform;
use serde_json::{Map, Value, json};

use super::resolve::{self, Branch};

/// The `params` schema both mail routines declare.
pub(crate) fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "taxpayers": {
                "type": "array",
                "items": { "type": "string", "maxLength": 120 },
                "maxItems": 50,
                "description": "The taxpayers this instance watches, by TIN or name. Empty watches every taxpayer."
            },
            "forms": {
                "type": "array",
                "items": { "type": "string", "maxLength": 12 },
                "maxItems": 20,
                "description": "The BIR forms it watches, e.g. 2551Q, 1601C. Empty watches every form."
            }
        },
        "additionalProperties": false
    })
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Watch {
    taxpayers: Vec<String>,
    forms: Vec<&'static str>,
}

impl Watch {
    /// The watch `params` names; unknown keys are not BIR's to judge (the
    /// broker checked them against [`schema`]).
    pub(crate) fn of(params: &Map<String, Value>) -> Result<Self, String> {
        let list = |key: &str| -> Result<Vec<String>, String> {
            match params.get(key) {
                None | Some(Value::Null) => Ok(Vec::new()),
                Some(Value::Array(items)) => items
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .map(|text| text.trim().to_string())
                            .ok_or_else(|| format!("`{key}` must be a list of text (got {item})"))
                    })
                    .filter(|item| item.as_ref().map_or(true, |text| !text.is_empty()))
                    .collect(),
                Some(other) => Err(format!("`{key}` must be a list (got {other})")),
            }
        };
        let forms = list("forms")?
            .iter()
            .map(|form| {
                taxform::form_code_of(form).ok_or_else(|| format!("`forms`: `{form}` is not a BIR form code"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { taxpayers: list("taxpayers")?, forms })
    }

    /// The branches it watches; `None` watches every taxpayer.
    pub(crate) fn branches(&self, db: &Database) -> Result<Option<Vec<Branch>>, String> {
        if self.taxpayers.is_empty() {
            return Ok(None);
        }
        let all = resolve::branches(db, None)?;
        let mut out: Vec<Branch> = Vec::new();
        for wanted in &self.taxpayers {
            for branch in matching(&all, wanted)? {
                if !out.iter().any(|known| known.tin == branch.tin) {
                    out.push(branch.clone());
                }
            }
        }
        Ok(Some(out))
    }

    pub(crate) fn has_form(&self, code: &str) -> bool {
        self.forms.is_empty() || self.forms.iter().any(|form| form.eq_ignore_ascii_case(code))
    }

    /// The forms it watches, for the search's answer (empty: every form).
    pub(crate) fn forms(&self) -> &[&'static str] {
        &self.forms
    }

    /// Whether a write for `tin`'s `form` stays inside the watch.
    pub(crate) fn allows(&self, db: &Database, tin: Option<&str>, form: Option<&str>) -> Result<(), String> {
        if !self.forms.is_empty() {
            let code = form.and_then(taxform::form_code_of);
            if !code.is_some_and(|code| self.has_form(code)) {
                return Err(format!(
                    "this routine instance watches only {}, and this return is {}",
                    self.forms.join(", "),
                    form.unwrap_or("no form")
                ));
            }
        }
        let Some(branches) = self.branches(db)? else { return Ok(()) };
        let digits = tin.and_then(taxform::tin_digits);
        let inside = digits.is_some_and(|digits| {
            branches.iter().any(|branch| if digits.len() == 9 { branch.root == digits } else { branch.tin == digits })
        });
        if inside {
            Ok(())
        } else {
            Err("this routine instance does not watch this taxpayer".into())
        }
    }
}

/// The branches one `taxpayers` entry names: a TIN (a 9-digit root is every
/// branch), else a name, exact first, then the one taxpayer whose name
/// contains it.
fn matching<'a>(all: &'a [Branch], wanted: &str) -> Result<Vec<&'a Branch>, String> {
    if let Some(digits) = taxform::tin_digits(wanted) {
        let found: Vec<_> = all
            .iter()
            .filter(|branch| if digits.len() == 9 { branch.root == digits } else { branch.tin == digits })
            .collect();
        return if found.is_empty() {
            Err(format!("no taxpayer with TIN {}", resolve::format_tin(&digits)))
        } else {
            Ok(found)
        };
    }
    let fold = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let wanted = fold(wanted);
    let exact: Vec<_> = all.iter().filter(|branch| fold(&branch.name) == wanted).collect();
    if !exact.is_empty() {
        return Ok(exact);
    }
    let partial: Vec<_> = all.iter().filter(|branch| fold(&branch.name).contains(&wanted)).collect();
    let mut roots: Vec<&str> = partial.iter().map(|branch| branch.root.as_str()).collect();
    roots.sort_unstable();
    roots.dedup();
    match roots.len() {
        0 => Err(format!("no taxpayer named \"{wanted}\"")),
        1 => Ok(partial),
        _ => Err(format!("\"{wanted}\" names more than one taxpayer: give the TIN")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bir_core::profile::TaxpayerType;
    use bir_core::tax_profile::{BranchFacts, NewTaxpayer};
    use chrono::NaiveDate;

    const JUAN: &str = "123456788";
    const MARIA: &str = "987654321";

    fn db() -> Database {
        let db = Database::open_ephemeral().expect("ephemeral database");
        for (root, name) in [(JUAN, "Galang, Juan"), (MARIA, "Reyes, Maria")] {
            db.create_taxpayer(NewTaxpayer {
                tin_root: root.into(),
                name: name.into(),
                taxpayer_type: TaxpayerType::Individual,
                birth_date: None,
                head_office: BranchFacts {
                    rdo_code: "040".into(),
                    registered_address: "1 Example Street, Pasig".into(),
                    zip_code: "1600".into(),
                    phone: "0280000000".into(),
                    email: "office@example.com".into(),
                },
            })
            .expect("taxpayer");
        }
        db
    }

    fn watch(params: Value) -> Watch {
        Watch::of(params.as_object().unwrap()).unwrap()
    }

    fn roots(db: &Database, params: Value) -> Option<Vec<String>> {
        watch(params).branches(db).unwrap().map(|branches| branches.into_iter().map(|branch| branch.root).collect())
    }

    #[test]
    fn no_params_watch_everything() {
        let db = db();
        let all = watch(json!({}));
        assert_eq!(all, Watch::default());
        assert_eq!(roots(&db, json!({"taxpayers": []})), None);
        assert!(all.has_form("1601C"));
        assert!(all.allows(&db, None, None).is_ok());
    }

    #[test]
    fn a_taxpayer_is_named_by_tin_or_name() {
        let db = db();
        assert_eq!(roots(&db, json!({"taxpayers": [MARIA]})), Some(vec![MARIA.to_string()]));
        assert_eq!(roots(&db, json!({"taxpayers": ["123-456-788"]})), Some(vec![JUAN.to_string()]));
        assert_eq!(roots(&db, json!({"taxpayers": ["galang,  juan"]})), Some(vec![JUAN.to_string()]));
        assert_eq!(roots(&db, json!({"taxpayers": ["Maria"]})), Some(vec![MARIA.to_string()]));
        // "a" is in both names: give the TIN
        assert!(watch(json!({"taxpayers": ["a"]})).branches(&db).unwrap_err().contains("more than one"));
        assert!(watch(json!({"taxpayers": ["Santos"]})).branches(&db).unwrap_err().contains("no taxpayer named"));
    }

    #[test]
    fn forms_are_bir_form_codes() {
        let only = watch(json!({"forms": ["2551q"]}));
        assert_eq!(only.forms(), ["2551Q"]);
        assert!(only.has_form("2551Q") && !only.has_form("1601C"));
        assert!(Watch::of(json!({"forms": ["*"]}).as_object().unwrap()).unwrap_err().contains("not a BIR form code"));
    }

    #[test]
    fn a_write_stays_inside_the_watch() {
        let db = db();
        let maria_2551q = watch(json!({"taxpayers": ["Reyes, Maria"], "forms": ["2551Q"]}));
        assert!(maria_2551q.allows(&db, Some("98765432100000"), Some("2551Q")).is_ok());
        assert!(maria_2551q.allows(&db, Some(MARIA), Some("2551q")).is_ok());
        let other = maria_2551q.allows(&db, Some("12345678800000"), Some("2551Q")).unwrap_err();
        assert!(other.contains("does not watch this taxpayer"), "{other}");
        let form = maria_2551q.allows(&db, Some(MARIA), Some("1601C")).unwrap_err();
        assert!(form.contains("watches only 2551Q"), "{form}");
        assert!(maria_2551q.allows(&db, None, Some("2551Q")).is_err());
    }

    #[test]
    fn the_searches_keep_to_the_watch() {
        let db = db();
        let today = NaiveDate::from_ymd_opt(2026, 10, 26).unwrap();
        let params = json!({"taxpayers": ["Maria"], "forms": ["2551Q"]});
        for search in [super::super::confirm::search, super::super::payments::search] {
            let found = search(&db, params.as_object().unwrap(), today).unwrap();
            let query = found["query"].as_str().unwrap();
            assert!(query.contains(MARIA) && !query.contains(JUAN), "{query}");
            assert_eq!(found["forms"], json!(["2551Q"]));
        }
    }

    #[test]
    fn the_schema_is_one_the_broker_takes() {
        let schema = schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        assert!(schema.get("required").is_none(), "an instance made before params keeps working");
        for name in ["taxpayers", "forms"] {
            let prop = &schema["properties"][name];
            assert_eq!(prop["type"], "array");
            assert_eq!(prop["items"]["type"], "string");
        }
    }
}
