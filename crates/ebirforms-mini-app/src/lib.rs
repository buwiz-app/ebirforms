//! `buwiz-mcp`: Buwiz without a window, as the MCP server Buwiz Bot spawns
//! for the Forms mini app (plugin `dev.goldcoders.bir`, Mini App Protocol
//! v1, stdio only).
//!
//! It opens the Buwiz app's own database (same data dir, same keychain key)
//! through [`bir_app_core::BirApp`] and shares it with the app by the
//! owner-lock rules in [`owner`]. stdout carries MCP; every log line goes to
//! stderr.

mod jcs;
mod keys;
pub mod manifest;
mod owner;
mod protocol;
mod scope;
mod scrub;
mod server;
mod tickets;

use std::io::{BufRead, Write};

pub use manifest::{ManifestSource, PLUGIN_ID};
pub use server::Server;

/// One line in, one line out, until stdin closes. Sends started on the way
/// are waited for before this returns.
pub fn serve_stdio(server: &mut Server) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        if let Some(reply) = server.handle_line(&line?) {
            out.write_all(reply.as_bytes())?;
            out.write_all(b"\n")?;
            out.flush()?;
        }
    }
    server.finish();
    Ok(())
}
