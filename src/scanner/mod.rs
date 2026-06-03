use std::sync::Arc;

use anyhow::{Result, bail};
use futures::TryStreamExt;
use serde::Serialize;
use url::Url;

use crate::cli::Args;

mod client;
mod filters;
mod headers;
mod input;
mod output;
mod pacing;
mod probe;
mod url_generator;

use client::{build_client, method_for};
use filters::Filters;
use headers::RequestHeaders;
use input::{candidate_stream, normalize_extensions, open_dictionary, read_target};
use output::ResultWriter;
use pacing::RequestPacing;
use probe::probe;
use url_generator::UrlGenerator;

#[derive(Debug)]
pub(super) struct Candidate {
    pub(super) word: String,
    pub(super) url: Url,
}

#[derive(Debug, Serialize)]
pub(super) struct ProbeResult {
    pub(super) word: String,
    pub(super) url: String,
    pub(super) status: Option<u16>,
    pub(super) size: Option<u64>,
    pub(super) elapsed_ms: u128,
    pub(super) error: Option<String>,
}

pub async fn run(args: Args) -> Result<()> {
    if args.concurrency == 0 {
        bail!("concurrency must be greater than 0");
    }
    if args.timeout == 0 {
        bail!("timeout must be greater than 0 seconds");
    }

    let filters = Arc::new(Filters::new(&args)?);
    let target = read_target(&args.target).await?;
    let headers = Arc::new(RequestHeaders::parse(
        &args.headers,
        args.replace.as_deref(),
    )?);
    let generator = Arc::new(UrlGenerator::new(
        target,
        args.replace.clone(),
        headers.has_token(),
    )?);
    let extensions = normalize_extensions(&args.extensions);
    let client = build_client(&args)?;
    let method = method_for(args.method);
    let pacing = RequestPacing::new(args.request_jitter_ms);
    let dict = open_dictionary(std::path::Path::new(&args.dict), args.output.as_deref()).await?;
    let mut writer = ResultWriter::new(args.format, args.output.as_deref())?;

    let candidates = candidate_stream(dict, generator, extensions);
    let requests = candidates.map_ok(|candidate| {
        probe(
            client.clone(),
            method.clone(),
            candidate,
            Arc::clone(&filters),
            Arc::clone(&headers),
            args.follow_redirect,
            pacing,
        )
    });
    let results = requests.try_buffer_unordered(args.concurrency);
    futures::pin_mut!(results);

    while let Some(results) = results.try_next().await? {
        for result in results {
            writer.write(&result)?;
        }
    }
    writer.flush()?;
    Ok(())
}
