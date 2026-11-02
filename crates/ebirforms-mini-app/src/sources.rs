//! Where each box of a saved return got its value: the `source` (and the
//! optional `source_ref`) that `ebirforms_form_fill` was given.
//!
//! eBIRForms' draft has no room for this, and only `BirApp` is a supported
//! surface, so the mini app keeps it in its own data folder:
//!
//! - `fill-sources.json` (0600): one entry per stored return, its boxes'
//!   `{source, source_ref?, from_card?, value_mac}` and when it was saved;
//! - `fill-sources.key` (0600, made on first use): 32 random bytes;
//! - `fill-sources.lock`: held while a save reads and rewrites the file.
//!
//! A return is named by an HMAC-SHA-256, under that key, of its taxpayer,
//! form, year and stored period; each box keeps an HMAC of its value, not the
//! value. Without the key file, which sits beside the database's own key
//! with the same protection, neither a TIN nor an amount can be recovered
//! from the file. A box whose value has changed since it was filled (the
//! person typed over it in the Forms app) has no source any more.
//!
//! Several processes may run at once (the Bot respawns this one back to
//! back): a save takes the lock, re-reads the file, replaces only its own
//! return's entry and writes the file back through a temp file and a rename.
//! A file that cannot be read or parsed is never overwritten: the save fails
//! and says so. The file keeps the [`MAX_RETURNS`] most recently saved
//! returns.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use hmac::{Hmac, Mac};
use serde_json::{Map, Value, json};
use sha2::Sha256;

/// The sources file in the data folder.
pub const FILE: &str = "fill-sources.json";
/// The key file in the data folder (64 hex digits, mode 0600).
pub const KEY_FILE: &str = "fill-sources.key";
/// The lock file in the data folder.
pub const LOCK_FILE: &str = "fill-sources.lock";
/// The most returns the file keeps; the least recently saved go first.
pub const MAX_RETURNS: usize = 1000;

/// The labels `ebirforms_form_fill` takes for `source`.
pub const LABELS: [&str; 5] = ["profile", "past_return", "document", "ai", "user"];

/// What one box was filled from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub label: String,
    /// The `source_ref` the fill carried, already checked.
    pub reference: Option<Map<String, Value>>,
    /// The assistant said this is the person's answer on a card. Trusted as
    /// stated: eBIRForms has no record of card answers to check it against.
    pub from_card: bool,
    /// [`Sources::value_mac`] of the value eBIRForms holds after the fill.
    pub value_mac: String,
}

impl Source {
    /// The source as `ebirforms_form_fields` shows it.
    pub fn shown(&self) -> Value {
        let mut out = Map::new();
        out.insert("source".into(), json!(self.label));
        if let Some(reference) = &self.reference {
            out.insert("source_ref".into(), Value::Object(reference.clone()));
        }
        if self.from_card {
            out.insert("from_card".into(), json!(true));
        }
        Value::Object(out)
    }

