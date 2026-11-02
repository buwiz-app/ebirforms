//! `ebirforms-mini-app import-legacy`: a one-time copy of the retired BIR
//! app's taxpayers into this mini app's own database.
//!
//! The old app (dev.goldcoders.bir) kept eBIRForms' database (`bir_data.db`,
//! SQLCipher) with its key in a file the broker handed it through
//! eBIRForms' `BUWIZ_DB_KEY_FILE` / `BUWIZ_DB_KEY` (64 hex digits, a newline
//! after them; eBIRForms trims it). Both databases are eBIRForms' own, so the
//! copy goes through eBIRForms' own paths, never SQL of ours:
//!
//! 1. **Stage** ([`stage`]): refuse while eBIRForms has the target open (its
//!    `.open.lock` or `.worker.lock`); copy the old database and its `-wal`
//!    and `-shm` into a fresh 0700 folder, so the original is only ever
//!    read; open the copy in a child process of this binary with the old
//!    key ([`export_child`]: eBIRForms migrates the copy and writes every
//!    profile's archive, `BirApp::export_backup`, and each branch's yearly
//!    forms); back the target up (or, for a dry run, copy it to a
//!    throwaway target).
//! 2. **Import** ([`import`]): open the target with its own key and restore,
//!    one taxpayer at a time (`BirApp::restore_backup`), the profiles it
//!    does not have yet, then their yearly forms. A branch already there is
//!    skipped and counted, never overwritten.
//!
//! The key lives in the environment of the child only, so two databases
//! with two keys never share a process. Nothing printed or logged names a
//! taxpayer, a TIN, an amount or the key: [`Report`] is counts.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use bir_app_core::BirApp;
use serde_json::{Value, json};

use crate::owner::{self, DATABASE_FILE, KEY_FILE};

/// What a held target answers.
pub const IN_USE: &str = "pause eBIRForms in Buwiz Bot first: it has its database open";

/// The child's command: open the old copy and write its archive.
pub const EXPORT_COMMAND: &str = "import-legacy-export";

const USAGE: &str = "usage: ebirforms-mini-app import-legacy --from <old bir_data.db> --key-file <key> [--data-dir <dir>] [--dry-run [--explain]] [--rebuild-stale-backfill]";

/// The child's exit codes.
const EXIT_WRONG_KEY: u8 = 3;
const EXIT_UNREADABLE: u8 = 4;

/// The command line.
#[derive(Debug, Clone)]
pub struct Options {
    pub from: PathBuf,
    pub key_file: PathBuf,
    pub data_dir: PathBuf,
    pub dry_run: bool,
    /// A dry run also says why each refused branch was refused, by index.
    pub explain: bool,
    /// A taxpayer refused only because its Confirmed migration backfill no
    /// longer matches its profile is imported with the backfill made again.
    pub rebuild: bool,
}

impl Options {
    pub fn parse(args: &[&str]) -> Result<Self, String> {
        let (mut from, mut key_file, mut data_dir) = (None, None, None);
        let (mut dry_run, mut explain, mut rebuild) = (false, false, false);
        let mut rest = args.iter();
        while let Some(arg) = rest.next() {
            let mut value = || {
                rest.next()
                    .map(PathBuf::from)
                    .ok_or_else(|| format!("{arg} needs a value\n{USAGE}"))
            };
            match *arg {
                "--from" => from = Some(value()?),
                "--key-file" => key_file = Some(value()?),
                "--data-dir" => data_dir = Some(value()?),
                "--dry-run" => dry_run = true,
                "--explain" => explain = true,
                "--rebuild-stale-backfill" => rebuild = true,
                other => return Err(format!("unknown argument `{other}`\n{USAGE}")),
            }
        }
        if explain && !dry_run {
            return Err(format!("--explain goes with --dry-run\n{USAGE}"));
        }
        Ok(Self {
            from: from.ok_or_else(|| format!("--from is required\n{USAGE}"))?,
            key_file: key_file.ok_or_else(|| format!("--key-file is required\n{USAGE}"))?,
            data_dir: data_dir.unwrap_or_else(owner::data_dir),
            dry_run,
            explain,
            rebuild,
        })
    }
}

/// A 0700 folder of this run's, removed with everything in it when dropped.
struct WorkDir(PathBuf);

