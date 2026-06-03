use anyhow::{Context, Result, bail};
use url::{Position, Url};

#[derive(Debug)]
pub(super) enum UrlGenerator {
    Append(Url),
    Fixed(Url),
    Replace { template: String, token: String },
}

impl UrlGenerator {
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

    #[test]
    fn appends_dictionary_path_after_target_path() {
        let generator = UrlGenerator::new("https://example.test/root?x=1".to_owned(), None, false)
            .expect("valid generator");

        let url = generator.build("api/v1").expect("valid candidate");

        assert_eq!(url.as_str(), "https://example.test/root/api/v1?x=1");
    }

    #[test]
    fn appending_path_preserves_existing_url_escaping() {
        let generator = UrlGenerator::new("https://example.test/a%20b/".to_owned(), None, false)
            .expect("valid generator");

        let url = generator.build("admin").expect("valid candidate");

        assert_eq!(url.as_str(), "https://example.test/a%20b/admin");
    }

    #[test]
    fn appending_path_preserves_dictionary_percent_encoding() {
        let generator = UrlGenerator::new("https://example.test/root".to_owned(), None, false)
            .expect("valid target");

        let url = generator
            .build("a%20b/next%2Fpart")
            .expect("valid encoded candidate");

        assert_eq!(url.as_str(), "https://example.test/root/a%20b/next%2Fpart");
    }

    #[test]
    fn appending_path_preserves_non_utf8_percent_encoded_bytes() {
        let generator = UrlGenerator::new("https://example.test/root".to_owned(), None, false)
            .expect("valid target");

        let url = generator.build("binary%FFname").expect("valid raw path");

        assert_eq!(url.as_str(), "https://example.test/root/binary%FFname");
    }

    #[test]
    fn appending_parent_segment_follows_url_resolution_rules() {
        let generator = UrlGenerator::new("https://example.test/root/base".to_owned(), None, false)
            .expect("valid target");

        let url = generator.build("..").expect("valid parent segment");

        assert_eq!(url.as_str(), "https://example.test/root/");
    }

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

    #[test]
    fn rejects_http_url_without_a_host() {
        let error = parse_http_url("http:/").expect_err("hostless target must fail");

        assert!(error.to_string().contains("host") || error.to_string().contains("parse"));
    }
}
