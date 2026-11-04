//! Why eBIRForms refuses a legacy profile on import, each suspected cause
//! made with synthetic data in a scratch legacy database, imported by the
//! real binary with `--dry-run --explain`. One test: the database key is
//! process-wide. Every TIN and name here is synthetic.

#![allow(unsafe_code)] // the test sets the process env, as main does

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use bir_app_core::{BirApp, BranchFacts, NewTaxpayer};
use serde_json::{Value, json};

const KEY: &str = "3333333333333333333333333333333333333333333333333333333333333333";
const PLAIN: &str = "000111222";
const STALE: &str = "000555666";
const LOOSE: &str = "000777888";
const MIXED: &str = "000999111";
const NAMES: [&str; 5] = [
    "Synthetic Plain Trading",
    "Synthetic Old Registration",
    "Synthetic Loose Facts",
    "Synthetic Other Name",
    "Synthetic Mixed Registration",
];
// (the rename at the end happens after the last output is read)

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ebirforms-mini-app"))
        .arg("import-legacy")
        .args(args)
        .env_remove("EBIRFORMS_DATA_DIR")
        .output()
        .expect("the binary runs")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn facts() -> BranchFacts {
    BranchFacts {
        rdo_code: "043".into(),
        registered_address: "1 Sample Street, Sample City".into(),
        zip_code: "1600".into(),
        phone: "09170000000".into(),
        email: "sample@example.invalid".into(),
    }
}

/// A new profile saved the way the old app's profile page saved one
/// (`save_profile`, no checks of ours): `template`'s profile under another
/// TIN, with `edit` applied.
fn save_like_the_old_app(
    app: &BirApp,
    template: &str,
    from_root: &str,
    to: &str,
    edit: impl FnOnce(&mut Value),
) {
    let detail = app.call("profile_detail", vec![json!(template)]).unwrap();
    let dashed = |root: &str| format!("{}-{}-{}", &root[..3], &root[3..6], &root[6..9]);
    let moved = detail
        .to_string()
        .replace(from_root, &to[..9])
        .replace(&dashed(from_root), &dashed(&to[..9]));
    let mut profile: Value = serde_json::from_str(&moved).unwrap();
    profile["tin"] = json!({ "segment1": &to[..3], "segment2": &to[3..6], "segment3": &to[6..9], "branch": &to[9..] });
    profile["id"] = Value::Null;
    profile["profile_versions"] = json!([]);
    edit(&mut profile);
    app.call("save_profile", vec![profile]).unwrap();
}

fn names_nothing(output: &str) {
    for root in [PLAIN, STALE, LOOSE, MIXED] {
        assert!(!output.contains(root), "a TIN in the output: {output}");
        assert!(
            !output.contains(&format!("{}-{}-{}", &root[..3], &root[3..6], &root[6..])),
            "{output}"
        );
    }
    for name in NAMES {
        assert!(!output.contains(name), "a name in the output: {output}");
    }
    assert!(
        !output.contains("Sample Street") && !output.contains("Changed Street"),
        "an address: {output}"
    );
}