    fn stored(&self) -> Value {
        let mut out = match self.shown() {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        out.insert("value_mac".into(), json!(self.value_mac));
        Value::Object(out)
    }

    fn from_stored(value: &Value) -> Option<Self> {
        let label = value.get("source")?.as_str()?;
        if !LABELS.contains(&label) {
            return None;
        }
        let reference = match value.get("source_ref") {
            None => None,
            Some(reference) => Some(check_reference(reference).ok()?),
        };
        Some(Self {
            label: label.to_string(),
            reference,
            from_card: value.get("from_card").and_then(Value::as_bool) == Some(true),
            value_mac: value.get("value_mac")?.as_str()?.to_string(),
        })
    }
}

/// An amount as a BIR form shows it, in centavos: decimal text with two
/// places, thousands separators in groups of three (`"1,200,000.00"`) and
/// whole pesos (`"1,200,000"`) allowed. The same rule as Books'
/// `money::parse_shown`.
pub fn shown_amount(text: &str) -> Option<i64> {
    let text = text.trim();
    let (negative, body) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (whole, cents) = body.split_once('.').unwrap_or((body, "00"));
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let mut groups = whole.split(',');
    let first = groups.next().unwrap_or_default();
    let mut plain = first.to_string();
    let mut grouped = false;
    for group in groups {
        if group.len() != 3 || !digits(group) {
            return None;
        }
        grouped = true;
        plain.push_str(group);
    }
    if !digits(first)
        || (first.len() > 1 && first.starts_with('0'))
        || (grouped && (first.len() > 3 || first.starts_with('0')))
        || cents.len() != 2
        || !digits(cents)
    {
        return None;
    }
    let magnitude = plain
        .parse::<i64>()
        .ok()?
        .checked_mul(100)?
        .checked_add(cents.parse::<i64>().ok()?)?;
    if negative && magnitude == 0 {
        return None;
    }
    Some(if negative { -magnitude } else { magnitude })
}

/// The value given and the value eBIRForms holds are the same: as amounts
/// when both are amounts ([`shown_amount`]), else as text.
pub fn same_value(given: &str, held: &str) -> bool {
    match (shown_amount(given), shown_amount(held)) {
        (Some(given), Some(held)) => given == held,
        _ => given.trim() == held.trim(),
    }
}

/// A `source_ref`: which hand-off in which mini app the values came from.
/// `{plugin, handoff, inputs_hash}`, all required, nothing else.
pub fn check_reference(value: &Value) -> Result<Map<String, Value>, String> {
    let Some(map) = value.as_object() else {
        return Err("`source_ref` must be an object {plugin, handoff, inputs_hash}".into());
    };
    if let Some(key) = map
        .keys()
        .find(|key| !["plugin", "handoff", "inputs_hash"].contains(&key.as_str()))
    {
        return Err(format!(
            "`source_ref` has an unknown field `{key}` (allowed: plugin, handoff, inputs_hash)"
        ));
    }
    let text = |key: &str| map.get(key).and_then(Value::as_str).unwrap_or_default();
    let plugin = text("plugin");
    if !(3..=64).contains(&plugin.len())
        || !plugin.contains('.')
        || !plugin
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
    {
        return Err("`source_ref.plugin` must be a plugin id like app.buwiz.books".into());
    }
    let handoff = text("handoff");
    if !(1..=64).contains(&handoff.len())
        || !handoff
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
    {
        return Err(
            "`source_ref.handoff` must be the hand-off's id (1-64 letters, digits, . _ : -)".into(),
        );
    }
    let hash = text("inputs_hash");
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("`source_ref.inputs_hash` must be 64 lowercase hex digits".into());
    }
    Ok(map.clone())
}

/// The sources of a data folder's returns.
pub struct Sources {
    dir: PathBuf,
    key: OnceLock<[u8; 32]>,
}