impl WorkDir {
    fn new() -> Result<Self, String> {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).map_err(|error| format!("no random bytes: {error}"))?;
        let dir = std::env::temp_dir().join(format!("ebirforms-import-{}", hex::encode(bytes)));
        fs::create_dir(&dir)
            .map_err(|error| format!("the work folder could not be made: {error}"))?;
        owner::restrict(&dir, 0o700)?;
        Ok(Self(dir))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One branch's profile in the old archive.
#[derive(Debug, Clone)]
struct Profile {
    /// Its entries' folder in the archive (`Profiles/<tin>/`).
    base: String,
    root: String,
    branch: u32,
    returns: usize,
    drafts: usize,
}

/// What [`stage`] made ready for [`import`].
pub struct Staged {
    work: WorkDir,
    target: PathBuf,
    archive: PathBuf,
    profiles: Vec<Profile>,
    /// `(root, branch) -> [(year, forms)]`, from the old database.
    yearly: BTreeMap<(String, u32), Vec<(i64, Value)>>,
    pub backup: Option<PathBuf>,
    pub dry_run: bool,
    pub explain: bool,
    pub rebuild: bool,
}

impl Staged {
    /// The data folder to import into: the real one, or a dry run's copy.
    pub fn target_dir(&self) -> &Path {
        &self.target
    }
}

/// The counts a run prints: no names, TINs, amounts or contents.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub taxpayers: usize,
    pub taxpayers_there: usize,
    pub branches: usize,
    pub branches_there: usize,
    pub returns: usize,
    pub drafts: usize,
    pub years: usize,
    /// Taxpayers none of whose new branches came across.
    pub taxpayers_refused: usize,
    /// Refusal kind (`refused_by_ebirforms:<why>`) -> branches.
    pub failed: BTreeMap<String, usize>,
    /// `--explain`: one line per refused branch, by index only.
    pub explained: Vec<String>,
    /// `--rebuild-stale-backfill`: taxpayers whose stale migration backfill
    /// eBIRForms made again from the profile.
    pub backfills_rebuilt: usize,
}

impl Report {
    pub fn lines(&self, staged: &Staged) -> Vec<String> {
        let verb = if staged.dry_run {
            "to import"
        } else {
            "imported"
        };
        let mut out = vec![if staged.dry_run {
            "eBIRForms legacy import: dry run (nothing was written to eBIRForms' data)".to_string()
        } else {
            "eBIRForms legacy import: done".to_string()
        }];
        match &staged.backup {
            Some(path) => out.push(format!("backup: {}", path.display())),
            None if !staged.dry_run => {
                out.push("backup: none needed (the target held no data yet)".into())
            }
            None => {}
        }
        out.push(format!(
            "taxpayers: {} {verb}, {} already there, {} refused",
            self.taxpayers, self.taxpayers_there, self.taxpayers_refused
        ));
        out.push(format!(
            "branches: {} {verb}, {} already there",
            self.branches, self.branches_there
        ));
        out.push(format!("filed returns: {} {verb}", self.returns));
        out.push(format!("saved returns and drafts: {} {verb}", self.drafts));
        out.push(format!("tax years with their forms: {} {verb}", self.years));
        if staged.rebuild {
            out.push(format!(
                "taxpayers whose stale registration backfill was rebuilt: {}",
                self.backfills_rebuilt
            ));
        }
        let failed: usize = self.failed.values().sum();
        if failed == 0 {
            out.push("failed: 0".into());
        } else {
            let why: Vec<String> = self
                .failed
                .iter()
                .map(|(reason, n)| format!("{n} {reason}"))
                .collect();
            out.push(format!("failed: {failed} branches ({})", why.join(", ")));
        }
        out.extend(self.explained.iter().cloned());
        out
    }
}

fn sidecar(db: &Path, suffix: &str) -> PathBuf {
    let mut raw = db.as_os_str().to_os_string();
    raw.push(suffix);
    PathBuf::from(raw)
}

/// Whether a process holds eBIRForms' open or worker lock on `db`.
pub fn in_use(db: &Path) -> Result<bool, String> {
    for suffix in [".open.lock", ".worker.lock"] {
        let lock = sidecar(db, suffix);
        if !lock.exists() {
            continue;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock)
            .map_err(|error| format!("eBIRForms' lock file could not be read: {error}"))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(true),
            Err(TryLockError::Error(error)) => {
                return Err(format!(
                    "eBIRForms' lock file could not be checked: {error}"
                ));
            }
        }
    }
    Ok(false)
}

