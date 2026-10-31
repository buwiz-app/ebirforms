//! Seeds a scratch database for a test Rox run (not a unit test).
//!
//! `BIR_E2E_SEED=1 EBIR_TEST_ENV=1 BIR_DATABASE_PATH=<scratch>/bir.db cargo test
//! --features agent --lib mini_app::e2e_seed -- --ignored` writes two made-up
//! taxpayers with 2026 forms and one confirmed (sent, BIR-confirmed, not
//! paid) 1601C, through BIR's own database paths, so the Forms app can be
//! shown on a read-only return. It refuses to run without `EBIR_TEST_ENV`
//! and a `BIR_DATABASE_PATH`, so it never touches a real database.

use bir_core::profile::TaxpayerType;
use bir_core::tax_profile::{BranchFacts, NewTaxpayer};
use serde_json::json;

#[test]
#[ignore = "seeds a test Rox's scratch database; run on purpose"]
fn seed_a_scratch_database_for_a_test_rox() {
    assert!(std::env::var_os("BIR_E2E_SEED").is_some(), "set BIR_E2E_SEED=1");
    assert!(std::env::var_os("EBIR_TEST_ENV").is_some(), "test databases only");
    assert!(std::env::var_os("BIR_DATABASE_PATH").is_some(), "a scratch BIR_DATABASE_PATH");
    let (db, _, _) = bir_core::db::open_live_database().expect("the scratch database opens");
    for (root, name) in [("123456788", "Dela Cruz Jane"), ("987654321", "Santos Mario")] {
        if db.get_taxpayer(root).ok().flatten().is_none() {
            db.create_taxpayer(NewTaxpayer {
                tin_root: root.into(),
                name: name.into(),
                taxpayer_type: TaxpayerType::Individual,
                birth_date: None,
                head_office: BranchFacts {
                    rdo_code: "040".into(),
                    registered_address: "1 Example Street, Pasig".into(),
                    zip_code: "1600".into(),
                    phone: "0280000000".into(),
                    email: "office@example.com".into(),
                },
            })
            .expect("taxpayer");
        }
        db.set_yearly_forms(&format!("{root}00000"), 2026, &["1601C", "2551Q"]).expect("forms");
    }
    const TIN: &str = "12345678800000";
    let profile = db.get_profile(TIN).unwrap().unwrap();
    let mut queued = bir_core::forms::form_1601c::Form1601CDraft::new_from_profile(&profile, 2026, 9);
    queued.any_taxes_withheld = false;
    queued.category_of_agent = "P".to_string();
    queued.transition_to_queued().unwrap_or_else(|errors| panic!("queue: {errors:?}"));
    db.save_queued_1601c_draft(&queued).unwrap();
    let claim = db
        .claim_queued_1601c_submission(
            &queued.tin,
            queued.taxable_year,
            queued.month,
            &queued.queued_submission_fingerprint.clone(),
            &queued.next_retry_at.clone(),
            0,
        )
        .unwrap();
    let bir_core::db::Claim1601CSubmissionResult::Claimed { mut draft, .. } = claim else {
        panic!("expected a claim");
    };
    let filename = queued.default_submission_filename();
    draft.transition_to_submitted(filename.clone());
    let data = serde_json::to_string(&draft).unwrap().replace('\'', "''");
    db.conn_for_tests()
        .execute_batch(&format!(
            "UPDATE form_drafts SET status = 'Submitted', data_json = '{data}'
             WHERE tin = '{TIN}' AND form_code = '1601C' AND taxable_year = 2026 AND quarter = 9"
        ))
        .unwrap();
    let email = json!({
        "tin": TIN, "form": "1601C", "period": "2026-09",
        "reference": format!("{}.xml", super::resolve::filing_reference(&filename)),
        "received_at": "2099-12-31T15:00:00+08:00", "message_id": "seed-1",
    });
    let answer = super::confirm::confirm(
        &db,
        email.as_object().unwrap(),
        bir_core::forms::taxform::today(),
    )
    .unwrap();
    assert_eq!(answer["confirmed"], true, "{answer}");
}
