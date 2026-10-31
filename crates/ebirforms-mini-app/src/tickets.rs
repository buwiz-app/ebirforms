//! Tickets (spec 9.5, P1-P3): what the Bot stores, links and shows the
//! model in place of a TIN.
//!
//! A ticket is `tkt_` and 26 Crockford base32 characters, 130 random bits,
//! with no subject data in it. The map from ticket to subject is one row of
//! Buwiz's own (encrypted) settings, so it survives restarts and never
//! leaves this process: the Bot learns a title only (`rox.ticket.resolve`),
//! and an app learns the TIN of a ticket only by asking with that ticket
//! (`bir_ticket_subject`). One subject has one ticket.
//!
//! Subjects are a taxpayer (its 9-digit root), one branch (14 digits) or
//! one return (`<tin>/<FORM>-<period>`). Every subject's scope is its TIN
//! root. Titles are read live, so a renamed taxpayer's chips follow and a
//! deleted one's tickets no longer resolve.
//!
//! The row is bookkeeping of this protocol, not filing data, so it is
//! written without the owner lock (see `owner.rs`).

use bir_app_core::BirApp;
use bir_app_core::bir_core::forms::taxform;
use chrono::NaiveDate;
use serde_json::{Map, Value};

/// The settings row holding `{ticket: subject}`.
const STORE_KEY: &str = "mcp.v1.tickets";
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// The longest title the Bot shows for a token (9.2).
const TITLE_MAX: usize = 60;

/// What a ticket names.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

    /// The root, the branch's 14 digits, or the return's TIN.
    pub(crate) fn tin(&self) -> String {
        match self {
            Self::Taxpayer(root) => root.clone(),
            Self::Branch(tin) => tin.clone(),
            Self::Return(value) => value.split('/').next().unwrap_or("").to_string(),
        }
    }

    /// All 14 digits (a taxpayer is its head office, `00000`).
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
    pub(crate) fn title(&self, app: &BirApp, today: NaiveDate) -> Option<String> {
        let taxpayer = app.get_taxpayer(&self.root()).ok().flatten()?;
        let full = self.full_tin();
        let branch = taxpayer
            .branches
            .iter()
            .find(|branch| digits(&branch.tin) == full);
        let title = match self {
            Self::Taxpayer(_) => taxpayer.name,
            Self::Branch(_) => {
                let branch = branch?;
                let label = if branch.name.trim().is_empty() {
                    format!("Branch {}", branch.code)
                } else {
                    branch.name.clone()
                };
                format!("{} · {label}", taxpayer.name)
            }
            Self::Return(value) => {
                let named = taxform::parse(value, today);
                format!("{} · {} · {}", named.form?, named.period?.label(), taxpayer.name)
            }
        };
        Some(clip(&title))
    }

    /// Kind and fields, for the app that holds the ticket
    /// (`bir_ticket_subject`). Apps show the TIN to the user; the model
    /// never sees this.
    pub(crate) fn describe(&self, today: NaiveDate) -> Value {
        let mut out = Map::new();
        let kind = match self {
            Self::Taxpayer(_) => "taxpayer",
            Self::Branch(_) => "branch",
            Self::Return(value) => {
                let named = taxform::parse(value, today);
                if let Some(form) = named.form {
                    out.insert("form".into(), Value::from(form));
                }
                if let Some(period) = named.period {
                    out.insert("period".into(), Value::from(period.canonical()));
                }
                "return"
            }
        };
        let tin = self.full_tin();
        out.insert("kind".into(), Value::from(kind));
        out.insert("root".into(), Value::from(&tin[..9]));
        out.insert("branch_code".into(), Value::from(&tin[9..]));
        out.insert("tin".into(), Value::from(tin));
        Value::Object(out)
    }
}

pub(crate) fn digits(text: &str) -> String {
    text.chars().filter(char::is_ascii_digit).collect()
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
    text.strip_prefix("tkt_")
        .is_some_and(|rest| rest.len() == 26 && rest.bytes().all(|byte| CROCKFORD.contains(&byte)))
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

fn load(app: &BirApp) -> Result<Map<String, Value>, String> {
    let stored = app.get_setting(STORE_KEY).map_err(|error| error.to_string())?;
    Ok(stored
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default())
}

fn save(app: &BirApp, map: &Map<String, Value>) -> Result<(), String> {
    app.set_setting(STORE_KEY, &Value::Object(map.clone()).to_string())
        .map_err(|error| error.to_string())
}

/// The ticket of `subject`: the one it already has, else a new one.
pub(crate) fn issue(app: &BirApp, subject: &Subject) -> Result<String, String> {
    let mut map = load(app)?;
    let encoded = subject.encode();
    if let Some((ticket, _)) = map.iter().find(|(_, stored)| stored.as_str() == Some(&encoded)) {
        return Ok(ticket.clone());
    }
    let ticket = new_ticket()?;
    map.insert(ticket.clone(), Value::String(encoded));
    save(app, &map)?;
    Ok(ticket)
}

/// The tickets of many subjects at once (one read, at most one write),
/// `None` where there is no subject.
pub(crate) fn issue_all(app: &BirApp, subjects: &[Option<Subject>]) -> Result<Vec<Option<String>>, String> {
    let mut map = load(app)?;
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
        save(app, &map)?;
    }
    Ok(out)
}

/// The subject of a ticket this plugin issued; `None` for an unknown one.
pub(crate) fn subject(app: &BirApp, ticket: &str) -> Result<Option<Subject>, String> {
    if !is_ticket(ticket) {
        return Ok(None);
    }
    Ok(load(app)?
        .get(ticket)
        .and_then(Value::as_str)
        .and_then(Subject::decode))
}

/// A ticket this plugin issued, else why not.
pub(crate) fn subject_of(app: &BirApp, ticket: &str) -> Result<Subject, String> {
    subject(app, ticket)?.ok_or_else(|| "the ticket is not one Buwiz issued, or it was revoked".to_string())
}

/// `rox.uninstall`: every ticket forgotten.
pub(crate) fn forget_all(app: &BirApp) -> Result<(), String> {
    save(app, &Map::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tickets_have_the_grammar_and_subjects_round_trip() {
        let ticket = new_ticket().unwrap();
        assert!(is_ticket(&ticket), "{ticket}");
        assert!(!is_ticket("tkt_short") && !is_ticket(&ticket.to_lowercase()));
        for subject in [
            Subject::Taxpayer("000123453".into()),
            Subject::Branch("00012345300002".into()),
            Subject::Return("000123453/2551Q-2026-Q3".into()),
        ] {
            assert_eq!(Subject::decode(&subject.encode()), Some(subject));
        }
        assert_eq!(Subject::of_tin("000-123-453-00000"), Some(Subject::Taxpayer("000123453".into())));
        assert_eq!(Subject::of_tin("00012345300002"), Some(Subject::Branch("00012345300002".into())));
        assert_eq!(Subject::Taxpayer("000123453".into()).full_tin(), "00012345300000");
    }
}
