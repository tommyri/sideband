//! Signs in with a bot token and answers `!echo` and `!roll`.
//!
//! ```text
//! SIDEBAND_HOST=chat.example.org SIDEBAND_PORT=443 \
//! SIDEBAND_BOT_TOKEN=... [SIDEBAND_FINGERPRINT=<hex sha-256 of the leaf>] \
//!   cargo run -p sideband-bot-example
//! ```
//!
//! Without `SIDEBAND_FINGERPRINT` the server must have a publicly trusted
//! certificate. With it, the self-signed leaf must match.

use std::sync::Arc;

use anyhow::Context;
use ss_client_core::{Client, Credentials, ServerAddr};
use ss_net::pins::{MemoryPinStore, PinStore, TrustPolicy};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let host = std::env::var("SIDEBAND_HOST").context("SIDEBAND_HOST is not set")?;
    let port: u16 = std::env::var("SIDEBAND_PORT")
        .unwrap_or_else(|_| "443".into())
        .parse()
        .context("SIDEBAND_PORT is not a port")?;
    let token = std::env::var("SIDEBAND_BOT_TOKEN").context("SIDEBAND_BOT_TOKEN is not set")?;
    let address = ServerAddr::new(&host, port)?;
    let pins = Arc::new(MemoryPinStore::default());
    match std::env::var("SIDEBAND_FINGERPRINT") {
        Ok(hex) => pins.set(&address.id()?, hex.try_into().map_err(anyhow::Error::msg)?)?,
        Err(_) => pins.compare_exchange(&address.id()?, None, TrustPolicy::PublicCertificate)?,
    }
    let client = Client::connect(address, Credentials::Bot(token), pins).await?;
    println!("signed in; answering !echo and !roll");
    sideband_bot_example::run(&client).await
}
