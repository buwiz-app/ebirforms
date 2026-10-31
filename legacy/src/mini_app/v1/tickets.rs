//! Tickets (spec 9.5, P1-P3): what Rox stores, links and shows the model in
//! place of a TIN.
//!
//! A ticket is `tkt_` and 26 Crockford base32 characters, 130 random bits,
//! with no subject data in it. The map from ticket to subject is kept in
//! BIR's own (encrypted) database, in one settings row, so it survives
//! restarts and never leaves this process: Rox learns a title only
//! (`rox.ticket.resolve`), and an app learns the TIN of a ticket only by
//! asking a BIR tool with that ticket (`bir_ticket_subject`). One subject
//! has one ticket, so a taxpayer pinned twice is the same chip.
//!
//! Subjects are a taxpayer (its 9-digit root: the head office and the
//! whole taxpayer), one branch (14 digits) or one return (`<tin>/<FORM>-
//! <period>`, the canonical value of a `@bir:taxform:` token). Every
//! subject's scope is its TIN root: an app holding a ticket reaches that
//! taxpayer and no other ([`super::super::scope`]). Titles are read live,
//! so a renamed taxpayer's chips follow, and a deleted one's tickets no
//! longer resolve.

use bir_core::db::Database;
use bir_core::forms::taxform;
use chrono::NaiveDate;
use serde_json::{Map, Value};

use super::super::resolve;

/// The settings row holding `{ticket: subject}`.
const STORE_KEY: &str = "rox.v1.tickets";
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// The longest title Rox shows for a token (9.2).
const TITLE_MAX: usize = 60;

/// What a ticket names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Subject {
    /// A taxpayer by its 9-digit root (its head office).
    Taxpayer(String),
    /// One branch by its 14-digit TIN.
    Branch(String),
    /// One return: `<token tin>/<FORM>-<period>`.
    Return(String),
}

impl Subject {
    /// The subject a TIN names: 9 digits, or 14 with branch `00000`, is the
    /// taxpayer; any other 14 digits is that branch.
    pub(crate) fn of_tin(tin: &str) -> Option<Self> {
        let digits = taxform::tin_digits(tin)?;
        Some(match digits.len() {
            14 if !digits[9..].chars().all(|c| c == '0') => Self::Branch(digits),
            _ => Self::Taxpayer(digits[..9].to_string()),
        })
    }

    /// The return a taxform value names (`<tin>/<FORM>-<period>`, a
    /// complete one), canonicalized.
    pub(crate) fn of_return(value: &str, today: NaiveDate) -> Option<Self> {
        let named = taxform::parse(value, today);
        let (tin, form, period) = (named.tin?, named.form?, named.period?);
        named
            .unknown
            .is_empty()
            .then(|| Self::Return(taxform::canonical_value(&tin, form, period)))
    }

    fn encode(&self) -> String {
        match self {
            Self::Taxpayer(root) => format!("tp:{root}"),
            Self::Branch(tin) => format!("br:{tin}"),
            Self::Return(value) => format!("rt:{value}"),
        }
    }

    fn decode(text: &str) -> Option<Self> {
        let (kind, rest) = text.split_once(':')?;
        match kind {
            "tp" => Some(Self::Taxpayer(rest.to_string())),
            "br" => Some(Self::Branch(rest.to_string())),
            "rt" => Some(Self::Return(rest.to_string())),
            _ => None,
        }
    }

    /// The TIN the subject stands for in a tool argument: the root, the
    /// branch's 14 digits, or the return's TIN.
    pub(crate) fn tin(&self) -> String {
        match self {
            Self::Taxpayer(root) => root.clone(),
            Self::Branch(tin) => tin.clone(),
            Self::Return(value) => value.split('/').next().unwrap_or("").to_string(),
        }
    }

    /// The TIN a tool's `tin` argument takes for this subject: all 14
    /// digits (a taxpayer is its head office, `00000`).
    pub(crate) fn full_tin(&self) -> String {
        let tin = self.tin();
        if tin.len() == 9 { format!("{tin}00000") } else { tin }
    }

    /// The 9-digit root every call with this ticket is held to.
    pub(crate) fn root(&self) -> String {
        self.tin().chars().take(9).collect()
    }

    /// The return's canonical value, for a return ticket.
    pub(crate) fn return_value(&self) -> Option<&str> {
        match self {
            Self::Return(value) => Some(value),
            _ => None,
        }
    }

