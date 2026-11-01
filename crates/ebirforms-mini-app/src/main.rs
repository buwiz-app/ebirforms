//! `ebirforms-mini-app`: see the library docs.

use std::process::ExitCode;

use ebirforms_mini_app::owner;

const USAGE: &str = "ebirforms-mini-app: the eBIRForms mini app (app.buwiz.ebirforms), the MCP server the Buwiz Bot spawns.

Usage: ebirforms-mini-app mcp              speak MCP (JSON-RPC 2.0, one message per line) on stdin/stdout
       ebirforms-mini-app manifest [--key-id <id>]
                                           print this build's unsigned manifest, for signing
                                           (key id gc-2026-01 unless a test key is named)
       ebirforms-mini-app app-source <id>  print an app's source exactly as it is served
       ebirforms-mini-app package <plugin.json> <out> <platform> [--published <plugin.json>]
                                           fill a release's plugin.json (protocol 1.0 section 7)
                                           from the signed manifest and the binary in <out>, for
                                           `buwiz-broker sign-manifest --block buwiz`
                                           (tools/release/publish.sh runs it)
       ebirforms-mini-app --version
       ebirforms-mini-app --help

It keeps its own taxpayer database, shared with no desktop app, and never reads a
keychain. Sending returns to the BIR needs a Buwiz sign-in (the Sign-in app).

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
        ["manifest"] => print_manifest(ebirforms_mini_app::manifest::RELEASE_KEY_ID),
        ["manifest", "--key-id", key_id] => print_manifest(key_id),
        ["package", template, out, platform] => package(template, out, platform, None),
        ["package", template, out, platform, "--published", published] => {
            package(template, out, platform, Some(published))
        }
        ["app-source", id] => match ebirforms_mini_app::manifest::app_source(id) {
            Some(source) => {
                print!("{source}");
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("no app `{id}`");
                ExitCode::from(2)
            }
        },
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

/// The release's plugin.json to sign: the binary is `ebirforms-mini-app`, so
/// its asset is `ebirforms-mini-app-<platform>` and the signed manifest
/// `app.buwiz.ebirforms.manifest.json`.
fn package(template: &str, out: &str, platform: &str, published: Option<&str>) -> ExitCode {
    use std::path::Path;
    match buwiz_plugin::package::run(
        Path::new(template),
        Path::new(out),
        ebirforms_mini_app::manifest::PLUGIN_ID,
        "ebirforms-mini-app",
        platform,
        published.map(Path::new),
    ) {
        Ok(()) => {
            eprintln!(
                "ebirforms-mini-app: wrote {out}/{}",
                buwiz_plugin::package::UNSIGNED
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("ebirforms-mini-app: package: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_manifest(key_id: &str) -> ExitCode {
    let manifest = ebirforms_mini_app::manifest::unsigned_for(key_id);
    println!(
        "{}",
        serde_json::to_string_pretty(&manifest).expect("json value")
    );
    ExitCode::SUCCESS
}

#[allow(unsafe_code)]
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