/// Copy `db` and its `-wal` and `-shm` into `dir` under `name`.
fn copy_database(db: &Path, dir: &Path, name: &str) -> Result<PathBuf, String> {
    let to = dir.join(name);
    for suffix in ["", "-wal", "-shm"] {
        let from = sidecar(db, suffix);
        if from.is_file() {
            fs::copy(&from, sidecar(&to, suffix))
                .map_err(|error| format!("a database file could not be copied: {error}"))?;
        }
    }
    Ok(to)
}

/// Copy every file under `from` to `to` (made 0700).
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to)
        .map_err(|error| format!("the backup folder could not be made: {error}"))?;
    owner::restrict(to, 0o700)?;
    for entry in
        fs::read_dir(from).map_err(|error| format!("the data folder could not be read: {error}"))?
    {
        let entry = entry.map_err(|error| format!("the data folder could not be read: {error}"))?;
        let path = entry.path();
        let into = to.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &into)?;
        } else if path.is_file() {
            fs::copy(&path, &into)
                .map_err(|error| format!("a file could not be backed up: {error}"))?;
        }
    }
    Ok(())
}

/// The 9-digit root and the branch number of a TIN as eBIRForms writes it
/// (dashes or not; a 3- or 5-digit branch code).
fn tin_key(tin: &str) -> Option<(String, u32)> {
    let digits: String = tin.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 9 {
        return None;
    }
    let branch = digits[9..].parse::<u32>().unwrap_or(0);
    Some((digits[..9].to_string(), branch))
}

fn json_len(archive: &mut zip::ZipArchive<File>, name: &str) -> usize {
    let Ok(mut file) = archive.by_name(name) else {
        return 0;
    };
    let mut text = String::new();
    if file.read_to_string(&mut text).is_err() {
        return 0;
    }
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|value| value.as_array().map(Vec::len))
        .unwrap_or(0)
}

/// The profiles in the archive `export_backup` wrote.
fn read_profiles(path: &Path) -> Result<Vec<Profile>, String> {
    let file = File::open(path).map_err(|_| "the old database's export is missing".to_string())?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|_| "the old database's export is unreadable".to_string())?;
    let bases: Vec<String> = archive
        .file_names()
        .filter_map(|name| name.strip_suffix("profile.json").map(str::to_string))
        .collect();
    let mut profiles = Vec::new();
    for base in bases {
        let Some((root, branch)) = base
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .and_then(tin_key)
        else {
            continue;
        };
        let returns = json_len(&mut archive, &format!("{base}submissions.json"));
        let drafts = json_len(&mut archive, &format!("{base}drafts.json"));
        profiles.push(Profile {
            base,
            root,
            branch,
            returns,
            drafts,
        });
    }
    Ok(profiles)
}

