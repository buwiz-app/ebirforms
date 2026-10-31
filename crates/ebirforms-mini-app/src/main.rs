//! `buwiz-mcp`: see the library docs.

use std::process::ExitCode;

const USAGE: &str = "buwiz-mcp: Buwiz without a window, as the Forms mini app's MCP server for Buwiz Bot.

Usage: buwiz-mcp mcp              speak MCP (JSON-RPC 2.0, one message per line) on stdin/stdout,
                                  as the Mini App Protocol v1 plugin dev.goldcoders.bir
       buwiz-mcp print-manifest   print this build's unsigned v1 manifest, for signing
       buwiz-mcp --version
       buwiz-mcp --help

It works on the Buwiz app's taxpayer database. While the Buwiz app is open it
owns that database: reading works here, saving and queueing are refused.

Environment:
  BUWIZ_MCP_MANIFEST   the signed v1 manifest to serve at rox://manifest (default:
                       dev.goldcoders.bir.manifest.json next to this executable; without
                       either the unsigned one is served, and the Bot refuses the plugin)
  BUWIZ_DATABASE_PATH  the database file (default: the Buwiz app's data directory)
  BUWIZ_DB_KEY, BUWIZ_DB_KEY_FILE
                       the database key (default: the Buwiz app's keychain entry)
  BUWIZ_API_URL        the Buwiz server sending goes through (default: the app's setting)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["mcp"] => run(),
        ["print-manifest"] => {
            println!("{}", serde_json::to_string_pretty(&buwiz_mcp::manifest::unsigned()).expect("json value"));
            ExitCode::SUCCESS
        }
        ["--version" | "-V"] => {
            println!("buwiz-mcp {}", env!("CARGO_PKG_VERSION"));
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
    // stdout carries MCP: the console copy of the log goes to stderr (and
    // the app's rolling log file, as the app writes it).
    use bir_app_core::bir_core::logging;
    let developer_mode = logging::developer_mode_enabled();
    let _ = logging::install_stderr(logging::InstallOptions {
        developer_mode,
        default_filter: if developer_mode {
            "buwiz_mcp=debug,bir_app_core=debug,bir_core=info,bir_submit=debug,panic=error"
        } else {
            "buwiz_mcp=info,bir_app_core=info,bir_core=info,bir_submit=info,panic=error"
        },
    });
    let manifest = buwiz_mcp::ManifestSource::from_env();
    if let Some(problem) = manifest.staleness() {
        tracing::warn!("{problem}");
    }
    let mut server = buwiz_mcp::Server::open_live(manifest);
    match buwiz_mcp::serve_stdio(&mut server) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("buwiz-mcp: {error}");
            ExitCode::from(1)
        }
    }
}
