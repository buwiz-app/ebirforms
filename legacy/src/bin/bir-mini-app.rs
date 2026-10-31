//! `bir-mini-app` — BIR with no window, serving MCP for Rox mini apps.
//! See `bir_shell::mini_app`.

fn main() -> std::process::ExitCode {
    // Same rolling log file and filter as bir-shell. The console copy goes
    // to stderr: `bir-mini-app mcp` speaks MCP on stdout. The commands that
    // only print (the v1 manifest for signing, help, version) open no log:
    // they run on build and signing machines, outside BIR's data directory.
    let prints_only = matches!(
        std::env::args().nth(1).as_deref(),
        Some("print-v1-manifest" | "--help" | "-h" | "--version" | "-V")
    );
    if !prints_only {
        let developer_mode = bir_core::logging::developer_mode_enabled();
        let _ = bir_core::logging::install_stderr(bir_core::logging::InstallOptions {
            developer_mode,
            default_filter: bir_core::logging::shell_filter(developer_mode),
        });
    }
    bir_shell::mini_app::main()
}
