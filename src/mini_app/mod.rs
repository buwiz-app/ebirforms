//! `bir-mini-app`: BIR with no window, as an MCP server for Rox.
//!
//! It owns the live database and the submission worker the way bir-shell
//! does (same account gate, same single-owner lock), and answers MCP itself:
//!
//! - `bir-mini-app mcp` speaks MCP on stdin/stdout. Rox spawns it as the v1
//!   plugin `dev.goldcoders.bir` ([`v1`]): it answers nothing before
//!   `rox.provision`, and serves the signed manifest `BIR_V1_MANIFEST`
//!   names. stdio is Rox's only transport (spec 14.3).
//! - `bir-mini-app serve --http 127.0.0.1:PORT` is a long-lived daemon on
//!   bir-mcp's loopback Streamable HTTP door (OAuth bearer per request), so
//!   queued filings keep going when Rox is closed. It is for the BIR desktop
//!   side, not for Rox: no provisioning, and the apps' tools refuse it.
//! - `bir-mini-app print-v1-manifest` prints the unsigned v1 manifest of
//!   this build's apps, for `scripts/sign-v1-manifest.sh`.
//!
//! Everything that touches the database runs on the main thread: the `bir`
//! module keeps its handle thread-local. See [`server`] for the tools,
//! [`apps`] for the TSX mini apps it serves as resources and [`resolve`] for
//! the commands and `@` scopes it announces to Rox.

mod apps;
mod bridge;
mod calendar;
mod confirm;
mod payments;
mod plain;
#[cfg(test)]
mod demo_tests;
#[cfg(test)]
mod e2e_seed;
mod resolve;
mod scope;
mod server;
mod taxpayer;
mod v1;

use std::io::{BufRead, Write};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use server::{MiniServer, Reply};

const USAGE: &str = "bir-mini-app: BIR without a window, as an MCP server for Rox mini apps.

Usage: bir-mini-app mcp                          speak MCP (JSON-RPC 2.0, one message per line) on stdin/stdout,
                                                 as Rox's Mini App Protocol v1 plugin dev.goldcoders.bir
       bir-mini-app serve --http 127.0.0.1:PORT  serve MCP Streamable HTTP at http://127.0.0.1:PORT/mcp
                                                 (loopback only; OAuth protected resource, as bir-mcp --http;
                                                 for the BIR desktop side, not a Rox transport)
       bir-mini-app print-v1-manifest            print this build's unsigned v1 manifest (app fingerprints
                                                 from the sources it serves) for scripts/sign-v1-manifest.sh
       bir-mini-app --version
       bir-mini-app --help

This process owns the BIR database (one owner at a time: quit the BIR app or
another bir-mini-app first) and runs queued filings in the background.

Environment:
  BIR_DATABASE_PATH    database file (default: the app's data directory)
  BIR_MINI_APPS_DIR    read the TSX mini apps and apps.json from this directory
                       instead of the copies built into the binary
  BIR_V1_MANIFEST      the signed v1 manifest to serve at rox://manifest (without it the
                       unsigned one is served, and Rox refuses the plugin)
  BUWIZ_ACCESS_TOKEN   stdio only: license tools with this agent token instead of
                       the Buwiz account saved on this computer";

/// Parse the command line and run. The binary's whole `main`.
pub fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["mcp"] => run(Transport::Stdio),
        ["print-v1-manifest"] => match v1::manifest::unsigned(&apps::Catalog::from_env()) {
            Ok(manifest) => {
                println!("{}", serde_json::to_string_pretty(&manifest).expect("json value"));
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("bir-mini-app: {error}");
                ExitCode::from(1)
            }
        },
        ["serve", "--http", addr] => run(Transport::Http(addr.to_string())),
        ["serve", ..] => {
            eprintln!("serve needs --http 127.0.0.1:PORT\n\n{USAGE}");
            ExitCode::from(2)
        }
        ["--version" | "-V"] => {
            println!("bir-mini-app {}", env!("CARGO_PKG_VERSION"));
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

enum Transport {
    Stdio,
    Http(String),
}

fn run(transport: Transport) -> ExitCode {
    // The account gate decides whether the filing database opens, as in
    // bir-shell. No updater: this binary is replaced by its installer.
    crate::cloud_account::prepare_launch();
    if let Err(error) = claim_database() {
        eprintln!("bir-mini-app: {error}");
        return ExitCode::from(2);
    }
    if crate::account_gate::is_unlocked() {
        crate::bir_module::open_database_at_launch();
    }
    let catalog = apps::Catalog::from_env();
    if let Some(problem) = v1::manifest::staleness(&catalog) {
        eprintln!("bir-mini-app: {problem}");
    }
    let rox = matches!(transport, Transport::Stdio);
    let mut server = MiniServer::new(catalog, rox);
    let code = match transport {
        Transport::Stdio => match serve_stdio(&mut server) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("bir-mini-app: {error}");
                ExitCode::from(1)
            }
        },
        Transport::Http(addr) => serve_http(&mut server, &addr),
    };
    drop(server);
    crate::bir_module::shutdown_for_quit();
    code
}

