//! The eBIRForms mini app (`app.buwiz.ebirforms`): the MCP server the eBIRForms
//! Bot spawns over stdio.
//!
//! It keeps its own taxpayer database ([`owner`]) and works on it through
//! eBIRForms' [`bir_app_core::BirApp`]. stdout carries MCP; log lines go to
//! stderr and to the mini app's own `logs/` folder.

pub mod account;
mod answers;
mod cards;
mod jcs;
mod keys;
pub mod manifest;
pub mod owner;
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
    Ok(())
}
