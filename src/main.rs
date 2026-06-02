use anyhow::Result;
use clap::Parser;

use url_enum::cli::Args;

#[tokio::main]
async fn main() -> Result<()> {
    url_enum::scanner::run(Args::parse()).await
}
