use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use futures::StreamExt;
use reqwest::{
    Client, Method, Response, StatusCode,
    header::{CONTENT_LENGTH, LOCATION},
};
use url::Url;

use super::{Candidate, Filters, ProbeResult, RequestHeaders, RequestPacing};

const MAX_REDIRECTS: usize = 10;

/// Signature: `async fn probe(client, method, candidate, filters, headers, follow_redirect, pacing) -> Result<Vec<ProbeResult>>`
///
/// Purpose: Sends one candidate request, optionally follows redirects manually,
/// and collects filter-accepted results.
///
/// Parameters:
/// - `client`: Shared reqwest client.
/// - `method`: HTTP method to use for every request in the redirect chain.
/// - `candidate`: Candidate word and initial URL to probe.
/// - `filters`: Shared status and size filters.
/// - `headers`: Shared request header templates.
/// - `follow_redirect`: Whether to follow supported HTTP redirects.
/// - `pacing`: Request jitter policy.
///
/// Returns: A vector of accepted [`ProbeResult`] values for the initial response
/// and any followed redirect responses.
///
/// Errors: Returns an error when header rendering fails or response processing
/// fails before it can be represented as a probe result.
///
/// Notes: Sensitive headers are only sent to the original authority and same
/// authority redirects; cross-authority redirects omit them.
pub(super) async fn probe(
    client: Client,
    method: Method,
    candidate: Candidate,
    filters: Arc<Filters>,
    headers: Arc<RequestHeaders>,
    follow_redirect: bool,
    pacing: RequestPacing,
) -> Result<Vec<ProbeResult>> {
    let mut results = Vec::new();
    let mut url = candidate.url;
    let mut redirects_followed = 0_usize;
    let mut include_sensitive_headers = true;
    let mut request_index = 0_usize;

    loop {
        pacing
            .wait_before_request(&candidate.word, &url, request_index)
            .await;
        request_index += 1;
        let started = Instant::now();
        let response = headers
            .apply(
                client.request(method.clone(), url.clone()),
                include_sensitive_headers,
                &candidate.word,
            )?
            .send()
            .await;

        match response {
            Ok(response) => {
                let redirect = follow_redirect
                    .then(|| redirect_target(&response, &url))
                    .flatten();
                if let Some(result) =
                    response_result(&candidate.word, &url, &method, response, &filters, started)
                        .await?
                {
                    results.push(result);
                }

                let Some(next_url) = redirect else {
                    return Ok(results);
                };
                if redirects_followed >= MAX_REDIRECTS {
                    return Ok(results);
                }
                if !same_authority(&url, &next_url) {
                    include_sensitive_headers = false;
                }
                redirects_followed += 1;
                url = next_url;
            }
            Err(error) => {
                if filters.accepts_failure() {
                    results.push(ProbeResult {
                        word: candidate.word,
                        url: url.to_string(),
                        status: None,
                        size: None,
                        elapsed_ms: started.elapsed().as_millis(),
                        error: Some(error.to_string()),
                    });
                }
                return Ok(results);
            }
        }
    }
}

/// Signature: `async fn response_result(word, url, method, response, filters, started) -> Result<Option<ProbeResult>>`
///
/// Purpose: Converts a reqwest response into an optional serialized scanner
/// result after applying status and size filters.
///
/// Parameters:
/// - `word`: Candidate dictionary word that produced the request.
/// - `url`: URL that produced the response.
/// - `method`: HTTP method used for the request.
/// - `response`: Received reqwest response.
/// - `filters`: Filters used to decide whether to emit the result.
/// - `started`: Timestamp captured before the request was sent.
///
/// Returns: `Ok(Some(result))` when the response passes filters, `Ok(None)` when
/// filtered out.
///
/// Errors: Propagates stream-processing errors that cannot be represented as an
/// emitted result.
///
/// Notes: `HEAD` uses `Content-Length` as size when available; other methods
/// stream the body and count bytes without retaining the body.
async fn response_result(
    word: &str,
    url: &Url,
    method: &Method,
    response: Response,
    filters: &Filters,
    started: Instant,
) -> Result<Option<ProbeResult>> {
    let status = response.status().as_u16();
    if !filters.accepts_status(status) {
        return Ok(None);
    }

    let size = if *method == Method::HEAD {
        response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
    } else {
        let mut bytes = response.bytes_stream();
        let mut total = 0_u64;
        while let Some(chunk) = bytes.next().await {
            match chunk {
                Ok(chunk) => total = total.saturating_add(chunk.len() as u64),
                Err(error) => {
                    return Ok(Some(ProbeResult {
                        word: word.to_owned(),
                        url: url.to_string(),
                        status: Some(status),
                        size: None,
                        elapsed_ms: started.elapsed().as_millis(),
                        error: Some(error.to_string()),
                    }));
                }
            }
        }
        Some(total)
    };
    if size.is_some_and(|size| !filters.accepts_size(size)) {
        return Ok(None);
    }

    Ok(Some(ProbeResult {
        word: word.to_owned(),
        url: url.to_string(),
        status: Some(status),
        size,
        elapsed_ms: started.elapsed().as_millis(),
        error: None,
    }))
}

/// Signature: `fn redirect_target(response, current) -> Option<Url>`
///
/// Purpose: Resolves a supported redirect response into the next HTTP(S) URL.
///
/// Parameters:
/// - `response`: Response whose status and `Location` header are inspected.
/// - `current`: Current URL used to resolve relative `Location` values.
///
/// Returns: `Some(url)` for valid supported HTTP(S) redirects, otherwise `None`.
///
/// Notes: Unsupported schemes and malformed `Location` values are ignored rather
/// than emitted as scanner errors.
fn redirect_target(response: &Response, current: &Url) -> Option<Url> {
    if !matches!(
        response.status(),
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    ) {
        return None;
    }
    let location = response.headers().get(LOCATION)?.to_str().ok()?;
    let next = current.join(location).ok()?;
    matches!(next.scheme(), "http" | "https").then_some(next)
}

/// Signature: `fn same_authority(left, right) -> bool`
///
/// Purpose: Checks whether two URLs share the same redirect authority.
///
/// Parameters:
/// - `left`: Original or current URL.
/// - `right`: Redirect target URL.
///
/// Returns: `true` when host and effective port match.
///
/// Notes: Scheme differences do not make authorities different for this check;
/// credential leakage protection is based on host and port.
fn same_authority(left: &Url, right: &Url) -> bool {
    left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Signature: `fn detects_changed_redirect_authority()`
    ///
    /// Purpose: Verifies default ports compare equal and explicit different
    /// ports compare unequal.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: This supports sensitive-header handling across redirects.
    #[test]
    fn detects_changed_redirect_authority() {
        let initial = Url::parse("https://example.test:443/start").expect("valid URL");
        let same = Url::parse("https://example.test/next").expect("valid URL");
        let changed = Url::parse("https://example.test:8443/next").expect("valid URL");

        assert!(same_authority(&initial, &same));
        assert!(!same_authority(&initial, &changed));
    }
}