/// Stage a run: everything before the target is opened. Writes to the
/// target only the backup (beside it, not in it).
pub fn stage(options: &Options) -> Result<Staged, String> {
    if !options.from.is_file() {
        return Err("--from names no file".into());
    }
    let key = fs::read_to_string(&options.key_file)
        .map_err(|_| "the key file could not be read".to_string())?;
    let key = key.trim();
    if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("the key file does not hold a 64-hex-digit database key".into());
    }
    let target_db = options.data_dir.join(DATABASE_FILE);
    if in_use(&target_db)? {
        return Err(IN_USE.into());
    }
    let work = WorkDir::new()?;
    let source = work.0.join("source");
    fs::create_dir(&source)
        .map_err(|error| format!("the work folder could not be made: {error}"))?;
    let copy = copy_database(&options.from, &source, "bir_data.db")?;

    // the old copy, opened by a child of this binary with the old key
    let archive = work.0.join("profiles.zip");
    let yearly_file = work.0.join("yearly.json");
    let exe = std::env::current_exe()
        .map_err(|error| format!("this program's path is unknown: {error}"))?;
    let status = Command::new(exe)
        .args([EXPORT_COMMAND, "--db"])
        .arg(&copy)
        .arg("--out")
        .arg(&work.0)
        .env("BUWIZ_NO_KEYCHAIN", "1")
        .env("BUWIZ_DB_KEY_FILE", &options.key_file)
        .env_remove("BUWIZ_DB_KEY")
        .env("BUWIZ_DATABASE_PATH", &copy)
        .env("BUWIZ_LOG_DIR", work.0.join("logs"))
        .env_remove(owner::DATA_DIR_ENV)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|error| format!("the old database could not be read: {error}"))?;
    match status.code() {
        Some(0) => {}
        Some(code) if code == i32::from(EXIT_WRONG_KEY) => {
            return Err("the key does not open the old database (wrong key file)".into());
        }
        _ => {
            return Err(
                "the old database could not be read (not an eBIRForms database, or damaged)".into(),
            );
        }
    }
    let profiles = read_profiles(&archive)?;
    let mut yearly: BTreeMap<(String, u32), Vec<(i64, Value)>> = BTreeMap::new();
    if let Ok(text) = fs::read_to_string(&yearly_file) {
        for row in serde_json::from_str::<Vec<Value>>(&text).unwrap_or_default() {
            let (Some(key), Some(year)) =
                (row["tin"].as_str().and_then(tin_key), row["year"].as_i64())
            else {
                continue;
            };
            yearly
                .entry(key)
                .or_default()
                .push((year, row["forms"].clone()));
        }
    }

    // the target: backed up first; a dry run imports into a copy
    let (target, backup) = if options.dry_run {
        let target = work.0.join("target");
        fs::create_dir(&target)
            .map_err(|error| format!("the work folder could not be made: {error}"))?;
        if target_db.is_file() {
            copy_database(&target_db, &target, DATABASE_FILE)?;
        }
        let key = options.data_dir.join(KEY_FILE);
        if key.is_file() {
            fs::copy(&key, target.join(KEY_FILE))
                .map_err(|error| format!("the target's key could not be copied: {error}"))?;
        }
        (target, None)
    } else if options.data_dir.is_dir()
        && fs::read_dir(&options.data_dir).is_ok_and(|mut entries| entries.next().is_some())
    {
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        let mut name = options.data_dir.as_os_str().to_os_string();
        name.push(format!(".backup-{stamp}"));
        let backup = PathBuf::from(name);
        copy_tree(&options.data_dir, &backup)?;
        (options.data_dir.clone(), Some(backup))
    } else {
        (options.data_dir.clone(), None)
    };
    Ok(Staged {
        work,
        target,
        archive,
        profiles,
        yearly,
        backup,
        dry_run: options.dry_run,
        explain: options.explain,
        rebuild: options.rebuild,
    })
}

