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

fn same_authority(left: &Url, right: &Url) -> bool {
    left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_changed_redirect_authority() {
        let initial = Url::parse("https://example.test:443/start").expect("valid URL");
        let same = Url::parse("https://example.test/next").expect("valid URL");
        let changed = Url::parse("https://example.test:8443/next").expect("valid URL");

        assert!(same_authority(&initial, &same));
        assert!(!same_authority(&initial, &changed));
    }
}
