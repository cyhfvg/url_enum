use anyhow::{Context, Result, bail};
use reqwest::{
    RequestBuilder,
    header::{
        AUTHORIZATION, COOKIE, HeaderName, HeaderValue, PROXY_AUTHORIZATION, WWW_AUTHENTICATE,
    },
};

#[derive(Debug)]
enum RequestHeaderName {
    Literal(HeaderName),
    Replace { template: String, token: String },
}

impl RequestHeaderName {
    /// Signature: `fn render(&self, word) -> Result<HeaderName>`
    ///
    /// Purpose: Renders a stored header-name template for one dictionary word.
    ///
    /// Parameters:
    /// - `word`: Candidate word used when the name contains the replacement
    ///   token.
    ///
    /// Returns: A valid reqwest [`HeaderName`].
    ///
    /// Errors: Returns an error if token replacement produces an invalid HTTP
    /// header name.
    ///
    /// Notes: Literal names are parsed once during configuration and cloned per
    /// request.
    fn render(&self, word: &str) -> Result<HeaderName> {
        match self {
            Self::Literal(name) => Ok(name.clone()),
            Self::Replace { template, token } => {
                let value = template.replace(token, word);
                HeaderName::from_bytes(value.as_bytes()).with_context(|| {
                    format!("word `{word}` produced invalid HTTP header name `{value}`")
                })
            }
        }
    }
}

#[derive(Debug)]
enum RequestHeaderValue {
    Literal(HeaderValue),
    Replace { template: String, token: String },
}

impl RequestHeaderValue {
    /// Signature: `fn render(&self, word) -> Result<HeaderValue>`
    ///
    /// Purpose: Renders a stored header-value template for one dictionary word.
    ///
    /// Parameters:
    /// - `word`: Candidate word used when the value contains the replacement
    ///   token.
    ///
    /// Returns: A valid reqwest [`HeaderValue`].
    ///
    /// Errors: Returns an error if token replacement produces an invalid HTTP
    /// header value.
    ///
    /// Notes: Literal values are parsed once during configuration and cloned per
    /// request.
    fn render(&self, word: &str) -> Result<HeaderValue> {
        match self {
            Self::Literal(value) => Ok(value.clone()),
            Self::Replace { template, token } => {
                HeaderValue::from_str(&template.replace(token, word))
                    .with_context(|| format!("word `{word}` produced an invalid HTTP header value"))
            }
        }
    }
}

#[derive(Debug)]
struct RequestHeader {
    name: RequestHeaderName,
    value: RequestHeaderValue,
}

#[derive(Debug, Default)]
pub(super) struct RequestHeaders {
    values: Vec<RequestHeader>,
    has_token: bool,
}

impl RequestHeaders {
    /// Signature: `fn parse(headers, replacement) -> Result<Self>`
    ///
    /// Purpose: Parses CLI header strings and records token-aware templates for
    /// dynamic names or values.
    ///
    /// Parameters:
    /// - `headers`: Raw `Name: value` header strings from CLI parsing.
    /// - `replacement`: Optional token to substitute with each dictionary word.
    ///
    /// Returns: Parsed request headers plus a flag indicating whether any header
    /// contains the replacement token.
    ///
    /// Errors: Returns an error for malformed header syntax, empty names, or
    /// invalid literal header names and values.
    ///
    /// Notes: Repeated headers are preserved in input order.
    pub(super) fn parse(headers: &[String], replacement: Option<&str>) -> Result<Self> {
        let mut parsed = Vec::with_capacity(headers.len());
        let mut has_token = false;
        for header in headers {
            let (name, value) = header.split_once(':').with_context(|| {
                format!("invalid HTTP header `{header}`, expected `Name: value`")
            })?;
            let name = name.trim();
            if name.is_empty() {
                bail!("invalid HTTP header `{header}`, header name cannot be empty");
            }
            let value = value.trim();
            let name = if let Some(token) = replacement.filter(|token| name.contains(token)) {
                has_token = true;
                RequestHeaderName::Replace {
                    template: name.to_owned(),
                    token: token.to_owned(),
                }
            } else {
                RequestHeaderName::Literal(
                    HeaderName::from_bytes(name.as_bytes())
                        .with_context(|| format!("invalid HTTP header name `{name}`"))?,
                )
            };
            let value = if let Some(token) = replacement.filter(|token| value.contains(token)) {
                has_token = true;
                RequestHeaderValue::Replace {
                    template: value.to_owned(),
                    token: token.to_owned(),
                }
            } else {
                RequestHeaderValue::Literal(
                    HeaderValue::from_str(value)
                        .with_context(|| format!("invalid HTTP header value `{header}`"))?,
                )
            };
            parsed.push(RequestHeader { name, value });
        }
        Ok(Self {
            values: parsed,
            has_token,
        })
    }

    /// Signature: `fn has_token(&self) -> bool`
    ///
    /// Purpose: Reports whether any configured header depends on the replacement
    /// token.
    ///
    /// Parameters: None.
    ///
    /// Returns: `true` when a header name or value contains the token.
    ///
    /// Notes: URL generation uses this to permit header-only replacement mode.
    pub(super) fn has_token(&self) -> bool {
        self.has_token
    }

