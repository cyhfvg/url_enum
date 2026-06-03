use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::{Client, Method, Proxy, redirect::Policy};
use url::Url;

use crate::cli::{Args, HttpMethod};

pub(super) fn build_client(args: &Args) -> Result<Client> {
    let mut builder = Client::builder()
        // Redirects are handled by `probe` so each received response can be emitted.
        .redirect(Policy::none())
        .danger_accept_invalid_certs(args.insecure)
        .user_agent(&args.user_agent)
        .timeout(Duration::from_secs(args.timeout))
        .connect_timeout(Duration::from_secs(args.timeout))
        .pool_max_idle_per_host(args.concurrency)
        .tcp_keepalive(Duration::from_secs(30));
    if let Some(proxy) = build_proxy(args)? {
        builder = builder.proxy(proxy);
    }
    builder.build().context("failed to create HTTP client")
}

fn build_proxy(args: &Args) -> Result<Option<Proxy>> {
    let Some(value) = args.proxy.as_deref() else {
        return Ok(None);
    };
    let url = Url::parse(value).context("failed to parse proxy URL")?;
    if !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h") {
        bail!("proxy URL supports only http, https, socks5, or socks5h schemes");
    }
    if url.host_str().is_none() {
        bail!("proxy URL must include a host");
    }

    let proxy = Proxy::all(url).context("failed to create proxy configuration")?;
    Ok(Some(proxy))
}

pub(super) fn method_for(method: HttpMethod) -> Method {
    match method {
        HttpMethod::Get => Method::GET,
        HttpMethod::Head => Method::HEAD,
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn accepts_supported_proxy_urls_with_embedded_credentials() {
        for proxy_url in [
            "http://127.0.0.1:8080",
            "https://proxy-user:secret@proxy.example.test:8443",
            "socks5://proxy-user:secret@127.0.0.1:1080",
            "socks5h://127.0.0.1:1080",
        ] {
            let args = Args::try_parse_from([
                "url_enum",
                "-t",
                "https://example.test",
                "-d",
                "dict.txt",
                "--proxy",
                proxy_url,
            ])
            .expect("valid proxy arguments");

            assert!(build_proxy(&args).expect("valid proxy").is_some());
        }
    }

    #[test]
    fn rejects_unsupported_proxy_protocol() {
        let unsupported = Args::try_parse_from([
            "url_enum",
            "-t",
            "https://example.test",
            "-d",
            "dict.txt",
            "--proxy",
            "ftp://127.0.0.1:2121",
        ])
        .expect("CLI accepts proxy value for scanner validation");
        let error = build_proxy(&unsupported).expect_err("unsupported proxy must fail");
        assert!(error.to_string().contains("supports only"));
    }
}