impl Sources {
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            key: OnceLock::new(),
        }
    }

    /// The folder's lock, held until the file it answers is dropped.
    fn lock(&self) -> std::io::Result<File> {
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(LOCK_FILE))?;
        lock.lock()?;
        Ok(lock)
    }

    /// The key file's key: none when it is missing or empty.
    fn read_key(&self) -> std::io::Result<Option<[u8; 32]>> {
        let text = match fs::read_to_string(self.dir.join(KEY_FILE)) {
            Ok(text) if text.trim().is_empty() => return Ok(None),
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut key = [0u8; 32];
        hex::decode_to_slice(text.trim(), &mut key).map_err(|_| {
            std::io::Error::new(
                ErrorKind::InvalidData,
                "the sources key is not 64 hex digits",
            )
        })?;
        Ok(Some(key))
    }

    /// The folder's key: read, or, when `make` and there is none, made under
    /// the lock.
    fn key(&self, make: bool) -> std::io::Result<Option<&[u8; 32]>> {
        if let Some(key) = self.key.get() {
            return Ok(Some(key));
        }
        if let Some(key) = self.read_key()? {
            return Ok(Some(self.key.get_or_init(|| key)));
        }
        if !make {
            return Ok(None);
        }
        let _lock = self.lock()?;
        self.key_locked().map(Some)
    }

    /// With the lock held: the key, made when there is none. A new key is
    /// written to a temp file and renamed into place, so no reader sees it
    /// half written; a key file left empty is made again.
    fn key_locked(&self) -> std::io::Result<&[u8; 32]> {
        if let Some(key) = self.key.get() {
            return Ok(key);
        }
        if let Some(key) = self.read_key()? {
            return Ok(self.key.get_or_init(|| key));
        }
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(std::io::Error::other)?;
        let path = self.dir.join(KEY_FILE);
        let temp = path.with_extension(format!("key.{}.tmp", std::process::id()));
        let mut file = private_file(&temp, false)?;
        file.write_all(hex::encode(key).as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &path)?;
        Ok(self.key.get_or_init(|| key))
    }

    fn mac(key: &[u8; 32], parts: &[&str]) -> String {
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC takes any key length");
        for part in parts {
            mac.update(part.as_bytes());
            mac.update(&[0]);
        }
        hex::encode(mac.finalize().into_bytes())
    }

    /// The HMAC a box's value goes by; `None` when no key can be had.
    pub fn value_mac(&self, key: &str, value: &str) -> Option<String> {
        match self.key(true) {
            Ok(Some(secret)) => Some(Self::mac(secret, &["value", key, value])),
            Ok(None) => None,
            Err(error) => {
                tracing::warn!("the boxes' sources have no key: {error}");
                None
            }
        }
    }

    /// The box still holds the value it was filled with.
    /// `Some(true)` the box still holds the value it was filled with,
    /// `Some(false)` it holds another (or none), `None` it can't be told (no
    /// key for the sources file).
    pub fn holds(&self, source: &Source, key: &str, value: Option<&String>) -> Option<bool> {
        let Some(value) = value else {
            return Some(false);
        };
        self.value_mac(key, value)
            .map(|mac| mac == source.value_mac)
    }

    /// The name a stored return goes by in the file.
    fn return_id(key: &[u8; 32], tin: &str, form: &str, year: i64, stored_period: i64) -> String {
        Self::mac(
            key,
            &[
                "return",
                tin,
                form,
                &year.to_string(),
                &stored_period.to_string(),
            ],
        )
    }

    /// The file's entries: none when it does not exist; an error when it
    /// cannot be read or is not a sources file.
    fn read_all(&self) -> std::io::Result<Map<String, Value>> {
        let text = match fs::read_to_string(self.dir.join(FILE)) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Map::new()),
            Err(error) => return Err(error),
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => Ok(map),
            _ => Err(std::io::Error::new(
                ErrorKind::InvalidData,
                format!("{FILE} is not a sources file; it is left as it is"),
            )),
        }
    }

    /// The boxes' sources a stored return was saved with: none when it has
    /// none (or the folder has no key yet), an error when the file or the key
    /// can't be read.
    pub fn load(
        &self,
        tin: &str,
        form: &str,
        year: i64,
        stored_period: i64,
    ) -> std::io::Result<BTreeMap<String, Source>> {
        let entry = self.key(false).map(|key| {
            key.map(|key| {
                (
                    Self::return_id(key, tin, form, year, stored_period),
                    self.read_all(),
                )
            })
        });
        let (id, all) = match entry? {
            Some((id, all)) => (id, all?),
            None => return Ok(BTreeMap::new()),
        };
        Ok(all
            .get(&id)
            .and_then(|entry| entry.get("boxes"))
            .and_then(Value::as_object)
            .map(|boxes| {
                boxes
                    .iter()
                    .filter_map(|(key, value)| Some((key.clone(), Source::from_stored(value)?)))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Replace a stored return's sources (none removes its entry), under the
    /// folder's lock. The old entry is removed first, in a write of its own:
    /// if writing the new one then fails, no older source comes back on
    /// reopen.
    pub fn save(
        &self,
        tin: &str,
        form: &str,
        year: i64,
        stored_period: i64,
        sources: &BTreeMap<String, Source>,
    ) -> std::io::Result<()> {
        let _lock = self.lock()?;
        let id = Self::return_id(self.key_locked()?, tin, form, year, stored_period);
        let mut all = self.read_all()?;
        if all.remove(&id).is_some() {
            self.write_all(&all, "clear")?;
        }
        if sources.is_empty() {
            return Ok(());
        }
        let boxes: Map<String, Value> = sources
            .iter()
            .map(|(key, source)| (key.clone(), source.stored()))
            .collect();
        let saved_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        all.insert(id, json!({ "saved_ms": saved_ms, "boxes": boxes }));
        prune(&mut all, MAX_RETURNS);
        self.write_all(&all, "save")
    }

    /// With the lock held: the file, through a temp file (named for the
    /// `step`) and a rename.
    fn write_all(&self, all: &Map<String, Value>, step: &str) -> std::io::Result<()> {
        let text = serde_json::to_string(all)?;
        let path = self.dir.join(FILE);
        let temp = path.with_extension(format!("json.{}.{step}.tmp", std::process::id()));
        let mut file = private_file(&temp, false)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &path)
    }
}

/// Keep the `max` most recently saved returns.
fn prune(all: &mut Map<String, Value>, max: usize) {
    if all.len() <= max {
        return;
    }
    let mut by_age: Vec<(u64, String)> = all
        .iter()
        .map(|(id, entry)| (entry["saved_ms"].as_u64().unwrap_or(0), id.clone()))
        .collect();
    by_age.sort();
    let extra = all.len() - max;
    for (_, id) in by_age.into_iter().take(extra) {
        all.remove(&id);
    }
}

/// A file only its owner reads (0600): new only, or replaced.
#[cfg(unix)]
fn private_file(path: &Path, new_only: bool) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut options = fs::OpenOptions::new();
    options.write(true).mode(0o600);
    if new_only {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    options.open(path)
}

#[cfg(not(unix))]
fn private_file(path: &Path, new_only: bool) -> std::io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.write(true);
    if new_only {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIN: &str = "00012345300000";

    fn reference() -> Value {
        json!({ "plugin": "app.buwiz.books", "handoff": "1", "inputs_hash": "a".repeat(64) })
    }

    fn boxes(sources: &Sources, value: &str) -> BTreeMap<String, Source> {
        BTreeMap::from([(
            "txtATCAmt1".to_string(),
            Source {
                label: "document".into(),
                reference: Some(check_reference(&reference()).unwrap()),
                from_card: false,
                value_mac: sources.value_mac("txtATCAmt1", value).unwrap(),
            },
        )])
    }

    #[test]
    fn a_reference_is_exactly_plugin_handoff_and_hash() {
        assert!(check_reference(&reference()).is_ok());
        let mut extra = reference();
        extra["tin"] = json!("x");
        assert!(check_reference(&extra).is_err());
        let mut short = reference();
        short["inputs_hash"] = json!("abc");
        assert!(check_reference(&short).is_err());
        let mut upper = reference();
        upper["inputs_hash"] = json!("A".repeat(64));
        assert!(check_reference(&upper).is_err());
        let mut plugin = reference();
        plugin["plugin"] = json!("Books");
        assert!(check_reference(&plugin).is_err());
        assert!(check_reference(&json!("app.buwiz.books")).is_err());
    }

    #[test]
    fn amounts_read_as_the_shared_vectors_say() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/books-handoff/amounts.json"
        );
        let vectors: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        let vectors = vectors["vectors"].as_array().unwrap();
        assert!(vectors.len() > 20);
        for v in vectors {
            let shown = v["shown"].as_str().unwrap();
            assert_eq!(shown_amount(shown), v["centavos"].as_i64(), "{shown:?}");
        }
    }

    #[test]
    fn amounts_compare_as_a_form_shows_them() {
        for (given, held) in [
            ("290000.00", "290,000.00"),
            ("10000.00", "10,000"),
            ("0.00", "0"),
            ("1", "1"),
        ] {
            assert!(same_value(given, held), "{given} {held}");
        }
        for (given, held) in [
            ("10000.50", "10,000"),
            ("1", "2"),
            ("1,20,000.00", "120000.00"),
            ("true", "false"),
        ] {
            assert!(!same_value(given, held), "{given} {held}");
        }
        assert_eq!(shown_amount("1,200,000"), Some(120_000_000));
        assert_eq!(shown_amount("-2,808.40"), Some(-280_840));
        for bad in ["01/15/2026", "12,0000.00", "0,100", "1.5", "", "-0", "007"] {
            assert_eq!(shown_amount(bad), None, "{bad}");
        }
    }

    #[test]
    fn sources_round_trip_keyed_and_a_changed_value_has_none() {
        let dir = tempfile::tempdir().unwrap();
        let sources = Sources::in_dir(dir.path());
        assert!(
            sources.load(TIN, "2551Q", 2026, 3).unwrap().is_empty(),
            "no key yet, no sources"
        );
        let kept = boxes(&sources, "290,000.00");
        sources.save(TIN, "2551Q", 2026, 3, &kept).unwrap();
        // a new process: the same key from the file
        let again = Sources::in_dir(dir.path());
        let loaded = again.load(TIN, "2551Q", 2026, 3).unwrap();
        assert_eq!(loaded, kept);
        let source = &loaded["txtATCAmt1"];
        assert_eq!(
            again.holds(source, "txtATCAmt1", Some(&"290,000.00".to_string())),
            Some(true)
        );
        assert_eq!(
            again.holds(source, "txtATCAmt1", Some(&"1.00".to_string())),
            Some(false)
        );
        assert_eq!(again.holds(source, "txtATCAmt1", None), Some(false));
        // nothing in the file names the return or the value without the key
        let text = fs::read_to_string(dir.path().join(FILE)).unwrap();
        let plain = |parts: &[&str]| {
            use sha2::Digest as _;
            hex::encode(sha2::Sha256::digest(parts.join("\0").as_bytes()))
        };
        for leak in [
            "000123453",
            "290",
            &plain(&[TIN, "2551Q", "2026", "3"]),
            &plain(&["txtATCAmt1", "290,000.00"]),
        ] {
            assert!(!text.contains(leak), "{leak} in {text}");
        }
        // another key, another name: the file is useless without its key
        let other_dir = tempfile::tempdir().unwrap();
        let other = Sources::in_dir(other_dir.path());
        assert_ne!(
            other.value_mac("txtATCAmt1", "290,000.00"),
            sources.value_mac("txtATCAmt1", "290,000.00")
        );
        // another return's entry is kept; an empty save removes this one
        again.save(TIN, "2551Q", 2026, 2, &kept).unwrap();
        again.save(TIN, "2551Q", 2026, 3, &BTreeMap::new()).unwrap();
        assert!(again.load(TIN, "2551Q", 2026, 3).unwrap().is_empty());
        assert_eq!(again.load(TIN, "2551Q", 2026, 2).unwrap().len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            for file in [FILE, KEY_FILE] {
                let mode = fs::metadata(dir.path().join(file))
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o600, "{file}");
            }
        }
    }

    #[test]
    fn an_empty_key_file_is_made_again() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(KEY_FILE), "").unwrap();
        let sources = Sources::in_dir(dir.path());
        assert!(sources.load(TIN, "2551Q", 2026, 3).unwrap().is_empty());
        let kept = boxes(&sources, "1.00");
        sources.save(TIN, "2551Q", 2026, 3, &kept).unwrap();
        let key = fs::read_to_string(dir.path().join(KEY_FILE)).unwrap();
        assert_eq!(key.trim().len(), 64);
        assert_eq!(
            Sources::in_dir(dir.path())
                .load(TIN, "2551Q", 2026, 3)
                .unwrap(),
            kept
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_save_leaves_no_old_source() {
        let dir = tempfile::tempdir().unwrap();
        let sources = Sources::in_dir(dir.path());
        let kept = boxes(&sources, "1.00");
        sources.save(TIN, "2551Q", 2026, 3, &kept).unwrap();
        // writing the new entry fails (its temp file can't be made)
        let temp = dir
            .path()
            .join(FILE)
            .with_extension(format!("json.{}.save.tmp", std::process::id()));
        fs::create_dir(&temp).unwrap();
        assert!(
            sources
                .save(TIN, "2551Q", 2026, 3, &boxes(&sources, "2.00"))
                .is_err()
        );
        // the old entry went first, so nothing older comes back on reopen
        assert!(
            Sources::in_dir(dir.path())
                .load(TIN, "2551Q", 2026, 3)
                .unwrap()
                .is_empty()
        );
        fs::remove_dir(&temp).unwrap();
    }

    #[test]
    fn an_unreadable_file_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let sources = Sources::in_dir(dir.path());
        let kept = boxes(&sources, "1.00");
        fs::write(dir.path().join(FILE), "{not json").unwrap();
        assert!(sources.save(TIN, "2551Q", 2026, 3, &kept).is_err());
        assert_eq!(
            fs::read_to_string(dir.path().join(FILE)).unwrap(),
            "{not json"
        );
        assert!(
            sources.load(TIN, "2551Q", 2026, 3).is_err(),
            "an unreadable file is an error, not none"
        );
    }

    #[test]
    fn concurrent_saves_keep_every_return() {
        let dir = tempfile::tempdir().unwrap();
        // one key for all, as the processes would read it
        let first = Sources::in_dir(dir.path());
        let kept = boxes(&first, "1.00");
        let threads: Vec<_> = (1..=8)
            .map(|period| {
                let dir = dir.path().to_path_buf();
                let kept = kept.clone();
                std::thread::spawn(move || {
                    let sources = Sources::in_dir(&dir);
                    for _ in 0..10 {
                        sources.save(TIN, "1601C", 2026, period, &kept).unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        for period in 1..=8 {
            assert_eq!(
                first.load(TIN, "1601C", 2026, period).unwrap().len(),
                1,
                "{period}"
            );
        }
    }

    #[test]
    fn the_file_keeps_the_most_recent_returns() {
        let mut all = Map::new();
        for (id, saved_ms) in [("a", 30), ("b", 10), ("c", 20), ("d", 40)] {
            all.insert(id.into(), json!({ "saved_ms": saved_ms, "boxes": {} }));
        }
        prune(&mut all, 2);
        assert_eq!(all.keys().collect::<Vec<_>>(), ["a", "d"]);
        prune(&mut all, 5);
        assert_eq!(all.len(), 2);
    }
}
