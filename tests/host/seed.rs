//! One synthetic taxpayer, via BirApp only, in the host test's scratch database.

use bir_app_core::{BirApp, BranchFacts, NewTaxpayer};
use ebirforms_mini_app::owner;

fn main() {
    if seed().is_err() {
        eprintln!("host-seed: scratch taxpayer creation failed (details suppressed)");
        std::process::exit(1);
    }
}

fn synthetic_root() -> Result<String, getrandom::Error> {
    let mut entropy = [0u8; 4];
    getrandom::fill(&mut entropy)?;
    let value = u32::from_le_bytes(entropy) % (10u32.pow(9) - 1) + 1;
    Ok(format!("{value:09}"))
}

#[allow(unsafe_code)]
fn seed() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::var_os("EBIRFORMS_DATA_DIR").ok_or("scratch data dir is required")?;
    let dir = std::path::Path::new(&dir);
    if !dir.join("host-test-scratch").is_file() {
        return Err("not a host-test scratch directory".into());
    }
    // SAFETY: called before BirApp or any other thread starts, as in main.
    let path = unsafe { owner::prepare(dir)? };
    if path.exists() {
        return Err("the host seed requires a fresh database".into());
    }
    let app = BirApp::open_at(&path)?;
    app.create_taxpayer(&NewTaxpayer {
        tin_root: synthetic_root()?,
        name: "Synthetic Host Taxpayer".into(),
        taxpayer_type: "Individual".into(),
        birth_date: None,
        head_office: BranchFacts {
            rdo_code: "043".into(),
            registered_address: "Synthetic address".into(),
            zip_code: "1600".into(),
            phone: String::new(),
            email: "host@example.invalid".into(),
        },
    })?;
    Ok(())
}
