//! Read-only shell adapter for the shared receipt reader.

use anyhow::Result;
use clap::Args;

#[derive(Debug, Args)]
pub(super) struct ReceiptArgs {
    /// Print the receipt as JSON, or null when absent.
    #[arg(long, required = true)]
    json: bool,
}

pub(super) fn run(_args: ReceiptArgs) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string(&agmen_client::read_install_receipt()?)?
    );
    Ok(())
}
