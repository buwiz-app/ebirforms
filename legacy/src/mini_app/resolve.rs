//! `bir_resolve`'s resolver: the slash commands and `@` scopes the v1
//! manifest announces to Rox, and what they name.
//!
//! Rox knows nothing about BIR. When the user types `/taxform 2551Q Q3
//! 2026` or a `@bir:taxform:...` token it calls the tool the manifest names,
//! here, and never the model. Every answer reads the local database only
//! (no network), so an unknown TIN comes back at once. This module answers
//! in TINs (a v0 `rox://app/bir/<app>?props=` link, its title and the
//! canonical TIN token); [`super::v1::resolve`] turns that into the spec's
//! `result` shapes with tickets, so nothing Rox stores names a TIN.
//!
//! `/taxprofile <tin>` and `@bir:branchcode:` set the current taxpayer,
//! kept in the database's settings so it survives restarts; a taxform
//! without a TIN is for that taxpayer. Periods are read by
//! [`bir_core::forms::taxform`]; with no period, the taxpayer's unsubmitted
//! draft of that form is reused first, else the return to file now.

use bir_core::db::Database;
use bir_core::forms::FilingStatus;
use bir_core::forms::FormDraftSummary;
use bir_core::forms::taxform::{self, Cadence, TaxPeriod, TaxformAction, TaxformArgs};
use bir_core::tax_profile::{BranchField, ResolvedBranch, TaxpayerWithBranches};
use chrono::{Datelike, NaiveDate};
use serde_json::{Value, json};

pub(crate) const RESOLVE_TOOL: &str = "bir_resolve";
/// Settings key of the current taxpayer: one branch's full TIN.
pub(crate) const CURRENT_TAXPAYER_KEY: &str = "mini_app.current_taxpayer";
/// The server id in Rox's `rox://app/<server>/<app>` links.
const SERVER: &str = "bir";
const MAX_CHOICES: usize = 20;
/// Periods offered for one form while typing.
const MAX_PERIODS: usize = 12;

/// One `bir_resolve` call.
pub(crate) struct Request {
    pub name: String,
    pub args: String,
    pub complete: bool,
    /// The TINs of the taxpayer and branch tokens attached where the user
    /// types (`context.attached`), in order: the default taxpayer there.
    pub attached: Vec<String>,
}

/// The TIN an attached token names: `@bir:taxprofile:<tin>` (a 9-digit TIN
/// is the head office) or `@bir:branchcode:<root>/<code>`. Other tokens
/// name no taxpayer to default to.
#[cfg(test)]
pub(crate) fn attached_tin(token: &str) -> Option<String> {
    let token = token.trim();
    if let Some(value) = token.strip_prefix("@bir:taxprofile:") {
        return taxform::tin_digits(value.trim().trim_end_matches('/'));
    }
    let (root, code) = token.strip_prefix("@bir:branchcode:")?.split_once('/')?;
    let root = taxform::tin_digits(root).filter(|root| root.len() == 9)?;
    let code = only_digits(code);
    (!code.is_empty() && code.len() <= 5).then(|| format!("{root}{code:0>5}"))
}

/// An answer that is not resolved: why, and the choices when the user picks
/// the next step.
pub(crate) struct Ask {
    pub error: String,
    pub choices: Vec<Value>,
}

impl Ask {
    fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            choices: Vec::new(),
        }
    }

    fn with(error: impl Into<String>, choices: Vec<Value>) -> Self {
        Self {
            error: error.into(),
            choices,
        }
    }

    pub(crate) fn json(self) -> Value {
        let mut answer = json!({ "ok": false, "error": self.error });
        if !self.choices.is_empty() {
            answer["choices"] = Value::Array(self.choices);
        }
        answer
    }
}

impl From<String> for Ask {
    fn from(error: String) -> Self {
        Self::new(error)
    }
}

/// Resolve one request. The answer is the tool's `structuredContent.result`.
pub(crate) fn resolve(db: &Database, request: &Request, today: NaiveDate) -> Value {
    let mut book = match Book::read(db) {
        Ok(book) => book,
        Err(error) => return Ask::new(error).json(),
    };
    book.attached.clone_from(&request.attached);
    let answer = match (request.name.as_str(), request.complete) {
        ("taxprofile", true) => Ok(choices(book.taxpayer_choices(&request.args))),
        ("taxprofile", false) => taxprofile(db, &book, &request.args, today),
        ("branchcode", true) => Ok(choices(branch_choices(db, &book, &request.args))),
        ("branchcode", false) => branchcode(db, &book, &request.args, today),
        ("taxform", true) => Ok(choices(taxform_choices(db, &book, &request.args, today))),
        ("taxform", false) => taxform_answer(db, &book, &request.args, today),
        (other, _) => Err(Ask::new(format!(
            "`{other}` is not one of this server's commands or scopes (taxprofile, branchcode, taxform)"
        ))),
    };
    answer.unwrap_or_else(Ask::json)
}

fn choices(mut list: Vec<Value>) -> Value {
    list.truncate(MAX_CHOICES);
    json!({ "choices": list })
}

/// Every saved taxpayer, read once per request.
struct Book {
    taxpayers: Vec<TaxpayerWithBranches>,
    /// The TINs attached where the user types: the default taxpayer
    /// before the current one.
    attached: Vec<String>,
}

/// One branch of one taxpayer.
#[derive(Clone, Copy)]
struct Who<'a> {
    taxpayer: &'a TaxpayerWithBranches,
    branch: &'a ResolvedBranch,
}

impl Who<'_> {
    fn root(&self) -> &str {
        &self.taxpayer.taxpayer.tin_root
    }

    fn name(&self) -> &str {
        &self.taxpayer.taxpayer.name
    }

    /// `Galang, Juan` -> `Galang`.
    fn short_name(&self) -> &str {
        self.name().split(',').next().unwrap_or("").trim()
    }
}

impl Book {
    fn read(db: &Database) -> Result<Self, String> {
        Ok(Self {
            taxpayers: db.list_taxpayers().map_err(|error| error.to_string())?,
            attached: Vec::new(),
        })
    }

    /// The first attached taxpayer that is still saved.
    fn attached(&self) -> Option<Who<'_>> {
        self.attached.iter().find_map(|tin| self.find(tin))
    }

    /// Every branch of every taxpayer not archived, head offices first.
    fn branches(&self) -> impl Iterator<Item = Who<'_>> {
        self.taxpayers
            .iter()
            .filter(|taxpayer| !taxpayer.taxpayer.archived)
            .flat_map(|taxpayer| {
                taxpayer
                    .branches
                    .iter()
                    .map(move |branch| Who { taxpayer, branch })
            })
    }

    /// The branch `digits` names: a 9-digit root is its head office.
    fn find(&self, digits: &str) -> Option<Who<'_>> {
        let root = digits.get(..9)?;
        let taxpayer = self
            .taxpayers
            .iter()
            .find(|taxpayer| taxpayer.taxpayer.tin_root == root)?;
        let branch = if digits.len() == 9 {
            taxpayer
                .branches
                .iter()
                .find(|branch| branch.is_head_office)
                .or(taxpayer.branches.first())?
        } else {
            taxpayer
                .branches
                .iter()
                .find(|branch| only_digits(&branch.tin) == digits)?
        };
        Some(Who { taxpayer, branch })
    }

    /// The taxpayer a request defaults to: an attached one, else the
    /// current taxpayer while it is still saved.
    fn current(&self, db: &Database) -> Result<Option<Who<'_>>, String> {
        if let Some(who) = self.attached() {
            return Ok(Some(who));
        }
        let stored = db
            .get_setting(CURRENT_TAXPAYER_KEY)
            .map_err(|error| error.to_string())?;
        Ok(stored.and_then(|tin| self.find(&only_digits(&tin))))
    }

    /// Taxpayers whose TIN starts with, or whose name contains, `query`.
    fn matching(&self, query: &str) -> Vec<&TaxpayerWithBranches> {
        let query = query.trim().to_lowercase();
        let digits = only_digits(&query);
        self.taxpayers
            .iter()
            .filter(|taxpayer| !taxpayer.taxpayer.archived)
            .filter(|taxpayer| {
                query.is_empty()
                    || (!digits.is_empty()
                        && digits.len() == query.chars().filter(|c| *c != '-').count()
                        && taxpayer
                            .taxpayer
                            .tin_root
                            .starts_with(&digits[..digits.len().min(9)]))
                    || taxpayer.taxpayer.name.to_lowercase().contains(&query)
            })
            .collect()
    }

    fn taxpayer_choices(&self, query: &str) -> Vec<Value> {
        self.matching(query)
            .into_iter()
            .map(|taxpayer| {
                let head = taxpayer
                    .branches
                    .iter()
                    .find(|branch| branch.is_head_office)
                    .or(taxpayer.branches.first());
                json!({
                    "value": taxpayer.taxpayer.tin_root,
                    "label": taxpayer.taxpayer.name,
                    "detail": detail(&format_tin(&taxpayer.taxpayer.tin_root), head),
                })
            })
            .collect()
    }
}