    /// The display title, read from the database; `None` when the taxpayer
    /// or branch is no longer saved.
    pub(crate) fn title(&self, db: &Database, today: NaiveDate) -> Option<String> {
        let who = resolve::who_names(db, &self.tin())?;
        let title = match self {
            Self::Taxpayer(_) => who.name,
            Self::Branch(_) => {
                let branch = if who.branch_name.trim().is_empty() {
                    format!("Branch {}", who.branch_code)
                } else {
                    who.branch_name
                };
                format!("{} · {branch}", who.name)
            }
            Self::Return(value) => {
                let named = taxform::parse(value, today);
                format!(
                    "{} · {} · {}",
                    named.form?,
                    named.period?.label(),
                    who.name
                )
            }
        };
        Some(clip(&title))
    }

    /// Kind and fields, for the app that holds the ticket
    /// (`bir_ticket_subject`). Apps show the TIN to the user; the model
    /// never sees this.
    pub(crate) fn describe(&self, today: NaiveDate) -> Value {
        let mut out = Map::new();
        let (kind, tin) = match self {
            Self::Taxpayer(root) => ("taxpayer", format!("{root}00000")),
            Self::Branch(tin) => ("branch", tin.clone()),
            Self::Return(value) => {
                let named = taxform::parse(value, today);
                if let Some(form) = named.form {
                    out.insert("form".into(), Value::from(form));
                }
                if let Some(period) = named.period {
                    out.insert("period".into(), Value::from(period.canonical()));
                }
                let tin = self.tin();
                let tin = if tin.len() == 9 { format!("{tin}00000") } else { tin };
                ("return", tin)
            }
        };
        out.insert("kind".into(), Value::from(kind));
        out.insert("root".into(), Value::from(&tin[..9]));
        out.insert("branch_code".into(), Value::from(&tin[9..]));
        out.insert("tin".into(), Value::from(tin));
        Value::Object(out)
    }
}

fn clip(text: &str) -> String {
    if text.chars().count() <= TITLE_MAX {
        return text.to_string();
    }
    let cut: String = text.chars().take(TITLE_MAX - 1).collect();
    format!("{}…", cut.trim_end())
}

/// Whether `text` has the ticket grammar `^tkt_[0-9A-HJKMNP-TV-Z]{26}$`.
pub(crate) fn is_ticket(text: &str) -> bool {
    text.strip_prefix("tkt_").is_some_and(|rest| {
        rest.len() == 26 && rest.bytes().all(|byte| CROCKFORD.contains(&byte))
    })
}

/// A fresh ticket: 130 bits from the OS CSPRNG, 5 bits per character.
fn new_ticket() -> Result<String, String> {
    let mut bytes = [0u8; 17];
    getrandom::fill(&mut bytes).map_err(|error| format!("no randomness for a ticket: {error}"))?;
    let mut out = String::from("tkt_");
    for i in 0..26 {
        let bit = i * 5;
        let pair = (u16::from(bytes[bit / 8]) << 8) | u16::from(bytes[bit / 8 + 1]);
        let symbol = (pair >> (11 - bit % 8)) & 31;
        out.push(char::from(CROCKFORD[usize::from(symbol)]));
    }
    Ok(out)
}

fn load(db: &Database) -> Result<Map<String, Value>, String> {
    let stored = db
        .get_setting(STORE_KEY)
        .map_err(|error| error.to_string())?;
    Ok(stored
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default())
}

fn save(db: &Database, map: &Map<String, Value>) -> Result<(), String> {
    db.set_setting(STORE_KEY, &Value::Object(map.clone()).to_string())
        .map_err(|error| error.to_string())
}

/// The ticket of `subject`: the one it already has, else a new one.
pub(crate) fn issue(db: &Database, subject: &Subject) -> Result<String, String> {
    let mut map = load(db)?;
    let encoded = subject.encode();
    if let Some((ticket, _)) = map.iter().find(|(_, stored)| stored.as_str() == Some(&encoded)) {
        return Ok(ticket.clone());
    }
    let ticket = new_ticket()?;
    map.insert(ticket.clone(), Value::String(encoded));
    save(db, &map)?;
    Ok(ticket)
}

/// The tickets of many subjects at once (one read and one write), `None`
/// where there is no subject.
pub(crate) fn issue_all(
    db: &Database,
    subjects: &[Option<Subject>],
) -> Result<Vec<Option<String>>, String> {
    let mut map = load(db)?;
    let mut by_subject: std::collections::HashMap<String, String> = map
        .iter()
        .filter_map(|(ticket, stored)| Some((stored.as_str()?.to_string(), ticket.clone())))
        .collect();
    let mut changed = false;
    let mut out = Vec::with_capacity(subjects.len());
    for subject in subjects {
        let Some(subject) = subject else {
            out.push(None);
            continue;
        };
        let encoded = subject.encode();
        let ticket = match by_subject.get(&encoded) {
            Some(ticket) => ticket.clone(),
            None => {
                let ticket = new_ticket()?;
                map.insert(ticket.clone(), Value::String(encoded.clone()));
                by_subject.insert(encoded, ticket.clone());
                changed = true;
                ticket
            }
        };
        out.push(Some(ticket));
    }
    if changed {
        save(db, &map)?;
    }
    Ok(out)
}

