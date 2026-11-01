//! The mini app keeps its own data: its own folder, its own key file, no
//! keychain, and a database it makes itself.

#![allow(unsafe_code)] // the test sets the process env, as main does

use std::sync::Arc;

use bir_app_core::BirApp;
use ebirforms_mini_app::owner::{self, DATABASE_FILE, KEY_FILE};
use ebirforms_mini_app::{ManifestSource, Server};
use serde_json::{Value, json};

#[test]
fn a_first_run_makes_its_own_folder_key_and_database() {
    // the default folder is named after the plugin id
    if std::env::var_os(owner::DATA_DIR_ENV).is_none() {
        assert!(owner::data_dir().ends_with("app.buwiz.ebirforms"));
    }
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("app.buwiz.ebirforms");
    // SAFETY: one test in this process, before any thread starts.
    let database = unsafe { owner::prepare(&data) }.unwrap();
    assert_eq!(database, data.join(DATABASE_FILE));

    let key = std::fs::read_to_string(data.join(KEY_FILE)).unwrap();
    assert_eq!(key.len(), 64);
    assert!(key.bytes().all(|b| b.is_ascii_hexdigit()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&data).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(data.join(KEY_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(std::env::var("BUWIZ_NO_KEYCHAIN").as_deref(), Ok("1"));
    assert_eq!(
        std::env::var_os("BUWIZ_DB_KEY_FILE"),
        Some(data.join(KEY_FILE).into_os_string())
    );
    assert_eq!(
        std::env::var_os("BUWIZ_DATABASE_PATH"),
        Some(database.clone().into_os_string())
    );
    assert_eq!(
        std::env::var_os("BUWIZ_LOG_DIR"),
        Some(data.join("logs").into_os_string())
    );

    // a second run keeps the key it made
    unsafe { owner::prepare(&data) }.unwrap();
    assert_eq!(std::fs::read_to_string(data.join(KEY_FILE)).unwrap(), key);

    // the server opens a database of its own there, empty at first
    let mut server = Server::open_own(ManifestSource::Unsigned, Ok(database.clone()));
    assert!(database.is_file());
    let app = Arc::new(BirApp::open_at(&database).unwrap());
    assert!(app.list_taxpayers().unwrap().is_empty());
    let init: Value = serde_json::from_str(
        &server
            .handle_line(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }).to_string())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "ebirforms-mini-app");
}