fn only_digits(text: &str) -> String {
    text.chars().filter(char::is_ascii_digit).collect()
}

/// `123-456-789`, `123-456-789-00001`.
pub(crate) fn format_tin(digits: &str) -> String {
    let digits = only_digits(digits);
    match digits.len() {
        9 => format!("{}-{}-{}", &digits[..3], &digits[3..6], &digits[6..]),
        14 => format!(
            "{}-{}-{}-{}",
            &digits[..3],
            &digits[3..6],
            &digits[6..9],
            &digits[9..]
        ),
        _ => digits,
    }
}

/// `123-456-789 · RDO 047`.
fn detail(tin: &str, branch: Option<&ResolvedBranch>) -> String {
    match branch
        .map(|branch| branch.value(BranchField::RdoCode))
        .filter(|rdo| !rdo.trim().is_empty())
    {
        Some(rdo) => format!("{tin} · RDO {rdo}"),
        None => tin.to_string(),
    }
}

/// `rox://app/bir/<app>?props=<urlencoded JSON>&token=<urlencoded token>`:
/// the token of what opens (see [`app_token`]) lets Rox attach and pin an
/// app opened from a link. No `&token=` when the props name none.
pub(crate) fn app_link(app: &str, props: &Value) -> String {
    let mut link = format!(
        "rox://app/{SERVER}/{app}?props={}",
        encode_component(&props.to_string())
    );
    if let Some(token) = app_token(app, props) {
        link.push_str("&token=");
        link.push_str(&encode_component(&token));
    }
    link
}

/// The canonical token of what an app opens on: a taxprofile (or branch)
/// token for tax-profile, a taxform token for forms on one form and period.
pub(crate) fn app_token(app: &str, props: &Value) -> Option<String> {
    let tin = taxform::tin_digits(props["tin"].as_str()?)?;
    match app {
        "tax-profile" | "calendar" => match props["branch"].as_str() {
            Some(branch) if !only_digits(branch).trim_start_matches('0').is_empty() => Some(format!(
                "@bir:branchcode:{}/{:0>5}",
                &tin[..9],
                only_digits(branch)
            )),
            _ => Some(format!("@bir:taxprofile:{}", taxform::token_tin(&tin))),
        },
        "forms" => {
            let form = taxform::form_code_of(props["form"].as_str()?)?;
            let period = props["period"].as_str()?;
            Some(format!(
                "{}{}/{form}-{period}",
                taxform::TOKEN_PREFIX,
                taxform::token_tin(&tin)
            ))
        }
        _ => None,
    }
}

/// The start of a v0 app link, as [`app_link`] writes it.
pub(crate) const V0_LINK_PREFIX: &str = "rox://app/bir/";

/// A v0 link's app and props: `rox://app/bir/<app>?props=<JSON>`.
pub(crate) fn parse_v0_link(link: &str) -> Option<(String, Value)> {
    let rest = link.strip_prefix(V0_LINK_PREFIX)?;
    let (app, query) = rest.split_once('?').unwrap_or((rest, ""));
    let props = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("props="))
        .and_then(decode_component)
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| json!({}));
    Some((app.to_string(), props))
}

/// The inverse of [`encode_component`]; `None` for broken escapes.
pub(crate) fn decode_component(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut input = text.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let hex = [input.next()?, input.next()?];
            bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).ok()
}

/// A saved branch's names, for a ticket's title.
pub(crate) struct WhoNames {
    pub name: String,
    pub branch_name: String,
    pub branch_code: String,
}

/// The RDO of the branch `tin` names, while it is saved.
pub(crate) fn who_rdo(db: &Database, tin: &str) -> Option<String> {
    let book = Book::read(db).ok()?;
    let who = book.find(&only_digits(tin))?;
    Some(who.branch.value(BranchField::RdoCode).to_string())
}

/// The names of the branch `tin` names (a 9-digit root is the head
/// office), while it is saved.
pub(crate) fn who_names(db: &Database, tin: &str) -> Option<WhoNames> {
    let book = Book::read(db).ok()?;
    let who = book.find(&only_digits(tin))?;
    Some(WhoNames {
        name: who.name().to_string(),
        branch_name: who.branch.name.clone(),
        branch_code: who.branch.branch_code.clone(),
    })
}

/// JavaScript's `encodeURIComponent`.
pub(crate) fn encode_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn resolved(open: String, title: String, token: String) -> Value {
    json!({ "ok": true, "open": open, "title": title, "token": token })
}

fn not_found(digits: &str) -> Ask {
    Ask::new(format!("No taxpayer with TIN {}", format_tin(digits)))
}

fn set_current(db: &Database, who: Who<'_>) -> Result<(), String> {
    db.set_setting(CURRENT_TAXPAYER_KEY, &only_digits(&who.branch.tin))
        .map_err(|error| error.to_string())
}

/// The tax-profile app's props: `{tin}`, with `branch` for a branch other
/// than the head office.
pub(crate) fn profile_props(root: &str, branch: Option<&ResolvedBranch>) -> Value {
    match branch.filter(|branch| !branch.is_head_office) {
        Some(branch) => json!({ "tin": root, "branch": branch.branch_code }),
        None => json!({ "tin": root }),
    }
}

// ------------------------------------------------------------ taxprofile

fn taxprofile(db: &Database, book: &Book, args: &str, today: NaiveDate) -> Result<Value, Ask> {
    let text = args.trim();
    // An attached taxpayer is this chat's, not the current one everywhere.
    let remember = !text.is_empty() || book.attached().is_none();
    let who = if text.is_empty() {
        book.current(db)?
            .ok_or_else(|| Ask::with("Which taxpayer?", book.taxpayer_choices("")))?
    } else if let Some(digits) = taxform::tin_digits(text) {
        book.find(&digits).ok_or_else(|| not_found(&digits))?
    } else {
        let found = book.matching(text);
        match found.as_slice() {
            [] => return Err(Ask::new(format!("No taxpayer named `{text}`"))),
            [one] => book
                .find(&one.taxpayer.tin_root)
                .ok_or_else(|| not_found(&one.taxpayer.tin_root))?,
            _ => return Err(Ask::with("Which taxpayer?", book.taxpayer_choices(text))),
        }
    };
    if remember {
        set_current(db, who)?;
    }
    let branch = (!who.branch.is_head_office).then_some(who.branch);
    let mut answer = resolved(
        app_link("tax-profile", &profile_props(who.root(), branch)),
        format!("{} · {}", who.name(), format_tin(&who.branch.tin)),
        format!("@bir:taxprofile:{}", taxform::token_tin(&who.branch.tin)),
    );
    answer["details"] = profile_details(db, who, today);
    Ok(answer)
}

// ------------------------------------------------------------ branchcode

/// `<root>/<code>`, a 14-digit TIN, a branch code (`1`, `00001`) or name of
/// the current taxpayer: the taxpayer, and what is left to find the branch by.
fn branch_query<'a>(
    db: &Database,
    book: &'a Book,
    args: &'a str,
) -> Result<(Option<Who<'a>>, &'a str), String> {
    let text = args.trim();
    if let Some((root, rest)) = text.split_once('/')
        && let Some(digits) = taxform::tin_digits(root)
    {
        return Ok((book.find(&digits[..9]), rest.trim()));
    }
    if let Some(digits) = taxform::tin_digits(text) {
        return Ok((
            book.find(&digits[..9]),
            if digits.len() == 14 {
                &text[text.len().saturating_sub(5)..]
            } else {
                ""
            },
        ));
    }
    Ok((book.current(db)?, text))
}

