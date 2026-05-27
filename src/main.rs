mod cli;
mod scanner;

use anyhow::Result;
use clap::Parser;

use crate::cli::Args;

#[tokio::main]
async fn main() -> Result<()> {
    scanner::run(Args::parse()).await
}
