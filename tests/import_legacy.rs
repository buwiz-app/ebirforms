//! `ebirforms-mini-app import-legacy` over scratch databases: a legacy
//! database made by eBIRForms itself under a key file of its own, as the
//! retired BIR app kept it (`bir_data.db`, its `-wal` and `-shm`, the key as
//! 64 hex digits and a newline), imported into a scratch data folder by the
//! real binary. One test: the database key is process-wide. Every TIN and
//! name here is synthetic.

#![allow(unsafe_code)] // the test sets the process env, as main does

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use bir_app_core::{BirApp, BranchFacts, NewTaxpayer, OpenReturn};
use serde_json::json;

const LEGACY_KEY: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const WRONG_KEY: &str = "2222222222222222222222222222222222222222222222222222222222222222";
/// Synthetic roots and names, never a real taxpayer's.
const ROOTS: [&str; 2] = ["000111222", "000333444"];
const NAMES: [&str; 2] = ["Synthetic Sample Trading", "Juana Sample"];

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

/// Every file under `dir`, by path, with its bytes.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    if !dir.exists() {
        return out;
    }
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(snapshot(&path));
        } else {
            out.insert(path.clone(), fs::read(&path).unwrap());
        }
    }
    out
}

fn key_file(path: &Path, key: &str) {
    fs::write(path, format!("{key}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

/// No output names a taxpayer or a TIN.
fn names_nothing(output: &str) {
    for root in ROOTS {
        assert!(!output.contains(root), "a TIN in the output: {output}");
        let dashed = format!("{}-{}-{}", &root[..3], &root[3..6], &root[6..]);
        assert!(!output.contains(&dashed), "a TIN in the output: {output}");
    }
    for name in NAMES {
        assert!(!output.contains(name), "a name in the output: {output}");
    }
}

#[test]
fn the_legacy_import_over_scratch_databases() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("group.dev.goldcoders.bir");
    fs::create_dir(&old).unwrap();
    let legacy = old.join("bir_data.db");
    let legacy_key = dir.path().join("bir-db.key");
    key_file(&legacy_key, LEGACY_KEY);
    let wrong_key = dir.path().join("wrong.key");
    key_file(&wrong_key, WRONG_KEY);
    let target = dir.path().join("app.buwiz.ebirforms");

    // the old database, made by eBIRForms under the old key: two taxpayers,
    // one with a branch, their yearly forms and a saved return
    // SAFETY: one test in this process, before any thread starts.
    unsafe {
        std::env::set_var("BUWIZ_NO_KEYCHAIN", "1");
        std::env::remove_var("BUWIZ_DB_KEY");
        std::env::set_var("BUWIZ_DB_KEY_FILE", &legacy_key);
    }
    {
        let app = BirApp::open_at(&legacy).unwrap();
        for (root, name) in ROOTS.iter().zip(NAMES) {
            app.create_taxpayer(&NewTaxpayer {
                tin_root: (*root).into(),
                name: name.into(),
                taxpayer_type: if name.starts_with("Juana") {
                    "Individual"
                } else {
                    "Corporation"
                }
                .into(),
                birth_date: name.starts_with("Juana").then(|| "1990-01-15".into()),
                head_office: BranchFacts {
                    rdo_code: "043".into(),
                    registered_address: "1 Sample Street, Sample City".into(),
                    zip_code: "1600".into(),
                    phone: "09170000000".into(),
                    email: "sample@example.invalid".into(),
                },
            })
            .unwrap();
        }
        app.add_branch(ROOTS[0], "00001", "Sample branch").unwrap();
        let head = format!("{}00000", ROOTS[0]);
        app.call(
            "set_yearly_forms",
            vec![json!(head), json!(2026), json!(["2551Q", "1601C"])],
        )
        .unwrap();
        let opened = app
            .open_return(&OpenReturn {
                tin: head.clone(),
                form_code: "2551Q".into(),
                year: 2026,
                period: 3,
                key: None,
            })
            .unwrap();
        app.save("2551Q", &opened.draft).unwrap();
    }
    let source_before = snapshot(&old);
    assert!(!source_before.is_empty());

    // a wrong key fails cleanly: nothing made, nothing named
    let wrong = run(&[
        "--from",
        legacy.to_str().unwrap(),
        "--key-file",
        wrong_key.to_str().unwrap(),
        "--data-dir",
        target.to_str().unwrap(),
    ]);
    assert!(!wrong.status.success());
    assert!(text(&wrong).contains("wrong key file"), "{}", text(&wrong));
    assert!(!target.exists(), "a failed run left a target");
    names_nothing(&text(&wrong));

    // a target eBIRForms holds open is refused
    fs::create_dir_all(&target).unwrap();
    let lock = fs::File::create(target.join("ebirforms.db.open.lock")).unwrap();
    lock.lock_shared().unwrap();
    let held = run(&[
        "--from",
        legacy.to_str().unwrap(),
        "--key-file",
        legacy_key.to_str().unwrap(),
        "--data-dir",
        target.to_str().unwrap(),
    ]);
    assert!(!held.status.success());
    assert!(
        text(&held).contains("pause eBIRForms in Buwiz Bot first"),
        "{}",
        text(&held)
    );
    drop(lock);

    // the dry run counts and writes nothing to the target
    let target_before = snapshot(&target);
    let dry = run(&[
        "--from",
        legacy.to_str().unwrap(),
        "--key-file",
        legacy_key.to_str().unwrap(),
        "--data-dir",
        target.to_str().unwrap(),
        "--dry-run",
    ]);
    let said = text(&dry);
    assert!(dry.status.success(), "{said}");
    assert!(said.contains("dry run"), "{said}");
    assert!(
        said.contains("taxpayers: 2 to import, 0 already there"),
        "{said}"
    );
    assert!(
        said.contains("branches: 3 to import, 0 already there"),
        "{said}"
    );
    assert!(
        said.contains("saved returns and drafts: 1 to import"),
        "{said}"
    );
    assert!(said.contains("failed: 0"), "{said}");
    assert!(!said.contains("backup:"), "{said}");
    names_nothing(&said);
    assert_eq!(
        snapshot(&target),
        target_before,
        "the dry run wrote to the target"
    );
    assert!(
        fs::read_dir(dir.path()).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".backup-")),
        "the dry run made a backup"
    );

    // the import: backed up first, then everything across
    let real = run(&[
        "--from",
        legacy.to_str().unwrap(),
        "--key-file",
        legacy_key.to_str().unwrap(),
        "--data-dir",
        target.to_str().unwrap(),
    ]);
    let said = text(&real);
    assert!(real.status.success(), "{said}");
    assert!(
        said.contains("taxpayers: 2 imported, 0 already there"),
        "{said}"
    );
    assert!(
        said.contains("branches: 3 imported, 0 already there"),
        "{said}"
    );
    assert!(
        said.contains("saved returns and drafts: 1 imported"),
        "{said}"
    );
    assert!(
        said.contains("tax years with their forms: 1 imported"),
        "{said}"
    );
    names_nothing(&said);
    let backup = said
        .lines()
        .find_map(|line| line.strip_prefix("backup: "))
        .map(PathBuf::from)
        .expect("a backup line");
    assert!(
        backup
            .to_string_lossy()
            .contains("app.buwiz.ebirforms.backup-"),
        "{said}"
    );
    assert_eq!(
        snapshot(&backup).len(),
        target_before.len(),
        "the backup is the target as it was"
    );

    // run again: nothing added
    let again = run(&[
        "--from",
        legacy.to_str().unwrap(),
        "--key-file",
        legacy_key.to_str().unwrap(),
        "--data-dir",
        target.to_str().unwrap(),
    ]);
    let said = text(&again);
    assert!(again.status.success(), "{said}");
    assert!(
        said.contains("taxpayers: 0 imported, 2 already there"),
        "{said}"
    );
    assert!(
        said.contains("branches: 0 imported, 3 already there"),
        "{said}"
    );
    assert!(
        said.contains("saved returns and drafts: 0 imported"),
        "{said}"
    );
    names_nothing(&said);

    // the original files are byte-identical
    assert_eq!(
        snapshot(&old),
        source_before,
        "the import touched the old database"
    );

    // the target holds them, under its own key
    let target_key = target.join("ebirforms.key");
    assert_ne!(fs::read_to_string(&target_key).unwrap().trim(), LEGACY_KEY);
    // SAFETY: still one thread; the importer has exited.
    unsafe { std::env::set_var("BUWIZ_DB_KEY_FILE", &target_key) };
    let app = BirApp::open_at(target.join("ebirforms.db")).unwrap();
    let taxpayers = app.list_taxpayers().unwrap();
    assert_eq!(taxpayers.len(), 2);
    assert_eq!(taxpayers.iter().map(|t| t.branches.len()).sum::<usize>(), 3);
    let mut names: Vec<&str> = taxpayers.iter().map(|t| t.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["Juana Sample", "Synthetic Sample Trading"]);
    let head = format!("{}00000", ROOTS[0]);
    let forms = app
        .call("yearly_forms", vec![json!(head), json!(2026)])
        .unwrap();
    let forms: Vec<&str> = forms
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f.as_str())
        .collect();
    assert!(
        forms.contains(&"2551Q") && forms.contains(&"1601C"),
        "{forms:?}"
    );
    let returns = app.list_returns(bir_app_core::ReturnFilter::All).unwrap();
    assert_eq!(returns.len(), 1, "the saved return came across once");
}