/// Whether `query` names `branch`: its code (`7`, `00007`; while typing,
/// any code it starts) or a word of its name.
fn branch_matches(branch: &ResolvedBranch, query: &str, typing: bool) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let digits = only_digits(query);
    if digits.len() == query.len() && digits.len() <= 5 {
        let code = branch.branch_code.as_str();
        let short = digits.trim_start_matches('0');
        return code == format!("{digits:0>5}")
            || (typing
                && (code.starts_with(&digits)
                    || (!short.is_empty() && code.trim_start_matches('0').starts_with(short))));
    }
    branch.name.to_lowercase().contains(&query.to_lowercase())
}

fn branch_choice(who: Who<'_>) -> Value {
    json!({
        "value": format!("{}/{}", who.root(), who.branch.branch_code),
        "label": format!("{} · {}", who.name(), who.branch.name),
        "detail": detail(&format_tin(&who.branch.tin), Some(who.branch)),
    })
}

fn branch_choices(db: &Database, book: &Book, args: &str) -> Vec<Value> {
    let Ok((Some(who), query)) = branch_query(db, book, args) else {
        return Vec::new();
    };
    who.taxpayer
        .branches
        .iter()
        .filter(|branch| branch_matches(branch, query, true))
        .map(|branch| branch_choice(Who { branch, ..who }))
        .collect()
}

fn branchcode(db: &Database, book: &Book, args: &str, today: NaiveDate) -> Result<Value, Ask> {
    let (who, query) = branch_query(db, book, args)?;
    let who = match who {
        Some(who) => who,
        None => {
            return Err(
                match taxform::tin_digits(args.split('/').next().unwrap_or("")) {
                    Some(digits) => not_found(&digits[..9]),
                    None => Ask::with(
                        "Which taxpayer? Name the branch as <tin>/<branch code>",
                        book.taxpayer_choices(""),
                    ),
                },
            );
        }
    };
    let found: Vec<&ResolvedBranch> = who
        .taxpayer
        .branches
        .iter()
        .filter(|branch| branch_matches(branch, query, false))
        .collect();
    let branch = match (found.as_slice(), query.is_empty()) {
        ([one], _) => *one,
        ([], false) => {
            return Err(Ask::new(format!("{} has no branch `{query}`", who.name())));
        }
        _ => {
            return Err(Ask::with(
                "Which branch?",
                found
                    .into_iter()
                    .map(|branch| branch_choice(Who { branch, ..who }))
                    .collect(),
            ));
        }
    };
    let who = Who { branch, ..who };
    if !args.trim().is_empty() || book.attached().is_none() {
        set_current(db, who)?;
    }
    let mut answer = resolved(
        app_link(
            "tax-profile",
            &json!({ "tin": who.root(), "branch": branch.branch_code }),
        ),
        format!("{} · {}", who.name(), branch.name),
        format!("@bir:branchcode:{}/{}", who.root(), branch.branch_code),
    );
    answer["details"] = profile_details(db, who, today);
    Ok(answer)
}

// ------------------------------------------------------------ taxform

/// One return to open in the forms app.
pub(crate) struct ReturnTarget {
    /// The branch's full TIN.
    pub tin: String,
    pub form: &'static str,
    pub period: TaxPeriod,
    pub action: TaxformAction,
    pub draft_id: Option<i64>,
    pub title: String,
    pub token: String,
}

impl ReturnTarget {
    /// The forms app's props: `{tin, form, period, action, draft_id?}`.
    pub(crate) fn props(&self) -> Value {
        let mut props = json!({
            "tin": self.tin,
            "form": self.form,
            "period": self.period.canonical(),
            "action": self.action.as_str(),
        });
        if let Some(id) = self.draft_id {
            props["draft_id"] = json!(id);
        }
        props
    }
}

/// The period a stored return is for, as its form files it. An as-needed
/// return keyed by its transaction's `MMDD` is that day; one keyed by an
/// entry number is the day it was last saved.
fn stored_period(
    code: &str,
    summary: &FormDraftSummary,
    key: Option<i64>,
    today: NaiveDate,
) -> TaxPeriod {
    let year = summary.taxable_year;
    let day = |month: i64, day: i64| {
        NaiveDate::from_ymd_opt(
            i32::from(year),
            u32::try_from(month).ok()?,
            u32::try_from(day).ok()?,
        )
    };
    let raw = if let Some(month) = summary.month.filter(|month| (1..=12).contains(month)) {
        TaxPeriod::Month { year, month }
    } else if let Some(quarter) = summary.quarter.filter(|quarter| (1..=4).contains(quarter)) {
        TaxPeriod::Quarter { year, quarter }
    } else if let Some(date) = key
        .filter(|key| (101..=1231).contains(key))
        .and_then(|key| day(key / 100, key % 100))
    {
        TaxPeriod::Day(date)
    } else if taxform::cadence(code) == Some(Cadence::AsNeeded) {
        let saved = summary
            .updated_at
            .get(..10)
            .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
            .filter(|date| date.year() == i32::from(year));
        TaxPeriod::Day(saved.or_else(|| day(1, 1)).unwrap_or(today))
    } else {
        TaxPeriod::Year(year)
    };
    raw.for_form(code, today).unwrap_or(raw)
}

/// The same return: the same period, or for an as-needed return the same
/// month ("this month's draft").
pub(crate) fn same_return(code: &str, stored: TaxPeriod, wanted: TaxPeriod) -> bool {
    match (taxform::cadence(code), stored, wanted) {
        (Some(Cadence::AsNeeded), TaxPeriod::Day(a), TaxPeriod::Day(b)) => {
            (a.year(), a.month()) == (b.year(), b.month())
        }
        _ => stored == wanted,
    }
}

/// The return `named` names for `who`: the period named (as the form files
/// it), else the newest unsubmitted draft of the form, else the return to
/// file now; and the stored return of that period, a draft first.
fn return_target(
    db: &Database,
    who: Who<'_>,
    code: &'static str,
    period: Option<TaxPeriod>,
    action: Option<TaxformAction>,
    today: NaiveDate,
) -> Result<ReturnTarget, Ask> {
    let returns = db
        .list_form_returns(&who.branch.tin, code)
        .map_err(|error| error.to_string())?;
    let returns: Vec<(TaxPeriod, &FormDraftSummary)> = returns
        .iter()
        .map(|(summary, key)| (stored_period(code, summary, *key, today), summary))
        .collect();
    let (period, found) = match period {
        Some(named) => {
            let named = named.for_form(code, today)?;
            let of_period = || {
                returns
                    .iter()
                    .filter(move |(stored, _)| same_return(code, *stored, named))
            };
            let found = of_period()
                .find(|(_, summary)| summary.status == FilingStatus::Draft)
                .or_else(|| of_period().next())
                .map(|(_, summary)| summary.id);
            (named, found)
        }
        None => match returns
            .iter()
            .find(|(_, summary)| summary.status == FilingStatus::Draft)
        {
            Some((stored, summary)) => (*stored, Some(summary.id)),
            None => {
                let now = taxform::period_to_file(code, today)
                    .ok_or_else(|| Ask::new(format!("BIR has no form `{code}`")))?;
                let found = returns
                    .iter()
                    .find(|(stored, _)| same_return(code, *stored, now))
                    .map(|(_, summary)| summary.id);
                (now, found)
            }
        },
    };
    let tin = only_digits(&who.branch.tin);
    let short = who.short_name();
    Ok(ReturnTarget {
        token: format!(
            "{}{}",
            taxform::TOKEN_PREFIX,
            taxform::canonical_value(&tin, code, period)
        ),
        title: if short.is_empty() {
            format!("{code} · {}", period.label())
        } else {
            format!("{code} · {} · {short}", period.label())
        },
        tin,
        form: code,
        period,
        action: action.unwrap_or(TaxformAction::File),
        draft_id: found,
    })
}

const NOT_UNDERSTOOD: &str =
    "Name a form (2551Q), a period (Q3 2026, 2026-01, 2025) and view, file or submit";

