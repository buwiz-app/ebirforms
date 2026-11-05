//! Tickets in and out of the agent's tools (P1): the model never sees a TIN.
//!
//! In: a ticket where a tool takes `tin` becomes that TIN (a return's
//! ticket in `code` or `form` its `<tin>/<FORM>-<period>`), and an app must
//! pass tickets only. Out: the TINs in what a tool answered become tickets
//! (only saved taxpayers get one; any other TIN becomes `[TIN]`).

use std::collections::HashMap;

use bir_app_core::BirApp;
use chrono::NaiveDate;
use regex::Regex;
use serde_json::{Map, Value};

use crate::protocol::Origin;
use crate::tickets::{self, Subject};

/// The keys whose string value is a TIN in the answers.
const TIN_KEYS: [&str; 5] = ["tin", "tin_root", "tin_display", "branch_tin", "root"];
/// Arguments that hold a return in the agent's tools.
const RETURN_ARGS: [&str; 2] = ["code", "form"];

/// A TIN written in groups, with dashes, spaces or dots.
fn grouped_tin_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b[0-9]{3}[-. ][0-9]{3}[-. ][0-9]{3}(?:[-. ][0-9]{3,5})?\b").expect("regex")
    })
}

fn bare_tin_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b[0-9]{9,12}\b").expect("regex"))
}

/// Whether the digits at `start..end` of `text` are an amount: a peso sign
/// before them, one or two decimals after them, or they are the decimals of
/// `0.123456789`. A separator alone says nothing: `x,000123453,y` and a list
/// of TINs have those too. A ticket needs no such test: its digits touch `_`
/// or a letter, so they aren't a run.
fn amount_at(text: &str, start: usize, end: usize) -> bool {
    let mut before = text[..start].chars().rev();
    let (one, two) = (before.next(), before.next());
    let decimals_before = one == Some('.') && two.is_some_and(|c| c.is_ascii_digit());
    let mut after = text[end..].chars();
    let separated = matches!(after.next(), Some('.' | ','));
    let digits = after.clone().take_while(char::is_ascii_digit).count();
    let decimals_after = separated && (1..=2).contains(&digits);
    one == Some('₱') || decimals_before || decimals_after
}

/// `text` with what `re` finds as `word` says, amounts left as they are.
fn scrub_runs(text: &str, re: &Regex, word: &mut impl FnMut(&str) -> String) -> String {
    let (mut out, mut kept) = (String::new(), 0);
    for run in re.find_iter(text) {
        if amount_at(text, run.start(), run.end()) {
            continue;
        }
        out.push_str(&text[kept..run.start()]);
        out.push_str(&word(run.as_str()));
        kept = run.end();
    }
    out.push_str(&text[kept..]);
    out
}

/// `text` with each TIN as `word` says: in groups, or a bare run of 9 to 12
/// digits. An amount stays as it is.
fn scrub_tins(text: &str, mut word: impl FnMut(&str) -> String) -> String {
    let text = scrub_runs(text, grouped_tin_re(), &mut word);
    scrub_runs(&text, bare_tin_re(), &mut word)
}

/// Turn the tickets in a tool's arguments into what the tool reads.
pub(crate) fn expand(
    app: &BirApp,
    tool: &str,
    arguments: &mut Map<String, Value>,
    origin: Origin,
) -> Result<(), String> {
    let lookup = |ticket: &str| -> Result<Subject, String> {
        tickets::subject(app, ticket)?.ok_or_else(|| {
            format!("{tool}: the ticket is not one eBIRForms issued, or it was revoked")
        })
    };
    let mut implied_return = None;
    if let Some(value) = arguments.get("tin").cloned() {
        match value.as_str() {
            Some(text) if tickets::is_ticket(text) => {
                let subject = lookup(text)?;
                implied_return = subject.return_value().map(str::to_string);
                arguments.insert("tin".into(), Value::String(subject.full_tin()));
            }
            Some(_) if origin == Origin::App => {
                return Err(format!(
                    "{tool}: `tin` must be a ticket when an app calls (an app never names a TIN)"
                ));
            }
            _ => {}
        }
    }
    for key in RETURN_ARGS {
        let Some(text) = arguments
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        if !tickets::is_ticket(&text) {
            continue;
        }
        let subject = lookup(&text)?;
        let value = subject.return_value().ok_or_else(|| {
            format!("{tool}: `{key}` holds a taxpayer's ticket; pass it as `tin`")
        })?;
        arguments.insert(key.into(), Value::String(value.to_string()));
    }
    // A return's ticket passed as `tin` names its form and period too.
    if let Some(value) = implied_return {
        let key = match tool {
            crate::server::FORM_OPEN => Some("code"),
            crate::server::APP_OPEN | crate::server::DRAFTS_LIST => Some("form"),
            _ => None,
        };
        if let Some(key) = key
            && !arguments.contains_key(key)
        {
            arguments.insert(key.into(), Value::String(value));
        }
    }
    Ok(())
}

