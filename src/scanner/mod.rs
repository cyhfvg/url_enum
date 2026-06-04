use std::sync::Arc;

use anyhow::{Result, bail};
use futures::{StreamExt, TryStreamExt, stream::BoxStream};
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
use input::{
    candidate_stream, normalize_extensions, open_dictionary, read_dictionary_words, read_targets,
    target_dictionary_stream, target_word_stream,
};
use output::ResultWriter;
use pacing::RequestPacing;
use probe::probe;
use url_generator::UrlGenerator;

/// Fully rendered word and URL pair ready to be probed.
#[derive(Debug)]
pub(super) struct Candidate {
    pub(super) word: String,
    pub(super) url: Url,
}

/// Serializable result emitted for one attempted URL probe.
#[derive(Debug, Serialize)]
pub(super) struct ProbeResult {
    pub(super) word: String,
    pub(super) url: String,
    pub(super) status: Option<u16>,
    pub(super) size: Option<u64>,
    pub(super) elapsed_ms: u128,
    pub(super) error: Option<String>,
}

/// Signature: `pub async fn run(args: Args) -> Result<()>`
///
/// Purpose: Validates scanner settings, builds shared scanner components,
/// streams candidates, executes probes concurrently, and writes accepted
/// results.
///
/// Parameters:
/// - `args`: Parsed CLI options controlling targets, dictionary, headers,
///   filters, request behavior, and output destination.
///
/// Returns: `Ok(())` after all candidate URLs have been processed and output has
/// been flushed.
///
/// Errors: Returns an error when arguments are invalid, input files cannot be
/// read, target or header templates cannot be parsed, HTTP client construction
/// fails, probing fails before it can be represented as a result, or output
/// writing fails.
///
/// Notes: Candidate streams are selected to preserve target-major ordering by
/// default while avoiding loading the dictionary into memory unless randomized
/// sequencing requires the complete product.
pub async fn run(args: Args) -> Result<()> {
    if args.concurrency == 0 {
        bail!("concurrency must be greater than 0");
    }
    if args.timeout == 0 {
        bail!("timeout must be greater than 0 seconds");
    }

    let filters = Arc::new(Filters::new(&args)?);
    let targets = read_targets(&args.target).await?;
    let headers = Arc::new(RequestHeaders::parse(
        &args.headers,
        args.replace.as_deref(),
    )?);
    let extensions = normalize_extensions(&args.extensions);
    let generators = targets
        .into_iter()
        .map(|target| UrlGenerator::new(target, args.replace.clone(), headers.has_token()))
        .collect::<Result<Vec<_>>>()?;
    let client = build_client(&args)?;
    let method = method_for(args.method);
    let pacing = RequestPacing::new(args.request_jitter_ms);
    let dictionary_path = std::path::PathBuf::from(&args.dict);
    let dict = open_dictionary(&dictionary_path, args.output.as_deref()).await?;
    let mut writer = ResultWriter::new(args.format, args.output.as_deref())?;

    let candidates: BoxStream<'static, Result<Candidate>> =
        if !args.random_sequence && generators.len() == 1 {
            let generator = Arc::new(
                generators
                    .into_iter()
                    .next()
                    .expect("read_targets returns at least one target"),
            );
            candidate_stream(dict, generator, extensions).boxed()
        } else if !args.random_sequence {
            target_dictionary_stream(
                dict,
                dictionary_path,
                args.output.clone(),
                Arc::from(generators.into_boxed_slice()),
                extensions,
            )
            .boxed()
        } else {
            let words = Arc::from(
                read_dictionary_words(dict, &extensions)
                    .await?
                    .into_boxed_slice(),
            );
            target_word_stream(
                Arc::from(generators.into_boxed_slice()),
                words,
                args.random_sequence,
            )
            .boxed()
        };
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