/// The taxpayer a taxform names, else the current one.
fn taxform_who<'a>(db: &Database, book: &'a Book, named: &TaxformArgs) -> Result<Who<'a>, Ask> {
    match &named.tin {
        Some(digits) => book.find(digits).ok_or_else(|| not_found(digits)),
        None => book
            .current(db)?
            .ok_or_else(|| Ask::with("Which taxpayer?", book.taxpayer_choices(""))),
    }
}

/// `Monthly`, `Quarterly`, `Annual`, `As needed`.
fn cadence_word(code: &str) -> String {
    let word = taxform::cadence(code).map(Cadence::as_str).unwrap_or("");
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A form choice: value `<tin>/2551Q` (or the bare code with no
/// taxpayer), label the code, detail its cadence, title and whose it is.
fn form_choice(code: &str, prefix: &str, whose: &str) -> Value {
    let title = bir_core::forms::find_form(code)
        .map(|form| form.title)
        .unwrap_or("");
    let detail = [cadence_word(code), title.to_string(), whose.to_string()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    json!({ "value": format!("{prefix}{code}"), "label": code, "detail": detail })
}

/// The forms `who` files: the ones chosen in their tax profile for this
/// year, else for their latest year that has any. Empty when none are
/// chosen. Only forms BIR can open.
fn profile_form_codes(db: &Database, who: Who<'_>, today: NaiveDate) -> Vec<&'static str> {
    let tin = &who.branch.tin;
    let mut years = db.list_forms_set_years(tin).unwrap_or_default();
    years.sort_by_key(|year| (*year != today.year() as u16, std::cmp::Reverse(*year)));
    for year in years {
        let mut codes: Vec<&'static str> = Vec::new();
        for code in db.active_form_codes_for_year(tin, year).unwrap_or_default() {
            if let Some(code) = taxform::form_code_of(&code)
                && !codes.contains(&code)
            {
                codes.push(code);
            }
        }
        if !codes.is_empty() {
            return codes;
        }
    }
    Vec::new()
}

/// Every form BIR can open.
fn all_form_codes() -> Vec<&'static str> {
    bir_core::forms::agent::AgentFormKind::all()
        .into_iter()
        .map(|kind| kind.form_code())
        .collect()
}

/// The form choices for `who` while `typing` (the word being typed, upper
/// case; it may still grow into a longer code) is typed: the forms in their
/// profile that match, else every other form that does, marked `not in
/// profile`.
fn form_choices_for(
    db: &Database,
    who: Who<'_>,
    typing: &str,
    today: NaiveDate,
) -> Vec<Value> {
    let prefix = format!("{}/", taxform::token_tin(&who.branch.tin));
    let query = typing.to_lowercase();
    let matches = |code: &str| {
        query.is_empty()
            || code.starts_with(typing)
            || bir_core::forms::find_form(code)
                .is_some_and(|form| form.title.to_lowercase().contains(&query))
    };
    let mine = profile_form_codes(db, who, today);
    let whose = format!("for {}", who.name());
    let mut out: Vec<Value> = mine
        .iter()
        .filter(|code| matches(code))
        .map(|code| form_choice(code, &prefix, &whose))
        .collect();
    if out.is_empty() {
        out = all_form_codes()
            .into_iter()
            .filter(|code| !mine.contains(code))
            .filter(|code| matches(code))
            .map(|code| form_choice(code, &prefix, "not in profile"))
            .collect();
    }
    out.truncate(MAX_CHOICES);
    out
}

/// Taxpayers to file for, each branch on its own: value `<tin>/` (a
/// 9-digit TIN for a head office, all 14 for another branch). The default
/// taxpayer first.
fn taxpayer_pick_choices(db: &Database, book: &Book, query: &str) -> Vec<Value> {
    let current = book
        .current(db)
        .ok()
        .flatten()
        .map(|who| only_digits(&who.branch.tin));
    let matching: Vec<&str> = book
        .matching(query)
        .into_iter()
        .map(|taxpayer| taxpayer.taxpayer.tin_root.as_str())
        .collect();
    let mut out: Vec<(bool, Value)> = book
        .branches()
        .filter(|who| query.trim().is_empty() || matching.contains(&who.root()))
        .map(|who| {
            let digits = only_digits(&who.branch.tin);
            let label = if who.branch.is_head_office {
                format!("{} · {}", who.name(), format_tin(who.root()))
            } else {
                format!(
                    "{} · {} · {}",
                    who.name(),
                    who.branch.name,
                    format_tin(&digits)
                )
            };
            (
                current.as_deref() == Some(digits.as_str()),
                json!({
                    "value": format!("{}/", taxform::token_tin(&digits)),
                    "label": label,
                    "detail": "Choose who you're filing for",
                }),
            )
        })
        .collect();
    out.sort_by_key(|(first, _)| !*first);
    let mut out: Vec<Value> = out.into_iter().map(|(_, choice)| choice).collect();
    out.truncate(MAX_CHOICES);
    out
}

/// How far a stored return has gone: a draft, queued, filed, confirmed, paid.
fn progress(status: &FilingStatus) -> u8 {
    match status {
        FilingStatus::Draft => 1,
        FilingStatus::Queued => 2,
        FilingStatus::Submitted => 3,
        FilingStatus::Confirmed => 4,
        FilingStatus::Paid => 5,
    }
}

/// Every stored return of `code` for the branch `tin`, each with the period
/// it is for, newest first.
pub(crate) fn stored_returns(
    db: &Database,
    tin: &str,
    code: &str,
    today: NaiveDate,
) -> Vec<(TaxPeriod, FormDraftSummary)> {
    db.list_form_returns(tin, code)
        .unwrap_or_default()
        .into_iter()
        .map(|(summary, key)| (stored_period(code, &summary, key, today), summary))
        .collect()
}

/// The stored return of `period`, the one furthest along.
pub(crate) fn furthest<'a>(
    returns: &'a [(TaxPeriod, FormDraftSummary)],
    code: &str,
    period: TaxPeriod,
) -> Option<&'a FormDraftSummary> {
    returns
        .iter()
        .filter(|(stored, _)| same_return(code, *stored, period))
        .map(|(_, summary)| summary)
        .max_by_key(|summary| progress(&summary.status))
}

/// `Oct 20, 2026` from a stored timestamp (`2026-10-20…`).
fn short_date(stamp: &str) -> String {
    stamp
        .get(..10)
        .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
        .map(|date| TaxPeriod::Day(date).label())
        .unwrap_or_else(|| stamp.to_string())
}

/// BIR's reference for a filed return: its file name as BIR's confirmation
/// email quotes it (`<tin>-<form>-<period>`), without the `#email#` part.
pub(crate) fn filing_reference(filename: &str) -> String {
    let stem = filename.strip_suffix(".xml").unwrap_or(filename);
    stem.split('#').next().unwrap_or(stem).to_string()
}

/// A stored return's status in words: `Not started`, `Draft · Oct 20, 2026`,
/// `Queued · <date>`, `Filed · <date>`, `Confirmed · <date>`. Never BIR's
/// file name: it starts with the TIN, and this text reaches cards (spec 6.6).
pub(crate) fn status_detail(db: &Database, filing: Option<&FormDraftSummary>) -> String {
    let Some(filing) = filing else {
        return "Not started".to_string();
    };
    let marks = || db.draft_data_json(filing.id).ok().flatten().unwrap_or(Value::Null);
    let mark = |marks: &Value, key: &str| {
        marks[key]
            .as_str()
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string)
    };
    match filing.status {
        FilingStatus::Draft => format!("Draft · {}", short_date(&filing.updated_at)),
        FilingStatus::Queued => format!("Queued · {}", short_date(&filing.updated_at)),
        FilingStatus::Submitted => {
            let marks = marks();
            let at = mark(&marks, "submitted_at").unwrap_or_else(|| filing.updated_at.clone());
            format!("Filed · {}", short_date(&at))
        }
        FilingStatus::Confirmed | FilingStatus::Paid => {
            let marks = marks();
            let at = mark(&marks, "confirmed_at").unwrap_or_else(|| filing.updated_at.clone());
            format!("Confirmed · {}", short_date(&at))
        }
    }
}

/// `1 return`, `4 returns · 1 draft`, `No returns`.
fn return_counts(returns: usize, drafts: usize) -> String {
    let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    match (returns, drafts) {
        (0, _) => "No returns".to_string(),
        (n, 0) => plural(n, "return"),
        (n, d) => format!("{} · {}", plural(n, "return"), plural(d, "draft")),
    }
}

