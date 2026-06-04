use clap::Parser;
use url_enum::cli::Args;

/// Signature: `fn parses_request_jitter_milliseconds()`
///
/// Purpose: Verifies the CLI accepts millisecond request jitter values.
///
/// Parameters: None.
///
/// Returns: Nothing; assertions define success.
///
/// Notes: Keeps argument parsing coverage close to the public CLI surface.
#[test]
fn parses_request_jitter_milliseconds() {
    let args = Args::try_parse_from([
        "url_enum",
        "-t",
        "https://example.test",
        "-d",
        "dict.txt",
        "--request-jitter-ms",
        "250",
    ])
    .expect("valid jitter arguments");

    assert_eq!(args.request_jitter_ms, 250);
}

/// Signature: `fn parses_random_sequence_flag()`
///
/// Purpose: Verifies the random sequencing flag is parsed as a boolean switch.
///
/// Parameters: None.
///
/// Returns: Nothing; assertions define success.
///
/// Notes: The scanner uses this flag to choose between streaming and preloaded
/// dictionary modes.
#[test]
fn parses_random_sequence_flag() {
    let args = Args::try_parse_from([
        "url_enum",
        "-t",
        "https://example.test",
        "-d",
        "dict.txt",
        "--random-sequence",
    ])
    .expect("valid random sequence arguments");

    assert!(args.random_sequence);
}

/// Signature: `fn rejects_removed_proxy_short_and_credentials_options()`
///
/// Purpose: Verifies removed proxy-related CLI options are no longer accepted.
///
/// Parameters: None.
///
/// Returns: Nothing; assertions define success.
///
/// Notes: Proxy credentials should be embedded in the proxy URL instead of
/// passed through separate flags.
#[test]
fn rejects_removed_proxy_short_and_credentials_options() {
    let short_proxy = Args::try_parse_from([
        "url_enum",
        "-t",
        "https://example.test",
        "-d",
        "dict.txt",
        "-x",
        "http://127.0.0.1:8080",
    ]);
    assert!(short_proxy.is_err());

    for credentials_option in ["-U", "--proxy-user"] {
        let separate_credentials = Args::try_parse_from([
            "url_enum",
            "-t",
            "https://example.test",
            "-d",
            "dict.txt",
            "--proxy",
            "http://127.0.0.1:8080",
            credentials_option,
            "analyst:password",
        ]);
        assert!(separate_credentials.is_err());
    }
}
