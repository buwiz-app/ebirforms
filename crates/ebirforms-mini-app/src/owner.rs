//! The mini app's own data.
//!
//! The eBIRForms mini app keeps its own taxpayer database in its own folder:
//! `EBIRFORMS_DATA_DIR`, else the platform data folder named after the plugin
//! id (`~/Library/Application Support/app.buwiz.ebirforms` on macOS). It
//! shares nothing with either eBIRForms desktop app and never reads a
//! keychain: the database key is a 0600 file in that folder, made on first
//! run. Old desktop data comes in by export, later.
//!
//! The database's owner lock is taken per write and let go after, because
//! the Bot respawns this process back to back: a second process waits for it
//! briefly, then gets a plain refusal.
//!
//! Nothing is sent from here: sending would need the desktop's keychain
//! entries (bir-submit), so a return can be saved but not queued.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bir_app_core::bir_core::db::{DbError, try_acquire_live_owner_lock};

use crate::manifest::PLUGIN_ID;

/// Names the data folder in place of the platform default.
pub const DATA_DIR_ENV: &str = "EBIRFORMS_DATA_DIR";
/// The database file in the data folder.
pub const DATABASE_FILE: &str = "ebirforms.db";
/// The database key file in the data folder (64 hex digits, mode 0600).
pub const KEY_FILE: &str = "ebirforms.key";

/// What a refused write says while another process holds the database.
pub(crate) const IN_USE: &str =
    "Another eBIRForms mini app process is saving right now: ask again in a moment.";

/// What queueing a return for filing says.
pub(crate) const NO_SENDING: &str = "Sending is not available in the eBIRForms mini app yet: the return is saved here, and nothing was queued or sent.";

/// How long a write waits for the lock.
const WAIT: Duration = Duration::from_millis(1000);
const STEP: Duration = Duration::from_millis(100);

/// The data folder: [`DATA_DIR_ENV`], else the platform's.
pub fn data_dir() -> PathBuf {
    match std::env::var_os(DATA_DIR_ENV) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => platform_data_dir().join(PLUGIN_ID),
    }
}

#[cfg(target_os = "macos")]
fn platform_data_dir() -> PathBuf {
    home().join("Library/Application Support")
}

#[cfg(target_os = "windows")]
fn platform_data_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(home)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_data_dir() -> PathBuf {
    match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home().join(".local/share"),
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Make the data folder (0700) and its key file (0600) when missing, and
/// point bir-core at them: the database path, the key file, and no
/// keychain. Returns the database path.
///
/// # Safety
///
/// Sets process environment variables, so call it before any other thread
/// starts (first thing in `main`, or in a test before the server opens).
#[allow(unsafe_code)]
pub unsafe fn prepare(dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|error| {
        format!(
            "the data folder {} could not be made: {error}",
            dir.display()
        )
    })?;
    restrict(dir, 0o700)?;
    let key = dir.join(KEY_FILE);
    if !key.is_file() {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|error| format!("no random bytes for the database key: {error}"))?;
        let mut file = File::create(&key)
            .map_err(|error| format!("the key file could not be made: {error}"))?;
        restrict(&key, 0o600)?;
        file.write_all(hex::encode(bytes).as_bytes())
            .map_err(|error| format!("the key file could not be written: {error}"))?;
    }
    let database = dir.join(DATABASE_FILE);
    // SAFETY: the caller runs this before any other thread exists.
    unsafe {
        std::env::set_var("BUWIZ_NO_KEYCHAIN", "1");
        std::env::set_var("BUWIZ_DB_KEY_FILE", &key);
        std::env::remove_var("BUWIZ_DB_KEY");
        std::env::set_var("BUWIZ_DATABASE_PATH", &database);
        std::env::set_var("BIR_LOG_DIR", dir.join("logs"));
    }
    Ok(database)
}

#[cfg(unix)]
pub(crate) fn restrict(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|error| format!("{} could not be made private: {error}", path.display()))
}

#[cfg(not(unix))]
pub(crate) fn restrict(_path: &Path, _mode: u32) -> Result<(), String> {
    Ok(())
}

/// The owner lock, held while this value lives.
pub(crate) struct OwnerLock(#[allow(dead_code)] File);

/// Take the owner lock of the database at `path`, waiting up to a second.
pub(crate) fn acquire(path: &Path) -> Result<OwnerLock, String> {
    let mut waited = Duration::ZERO;
    loop {
        match try_acquire_live_owner_lock(path) {
            Ok(file) => return Ok(OwnerLock(file)),
            Err(DbError::LiveDatabaseInUse(_)) if waited < WAIT => {
                std::thread::sleep(STEP);
                waited += STEP;
            }
            Err(DbError::LiveDatabaseInUse(_)) => return Err(IN_USE.to_string()),
            Err(error) => return Err(format!("the database lock: {error}")),
        }
    }
}

/// Whether a write would get the lock now (for `ebirforms_status`); the lock is
/// let go again at once.
pub(crate) fn writable(path: &Path) -> bool {
    try_acquire_live_owner_lock(path).is_ok()
}
