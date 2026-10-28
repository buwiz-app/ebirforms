//! Plain words for "signed out" and "the Buwiz server isn't answering".
//!
//! A licensed tool refused for the account used to answer with what the
//! check said underneath ("could not reach buwiz-server at
//! http://localhost:3008 to refresh sign-in: error sending request ...",
//! "Sign in to Buwiz before using this computer."). Shown to the user by
//! the assistant or an app, that says neither what happened nor what to
//! do. Every tool answer passes through [`rewrite`]:
//!
//! - signed out (no account, an expired or ended sign-in, the filing
//!   database locked): "You're signed out of Buwiz, so BIR can't ...".
//!   For the assistant this is not an error, and it carries the sign-in
//!   answer card (`_meta["rox/open"]`, spec 9.8: Sign in opens Buwiz
//!   Sign-in, Try again re-checks); an error result's card would be
//!   dropped (9.6). An app gets the same words as its error.
//! - the server unreachable: "BIR can't reach the Buwiz server (host:port).
//!   Start it, then try again." It stays an error.
//!
//! Only the server's host and port are shown, never the URL or the
//! underlying error. Resolve (`bir_resolve`) is local and never refused.

use serde_json::{Value, json};

/// Why a licensed tool did not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    SignedOut,
    /// The Buwiz server did not answer; its host and port.
    Unreachable(String),
}

/// What an answer's text says, if it is one of these refusals.
pub(crate) fn classify(text: &str) -> Option<Refusal> {
    const REACH: &str = "could not reach buwiz-server at ";
    if let Some(at) = text.find(REACH) {
        let rest = &text[at + REACH.len()..];
        let rest = rest.split_once("://").map_or(rest, |(_, tail)| tail);
        let host: String = rest
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != '/' && *c != ',')
            .collect();
        let host = host.trim_end_matches([':', '.']).to_string();
        return Some(Refusal::Unreachable(if host.is_empty() {
            "the Buwiz server".into()
        } else {
            host
        }));
    }
    let signed_out = [
        crate::account_gate::LOCKED_MESSAGE,
        "Buwiz sign-in required",
        "Sign in again: Buwiz ended",
        "Sign in with Buwiz",
        "sign-in expired",
    ];
    signed_out
        .iter()
        .any(|needle| text.contains(needle))
        .then_some(Refusal::SignedOut)
}

/// What the tool would have done, in the user's words.
fn action(tool: &str) -> &'static str {
    match tool {
        "bir_app_open" => "open its apps",
        "bir_profiles_list" | "bir_dues_list" | "bir_drafts_list" | "bir_calendar_get"
        | "bir_calendar_changes" => "read this computer's taxpayers and their returns",
        "bir_calendar_set" | "bir_calendar_link" => "change the filing calendar",
        "bir_confirmation_search" | "bir_confirm_filing" => "check or record BIR's filing confirmations",
        "bir_payment_search" | "bir_payment_parse" | "bir_record_payment" | "bir_mark_paid"
        | "bir_payments_list" | "bir_filing_record" => "read or record this taxpayer's payments",
        name if name.starts_with("bir_deadline_") => "read or move deadlines",
        name if name.starts_with("bir_form_") => "open or change this taxpayer's returns",
        _ => "read this taxpayer's returns",
    }
}

pub(crate) fn signed_out_text(tool: &str) -> String {
    format!(
        "You're signed out of Buwiz, so BIR can't {}. Open Buwiz Sign-in to sign in again.",
        action(tool)
    )
}

pub(crate) fn unreachable_text(host: &str) -> String {
    format!("BIR can't reach the Buwiz server ({host}). Start it, then try again.")
}

/// Rewrite a refused tool answer in plain words (see the module docs).
/// `for_app`: an app's call, which keeps its error and gets no card.
pub(crate) fn rewrite(tool: &str, result: &mut Value, for_app: bool) {
    if result["isError"] != true {
        return;
    }
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|content| content["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    match classify(&text) {
        None => {}
        Some(Refusal::Unreachable(host)) => {
            *result = json!({
                "content": [{ "type": "text", "text": unreachable_text(&host) }],
                "isError": true,
            });
        }
        Some(Refusal::SignedOut) if for_app => {
            *result = json!({
                "content": [{ "type": "text", "text": signed_out_text(tool) }],
                "isError": true,
            });
        }
        Some(Refusal::SignedOut) => {
            let message = signed_out_text(tool);
            let card = super::cards::open(
                "signin",
                "Buwiz sign-in",
                None,
                json!({ "message": message }),
                "bir_status",
                json!({}),
            );
            *result = json!({
                "content": [{ "type": "text", "text": message }],
                "structuredContent": { "signed_in": false, "message": message },
                "isError": false,
                "_meta": { "rox/open": card },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(text: &str) -> Value {
        json!({ "content": [{ "type": "text", "text": text }], "isError": true })
    }

    #[test]
    fn an_unreachable_server_names_only_its_host_and_port() {
        let mut result = refused(
            "bir_drafts_list: could not reach buwiz-server at http://localhost:3008 to refresh sign-in: error sending request for url (http://localhost:3008/auth/refresh)",
        );
        rewrite("bir_drafts_list", &mut result, false);
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["content"][0]["text"],
            "BIR can't reach the Buwiz server (localhost:3008). Start it, then try again."
        );
        assert!(result.get("_meta").is_none());
    }

    #[test]
    fn signed_out_is_an_answer_with_a_sign_in_card_for_the_assistant() {
        for text in [
            crate::account_gate::LOCKED_MESSAGE,
            bir_mcp::license::UNAUTHENTICATED,
            bir_mcp::license::SIGN_IN_AGAIN,
        ] {
            let mut result = refused(text);
            rewrite("bir_drafts_list", &mut result, false);
            assert_eq!(result["isError"], false, "{text}");
            assert_eq!(
                result["content"][0]["text"],
                "You're signed out of Buwiz, so BIR can't read this computer's taxpayers and their returns. Open Buwiz Sign-in to sign in again."
            );
            assert_eq!(result["_meta"]["rox/open"]["app"], "card-help");
            assert_eq!(result["_meta"]["rox/open"]["props"]["card"], "signin");
            assert_eq!(result["_meta"]["rox/open"]["placement"], "inline");
        }
        // an app keeps the error, in the same words
        let mut result = refused(crate::account_gate::LOCKED_MESSAGE);
        rewrite("bir_app_list_returns", &mut result, true);
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"].as_str().unwrap().starts_with("You're signed out of Buwiz"));
        // anything else is left alone
        let mut other = refused("`tin` must be a ticket");
        rewrite("bir_drafts_list", &mut other, false);
        assert_eq!(other, refused("`tin` must be a ticket"));
    }
}
