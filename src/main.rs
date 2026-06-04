use anyhow::Result;
use clap::Parser;

use url_enum::cli::Args;

/// Signature: `async fn main() -> Result<()>`
///
/// Purpose: Parses command-line arguments and starts the asynchronous scanner.
///
/// Parameters: None; arguments are read from the process command line through
/// [`Args::parse`].
///
/// Returns: `Ok(())` when scanning and output writing complete successfully.
///
/// Errors: Propagates scanner, argument-derived validation, network, and output
/// errors from [`url_enum::scanner::run`].
///
/// Notes: The Tokio runtime is installed by the `#[tokio::main]` macro.
#[tokio::main]
async fn main() -> Result<()> {
    url_enum::scanner::run(Args::parse()).await
}
