use anyhow::{Context, Result, bail};
use url::{Position, Url};

#[derive(Debug)]
pub(super) enum UrlGenerator {
    Append(Url),
    Fixed(Url),
    Replace { template: String, token: String },
}

impl UrlGenerator {
    /// Signature: `fn new(target, replace, header_has_token) -> Result<Self>`
    ///
    /// Purpose: Builds the URL generation strategy for one target.
    ///
    /// Parameters:
    /// - `target`: Raw target URL or URL template.
    /// - `replace`: Optional replacement token.
    /// - `header_has_token`: Whether any configured header contains the token.
    ///
    /// Returns: A [`UrlGenerator`] that appends dictionary words, substitutes a
    /// URL token, or keeps a fixed URL for header-only substitution.
    ///
    /// Errors: Returns an error if the target is not a valid HTTP(S) URL, the
    /// replacement token is empty, or the token appears in neither URL nor
    /// headers.
    ///
    /// Notes: Header-only replacement intentionally avoids appending dictionary
    /// words to the URL.
    pub(super) fn new(
        target: String,
        replace: Option<String>,
        header_has_token: bool,
    ) -> Result<Self> {
        let parsed_target = parse_http_url(&target)?;

        match replace {
            Some(token) => {
                if token.is_empty() {
                    bail!("replacement token cannot be empty");
                }
                if target.contains(&token) {
                    Ok(Self::Replace {
                        template: target,
                        token,
                    })
                } else if header_has_token {
                    Ok(Self::Fixed(parsed_target))
                } else {
                    bail!(
                        "replacement token `{token}` was not found in the target URL or HTTP headers"
                    );
                }
            }
            None => Ok(Self::Append(parsed_target)),
        }
    }

    /// Signature: `fn build(&self, word) -> Result<Url>`
    ///
    /// Purpose: Renders the candidate URL for one dictionary word.
    ///
    /// Parameters:
    /// - `word`: Candidate dictionary word after extension expansion.
    ///
    /// Returns: A validated HTTP(S) [`Url`].
    ///
    /// Errors: Returns an error if appended or substituted URL text cannot be
    /// parsed as HTTP(S).
    ///
    /// Notes: Append mode trims leading slashes from words so dictionary entries
    /// are resolved below the target path rather than replacing it.
    pub(super) fn build(&self, word: &str) -> Result<Url> {
        match self {
            Self::Append(base) => {
                let word = word.trim_start_matches('/');
                append_path(base, word)
            }
            Self::Fixed(target) => Ok(target.clone()),
            Self::Replace { template, token } => {
                let value = template.replace(token, word);
                parse_http_url(&value)
                    .with_context(|| format!("word `{word}` produced an invalid URL"))
            }
        }
    }
}

/// Signature: `fn append_path(base, word) -> Result<Url>`
///
/// Purpose: Appends one dictionary word to the target path while preserving the
/// original query and fragment suffix.
///
/// Parameters:
/// - `base`: Parsed base target URL.
/// - `word`: Candidate path segment or nested path to append.
///
/// Returns: A validated HTTP(S) [`Url`].
///
/// Errors: Returns an error if the composed URL cannot be parsed as HTTP(S).
///
/// Notes: Literal `?` and `#` characters from dictionary words are escaped so
/// they remain path bytes instead of starting a query or fragment.
fn append_path(base: &Url, word: &str) -> Result<Url> {
    let before_suffix = &base[..Position::AfterPath];
    let suffix = &base[Position::AfterPath..];
    let before_suffix = before_suffix.strip_suffix('/').unwrap_or(before_suffix);
    let mut value = String::with_capacity(base.as_str().len() + word.len() + 1);
    value.push_str(before_suffix);
    value.push('/');
    for character in word.chars() {
        match character {
            '?' => value.push_str("%3F"),
            '#' => value.push_str("%23"),
            _ => value.push(character),
        }
    }
    value.push_str(suffix);
    parse_http_url(&value)
}