/// `000-000-000-00000 · Head office · RDO 040`.
fn who_subtitle(who: Who<'_>) -> String {
    let branch = if who.branch.is_head_office {
        "Head office".to_string()
    } else if who.branch.name.trim().is_empty() {
        format!("Branch {}", who.branch.branch_code)
    } else {
        who.branch.name.clone()
    };
    let rdo = who.branch.value(BranchField::RdoCode).trim().to_string();
    let mut parts = vec![format_tin(&who.branch.tin), branch];
    if !rdo.is_empty() {
        parts.push(format!("RDO {rdo}"));
    }
    parts.join(" · ")
}

/// App Details for a taxpayer or branch: tax years newest first, each
/// opening the tax profile on that year.
fn profile_details(db: &Database, who: Who<'_>, today: NaiveDate) -> Value {
    let tin = &who.branch.tin;
    let summaries = db.list_draft_summaries_all_years(tin).unwrap_or_default();
    let mut years: Vec<u16> = db.list_forms_set_years(tin).unwrap_or_default();
    years.extend(summaries.iter().map(|summary| summary.taxable_year));
    years.sort_unstable_by(|a, b| b.cmp(a));
    years.dedup();
    let branch = (!who.branch.is_head_office).then_some(who.branch);
    let versions: Vec<Value> = years
        .iter()
        .map(|year| {
            let of_year = summaries.iter().filter(|summary| summary.taxable_year == *year);
            let returns = of_year.clone().count();
            let drafts = of_year
                .filter(|summary| summary.status == FilingStatus::Draft)
                .count();
            let mut props = profile_props(who.root(), branch);
            props["year"] = json!(year);
            json!({
                "label": year.to_string(),
                "detail": return_counts(returns, drafts),
                "open": app_link("tax-profile", &props),
            })
        })
        .collect();
    let forms = profile_form_codes(db, who, today);
    let forms = if forms.is_empty() {
        "None chosen".to_string()
    } else {
        forms.join(", ")
    };
    let mut facts = vec![
        json!(["Type", format!("{:?}", who.taxpayer.taxpayer.taxpayer_type)]),
        json!(["Forms", forms]),
    ];
    if who.taxpayer.branches.len() > 1 {
        facts.push(json!(["Branches", who.taxpayer.branches.len().to_string()]));
    }
    json!({
        "name": who.name(),
        "subtitle": who_subtitle(who),
        "image": null,
        "versions": versions,
        "facts": facts,
    })
}

/// App Details for a return: that form's tax years for the taxpayer, each
/// opening the forms app on the form and year.
fn taxform_details(
    db: &Database,
    who: Who<'_>,
    target: &ReturnTarget,
    today: NaiveDate,
) -> Value {
    let code = target.form;
    let returns = stored_returns(db, &who.branch.tin, code, today);
    let mut years: Vec<u16> = returns.iter().map(|(period, _)| period.year()).collect();
    years.push(target.period.year());
    years.sort_unstable_by(|a, b| b.cmp(a));
    years.dedup();
    let versions: Vec<Value> = years
        .iter()
        .map(|year| {
            let of_year = returns.iter().filter(|(period, _)| period.year() == *year);
            let count = of_year.clone().count();
            let drafts = of_year
                .filter(|(_, summary)| summary.status == FilingStatus::Draft)
                .count();
            json!({
                "label": year.to_string(),
                "detail": return_counts(count, drafts),
                "open": app_link("forms", &json!({ "tin": target.tin, "form_code": code, "year": year })),
            })
        })
        .collect();
    let form = match bir_core::forms::find_form(code).map(|form| form.title) {
        Some(title) if !title.is_empty() => format!("{code} · {title}"),
        _ => code.to_string(),
    };
    let mut facts = vec![
        json!(["Taxpayer", who.name()]),
        json!(["Form", form]),
        json!(["Period", target.period.label()]),
        json!(["Status", status_detail(db, furthest(&returns, code, target.period))]),
    ];
    if let Some(due) = super::calendar::due_date(db, &who.branch.tin, code, target.period) {
        facts.push(json!(["Due", TaxPeriod::Day(due).label()]));
    }
    json!({
        "name": format!("{code} · {}", target.period.label()),
        "subtitle": format!("{} · {}", who.name(), who_subtitle(who)),
        "image": null,
        "versions": versions,
        "facts": facts,
    })
}

fn taxform_answer(db: &Database, book: &Book, args: &str, today: NaiveDate) -> Result<Value, Ask> {
    let named = taxform::parse(args, today);
    if !named.unknown.is_empty() {
        return Err(Ask::new(format!(
            "Not understood: `{}`. {NOT_UNDERSTOOD}.",
            named.unknown.join(" ")
        )));
    }
    let who = taxform_who(db, book, &named)?;
    let Some(code) = named.form else {
        return Err(Ask::with(
            "Which form?",
            form_choices_for(db, who, "", today),
        ));
    };
    let target = return_target(db, who, code, named.period, named.action, today)?;
    let mut answer = resolved(
        app_link("forms", &target.props()),
        target.title.clone(),
        target.token.clone(),
    );
    answer["details"] = taxform_details(db, who, &target, today);
    Ok(answer)
}

/// Suggestions while the user types, one step at a time: who the return is
/// for (when no taxpayer is known, or nothing is typed and there are
/// several), then that taxpayer's forms, then the form's periods newest
/// first (the one to file now first), each with its status.
fn taxform_choices(db: &Database, book: &Book, args: &str, today: NaiveDate) -> Vec<Value> {
    let named = taxform::parse(args, today);
    let who = match &named.tin {
        Some(digits) => book.find(digits),
        None => book.current(db).ok().flatten(),
    };
    let several = book.branches().nth(1).is_some();
    let ask_who = match (&named.tin, who) {
        (_, None) => true,
        (None, Some(_)) => args.trim().is_empty() && several && book.attached().is_none(),
        (Some(_), Some(_)) => false,
    };
    if ask_who {
        let query = match (&named.tin, named.form) {
            (Some(digits), _) => digits.clone(),
            (None, None) => named.unknown.join(" "),
            (None, Some(_)) => String::new(),
        };
        let found = taxpayer_pick_choices(db, book, &query);
        return if found.is_empty() {
            taxpayer_pick_choices(db, book, "")
        } else {
            found
        };
    }
    let Some(who) = who else {
        return Vec::new();
    };
    let action = named
        .action
        .map(|action| format!(" {}", action.as_str()))
        .unwrap_or_default();
    // The word being typed, when it may still grow into another code
    // (`1701` before `1701Q`).
    let typing = (!args.ends_with(char::is_whitespace))
        .then(|| args.split_whitespace().last())
        .flatten()
        .map(|word| {
            word.rsplit('/')
                .next()
                .unwrap_or(word)
                .replace('-', "")
                .to_ascii_uppercase()
        })
        .unwrap_or_default();
    let growing = |code: &str| !typing.is_empty() && code.starts_with(typing.as_str());
    let form = named.form.filter(|form| {
        !(typing == *form && all_form_codes().iter().filter(|code| growing(code)).count() > 1)
    });
    let Some(code) = form else {
        let found = form_choices_for(db, who, &typing, today);
        if found.is_empty() && !typing.is_empty() && typing.chars().all(|c| c.is_ascii_digit()) {
            return taxpayer_pick_choices(db, book, &typing);
        }
        return found;
    };
    let returns = stored_returns(db, &who.branch.tin, code, today);
    let choice = |period: TaxPeriod, detail: String| {
        json!({
            "value": format!("{}{action}", taxform::canonical_value(&who.branch.tin, code, period)),
            "label": format!("{code} · {}", period.label()),
            "detail": detail,
        })
    };
    let status = |period: TaxPeriod| status_detail(db, furthest(&returns, code, period));
    if let Some(period) = named.period {
        return match period.for_form(code, today) {
            Ok(period) => vec![choice(period, status(period))],
            Err(_) => Vec::new(),
        };
    }
    let mut periods: Vec<TaxPeriod> = Vec::new();
    if let Some(now) = taxform::period_to_file(code, today) {
        periods.push(now);
        if matches!(now, TaxPeriod::Day(_)) {
            // As needed: today, then the days of its stored returns.
            let mut days: Vec<TaxPeriod> = returns.iter().map(|(period, _)| *period).collect();
            days.sort_unstable_by(|a, b| b.cmp(a));
            for day in days {
                if !periods.iter().any(|seen| same_return(code, *seen, day)) {
                    periods.push(day);
                }
            }
        } else {
            let mut period = now;
            while periods.len() < MAX_PERIODS {
                match previous(code, period, today) {
                    Some(before) => {
                        periods.push(before);
                        period = before;
                    }
                    None => break,
                }
            }
        }
    }
    periods.truncate(MAX_PERIODS);
    periods
        .into_iter()
        .enumerate()
        .map(|(step, period)| {
            let status = status(period);
            choice(
                period,
                if step == 0 {
                    format!("To file now · {status}")
                } else {
                    status
                },
            )
        })
        .collect()
}

