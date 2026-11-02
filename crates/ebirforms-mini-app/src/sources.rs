//! Where each box of a saved return got its value: the `source` (and the
//! optional `source_ref`) that `ebirforms_form_fill` was given.
//!
//! eBIRForms' draft has no room for this, so the mini app keeps it in its own
//! data folder, in `fill-sources.json` (mode 0600), one entry per stored
//! return. A return is named by a SHA-256 of its taxpayer, form, year and
//! stored period, so the file holds no TIN. Each box keeps a SHA-256 of the
//! value it was filled with, not the value: a box whose value has changed
//! since (the person typed over it in the Forms app) has no source any more.
//!
//! Several processes may run at once (the Bot respawns this one back to
//! back), so a save re-reads the file and replaces only its own return's
//! entry before writing it back through a temp file and a rename.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// The file in the data folder.
pub const FILE: &str = "fill-sources.json";

/// The labels `ebirforms_form_fill` takes for `source`.
pub const LABELS: [&str; 5] = ["profile", "past_return", "document", "ai", "user"];

/// What one box was filled from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub label: String,
    /// The `source_ref` the fill carried, already checked.
    pub reference: Option<Map<String, Value>>,
    /// True when the person's answer on a card came through the assistant.
    pub from_card: bool,
    /// SHA-256 of `key \0 value` as eBIRForms holds the value after the fill.
    pub value_sha256: String,
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
        out.insert("value_sha256".into(), json!(self.value_sha256));
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
            value_sha256: value.get("value_sha256")?.as_str()?.to_string(),
        })
    }

    /// Still the box's source: the box holds the value it was filled with.
    pub fn holds(&self, key: &str, value: Option<&String>) -> bool {
        value.is_some_and(|value| value_sha256(key, value) == self.value_sha256)
    }
}

/// SHA-256 of `key \0 value`, lowercase hex.
pub fn value_sha256(key: &str, value: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(key.as_bytes());
    hash.update([0]);
    hash.update(value.as_bytes());
    hex::encode(hash.finalize())
}

/// The name a stored return goes by in the file.
pub fn return_id(tin: &str, form: &str, year: i64, stored_period: i64) -> String {
    let mut hash = Sha256::new();
    hash.update(format!("{tin}\0{form}\0{year}\0{stored_period}").as_bytes());
    hex::encode(hash.finalize())
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

/// The sources file in a data folder.
pub struct Sources {
    path: PathBuf,
}

impl Sources {
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            path: dir.join(FILE),
        }
    }

    fn read_all(&self) -> Map<String, Value> {
        fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| match value {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// The boxes' sources a stored return was saved with.
    pub fn load(&self, return_id: &str) -> BTreeMap<String, Source> {
        let all = self.read_all();
        let Some(Value::Object(boxes)) = all.get(return_id) else {
            return BTreeMap::new();
        };
        boxes
            .iter()
            .filter_map(|(key, value)| Some((key.clone(), Source::from_stored(value)?)))
            .collect()
    }

    /// Replace a stored return's sources (none removes its entry).
    pub fn save(&self, return_id: &str, sources: &BTreeMap<String, Source>) -> std::io::Result<()> {
        let mut all = self.read_all();
        if sources.is_empty() {
            if all.remove(return_id).is_none() {
                return Ok(());
            }
        } else {
            let boxes: Map<String, Value> = sources
                .iter()
                .map(|(key, source)| (key.clone(), source.stored()))
                .collect();
            all.insert(return_id.to_string(), Value::Object(boxes));
        }
        let text = serde_json::to_string(&Value::Object(all))?;
        let temp = self
            .path
            .with_extension(format!("json.{}.tmp", std::process::id()));
        let mut file = private_file(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &self.path)
    }
}

#[cfg(unix)]
fn private_file(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> std::io::Result<fs::File> {
    fs::File::create(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference() -> Value {
        json!({ "plugin": "app.buwiz.books", "handoff": "1", "inputs_hash": "a".repeat(64) })
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
    fn sources_round_trip_and_a_changed_value_has_none() {
        let dir = tempfile::tempdir().unwrap();
        let sources = Sources::in_dir(dir.path());
        let id = return_id("00012345300000", "2551Q", 2026, 3);
        let mut boxes = BTreeMap::new();
        boxes.insert(
            "txtATCAmt1".to_string(),
            Source {
                label: "document".into(),
                reference: Some(check_reference(&reference()).unwrap()),
                from_card: false,
                value_sha256: value_sha256("txtATCAmt1", "290,000.00"),
            },
        );
        sources.save(&id, &boxes).unwrap();
        let text = fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(
            !text.contains("000123453") && !text.contains("290"),
            "{text}"
        );
        let loaded = sources.load(&id);
        assert_eq!(loaded, boxes);
        let source = &loaded["txtATCAmt1"];
        assert!(source.holds("txtATCAmt1", Some(&"290,000.00".to_string())));
        assert!(!source.holds("txtATCAmt1", Some(&"1.00".to_string())));
        assert!(!source.holds("txtATCAmt1", None));
        // another return's entry is kept; an empty save removes this one
        let other = return_id("00012345300000", "2551Q", 2026, 2);
        sources.save(&other, &boxes).unwrap();
        sources.save(&id, &BTreeMap::new()).unwrap();
        assert!(sources.load(&id).is_empty());
        assert_eq!(sources.load(&other).len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(dir.path().join(FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