/// One taxpayer's profiles of `archive`, as an archive of their own.
///
/// With `drop_backfill`, each `profile.json` goes without its migration
/// backfill versions (`--rebuild-stale-backfill`): eBIRForms then makes the
/// backfill again from the profile as it is (`ensure_profile_version_ledger`).
fn taxpayer_archive(
    archive: &Path,
    bases: &[&str],
    to: &Path,
    drop_backfill: bool,
) -> Result<(), String> {
    let file = File::open(archive).map_err(|error| error.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    let out = File::create(to).map_err(|error| error.to_string())?;
    let mut writer = zip::ZipWriter::new(out);
    for i in 0..archive.len() {
        let name = archive.name_for_index(i).unwrap_or_default().to_string();
        if !bases.iter().any(|base| name.starts_with(base)) {
            continue;
        }
        if drop_backfill && name.ends_with("profile.json") {
            let mut text = String::new();
            archive
                .by_index(i)
                .map_err(|error| error.to_string())?
                .read_to_string(&mut text)
                .map_err(|error| error.to_string())?;
            let mut profile: Value =
                serde_json::from_str(&text).map_err(|error| error.to_string())?;
            if let Some(versions) = profile["profile_versions"].as_array_mut() {
                versions.retain(|version| version["source"] != "MigrationBackfill");
            }
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer
                .start_file(name, options)
                .map_err(|error| error.to_string())?;
            writer
                .write_all(profile.to_string().as_bytes())
                .map_err(|error| error.to_string())?;
            continue;
        }
        let entry = archive.by_index_raw(i).map_err(|error| error.to_string())?;
        writer
            .raw_copy_file(entry)
            .map_err(|error| error.to_string())?;
    }
    writer.finish().map_err(|error| error.to_string())?;
    Ok(())
}

/// Restore `bases` of `archive` (one taxpayer's profiles) through
/// eBIRForms, by way of an archive of their own at `one`; a refusal as its
/// kind ([`refusal_kind`]).
///
/// A taxpayer refused only for a stale migration backfill is tried again
/// with it rebuilt when `rebuild` holds; `Ok(true)` then.
fn restore(
    app: &BirApp,
    archive: &Path,
    bases: &[&str],
    one: &Path,
    rebuild: bool,
) -> Result<bool, String> {
    match restore_once(app, archive, bases, one, false) {
        Err(kind)
            if rebuild
                && kind.ends_with(":confirmed_version_needs_review:StaleMigrationBackfill") =>
        {
            restore_once(app, archive, bases, one, true).map(|()| true)
        }
        other => other.map(|()| false),
    }
}

fn restore_once(
    app: &BirApp,
    archive: &Path,
    bases: &[&str],
    one: &Path,
    drop_backfill: bool,
) -> Result<(), String> {
    let result = taxpayer_archive(archive, bases, one, drop_backfill)
        .map_err(|_| "unreadable_archive".to_string())
        .and_then(|()| {
            app.restore_backup(one).map(drop).map_err(|error| {
                let kind = refusal_kind(&error.to_string());
                // which confirmed versions the rule is about: their sources,
                // eBIRForms' own enum names
                let kind = if kind.starts_with("confirmed_version") {
                    format!("{kind}:{}", confirmed_sources(archive, bases))
                } else {
                    kind
                };
                format!("refused_by_ebirforms:{kind}")
            })
        });
    let _ = fs::remove_file(one);
    result
}

/// The sources of the confirmed profile versions in `bases`' profiles
/// (`MigrationBackfill`, `ManualCor`, ...), `+`-joined; a migration
/// backfill that no longer matches its profile reads `StaleMigrationBackfill`.
fn confirmed_sources(archive: &Path, bases: &[&str]) -> String {
    let Ok(file) = File::open(archive) else {
        return "unknown".into();
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return "unknown".into();
    };
    let mut sources = std::collections::BTreeSet::new();
    for base in bases {
        let Ok(mut entry) = archive.by_name(&format!("{base}profile.json")) else {
            continue;
        };
        let mut text = String::new();
        if entry.read_to_string(&mut text).is_err() {
            continue;
        }
        let Ok(profile) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        for version in profile["profile_versions"].as_array().into_iter().flatten() {
            if version["status"] != "Confirmed" {
                continue;
            }
            let source = version["source"].as_str().unwrap_or("unknown");
            let source = if source == "MigrationBackfill" && !backfill_matches(&profile, version) {
                "StaleMigrationBackfill"
            } else {
                source
            };
            if source.chars().all(|c| c.is_ascii_alphanumeric()) {
                sources.insert(source.to_string());
            }
        }
    }
    if sources.is_empty() {
        "none".into()
    } else {
        sources.into_iter().collect::<Vec<_>>().join("+")
    }
}

/// Whether a migration backfill still holds its profile's registration
/// facts (the ones eBIRForms compares, `from_profile_backfill`): name,
/// address, RDO, type and start date.
fn backfill_matches(profile: &Value, version: &Value) -> bool {
    let cor = &version["cor"];
    cor["registered_name"] == profile["full_name"]
        && cor["registered_address"] == profile["registered_address"]
        && cor["rdo_code"] == profile["rdo_code"]
        && version["taxpayer_type"] == profile["taxpayer_type"]
        && version["effective_from"] == profile["business_start_date"]
}

/// What an eBIRForms refusal is about, as a kind that names no data. The
/// core answers in sentences (`DbError`'s text through the host call), so
/// the kind comes from patterns that are its own words; anything else is
/// `other:` and the text with every digit `#` and every word not in
/// [`SAFE_WORDS`] `…`.
pub fn refusal_kind(message: &str) -> String {
    let text = message.to_lowercase();
    let after = |marker: &str| -> Option<String> {
        let at = text.find(marker)? + marker.len();
        let rest: String = text[at..]
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
            .collect();
        (!rest.is_empty()).then(|| rest.replace(|c: char| c.is_ascii_digit(), "#"))
    };
    let known: &[(&str, &str)] = &[
        (
            "requires an explicit reviewed confirmation plan",
            "confirmed_version_needs_review",
        ),
        (
            "authorizes only one new confirmed",
            "confirmed_version_plan_mismatch",
        ),
        (
            "must have an effective start date",
            "confirmed_version_without_start",
        ),
        (
            "ends before it starts",
            "confirmed_version_ends_before_start",
        ),
        ("already exists", "profile_already_exists"),
        ("no profile.json", "archive_empty"),
        ("foreign key constraint failed", "foreign_key"),
        ("another app is saving", "database_busy"),
        ("zip error", "archive_unreadable"),
    ];
    if text.contains("confirmed profile versions") && text.contains("overlap") {
        return "confirmed_versions_overlap".into();
    }
    if let Some((_, kind)) = known.iter().find(|(pattern, _)| text.contains(pattern)) {
        return (*kind).to_string();
    }
    for (marker, kind) in [
        ("unique constraint failed:", "unique"),
        ("not null constraint failed:", "not_null"),
        ("check constraint failed:", "check"),
        ("no such table:", "missing_table"),
        ("no such column:", "missing_column"),
    ] {
        if text.contains(marker) {
            return match after(marker) {
                Some(what) => format!("{kind}:{what}"),
                None => kind.to_string(),
            };
        }
    }
    if text.contains("serialization error") {
        return format!("archive_json:{}", masked(&text));
    }
    format!("other:{}", masked(&text))
}

/// Words a masked refusal may keep: eBIRForms' own error words and field
/// names, never data.
const SAFE_WORDS: &[&str] = &[
    "a",
    "an",
    "the",
    "is",
    "are",
    "not",
    "no",
    "must",
    "have",
    "has",
    "be",
    "of",
    "for",
    "to",
    "in",
    "on",
    "at",
    "with",
    "and",
    "or",
    "by",
    "missing",
    "invalid",
    "unknown",
    "duplicate",
    "expected",
    "found",
    "field",
    "fields",
    "value",
    "type",
    "string",
    "integer",
    "number",
    "null",
    "map",
    "sequence",
    "struct",
    "variant",
    "line",
    "column",
    "error",
    "other",
    "database",
    "constraint",
    "failed",
    "table",
    "json",
    "data",
    "key",
    "profile",
    "profiles",
    "tin",
    "rdo",
    "rdo_code",
    "zip",
    "zip_code",
    "phone",
    "email",
    "address",
    "registered_address",
    "name",
    "full_name",
    "taxpayer",
    "taxpayer_type",
    "branch",
    "branch_code",
    "code",
    "birth_date",
    "date",
    "year",
    "years",
    "version",
    "versions",
    "confirmed",
    "effective",
    "start",
    "profile_versions",
    "per_year_forms",
    "tax_elections",
    "atc_codes",
    "submissions",
    "drafts",
    "form",
    "forms",
    "period",
    "status",
    "archive",
];

/// `text` with every digit `#` and every word not in [`SAFE_WORDS`] `…`
/// (runs of them one `…`), 120 characters at most.
fn masked(text: &str) -> String {
    let mut out = String::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if word.is_empty() {
            return;
        }
        if SAFE_WORDS.contains(&word.as_str()) {
            out.push_str(word);
        } else if !out.ends_with('…') {
            out.push('…');
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphabetic() || c == '_' {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            if c.is_ascii_digit() {
                if !out.ends_with('#') {
                    out.push('#');
                }
            } else if c.is_alphanumeric() {
                // a letter outside ASCII: part of some word of data
                if !out.ends_with('…') {
                    out.push('…');
                }
            } else if matches!(c, ' ' | ':' | ',' | '.' | '(' | ')' | '-' | '/') {
                out.push(c);
            }
        }
    }
    flush(&mut word, &mut out);
    let mut out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    while out.contains("… …") {
        out = out.replace("… …", "…");
    }
    out.chars().take(120).collect()
}

/// The (root, branch) pairs a database holds, and each one's TIN there.
fn branches_of(app: &BirApp) -> Result<BTreeMap<(String, u32), String>, String> {
    let taxpayers = app
        .list_taxpayers()
        .map_err(|_| "the target's taxpayers could not be read".to_string())?;
    Ok(taxpayers
        .iter()
        .flat_map(|taxpayer| taxpayer.branches.iter())
        .filter_map(|branch| tin_key(&branch.tin).map(|key| (key, branch.tin.clone())))
        .collect())
}

/// Import what [`stage`] read into the target database `db` (its key in
/// the environment: `owner::prepare`).
pub fn import(staged: &Staged, db: &Path) -> Result<Report, String> {
    let app = BirApp::open_at(db)
        .map_err(|_| "eBIRForms' own database could not be opened".to_string())?;
    let mut there = branches_of(&app)?;
    let mut report = Report::default();
    let mut by_root: BTreeMap<&str, Vec<&Profile>> = BTreeMap::new();
    for profile in &staged.profiles {
        by_root
            .entry(profile.root.as_str())
            .or_default()
            .push(profile);
    }
    for (n, (root, profiles)) in by_root.iter().enumerate() {
        let new: Vec<&&Profile> = profiles
            .iter()
            .filter(|profile| !there.contains_key(&(root.to_string(), profile.branch)))
            .collect();
        report.branches_there += profiles.len() - new.len();
        if new.is_empty() {
            report.taxpayers_there += 1;
            continue;
        }
        let bases: Vec<&str> = new.iter().map(|profile| profile.base.as_str()).collect();
        let one = staged.work.0.join(format!("taxpayer-{n}.zip"));
        let restored = restore(&app, &staged.archive, &bases, &one, staged.rebuild);
        if let Ok(true) = restored {
            report.backfills_rebuilt += 1;
        }
        if let Err(kind) = restored {
            report.taxpayers_refused += 1;
            *report.failed.entry(kind.clone()).or_default() += new.len();
            if staged.explain {
                // each branch alone, into the dry run's throwaway target:
                // which one the taxpayer was refused for
                let mut sorted = new.clone();
                sorted.sort_by_key(|profile| profile.branch);
                for (k, profile) in sorted.iter().enumerate() {
                    let alone = staged.work.0.join(format!("taxpayer-{n}-{k}.zip"));
                    let why = match restore(
                        &app,
                        &staged.archive,
                        &[profile.base.as_str()],
                        &alone,
                        false,
                    ) {
                        Ok(_) => format!("imports alone; refused with the others ({kind})"),
                        Err(own) => own,
                    };
                    report.explained.push(format!(
                        "branch #{} of taxpayer #{}: {why}",
                        k + 1,
                        n + 1
                    ));
                }
                there = branches_of(&app)?;
            }
            continue;
        }
        report.taxpayers += 1;
        report.branches += new.len();
        report.returns += new.iter().map(|profile| profile.returns).sum::<usize>();
        report.drafts += new.iter().map(|profile| profile.drafts).sum::<usize>();
        there = branches_of(&app)?;
        for profile in &new {
            let key = (root.to_string(), profile.branch);
            let (Some(tin), Some(years)) = (there.get(&key), staged.yearly.get(&key)) else {
                continue;
            };
            for (year, forms) in years {
                let set = app.call(
                    "set_yearly_forms",
                    vec![json!(tin), json!(year), forms.clone()],
                );
                if set.is_ok() {
                    report.years += 1;
                } else {
                    *report
                        .failed
                        .entry("tax_year_refused".to_string())
                        .or_default() += 1;
                }
            }
        }
    }
    Ok(report)
}

/// Whether an open's error is SQLCipher's answer to a wrong key: eBIRForms'
/// `Encryption`, or SQLite's "file is not a database".
fn wrong_key(error: &str) -> bool {
    error.contains("Encryption key invalid") || error.contains("file is not a database")
}

/// The child: open the old copy (its key in this process's environment,
/// set by [`stage`]), let eBIRForms migrate it, and write every profile's
/// archive and each branch's yearly forms into `out`. Prints nothing.
pub fn export_child(db: &str, out: &str) -> ExitCode {
    let app = match BirApp::open_at(db) {
        Ok(app) => app,
        // SQLCipher reads a wrong key as a file that is no database
        Err(error) if wrong_key(&error.to_string()) => return ExitCode::from(EXIT_WRONG_KEY),
        Err(_) => return ExitCode::from(EXIT_UNREADABLE),
    };
    let out = Path::new(out);
    if app.export_backup(&out.join("profiles.zip")).is_err() {
        return ExitCode::from(EXIT_UNREADABLE);
    }
    let mut yearly = Vec::new();
    for taxpayer in app.list_taxpayers().unwrap_or_default() {
        for branch in &taxpayer.branches {
            let years = app.call("list_tax_years", vec![json!(branch.tin)]).ok();
            for year in years
                .as_ref()
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(year) = year.as_f64().map(|year| year as i64) else {
                    continue;
                };
                if let Ok(forms) = app.call("yearly_forms", vec![json!(branch.tin), json!(year)]) {
                    yearly.push(json!({ "tin": branch.tin, "year": year, "forms": forms }));
                }
            }
        }
    }
    let written = File::create(out.join("yearly.json"))
        .and_then(|mut file| file.write_all(Value::Array(yearly).to_string().as_bytes()));
    if written.is_err() {
        return ExitCode::from(EXIT_UNREADABLE);
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tin_names_its_root_and_branch_however_it_is_written() {
        assert_eq!(tin_key("000-111-222-00000"), Some(("000111222".into(), 0)));
        assert_eq!(tin_key("00011122200001"), Some(("000111222".into(), 1)));
        assert_eq!(tin_key("000111222000"), Some(("000111222".into(), 0)));
        assert_eq!(tin_key("000111222"), Some(("000111222".into(), 0)));
        assert_eq!(tin_key("12345"), None);
    }

    #[test]
    fn the_command_line_needs_a_source_and_a_key() {
        let parsed = Options::parse(&[
            "--from",
            "/x/bir_data.db",
            "--key-file",
            "/x/k",
            "--data-dir",
            "/y",
            "--dry-run",
        ])
        .unwrap();
        assert_eq!(parsed.from, PathBuf::from("/x/bir_data.db"));
        assert!(parsed.dry_run);
        assert!(Options::parse(&["--key-file", "/x/k"]).is_err());
        assert!(Options::parse(&["--from", "/x"]).is_err());
        assert!(Options::parse(&["--from"]).is_err());
        assert!(Options::parse(&["--from", "/x", "--key-file", "/k", "--nope"]).is_err());
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::*;

    #[test]
    fn a_refusal_is_a_kind_that_names_no_data() {
        for (message, kind) in [
            (
                "Other error: A new confirmed COR or manual profile version requires an explicit reviewed confirmation plan; only the exact one-time migration backfill may be generated automatically",
                "confirmed_version_needs_review",
            ),
            (
                "Other error: Confirmed profile version 'Juana Sample 2020' must have an effective start date",
                "confirmed_version_without_start",
            ),
            (
                "Other error: Confirmed profile versions 'A' and 'B' overlap",
                "confirmed_versions_overlap",
            ),
            (
                "Database error: A profile with TIN 00011122200000 already exists",
                "profile_already_exists",
            ),
            (
                "Database error: UNIQUE constraint failed: form_drafts.tin, form_drafts.form_code",
                "unique:form_drafts.tin",
            ),
            (
                "Database error: NOT NULL constraint failed: profiles.data_json",
                "not_null:profiles.data_json",
            ),
            (
                "Database error: FOREIGN KEY constraint failed",
                "foreign_key",
            ),
        ] {
            assert_eq!(refusal_kind(message), kind, "{message}");
        }
        // anything else: digits and unknown words masked
        let other = refusal_kind(
            "Serialization error: unknown variant `Partnersheep` for Juana Sample at line 3 column 120, TIN 000-111-222",
        );
        assert!(other.starts_with("archive_json:"), "{other}");
        for leak in [
            "Juana",
            "Sample",
            "Partnersheep",
            "000",
            "111",
            "222",
            "120",
        ] {
            assert!(
                !other.to_lowercase().contains(&leak.to_lowercase()),
                "{other}"
            );
        }
        assert!(
            other.contains("unknown variant") && other.contains("line #"),
            "{other}"
        );
        let unknown = refusal_kind("Something odd happened to Maria Santos at 1 Rizal St.");
        assert_eq!(unknown, "other:… to … at # ….");
    }
}