/// The period before `period` of a periodic form.
fn previous(code: &str, period: TaxPeriod, today: NaiveDate) -> Option<TaxPeriod> {
    let before = match period {
        TaxPeriod::Month { year, month: 1 } => TaxPeriod::Month {
            year: year - 1,
            month: 12,
        },
        TaxPeriod::Month { year, month } => TaxPeriod::Month {
            year,
            month: month - 1,
        },
        TaxPeriod::Quarter { year, quarter: 1 } => TaxPeriod::Quarter {
            year: year - 1,
            quarter: 4,
        },
        TaxPeriod::Quarter { year, quarter } => TaxPeriod::Quarter {
            year,
            quarter: quarter - 1,
        },
        TaxPeriod::Year(year) => TaxPeriod::Year(year - 1),
        TaxPeriod::Day(_) => return None,
    };
    before.for_form(code, today).ok()
}

/// The forms app for `tin` (else the current taxpayer) on the return `form`
/// names (a code or a token value) and `period`: what `bir_app_open` opens.
pub(crate) fn app_return(
    db: &Database,
    tin: Option<&str>,
    form: &str,
    period: Option<&str>,
    action: Option<TaxformAction>,
    today: NaiveDate,
) -> Result<ReturnTarget, String> {
    let book = Book::read(db)?;
    let mut named = taxform::parse(form, today);
    if let Some(period) = period {
        let read = taxform::parse(period, today);
        if read.period.is_none() || read.form.is_some() || !read.unknown.is_empty() {
            return Err(format!(
                "`period` `{period}` is not a period (e.g. 2026-Q3, jan 2026, 2025)"
            ));
        }
        named.period = read.period;
    }
    if !named.unknown.is_empty() || named.form.is_none() {
        return Err(format!(
            "`form` `{form}` is not a BIR form code or return token (e.g. 2551Q, 000000000/2551Q-2026-Q3)"
        ));
    }
    if let Some(tin) = tin {
        let digits = taxform::tin_digits(tin)
            .ok_or_else(|| format!("`{tin}` is not a TIN (9 or 14 digits)"))?;
        if named
            .tin
            .as_ref()
            .is_some_and(|named| named[..9] != digits[..9])
        {
            return Err(format!("`tin` {tin} is not the TIN `{form}` names"));
        }
        named.tin = Some(digits);
    }
    let code = named.form.expect("checked above");
    let who = taxform_who(db, &book, &named).map_err(|ask| match named.tin.as_deref() {
        Some(_) => format!(
            "{}; list the saved taxpayers with bir_profiles_list",
            ask.error
        ),
        None => "no taxpayer is named and none is current: pass `tin` (see bir_profiles_list)"
            .to_string(),
    })?;
    return_target(db, who, code, named.period, action.or(named.action), today)
        .map_err(|ask| ask.error)
}

/// The tax-profile app's props for `tin`: its root, and its branch when it
/// is not the head office. `Ok(None)` when no such taxpayer is saved.
pub(crate) fn app_profile(db: &Database, tin: &str) -> Result<Option<(Value, String)>, String> {
    let book = Book::read(db)?;
    let digits =
        taxform::tin_digits(tin).ok_or_else(|| format!("`{tin}` is not a TIN (9 or 14 digits)"))?;
    Ok(book.find(&digits).map(|who| {
        (
            profile_props(who.root(), Some(who.branch)),
            who.name().to_string(),
        )
    }))
}

/// The current taxpayer's full TIN, while it is still saved.
pub(crate) fn current_tin(db: &Database) -> Result<Option<String>, String> {
    let book = Book::read(db)?;
    Ok(book.current(db)?.map(|who| only_digits(&who.branch.tin)))
}

/// One saved branch, as the calendar and the confirmation tools name it.
#[derive(Debug, Clone)]
pub(crate) struct Branch {
    /// The branch's full TIN, digits only.
    pub tin: String,
    pub root: String,
    /// The taxpayer's name.
    pub name: String,
    pub is_head_office: bool,
}

impl Branch {
    fn of(who: Who<'_>) -> Self {
        Self {
            tin: only_digits(&who.branch.tin),
            root: who.root().to_string(),
            name: who.name().to_string(),
            is_head_office: who.branch.is_head_office,
        }
    }
}

/// The branch `tin` names (a 9-digit root is its head office), or every
/// branch of every taxpayer not archived.
pub(crate) fn branches(db: &Database, tin: Option<&str>) -> Result<Vec<Branch>, String> {
    let book = Book::read(db)?;
    match tin {
        Some(tin) => {
            let digits = taxform::tin_digits(tin)
                .ok_or_else(|| format!("`{tin}` is not a TIN (9 or 14 digits)"))?;
            let who = book.find(&digits).ok_or_else(|| not_found(&digits).error)?;
            Ok(vec![Branch::of(who)])
        }
        None => Ok(book.branches().map(Branch::of).collect()),
    }
}

