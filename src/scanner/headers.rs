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

    pub(super) fn has_token(&self) -> bool {
        self.has_token
    }

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

    #[test]
    fn rejects_header_without_a_colon() {
        let error = RequestHeaders::parse(&["Authorization token".to_owned()], None)
            .expect_err("invalid header");

        assert!(error.to_string().contains("Name: value"));
    }

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