/// Signature: `fn parse_http_url(value) -> Result<Url>`
///
/// Purpose: Parses and validates a target or rendered candidate URL.
///
/// Parameters:
/// - `value`: URL string to parse.
///
/// Returns: A parsed [`Url`] with an `http` or `https` scheme and a host.
///
/// Errors: Returns an error for malformed URLs, unsupported schemes, or missing
/// hosts.
///
/// Notes: Centralizing URL validation keeps target parsing and replacement
/// rendering under the same rules.
pub(super) fn parse_http_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("failed to parse target URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("target URL supports only http or https schemes");
    }
    if url.host_str().is_none() {
        bail!("target URL must include a host");
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use reqwest::Client;

    use super::*;
    use crate::scanner::headers::RequestHeaders;

    /// Signature: `fn appends_dictionary_path_after_target_path()`
    ///
    /// Purpose: Verifies append mode places dictionary entries under the target
    /// path.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: Existing query strings must remain attached after the appended
    /// path.
    #[test]
    fn appends_dictionary_path_after_target_path() {
        let generator = UrlGenerator::new("https://example.test/root?x=1".to_owned(), None, false)
            .expect("valid generator");

        let url = generator.build("api/v1").expect("valid candidate");

        assert_eq!(url.as_str(), "https://example.test/root/api/v1?x=1");
    }

    /// Signature: `fn appending_path_preserves_existing_url_escaping()`
    ///
    /// Purpose: Verifies existing percent-escaped target path bytes survive URL
    /// building.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: Avoids accidental decode-then-reencode drift.
    #[test]
    fn appending_path_preserves_existing_url_escaping() {
        let generator = UrlGenerator::new("https://example.test/a%20b/".to_owned(), None, false)
            .expect("valid generator");

        let url = generator.build("admin").expect("valid candidate");

        assert_eq!(url.as_str(), "https://example.test/a%20b/admin");
    }

    /// Signature: `fn appending_path_preserves_dictionary_percent_encoding()`
    ///
    /// Purpose: Verifies dictionary-provided percent escapes remain intact.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: This permits wordlists to contain intentionally encoded path
    /// bytes.
    #[test]
    fn appending_path_preserves_dictionary_percent_encoding() {
        let generator = UrlGenerator::new("https://example.test/root".to_owned(), None, false)
            .expect("valid target");

        let url = generator
            .build("a%20b/next%2Fpart")
            .expect("valid encoded candidate");

        assert_eq!(url.as_str(), "https://example.test/root/a%20b/next%2Fpart");
    }

    /// Signature: `fn appending_path_preserves_non_utf8_percent_encoded_bytes()`
    ///
    /// Purpose: Verifies non-UTF-8 percent-encoded dictionary bytes are
    /// preserved.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: Wordlists may target raw byte paths.
    #[test]
    fn appending_path_preserves_non_utf8_percent_encoded_bytes() {
        let generator = UrlGenerator::new("https://example.test/root".to_owned(), None, false)
            .expect("valid target");

        let url = generator.build("binary%FFname").expect("valid raw path");

        assert_eq!(url.as_str(), "https://example.test/root/binary%FFname");
    }

    /// Signature: `fn appending_parent_segment_follows_url_resolution_rules()`
    ///
    /// Purpose: Verifies appended parent segments are normalized by URL parsing.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: This documents the current reliance on standard URL path
    /// resolution.
    #[test]
    fn appending_parent_segment_follows_url_resolution_rules() {
        let generator = UrlGenerator::new("https://example.test/root/base".to_owned(), None, false)
            .expect("valid target");

        let url = generator.build("..").expect("valid parent segment");

        assert_eq!(url.as_str(), "https://example.test/root/");
    }

    /// Signature: `fn replaces_enum_placeholder()`
    ///
    /// Purpose: Verifies token replacement across URL path and query text.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: Replacement mode does not append the dictionary word as a path
    /// suffix.
    #[test]
    fn replaces_enum_placeholder() {
        let generator = UrlGenerator::new(
            "https://example.test/ENUM/index?name=ENUM".to_owned(),
            Some("ENUM".to_owned()),
            false,
        )
        .expect("valid generator");

        let url = generator.build("admin").expect("valid candidate");

        assert_eq!(url.as_str(), "https://example.test/admin/index?name=admin");
    }

    /// Signature: `fn replaces_same_candidate_in_url_and_header_name_and_value()`
    ///
    /// Purpose: Verifies the same candidate word is applied to URL and header
    /// templates.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: This covers cross-module replacement coordination.
    #[test]
    fn replaces_same_candidate_in_url_and_header_name_and_value() {
        let headers =
            RequestHeaders::parse(&["X-ENUM-TRACE: ENUM.example.com".to_owned()], Some("ENUM"))
                .expect("valid headers");
        let generator = UrlGenerator::new(
            "http://example.com/ENUM/a".to_owned(),
            Some("ENUM".to_owned()),
            headers.has_token(),
        )
        .expect("valid replacement generator");
        let url = generator.build("word1").expect("valid candidate URL");
        let request = headers
            .apply(Client::new().get(url), true, "word1")
            .expect("valid rendered headers")
            .build()
            .expect("valid request");

        assert_eq!(request.url().as_str(), "http://example.com/word1/a");
        assert_eq!(
            request
                .headers()
                .get("x-word1-trace")
                .expect("replaced header"),
            "word1.example.com"
        );
    }

    /// Signature: `fn permits_replace_token_only_in_header_value_without_appending_path()`
    ///
    /// Purpose: Verifies header-value-only replacement leaves the target URL
    /// fixed.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: This supports virtual-host or header fuzzing workflows.
    #[test]
    fn permits_replace_token_only_in_header_value_without_appending_path() {
        let headers = RequestHeaders::parse(&["Host: ENUM.example.test".to_owned()], Some("ENUM"))
            .expect("valid headers");
        let generator = UrlGenerator::new(
            "https://example.test/base".to_owned(),
            Some("ENUM".to_owned()),
            headers.has_token(),
        )
        .expect("valid header-only replacement");

        let url = generator.build("admin").expect("unchanged URL");

        assert_eq!(url.as_str(), "https://example.test/base");
    }

    /// Signature: `fn permits_replace_token_only_in_header_name_without_appending_path()`
    ///
    /// Purpose: Verifies header-name-only replacement leaves the target URL
    /// fixed.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: Dynamic header names share the same candidate word as URL
    /// replacement mode.
    #[test]
    fn permits_replace_token_only_in_header_name_without_appending_path() {
        let headers = RequestHeaders::parse(&["X-ENUM-Trace: fixed".to_owned()], Some("ENUM"))
            .expect("valid headers");
        let generator = UrlGenerator::new(
            "https://example.test/base".to_owned(),
            Some("ENUM".to_owned()),
            headers.has_token(),
        )
        .expect("valid header-name-only replacement");
        let url = generator.build("admin").expect("unchanged URL");
        let request = headers
            .apply(Client::new().get(url), true, "admin")
            .expect("valid replacement")
            .build()
            .expect("valid request");

        assert_eq!(request.url().as_str(), "https://example.test/base");
        assert_eq!(
            request
                .headers()
                .get("x-admin-trace")
                .expect("replaced header"),
            "fixed"
        );
    }

    /// Signature: `fn rejects_replace_token_missing_from_url_and_headers()`
    ///
    /// Purpose: Ensures replacement mode fails when the token is unused.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: Prevents users from accidentally scanning a fixed URL repeatedly.
    #[test]
    fn rejects_replace_token_missing_from_url_and_headers() {
        let error = UrlGenerator::new(
            "https://example.test/base".to_owned(),
            Some("ENUM".to_owned()),
            false,
        )
        .expect_err("missing replacement token must fail");

        assert!(error.to_string().contains("target URL or HTTP headers"));
    }

    /// Signature: `fn rejects_http_url_without_a_host()`
    ///
    /// Purpose: Verifies target URL validation rejects hostless HTTP URLs.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: The exact error may come from parsing or scanner host validation.
    #[test]
    fn rejects_http_url_without_a_host() {
        let error = parse_http_url("http:/").expect_err("hostless target must fail");

        assert!(error.to_string().contains("host") || error.to_string().contains("parse"));
    }
}
