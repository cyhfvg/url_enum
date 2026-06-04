use clap::Parser;
use url_enum::cli::Args;

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