/// Open the database file now, signed in or not, so a second owner fails at
/// start with the database's own "already open" sentence rather than on the
/// first tool call. The account functions use this same handle.
///
/// Rox respawns this process back to back (an install reviews one spawn,
/// then starts another), and the previous process may still hold the
/// owner lock for a moment after it was told to stop: wait up to two
/// seconds for it before giving up.
fn claim_database() -> Result<(), String> {
    let mut tries = 0;
    loop {
        match crate::bir_module::with_account_database(|_| Ok(())) {
            Ok(()) => return Ok(()),
            Err(error) if tries < 20 && error.message().contains("Single live-DB owner") => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(error) => return Err(error.message().to_string()),
        }
    }
}

/// One line in, one line out. A `Later` reply (an async host call such as a
/// sign-in waiting for device approval) runs on its own thread so the next
/// requests are answered meanwhile; JSON-RPC ids pair them up.
fn serve_stdio(server: &mut MiniServer) -> std::io::Result<()> {
    let output = Arc::new(Mutex::new(std::io::stdout()));
    let write =
        |output: &Mutex<std::io::Stdout>, value: &serde_json::Value| -> std::io::Result<()> {
            let mut out = output.lock().expect("stdout");
            out.write_all(value.to_string().as_bytes())?;
            out.write_all(b"\n")?;
            out.flush()
        };
    let mut pending = Vec::new();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        match server.handle_line(&line) {
            Reply::Now(Some(value)) => write(&output, &value)?,
            Reply::Now(None) => {}
            Reply::Later(run) => {
                let output = output.clone();
                pending.push(std::thread::spawn(move || {
                    let _ = write(&output, &run());
                }));
            }
        }
        pending.retain(|worker| !worker.is_finished());
    }
    for worker in pending {
        let _ = worker.join();
    }
    Ok(())
}

fn serve_http(server: &mut MiniServer, addr: &str) -> ExitCode {
    let listener = match bir_mcp::http::bind(addr) {
        Ok(listener) => Arc::new(listener),
        Err(error) => {
            eprintln!("bir-mini-app: {error}");
            return ExitCode::from(2);
        }
    };
    let issuer = match bir_mcp::oauth::configured_authorization_server() {
        Ok(issuer) => issuer,
        Err(error) => {
            eprintln!("bir-mini-app: {error}");
            return ExitCode::from(2);
        }
    };
    let Some(address) = listener.server_addr().to_ip() else {
        eprintln!("bir-mini-app: the listener has no socket address");
        return ExitCode::from(2);
    };
    let resource = bir_mcp::oauth::Resource::from_bind(address, &issuer);
    eprintln!("bir-mini-app: serving MCP at {}", resource.mcp);
    eprintln!("bir-mini-app: OAuth metadata at {}", resource.metadata);
    let stopping = listener.clone();
    stop_on_signal(move || stopping.unblock());
    // Closed until a request's bearer token is checked, as in bir-mcp --http.
    server.set_license(bir_mcp::license::License::settled(Err(
        bir_mcp::license::UNAUTHENTICATED.to_string(),
    )));
    let auth = bir_mcp::oauth::Introspection::new(resource.authorization_server.clone());
    bir_mcp::http::serve_http(server, &listener, &auth, &resource);
    eprintln!("bir-mini-app: stopped");
    ExitCode::SUCCESS
}

/// Ctrl-C or SIGTERM unblocks the listener, so the main thread leaves the
/// request loop and closes the database through `shutdown_for_quit`.
fn stop_on_signal(unblock: impl FnOnce() + Send + 'static) {
    let spawned = std::thread::Builder::new()
        .name("bir-mini-app-signals".into())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async {
                #[cfg(unix)]
                {
                    use tokio::signal::unix::{SignalKind, signal};
                    let Ok(mut term) = signal(SignalKind::terminate()) else {
                        let _ = tokio::signal::ctrl_c().await;
                        return;
                    };
                    let interrupt = Box::pin(tokio::signal::ctrl_c());
                    let terminate = Box::pin(term.recv());
                    futures::future::select(interrupt, terminate).await;
                }
                #[cfg(not(unix))]
                {
                    let _ = tokio::signal::ctrl_c().await;
                }
            });
            unblock();
        });
    if let Err(error) = spawned {
        tracing::warn!("bir-mini-app: no signal handler: {error}");
    }
}
