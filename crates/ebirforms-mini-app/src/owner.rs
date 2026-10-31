//! Sharing the database with the Buwiz app.
//!
//! The app holds the live database's owner lock (`bir_data.db.owner.lock`)
//! for as long as it runs, and refuses to open when another process holds
//! it. This process therefore never keeps the lock: it opens the database
//! without it, and reads freely. A call that changes filing data (save, the
//! queue, a send grant) takes the lock for that call only and lets it go
//! after, so:
//!
//! - Buwiz open: writes are refused with a sentence that says to make the
//!   change in Buwiz (or quit it); reads, previews and fills that save
//!   nothing still work.
//! - Buwiz closed: writes go through. Buwiz opening during one waits for
//!   nothing; it only fails if it starts in the instant the call runs.
//!
//! This process's own bookkeeping (its ticket table, a one-time note marked
//! seen) is written without the lock: the app never reads it.
//!
//! Sending: a return queued here goes out through bir-submit's relay, the
//! same transport the app's queue worker uses, on a thread that keeps the
//! lock until the queue has run once. There is no queue worker in this
//! process, so a return that fails to go out is retried by the app's worker
//! (or the next queue here), never by a timer here.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use bir_app_core::BirApp;
use bir_app_core::bir_core::db::{DbError, try_acquire_live_owner_lock};
use bir_app_core::bir_submit::SignInStatus;

/// What a refused write says while the app is open.
pub(crate) const APP_OPEN: &str = "Buwiz is open on this computer, so it owns the taxpayer database: make this change in Buwiz, or quit Buwiz and ask again.";

/// How long a write waits for the lock: another `buwiz-mcp` (the Bot
/// respawns this process back to back) or a send in progress may let go of
/// it in a moment.
const WAIT: Duration = Duration::from_millis(1000);
const STEP: Duration = Duration::from_millis(100);

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
            Err(DbError::LiveDatabaseInUse(_)) => return Err(APP_OPEN.to_string()),
            Err(error) => return Err(format!("the database lock: {error}")),
        }
    }
}

/// Whether a write would get the lock now (for `bir_status`); the lock is
/// let go again at once.
pub(crate) fn writable(path: &Path) -> bool {
    try_acquire_live_owner_lock(path).is_ok()
}

/// Whether this computer is signed in to Buwiz for sending (`Some`, with
/// the account's email when known).
/// With `BUWIZ_NO_KEYCHAIN` set the keychain, where the sign-in lives, is
/// not read.
pub(crate) fn sign_in(app: &BirApp) -> Result<Option<Option<String>>, String> {
    if bir_app_core::bir_core::keychain_disabled() {
        return Err("BUWIZ_NO_KEYCHAIN is set, so the sign-in in the keychain is not read".into());
    }
    let submission = app.submission().map_err(|error| error.to_string())?;
    Ok(match submission.sign_in_status() {
        SignInStatus::SignedIn { email } => Some(email),
        _ => None,
    })
}

/// Sends started by this process, joined before it exits so a send is never
/// cut off halfway.
#[derive(Default)]
pub(crate) struct Sends {
    running: Vec<JoinHandle<()>>,
}

impl Sends {
    /// Run the queue once through the relay on its own thread, holding
    /// `lock` until it is done. `Ok(false)`: not signed in, nothing sent
    /// (the return stays queued for the app's worker).
    pub(crate) fn start(&mut self, app: &Arc<BirApp>, lock: OwnerLock) -> Result<bool, String> {
        self.running.retain(|send| !send.is_finished());
        if bir_app_core::bir_core::keychain_disabled() {
            return Ok(false);
        }
        let submission = app.submission().map_err(|error| error.to_string())?;
        if !matches!(submission.sign_in_status(), SignInStatus::SignedIn { .. }) {
            return Ok(false);
        }
        let app = app.clone();
        let send = std::thread::Builder::new()
            .name("buwiz-mcp-send".into())
            .spawn(move || {
                app.send_queue_once(&submission);
                drop(lock);
            })
            .map_err(|error| format!("the send did not start: {error}"))?;
        self.running.push(send);
        Ok(true)
    }

    /// Wait for every send to end.
    pub(crate) fn join(&mut self) {
        for send in self.running.drain(..) {
            let _ = send.join();
        }
    }
}

/// The live database's path, as the app opens it (`BUWIZ_DATABASE_PATH`,
/// else the platform data dir `com.buwiz.oss`).
pub(crate) fn database_path() -> PathBuf {
    bir_app_core::bir_core::db::app_database_path()
}