    /// Signature: `fn apply(&self, request, include_sensitive, word) -> Result<RequestBuilder>`
    ///
    /// Purpose: Applies configured headers to a request builder for one
    /// candidate word.
    ///
    /// Parameters:
    /// - `request`: Request builder to decorate.
    /// - `include_sensitive`: Whether sensitive headers may be attached.
    /// - `word`: Candidate word used for token replacement.
    ///
    /// Returns: The updated reqwest [`RequestBuilder`].
    ///
    /// Errors: Returns an error if dynamic header rendering produces an invalid
    /// name or value.
    ///
    /// Notes: Sensitive headers are omitted after cross-authority redirects to
    /// avoid leaking credentials.
    pub(super) fn apply(
        &self,
        mut request: RequestBuilder,
        include_sensitive: bool,
        word: &str,
    ) -> Result<RequestBuilder> {
        for header in &self.values {
            let name = header.name.render(word)?;
            if !include_sensitive && is_sensitive_redirect_header(&name) {
                continue;
            }
            let value = header.value.render(word).with_context(|| {
                format!("word `{word}` produced invalid HTTP header value for `{name}`")
            })?;
            request = request.header(name, value);
        }
        Ok(request)
    }
}

/// Signature: `fn is_sensitive_redirect_header(name) -> bool`
///
/// Purpose: Identifies headers that should not be forwarded across authorities.
///
/// Parameters:
/// - `name`: Header name to classify.
///
/// Returns: `true` for authentication or cookie-style headers.
///
/// Notes: Includes `Cookie2` case-insensitively for compatibility with older
/// clients and servers.
fn is_sensitive_redirect_header(name: &HeaderName) -> bool {
    name == AUTHORIZATION
        || name == COOKIE
        || name == PROXY_AUTHORIZATION
        || name == WWW_AUTHENTICATE
        || name.as_str().eq_ignore_ascii_case("cookie2")
}

#[cfg(test)]
mod tests {
    use reqwest::Client;

    use super::*;

    /// Signature: `fn parses_and_appends_repeated_headers_in_value_order()`
    ///
    /// Purpose: Verifies repeated headers preserve their configured order.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: Also checks that literal authorization headers survive normal
    /// same-authority requests.
    #[test]
    fn parses_and_appends_repeated_headers_in_value_order() {
        let headers = RequestHeaders::parse(
            &[
                "X-Trace: first".to_owned(),
                "X-Trace: second".to_owned(),
                "Authorization: Bearer token:a".to_owned(),
            ],
            None,
        )
        .expect("valid headers");
        let request = headers
            .apply(Client::new().get("https://example.test"), true, "admin")
            .expect("valid rendered headers")
            .build()
            .expect("valid request");
        let trace_values: Vec<&str> = request
            .headers()
            .get_all("x-trace")
            .iter()
            .map(|value| value.to_str().expect("ASCII header value"))
            .collect();

        assert_eq!(trace_values, vec!["first", "second"]);
        assert_eq!(
            request
                .headers()
                .get("authorization")
                .expect("authorization"),
            "Bearer token:a"
        );
    }

    /// Signature: `fn rejects_header_without_a_colon()`
    ///
    /// Purpose: Ensures malformed header CLI values are rejected.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: The scanner requires the exact `Name: value` shape.
    #[test]
    fn rejects_header_without_a_colon() {
        let error = RequestHeaders::parse(&["Authorization token".to_owned()], None)
            .expect_err("invalid header");

        assert!(error.to_string().contains("Name: value"));
    }

    /// Signature: `fn omits_sensitive_headers_after_cross_authority_redirect()`
    ///
    /// Purpose: Verifies static sensitive headers are dropped after a
    /// cross-authority redirect.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: Non-sensitive headers should still be preserved.
    #[test]
    fn omits_sensitive_headers_after_cross_authority_redirect() {
        let headers = RequestHeaders::parse(
            &[
                "Authorization: Bearer secret".to_owned(),
                "Cookie: session=secret".to_owned(),
                "X-Trace: safe".to_owned(),
            ],
            None,
        )
        .expect("valid headers");
        let request = headers
            .apply(
                Client::new().get("https://other.example.test"),
                false,
                "admin",
            )
            .expect("valid rendered headers")
            .build()
            .expect("valid request");

        assert!(!request.headers().contains_key(AUTHORIZATION));
        assert!(!request.headers().contains_key(COOKIE));
        assert_eq!(request.headers().get("x-trace").expect("trace"), "safe");
    }

    /// Signature: `fn replaces_token_in_header_values_for_each_candidate()`
    ///
    /// Purpose: Verifies token replacement in header names and values.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: This test covers per-candidate rendering rather than parse-time
    /// substitution.
    #[test]
    fn replaces_token_in_header_values_for_each_candidate() {
        let headers = RequestHeaders::parse(
            &[
                "Host: ENUM.example.test".to_owned(),
                "X-Path: /api/ENUM".to_owned(),
            ],
            Some("ENUM"),
        )
        .expect("valid headers");
        let request = headers
            .apply(Client::new().get("https://example.test"), true, "admin")
            .expect("valid replacement")
            .build()
            .expect("valid request");

        assert!(headers.has_token());
        assert_eq!(
            request.headers().get("host").expect("host header"),
            "admin.example.test"
        );
        assert_eq!(
            request.headers().get("x-path").expect("path header"),
            "/api/admin"
        );
    }

    /// Signature: `fn omits_dynamically_named_sensitive_headers_after_cross_authority_redirect()`
    ///
    /// Purpose: Verifies dynamically rendered sensitive header names are also
    /// filtered after cross-authority redirects.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: The check happens after rendering so replacement-generated names
    /// receive the same protection as literal names.
    #[test]
    fn omits_dynamically_named_sensitive_headers_after_cross_authority_redirect() {
        let headers = RequestHeaders::parse(&["ENUM: secret".to_owned()], Some("ENUM"))
            .expect("valid headers");
        let request = headers
            .apply(
                Client::new().get("https://other.example.test"),
                false,
                "Authorization",
            )
            .expect("valid replacement")
            .build()
            .expect("valid request");

        assert!(!request.headers().contains_key(AUTHORIZATION));
    }
}