/// What `ticket` names, if this plugin issued it.
pub(crate) fn subject(db: &Database, ticket: &str) -> Result<Option<Subject>, String> {
    if !is_ticket(ticket) {
        return Ok(None);
    }
    Ok(load(db)?
        .get(ticket)
        .and_then(Value::as_str)
        .and_then(Subject::decode))
}

/// A taxpayer's TIN moved (`move_taxpayer_tin`): its tickets follow, so an
/// app or pin holding one keeps working.
pub(crate) fn move_root(db: &Database, old: &str, new: &str) -> Result<(), String> {
    let mut map = load(db)?;
    let mut changed = false;
    for stored in map.values_mut() {
        let Some(subject) = stored.as_str().and_then(Subject::decode) else {
            continue;
        };
        if subject.root() != old {
            continue;
        }
        let moved = match subject {
            Subject::Taxpayer(_) => Subject::Taxpayer(new.to_string()),
            Subject::Branch(tin) => Subject::Branch(format!("{new}{}", &tin[9..])),
            Subject::Return(value) => Subject::Return(format!("{new}{}", &value[9..])),
        };
        *stored = Value::String(moved.encode());
        changed = true;
    }
    if changed {
        save(db, &map)?;
    }
    Ok(())
}

/// `rox.uninstall`: every ticket and the subject map.
pub(crate) fn forget_all(db: &Database) -> Result<(), String> {
    db.delete_setting(STORE_KEY)
        .map_err(|error| error.to_string())
}

/// How many tickets are stored.
#[cfg(test)]
pub(crate) fn count(db: &Database) -> Result<usize, String> {
    Ok(load(db)?.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bir_core::profile::TaxpayerType;
    use bir_core::tax_profile::{BranchFacts, NewTaxpayer};

    const ROOT: &str = "123456788";

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

    #[test]
    fn tickets_have_the_grammar_and_130_random_bits() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let ticket = new_ticket().unwrap();
            assert!(is_ticket(&ticket), "{ticket}");
            // 130 bits fill all 26 characters: the first is not always 0.
            seen.insert(ticket.as_bytes()[4]);
            assert!(!ticket.contains(ROOT));
        }
        assert!(seen.len() > 8, "the first character varies");
        assert!(!is_ticket("tkt_0000000000000000000000000I"));
        assert!(!is_ticket("tkt_000"));
        assert!(!is_ticket(ROOT));
    }

    #[test]
    fn one_subject_has_one_ticket_and_the_title_is_the_name() {
        let db = db();
        let subject = Subject::of_tin(&format!("{ROOT}00000")).unwrap();
        assert_eq!(subject, Subject::Taxpayer(ROOT.into()));
        let ticket = issue(&db, &subject).unwrap();
        assert_eq!(issue(&db, &Subject::of_tin(ROOT).unwrap()).unwrap(), ticket);
        assert_eq!(subject.title(&db, today()).unwrap(), "Dela Cruz Jane");
        assert_eq!(super::subject(&db, &ticket).unwrap(), Some(subject));
        let other = "tkt_".to_string() + &"0".repeat(26);
        assert_eq!(super::subject(&db, &other).unwrap(), None);
        // a return's title names the form, the period and the taxpayer
        let ret = Subject::of_return(&format!("{ROOT}/2551Q-2026-Q3"), today()).unwrap();
        assert_eq!(ret.title(&db, today()).unwrap(), "2551Q · Q3 2026 · Dela Cruz Jane");
        assert_eq!(ret.root(), ROOT);
        // an unsaved taxpayer has no title
        assert!(Subject::Taxpayer("987654321".into()).title(&db, today()).is_none());
    }

    #[test]
    fn a_moved_tin_keeps_its_tickets_and_uninstall_forgets_them() {
        let db = db();
        let ticket = issue(&db, &Subject::Taxpayer(ROOT.into())).unwrap();
        move_root(&db, ROOT, "987654321").unwrap();
        assert_eq!(
            subject(&db, &ticket).unwrap(),
            Some(Subject::Taxpayer("987654321".into()))
        );
        assert_eq!(count(&db).unwrap(), 1);
        forget_all(&db).unwrap();
        assert_eq!(count(&db).unwrap(), 0);
        assert_eq!(subject(&db, &ticket).unwrap(), None);
    }
}
