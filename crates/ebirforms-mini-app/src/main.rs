//! `ebirforms-mini-app`: see the library docs.

use std::process::ExitCode;

use ebirforms_mini_app::owner;

const USAGE: &str = "ebirforms-mini-app: the eBIRForms mini app (app.buwiz.ebirforms), the MCP server the Buwiz Bot spawns.

Usage: ebirforms-mini-app mcp              speak MCP (JSON-RPC 2.0, one message per line) on stdin/stdout
       ebirforms-mini-app print-manifest   print this build's unsigned manifest, for signing
       ebirforms-mini-app --version
       ebirforms-mini-app --help

It keeps its own taxpayer database, shared with no desktop app, and never reads a
keychain. Sending returns to BIR is not available yet.

Environment:
  EBIRFORMS_MANIFEST   the signed manifest to serve (default: app.buwiz.ebirforms.manifest.json
                       next to this executable; without either the unsigned one is served,
                       and the Bot refuses the plugin)
  EBIRFORMS_DATA_DIR   the data folder (default: the platform data folder, app.buwiz.ebirforms):
                       ebirforms.db, its key ebirforms.key (0600, made on first run) and logs/";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["mcp"] => run(),
        ["print-manifest"] => {
            println!(
                "{}",
                serde_json::to_string_pretty(&ebirforms_mini_app::manifest::unsigned())
                    .expect("json value")
            );
            ExitCode::SUCCESS
        }
        ["--version" | "-V"] => {
            println!("ebirforms-mini-app {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        ["--help" | "-h"] => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        [] => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
        [other, ..] => {
            eprintln!("unknown argument `{other}`\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run() -> ExitCode {
    // SAFETY: first thing in the process, before any other thread starts.
    let database = unsafe { owner::prepare(&owner::data_dir()) };
    // stdout carries MCP: the log goes to stderr and to the data folder's logs/.
    use bir_app_core::bir_core::logging;
    let developer_mode = logging::developer_mode_enabled();
    let _ = logging::install_stderr(logging::InstallOptions {
        developer_mode,
        default_filter: if developer_mode {
            "ebirforms_mini_app=debug,bir_app_core=debug,bir_core=info,panic=error"
        } else {
            "ebirforms_mini_app=info,bir_app_core=info,bir_core=info,panic=error"
        },
    });
    let manifest = ebirforms_mini_app::ManifestSource::from_env();
    if let Some(problem) = manifest.staleness() {
        tracing::warn!("{problem}");
    }
    let mut server = ebirforms_mini_app::Server::open_own(manifest, database);
    match ebirforms_mini_app::serve_stdio(&mut server) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ebirforms-mini-app: {error}");
            ExitCode::from(1)
        }
    }
}