/// Rewrites an answer so it names taxpayers by ticket.
pub(crate) struct Scrubber<'a> {
    app: &'a BirApp,
    today: NaiveDate,
    issued: HashMap<Subject, Option<String>>,
}

impl<'a> Scrubber<'a> {
    pub(crate) fn new(app: &'a BirApp, today: NaiveDate) -> Self {
        Self {
            app,
            today,
            issued: HashMap::new(),
        }
    }

    /// The ticket of a saved subject.
    fn ticket(&mut self, subject: &Subject) -> Option<String> {
        if let Some(found) = self.issued.get(subject) {
            return found.clone();
        }
        let ticket = subject
            .title(self.app, self.today)
            .and_then(|_| tickets::issue(self.app, subject).ok());
        self.issued.insert(subject.clone(), ticket.clone());
        ticket
    }

    /// A whole tool result: `structuredContent`, every text content and the
    /// props of an app it opens.
    pub(crate) fn result(&mut self, result: &mut Value) {
        if let Some(structured) = result.get_mut("structuredContent") {
            self.value(structured);
        }
        if let Some(props) = result.pointer_mut("/_meta/buwiz~1open/props") {
            self.value(props);
        }
        if let Some(contents) = result.get_mut("content").and_then(Value::as_array_mut) {
            for content in contents {
                let Some(text) = content.get("text").and_then(Value::as_str) else {
                    continue;
                };
                let rewritten = match serde_json::from_str::<Value>(text) {
                    Ok(mut parsed) if parsed.is_object() || parsed.is_array() => {
                        self.value(&mut parsed);
                        parsed.to_string()
                    }
                    _ => self.text(text),
                };
                content["text"] = Value::String(rewritten);
            }
        }
    }

    pub(crate) fn value(&mut self, value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, item) in map.iter_mut() {
                    if TIN_KEYS.contains(&key.as_str())
                        && let Some(text) = item.as_str()
                    {
                        if tickets::is_ticket(text) {
                            continue;
                        }
                        if let Some(subject) = Subject::of_tin(text) {
                            *item = Value::String(
                                self.ticket(&subject).unwrap_or_else(|| "[TIN]".into()),
                            );
                            continue;
                        }
                    }
                    self.value(item);
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| self.value(item)),
            Value::String(text) => *text = self.text(text),
            _ => {}
        }
    }

    /// Free text: TINs become tickets ([`scrub_tins`]).
    pub(crate) fn text(&mut self, text: &str) -> String {
        scrub_tins(text, |tin| {
            let digits: String = tin.chars().filter(char::is_ascii_digit).collect();
            Subject::of_tin(&digits)
                .and_then(|subject| self.ticket(&subject))
                .unwrap_or_else(|| "[TIN]".into())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrubbed(text: &str) -> String {
        scrub_tins(text, |_| "[TIN]".into())
    }

    #[test]
    fn a_bare_run_of_nine_to_twelve_digits_is_a_tin() {
        for (text, want) in [
            ("TIN 000123453 is due", "TIN [TIN] is due"),
            ("TIN 000123453000.", "TIN [TIN]."),
            ("0001234530, then", "[TIN], then"),
            ("000123453/2551Q-2026-Q3", "[TIN]/2551Q-2026-Q3"),
            ("dashed 000-123-453-000", "dashed [TIN]"),
            ("x,000123453,y", "x,[TIN],y"),
            ("x.000123453.y", "x.[TIN].y"),
            ("000123453,000987654", "[TIN],[TIN]"),
            ("000123453.000", "[TIN].000"),
            ("spaced 000 123 453 000", "spaced [TIN]"),
            ("dotted 000.123.453.000 here", "dotted [TIN] here"),
            ("mixed 000 123-453", "mixed [TIN]"),
        ] {
            assert_eq!(scrubbed(text), want);
        }
    }

    #[test]
    fn amounts_tickets_and_other_numbers_stay() {
        for text in [
            "owed 123456789.50",
            "owed ₱123456789",
            "owed ₱123 456 789",
            "123.456.789.50",
            "123 456 789,50",
            "1,234,567,890.00",
            "12345678901.5 and 1234567890,50",
            "0.123456789",
            "tkt_000123453ABCDEFGHJKMNPQRS",
            "tkt_ABCDEFGHJKMNPQ000123453000",
            "20261105",
            "a 13-digit id 1793745245000",
        ] {
            assert_eq!(scrubbed(text), text);
        }
    }
}