#[test]
fn what_ebirforms_refuses_on_import_and_why() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("bir_data.db");
    let key = dir.path().join("bir-db.key");
    fs::write(&key, format!("{KEY}\n")).unwrap();
    let target = dir.path().join("target");
    // SAFETY: one test in this process, before any thread starts.
    unsafe {
        std::env::set_var("BUWIZ_NO_KEYCHAIN", "1");
        std::env::remove_var("BUWIZ_DB_KEY");
        std::env::set_var("BUWIZ_DB_KEY_FILE", &key);
    }
    {
        let app = BirApp::open_at(&legacy).unwrap();
        app.create_taxpayer(&NewTaxpayer {
            tin_root: PLAIN.into(),
            name: NAMES[0].into(),
            taxpayer_type: "Corporation".into(),
            birth_date: None,
            head_office: facts(),
        })
        .unwrap();
        app.add_branch(PLAIN, "00001", "Sample branch").unwrap();
        let head = format!("{PLAIN}00000");
        // a registration saved with its start date: its migration backfill
        // is Confirmed; then its address changed, so the backfill no longer
        // matches the profile
        save_like_the_old_app(&app, &head, PLAIN, &format!("{STALE}00000"), |p| {
            p["full_name"] = json!(NAMES[1]);
            p["business_start_date"] = json!("2020-01-06");
        });
        app.call(
            "update_branch",
            vec![
                json!(format!("{STALE}00000")),
                json!({ "fields": { "registered_address": "2 Changed Street, Sample City" } }),
            ],
        )
        .unwrap();
        // the same, and then a branch saved with the changed address: one
        // backfill stale and one that matches, under one taxpayer
        let mixed = format!("{MIXED}00000");
        save_like_the_old_app(&app, &head, PLAIN, &mixed, |p| {
            p["full_name"] = json!(NAMES[4]);
            p["business_start_date"] = json!("2020-01-06");
        });
        app.call(
            "update_branch",
            vec![
                json!(mixed),
                json!({ "fields": { "registered_address": "2 Changed Street, Sample City" } }),
            ],
        )
        .unwrap();
        save_like_the_old_app(&app, &mixed, MIXED, &format!("{MIXED}00001"), |_| {});
        // facts our New taxpayer form refuses: RDO, ZIP, phone and email
        save_like_the_old_app(&app, &head, PLAIN, &format!("{LOOSE}00000"), |p| {
            p["full_name"] = json!(NAMES[2]);
            p["rdo_code"] = json!("999");
            p["zip_code"] = json!("9999");
            p["phone"] = json!("12");
            p["email"] = json!("nope");
        });
        // a branch whose name and type differ from its head office's
        save_like_the_old_app(&app, &head, PLAIN, &format!("{PLAIN}00002"), |p| {
            p["full_name"] = json!(NAMES[3]);
            p["taxpayer_type"] = json!("Individual");
        });
    }
    let args = |extra: &[&'static str]| {
        let mut all = vec![
            "--from".to_string(),
            legacy.display().to_string(),
            "--key-file".into(),
            key.display().to_string(),
            "--data-dir".into(),
            target.display().to_string(),
        ];
        all.extend(extra.iter().map(|s| s.to_string()));
        all
    };
    let call = |extra: &[&'static str]| {
        let all = args(extra);
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        run(&refs)
    };
    // --explain is for dry runs only
    let refused = call(&["--explain"]);
    assert!(!refused.status.success() && text(&refused).contains("--explain goes with --dry-run"));

    // the dry run: the stale backfill is the one refusal; facts our form
    // would refuse and a branch named unlike its head office come across
    let dry = call(&["--dry-run", "--explain"]);
    let said = text(&dry);
    println!("{said}");
    names_nothing(&said);
    assert!(
        said.contains("taxpayers: 2 to import, 0 already there, 2 refused"),
        "{said}"
    );
    assert!(
        said.contains("branches: 4 to import, 0 already there"),
        "{said}"
    );
    assert!(
        said.contains("failed: 3 branches (2 refused_by_ebirforms:confirmed_version_needs_review:MigrationBackfill+StaleMigrationBackfill, 1 refused_by_ebirforms:confirmed_version_needs_review:StaleMigrationBackfill)"),
        "{said}"
    );
    assert!(
        said.contains("branch #1 of taxpayer #2: refused_by_ebirforms:confirmed_version_needs_review:StaleMigrationBackfill"),
        "{said}"
    );
    assert!(!target.exists(), "the dry run made the target");

    // with the backfill rebuilt from the profile as it is: all of it
    let rebuilt = call(&["--rebuild-stale-backfill"]);
    let said = text(&rebuilt);
    println!("{said}");
    names_nothing(&said);
    assert!(rebuilt.status.success(), "{said}");
    assert!(
        said.contains("taxpayers: 4 imported, 0 already there, 0 refused"),
        "{said}"
    );
    assert!(
        said.contains("taxpayers whose stale registration backfill was rebuilt: 2"),
        "{said}"
    );
    assert!(said.contains("failed: 0"), "{said}");

    // a branch added later, its head office already there: it comes alone
    // SAFETY: still one thread; the importer has exited.
    unsafe { std::env::set_var("BUWIZ_DB_KEY_FILE", &key) };
    {
        let app = BirApp::open_at(&legacy).unwrap();
        app.add_branch(PLAIN, "00003", "Later branch").unwrap();
    }
    let later = call(&[]);
    let said = text(&later);
    assert!(later.status.success(), "{said}");
    assert!(
        said.contains("branches: 1 imported, 7 already there"),
        "{said}"
    );

    // in the target: the loose facts as they were, the stale registration
    // with a backfill that matches it, the odd branch under its root
    unsafe { std::env::set_var("BUWIZ_DB_KEY_FILE", target.join("ebirforms.key")) };
    let app = BirApp::open_at(target.join("ebirforms.db")).unwrap();
    let taxpayers = app.list_taxpayers().unwrap();
    let loose = taxpayers
        .iter()
        .find(|t| t.tin_root == LOOSE)
        .expect("the loose facts came across");
    let facts = &loose.branches[0].facts;
    assert_eq!(
        (
            facts.rdo_code.as_str(),
            facts.zip_code.as_str(),
            facts.phone.as_str(),
            facts.email.as_str()
        ),
        ("999", "9999", "12", "nope")
    );
    let stale = app
        .call("profile_detail", vec![json!(format!("{STALE}00000"))])
        .unwrap();
    assert_eq!(stale["registered_address"], "2 Changed Street, Sample City");
    let versions = stale["profile_versions"].as_array().unwrap();
    assert_eq!(versions.len(), 1, "{versions:?}");
    assert_eq!(versions[0]["status"], "Confirmed");
    assert_eq!(
        versions[0]["cor"]["registered_address"],
        "2 Changed Street, Sample City"
    );
    for tin in [format!("{MIXED}00000"), format!("{MIXED}00001")] {
        let mixed = app.call("profile_detail", vec![json!(tin)]).unwrap();
        let versions = mixed["profile_versions"].as_array().unwrap();
        assert_eq!(versions.len(), 1, "{versions:?}");
        assert_eq!(versions[0]["status"], "Confirmed");
        assert_eq!(
            versions[0]["cor"]["registered_address"],
            "2 Changed Street, Sample City"
        );
    }
    let plain = taxpayers.iter().find(|t| t.tin_root == PLAIN).unwrap();
    assert_eq!(plain.branches.len(), 4);
    // an unrelated save goes through with the loose facts kept: the
    // profile editor's name save does not check the address section
    app.call(
        "update_taxpayer",
        vec![
            json!(LOOSE),
            json!({ "name": "Synthetic Loose Facts Renamed" }),
        ],
    )
    .unwrap();
    let _ = Path::new("");
}
