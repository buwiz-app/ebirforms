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

fn dashed_tin_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b\d{3}-\d{3}-\d{3}(?:-\d{3,5})?\b").expect("regex"))
}

/// Turn the tickets in a tool's arguments into what the tool reads.
pub(crate) fn expand(app: &BirApp, tool: &str, arguments: &mut Map<String, Value>, origin: Origin) -> Result<(), String> {
    let lookup = |ticket: &str| -> Result<Subject, String> {
        tickets::subject(app, ticket)?
            .ok_or_else(|| format!("{tool}: the ticket is not one Buwiz issued, or it was revoked"))
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
                return Err(format!("{tool}: `tin` must be a ticket when an app calls (an app never names a TIN)"));
            }
            _ => {}
        }
    }
    for key in RETURN_ARGS {
        let Some(text) = arguments.get(key).and_then(Value::as_str).map(str::to_string) else {
            continue;
        };
        if !tickets::is_ticket(&text) {
            continue;
        }
        let subject = lookup(&text)?;
        let value = subject
            .return_value()
            .ok_or_else(|| format!("{tool}: `{key}` holds a taxpayer's ticket; pass it as `tin`"))?;
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
        Self { app, today, issued: HashMap::new() }
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
        if let Some(props) = result.pointer_mut("/_meta/rox~1open/props") {
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
                            *item = Value::String(self.ticket(&subject).unwrap_or_else(|| "[TIN]".into()));
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

    /// Free text: formatted TINs become tickets.
    pub(crate) fn text(&mut self, text: &str) -> String {
        dashed_tin_re()
            .replace_all(text, |caps: &regex::Captures<'_>| {
                Subject::of_tin(&caps[0])
                    .and_then(|subject| self.ticket(&subject))
                    .unwrap_or_else(|| "[TIN]".into())
            })
            .into_owned()
    }
}