/// `bir_drafts_list`: the stored returns of `tin` (else the current
/// taxpayer), of one form when `form` names it (a code or a token value,
/// whose period then narrows the list too), newest first. Unsubmitted ones
/// only unless `all`.
pub(crate) fn drafts_list(
    db: &Database,
    tin: Option<&str>,
    form: Option<&str>,
    all: bool,
    today: NaiveDate,
) -> Result<Value, String> {
    let book = Book::read(db)?;
    let mut named = form
        .map(|form| taxform::parse(form, today))
        .unwrap_or_default();
    if let Some(form) = form
        && (named.form.is_none() || !named.unknown.is_empty())
    {
        return Err(format!(
            "`form` `{form}` is not a BIR form code or return token"
        ));
    }
    if let Some(tin) = tin {
        named.tin = Some(
            taxform::tin_digits(tin)
                .ok_or_else(|| format!("`{tin}` is not a TIN (9 or 14 digits)"))?,
        );
    }
    let named_tin = named.tin.is_some();
    let who = taxform_who(db, &book, &named).map_err(|ask| {
        if named_tin {
            ask.error
        } else {
            "no taxpayer is named and none is current: pass `tin` (see bir_profiles_list)"
                .to_string()
        }
    })?;
    let codes: Vec<&'static str> = match named.form {
        Some(code) => vec![code],
        None => bir_core::forms::agent::AgentFormKind::all()
            .into_iter()
            .map(|kind| kind.form_code())
            .collect(),
    };
    let wanted = match (named.form, named.period) {
        (Some(code), Some(period)) => Some(period.for_form(code, today)?),
        _ => None,
    };
    let mut rows: Vec<(String, Value)> = Vec::new();
    for code in codes {
        let returns = db
            .list_form_returns(&who.branch.tin, code)
            .map_err(|error| error.to_string())?;
        for (summary, key) in &returns {
            if !all && summary.status != FilingStatus::Draft {
                continue;
            }
            let period = stored_period(code, summary, *key, today);
            if wanted.is_some_and(|wanted| !same_return(code, period, wanted)) {
                continue;
            }
            rows.push((
                summary.updated_at.clone(),
                json!({
                    "id": summary.id,
                    "form": code,
                    "period": period.canonical(),
                    "label": format!("{code} · {}", period.label()),
                    "status": format!("{:?}", summary.status),
                    "updated_at": summary.updated_at,
                    "token": format!(
                        "{}{}",
                        taxform::TOKEN_PREFIX,
                        taxform::canonical_value(&who.branch.tin, code, period)
                    ),
                }),
            ));
        }
    }
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(json!({
        "tin": only_digits(&who.branch.tin),
        "taxpayer": who.name(),
        "drafts": rows.into_iter().map(|(_, row)| row).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bir_core::forms::Form2551QDraft;
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
            name: "Galang, Juan".into(),
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

    fn ask(db: &Database, name: &str, args: &str) -> Value {
        resolve(
            db,
            &Request {
                name: name.into(),
                args: args.into(),
                complete: false,
                attached: Vec::new(),
            },
            today(),
        )
    }

    fn complete(db: &Database, name: &str, args: &str) -> Value {
        resolve(
            db,
            &Request {
                name: name.into(),
                args: args.into(),
                complete: true,
                attached: Vec::new(),
            },
            today(),
        )
    }

    /// The props JSON of a `rox://app/bir/<app>?props=` link.
    fn props(open: &Value) -> Value {
        let link = open.as_str().unwrap();
        let encoded = link
            .split_once("?props=")
            .unwrap()
            .1
            .split('&')
            .next()
            .unwrap();
        let mut bytes = Vec::new();
        let mut chars = encoded.bytes();
        while let Some(byte) = chars.next() {
            if byte == b'%' {
                let hex = [chars.next().unwrap(), chars.next().unwrap()];
                bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).unwrap(), 16).unwrap());
            } else {
                bytes.push(byte);
            }
        }
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn taxform_resolves_to_a_forms_link_and_a_canonical_token() {
        let db = db();
        db.set_setting(CURRENT_TAXPAYER_KEY, TIN).unwrap();
        let answer = ask(&db, "taxform", "2551Q Q3 2026 submit");
        assert_eq!(answer["ok"], true, "{answer}");
        assert_eq!(
            answer["token"],
            format!("@bir:taxform:{ROOT}/2551Q-2026-Q3")
        );
        assert_eq!(answer["title"], "2551Q · Q3 2026 · Galang");
        assert!(
            answer["open"]
                .as_str()
                .unwrap()
                .starts_with("rox://app/bir/forms?props=")
        );
        assert_eq!(
            props(&answer["open"]),
            json!({"tin": TIN, "form": "2551Q", "period": "2026-Q3", "action": "submit"})
        );
        // The token itself resolves to the same return, action `file` by default.
        let again = ask(&db, "taxform", &format!("{ROOT}/2551Q-2026-Q3"));
        assert_eq!(again["token"], answer["token"]);
        assert_eq!(props(&again["open"])["action"], "file");
        // Compact and dashed spellings.
        assert_eq!(ask(&db, "taxform", "2551QQ32026")["token"], answer["token"]);
        assert_eq!(
            ask(&db, "taxform", "1601-C jan 2026")["token"],
            format!("@bir:taxform:{ROOT}/1601C-2026-01")
        );
    }

    #[test]
    fn taxform_defaults_to_the_period_to_file_and_reuses_a_draft() {
        let db = db();
        db.set_setting(CURRENT_TAXPAYER_KEY, TIN).unwrap();
        let now = ask(&db, "taxform", "2551Q");
        assert_eq!(now["token"], format!("@bir:taxform:{ROOT}/2551Q-2026-Q3"));
        assert!(props(&now["open"]).get("draft_id").is_none());
        assert_eq!(
            ask(&db, "taxform", "1701")["token"],
            format!("@bir:taxform:{ROOT}/1701-2025")
        );
        // An unsubmitted Q1 draft is reused before the period to file now.
        let profile = db.get_profile(TIN).unwrap().unwrap();
        let draft = Form2551QDraft::new_from_profile(&profile, 2026, 1);
        let id = db.save_2551q_draft(&draft).unwrap();
        let reused = ask(&db, "taxform", "2551Q view");
        assert_eq!(
            reused["token"],
            format!("@bir:taxform:{ROOT}/2551Q-2026-Q1")
        );
        assert_eq!(
            props(&reused["open"]),
            json!({"tin": TIN, "form": "2551Q", "period": "2026-Q1", "action": "view", "draft_id": id})
        );
        // A named period finds its own stored return.
        assert_eq!(
            props(&ask(&db, "taxform", "2551Q 2026-Q1")["open"])["draft_id"],
            id
        );
        assert!(
            props(&ask(&db, "taxform", "2551Q 2026-Q2")["open"])
                .get("draft_id")
                .is_none()
        );
        let listed = drafts_list(&db, None, Some("2551Q"), false, today()).unwrap();
        assert_eq!(listed["drafts"][0]["id"], id);
        assert_eq!(listed["drafts"][0]["period"], "2026-Q1");
        assert_eq!(
            listed["drafts"][0]["token"],
            format!("@bir:taxform:{ROOT}/2551Q-2026-Q1")
        );
        let narrowed = drafts_list(&db, Some(ROOT), Some("2551Q 2026-Q2"), false, today()).unwrap();
        assert!(narrowed["drafts"].as_array().unwrap().is_empty());
    }

    #[test]
    fn unresolved_taxforms_say_what_is_missing() {
        let db = db();
        let who = ask(&db, "taxform", "2551Q");
        assert_eq!(who["ok"], false);
        assert_eq!(who["error"], "Which taxpayer?");
        assert_eq!(who["choices"][0]["value"], ROOT);
        assert_eq!(who["choices"][0]["detail"], "123-456-788 · RDO 047");
        let missing = ask(&db, "taxform", "999999999/2551Q-2026-Q3");
        assert_eq!(
            missing,
            json!({"ok": false, "error": "No taxpayer with TIN 999-999-999"})
        );
        db.set_setting(CURRENT_TAXPAYER_KEY, TIN).unwrap();
        let form = ask(&db, "taxform", "Q3 2026");
        assert_eq!(form["error"], "Which form?");
        assert!(
            form["choices"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["value"] == format!("{ROOT}/2551Q"))
        );
        assert!(form["choices"].as_array().unwrap().len() <= MAX_CHOICES);
        let junk = ask(&db, "taxform", "2551Q someday");
        assert_eq!(junk["ok"], false);
        assert!(
            junk["error"]
                .as_str()
                .unwrap()
                .starts_with("Not understood"),
            "{junk}"
        );
        let monthly = ask(&db, "taxform", "1601C Q3 2026");
        assert_eq!(monthly["ok"], false);
        let unknown = ask(&db, "nope", "");
        assert_eq!(unknown["ok"], false);
    }

    #[test]
    fn taxprofile_sets_the_current_taxpayer_and_survives_a_reread() {
        let db = db();
        let none = ask(&db, "taxprofile", "");
        assert_eq!(none["error"], "Which taxpayer?");
        let missing = ask(&db, "taxprofile", "000-000-000");
        assert_eq!(missing["error"], "No taxpayer with TIN 000-000-000");
        assert!(db.get_setting(CURRENT_TAXPAYER_KEY).unwrap().is_none());
        let opened = ask(&db, "taxprofile", "123-456-788");
        assert_eq!(opened["ok"], true, "{opened}");
        assert_eq!(opened["token"], format!("@bir:taxprofile:{ROOT}"));
        assert_eq!(props(&opened["open"]), json!({"tin": ROOT}));
        assert!(
            opened["open"]
                .as_str()
                .unwrap()
                .starts_with("rox://app/bir/tax-profile?props=")
        );
        assert_eq!(
            db.get_setting(CURRENT_TAXPAYER_KEY).unwrap().as_deref(),
            Some(TIN)
        );
        // No TIN: the current taxpayer.
        assert_eq!(ask(&db, "taxprofile", "")["token"], opened["token"]);
        // By name too.
        assert_eq!(ask(&db, "taxprofile", "galang")["token"], opened["token"]);
        let suggested = complete(&db, "taxprofile", "1234");
        assert_eq!(suggested["choices"][0]["value"], ROOT);
        assert_eq!(suggested["choices"][0]["label"], "Galang, Juan");
        assert!(
            complete(&db, "taxprofile", "999")["choices"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn branchcode_opens_the_profile_on_that_branch() {
        let db = db();
        let opened = ask(&db, "branchcode", &format!("{ROOT}/00000"));
        assert_eq!(opened["ok"], true, "{opened}");
        assert_eq!(opened["token"], format!("@bir:branchcode:{ROOT}/00000"));
        assert_eq!(
            props(&opened["open"]),
            json!({"tin": ROOT, "branch": "00000"})
        );
        assert_eq!(
            db.get_setting(CURRENT_TAXPAYER_KEY).unwrap().as_deref(),
            Some(TIN)
        );
        assert_eq!(ask(&db, "branchcode", "0")["token"], opened["token"]);
        assert_eq!(ask(&db, "branchcode", "7")["ok"], false);
        assert_eq!(
            complete(&db, "branchcode", "")["choices"][0]["value"],
            format!("{ROOT}/00000")
        );
    }

    #[test]
    fn taxform_suggestions_grow_into_forms_then_periods() {
        let db = db();
        db.set_setting(CURRENT_TAXPAYER_KEY, TIN).unwrap();
        let forms = complete(&db, "taxform", "255");
        let values: Vec<&str> = forms["choices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|choice| choice["value"].as_str().unwrap())
            .collect();
        assert!(values.contains(&"123456788/2551Q"), "{values:?}");
        assert!(
            values
                .iter()
                .all(|value| value.starts_with("123456788/255")),
            "{values:?}"
        );
        // No form is chosen in this profile: every form, marked so.
        assert!(
            forms["choices"][0]["detail"]
                .as_str()
                .unwrap()
                .ends_with("· not in profile"),
            "{forms}"
        );
        // `1701` may still become 1701Q, 1701A or 1701MS.
        let growing = complete(&db, "taxform", "1701");
        assert!(
            growing["choices"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["value"] == format!("{ROOT}/1701Q"))
        );
        let periods = complete(&db, "taxform", "2551Q ");
        let choices = periods["choices"].as_array().unwrap();
        let first = &choices[0];
        assert_eq!(first["value"], format!("{ROOT}/2551Q-2026-Q3"));
        assert_eq!(first["detail"], "To file now · Not started");
        assert_eq!(choices[1]["value"], format!("{ROOT}/2551Q-2026-Q2"));
        assert_eq!(choices.len(), MAX_PERIODS);
        // A draft shows its status on its period.
        let profile = db.get_profile(TIN).unwrap().unwrap();
        db.save_2551q_draft(&Form2551QDraft::new_from_profile(&profile, 2026, 2))
            .unwrap();
        let periods = complete(&db, "taxform", &format!("{ROOT}/2551Q "));
        assert!(
            periods["choices"][1]["detail"]
                .as_str()
                .unwrap()
                .starts_with("Draft · "),
            "{periods}"
        );
    }

    fn add_taxpayer(db: &Database, root: &str, name: &str) {
        db.create_taxpayer(NewTaxpayer {
            tin_root: root.into(),
            name: name.into(),
            taxpayer_type: TaxpayerType::Individual,
            birth_date: None,
            head_office: BranchFacts {
                rdo_code: "040".into(),
                ..BranchFacts::default()
            },
        })
        .expect("taxpayer");
    }

    fn with_context(db: &Database, name: &str, args: &str, complete: bool, attached: &[&str]) -> Value {
        resolve(
            db,
            &Request {
                name: name.into(),
                args: args.into(),
                complete,
                attached: attached.iter().filter_map(|token| attached_tin(token)).collect(),
            },
            today(),
        )
    }

    #[test]
    fn completion_asks_for_the_taxpayer_first_then_their_forms() {
        let db = db();
        add_taxpayer(&db, "987654321", "Dela Cruz, Maria");
        db.set_setting(CURRENT_TAXPAYER_KEY, TIN).unwrap();
        let who = complete(&db, "taxform", "");
        let choices = who["choices"].as_array().unwrap();
        assert_eq!(choices.len(), 2, "{who}");
        assert_eq!(choices[0]["value"], format!("{ROOT}/"), "the current one first");
        assert_eq!(choices[0]["detail"], "Choose who you're filing for");
        assert_eq!(choices[1]["value"], "987654321/");
        // After the pick, that taxpayer's forms; the profile chose two.
        let set = bir_core::forms::PerYearFormsSet::from_codes(
            2026,
            ["2551Q", "1601C"],
            bir_core::forms::FormSetSource::Manual,
        );
        db.save_per_year_forms("98765432100000", 2026, &set).unwrap();
        let forms = complete(&db, "taxform", "987654321/");
        let values: Vec<&str> = forms["choices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|choice| choice["value"].as_str().unwrap())
            .collect();
        assert_eq!(values.len(), 2, "{forms}");
        assert!(values.contains(&"987654321/1601C"));
        let monthly = forms["choices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|choice| choice["label"] == "1601C")
            .unwrap();
        let detail = monthly["detail"].as_str().unwrap();
        assert!(detail.starts_with("Monthly · "), "{detail}");
        assert!(detail.ends_with("· for Dela Cruz, Maria"), "{detail}");
        // An attached taxpayer is the default: no question, its forms.
        let attached = with_context(&db, "taxform", "", true, &["@bir:taxprofile:987654321"]);
        assert!(
            attached["choices"]
                .as_array()
                .unwrap()
                .iter()
                .all(|choice| choice["value"].as_str().unwrap().starts_with("987654321/")),
            "{attached}"
        );
        assert_eq!(attached["choices"].as_array().unwrap().len(), 2, "{attached}");
        let opened = with_context(&db, "taxform", "1601C 2026-09", false, &["@bir:taxprofile:987654321"]);
        assert_eq!(opened["token"], "@bir:taxform:987654321/1601C-2026-09");
        // A branch token names its branch; an unknown one falls back.
        assert_eq!(attached_tin("@bir:branchcode:987654321/1").as_deref(), Some("98765432100001"));
        assert_eq!(attached_tin("@bir:taxform:987654321/1601C-2026-09"), None);
        let fallback = with_context(&db, "taxform", "2551Q 2026-Q3", false, &["@bir:taxprofile:555555555"]);
        assert_eq!(fallback["token"], format!("@bir:taxform:{ROOT}/2551Q-2026-Q3"));
        // Resolving the attached taxpayer leaves the current one alone.
        with_context(&db, "taxprofile", "", false, &["@bir:taxprofile:987654321"]);
        assert_eq!(db.get_setting(CURRENT_TAXPAYER_KEY).unwrap().as_deref(), Some(TIN));
    }

    #[test]
    fn resolved_answers_carry_details_and_token_links() {
        let db = db();
        let profile = db.get_profile(TIN).unwrap().unwrap();
        db.save_2551q_draft(&Form2551QDraft::new_from_profile(&profile, 2026, 2))
            .unwrap();
        let opened = ask(&db, "taxprofile", ROOT);
        let details = &opened["details"];
        assert_eq!(details["name"], "Galang, Juan");
        assert_eq!(details["subtitle"], "123-456-788-00000 · Head office · RDO 047");
        assert_eq!(details["image"], Value::Null);
        assert_eq!(details["versions"][0]["label"], "2026");
        assert_eq!(details["versions"][0]["detail"], "1 return · 1 draft");
        assert_eq!(props(&details["versions"][0]["open"]), json!({"tin": ROOT, "year": 2026}));
        assert_eq!(details["facts"][0], json!(["Type", "Individual"]));
        assert!(
            opened["open"]
                .as_str()
                .unwrap()
                .ends_with("&token=%40bir%3Ataxprofile%3A123456788")
        );
        let form = ask(&db, "taxform", &format!("{ROOT}/2551Q-2026-Q2"));
        assert_eq!(form["details"]["name"], "2551Q · Q2 2026");
        assert_eq!(form["details"]["versions"][0]["label"], "2026");
        assert!(
            form["details"]["facts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|fact| fact[0] == "Status" && fact[1].as_str().unwrap().starts_with("Draft · ")),
            "{form}"
        );
        assert!(
            form["open"]
                .as_str()
                .unwrap()
                .ends_with("&token=%40bir%3Ataxform%3A123456788%2F2551Q-2026-Q2")
        );
    }

    #[test]
    fn links_encode_like_encode_uri_component() {
        assert_eq!(
            encode_component("{\"a\":\"b c/é\"}"),
            "%7B%22a%22%3A%22b%20c%2F%C3%A9%22%7D"
        );
        assert_eq!(
            encode_component("@bir:taxform:1/2 "),
            "%40bir%3Ataxform%3A1%2F2%20"
        );
    }
}
