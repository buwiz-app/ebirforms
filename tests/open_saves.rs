//! Why `screen_form_open` is a write in `scope.rs`: opening a stored draft saves
//! it when the tax profile moved under it, with no save by anyone. If this stops
//! being true after a bir-app-core bump, the open can go back to a read.

#![allow(unsafe_code)] // the test sets the process env, as main does

use bir_app_core::bir_core::db::Database;
use bir_app_core::{BirApp, BranchFacts, NewTaxpayer, OpenReturn};
use serde_json::Value;

const TEST_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn stored(path: &std::path::Path, id: i64) -> Value {
    Database::open(path)
        .unwrap()
        .draft_data_json(id)
        .unwrap()
        .expect("the stored return")
}

#[test]
fn opening_a_stored_draft_saves_what_the_profile_changed() {
    // SAFETY: set before any thread of this test reads them (one test per process).
    unsafe {
        std::env::set_var("BUWIZ_DB_KEY", TEST_KEY);
        std::env::set_var("BUWIZ_NO_KEYCHAIN", "1");
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bir_data.db");
    let app = BirApp::open_at(&path).unwrap();
    let taxpayer = app
        .create_taxpayer(&NewTaxpayer {
            tin_root: "000123453".into(),
            name: "Juana Santos".into(),
            taxpayer_type: "Individual".into(),
            birth_date: Some("1990-01-15".into()),
            head_office: BranchFacts {
                rdo_code: "043".into(),
                registered_address: "1 Sample Street, Pasig City".into(),
                zip_code: "1600".into(),
                phone: "09171234567".into(),
                email: "juana@example.com".into(),
            },
        })
        .unwrap();
    let tin = taxpayer.branches[0].tin.clone();
    let open = OpenReturn {
        tin: tin.clone(),
        form_code: "2551Q".into(),
        year: 2026,
        period: 3,
        key: None,
    };
    let opened = app.open_return(&open).unwrap();
    let id = app
        .save("2551Q", &opened.draft)
        .unwrap()
        .id
        .expect("its id");
    let before = stored(&path, id);

    app.set_branch_fact(&tin, "rdo_code", "050").unwrap();
    assert_eq!(
        stored(&path, id),
        before,
        "changing the profile doesn't touch the return"
    );

    let reopened = app.open_return(&open).unwrap();
    assert!(reopened.status == "Draft", "{}", reopened.status);
    assert_ne!(
        stored(&path, id),
        before,
        "the open itself wrote the return"
    );
}
