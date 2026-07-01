use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_stream::try_stream;
use futures::Stream;
use rand::seq::SliceRandom;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

use super::{Candidate, UrlGenerator};

/// Signature: `fn candidate_stream(file, generator, extensions) -> impl Stream<Item = Result<Candidate>>`
///
/// Purpose: Lazily reads a dictionary and emits deduplicated candidates for a
/// single target URL generator.
///
/// Parameters:
/// - `file`: Open dictionary file to read line by line.
/// - `generator`: URL generator used to render each expanded dictionary word.
/// - `extensions`: Normalized extension suffixes to append to each word.
///
/// Returns: A stream of [`Candidate`] values wrapped in [`Result`].
///
/// Errors: Stream items fail if the dictionary cannot be read or a word cannot
/// be rendered into a valid URL.
///
/// Notes: Deduplication is scoped to this dictionary pass so repeated words and
/// repeated extension expansions are emitted only once for the target.
pub(super) fn candidate_stream(
    file: File,
    generator: Arc<UrlGenerator>,
    extensions: Vec<String>,
) -> impl Stream<Item = Result<Candidate>> {
    try_stream! {
        let mut lines = BufReader::new(file).lines();
        let mut seen = HashSet::new();

        while let Some(line) = lines.next_line().await.context("failed to read dictionary")? {
            let word = line.trim();
            if word.is_empty() {
                continue;
            }
            for candidate_word in expanded_words(word, &extensions) {
                if seen.insert(candidate_word.clone()) {
                    let url = generator.build(&candidate_word)?;
                    yield Candidate {
                        word: candidate_word,
                        url,
                    };
                }
            }
        }
    }
}

/// Signature: `fn target_dictionary_stream(first_file, dictionary_path, output, generators, extensions) -> impl Stream<Item = Result<Candidate>>`
///
/// Purpose: Streams candidates for multiple targets in target-major order while
/// rereading the dictionary once per target.
///
/// Parameters:
/// - `first_file`: Already opened dictionary handle for the first target.
/// - `dictionary_path`: Path used to reopen the dictionary for later targets.
/// - `output`: Optional output path used to prevent dictionary overwrite.
/// - `generators`: Ordered URL generators, one per target.
/// - `extensions`: Normalized extension suffixes to append to each word.
///
/// Returns: A stream of [`Candidate`] values wrapped in [`Result`].
///
/// Errors: Stream items fail if the dictionary cannot be reopened or read, if
/// the output path aliases the dictionary, or if URL generation fails.
///
/// Notes: Keeping the dictionary on disk avoids loading large wordlists when the
/// caller does not request randomized sequencing.
pub(super) fn target_dictionary_stream(
    first_file: File,
    dictionary_path: PathBuf,
    output: Option<String>,
    generators: Arc<[UrlGenerator]>,
    extensions: Vec<String>,
) -> impl Stream<Item = Result<Candidate>> {
    try_stream! {
        let mut first_file = Some(first_file);

        for generator in generators.iter() {
            let file = match first_file.take() {
                Some(file) => file,
                None => open_dictionary(&dictionary_path, output.as_deref()).await?,
            };
            let mut lines = BufReader::new(file).lines();
            let mut seen = HashSet::new();

            while let Some(line) = lines.next_line().await.context("failed to read dictionary")? {
                let word = line.trim();
                if word.is_empty() {
                    continue;
                }
                for candidate_word in expanded_words(word, &extensions) {
                    if seen.insert(candidate_word.clone()) {
                        let url = generator.build(&candidate_word)?;
                        yield Candidate {
                            word: candidate_word,
                            url,
                        };
                    }
                }
            }
        }
    }
}

/// Signature: `async fn read_dictionary_words(file, extensions) -> Result<Vec<String>>`
///
/// Purpose: Reads the full dictionary into a deduplicated, extension-expanded
/// word list.
///
/// Parameters:
/// - `file`: Open dictionary file to read line by line.
/// - `extensions`: Normalized extension suffixes to append to each word.
///
/// Returns: A vector containing each base word and extension variant in input
/// order, with duplicates removed.
///
/// Errors: Returns an error when the dictionary cannot be read.
///
/// Notes: This is used by randomized sequencing, which needs the complete
/// target-by-word product before shuffling.
pub(super) async fn read_dictionary_words(
    file: File,
    extensions: &[String],
) -> Result<Vec<String>> {
    let mut lines = BufReader::new(file).lines();
    let mut seen = HashSet::new();
    let mut words = Vec::new();

    while let Some(line) = lines
        .next_line()
        .await
        .context("failed to read dictionary")?
    {
        let word = line.trim();
        if word.is_empty() {
            continue;
        }
        for candidate_word in expanded_words(word, extensions) {
            if seen.insert(candidate_word.clone()) {
                words.push(candidate_word);
            }
        }
    }

    Ok(words)
}

/// Signature: `fn target_word_stream(generators, words, random_sequence) -> impl Stream<Item = Result<Candidate>>`
///
/// Purpose: Emits candidates from preloaded words across all targets, optionally
/// shuffling the full target-word product.
///
/// Parameters:
/// - `generators`: Ordered URL generators, one per target.
/// - `words`: Deduplicated dictionary words and extension variants.
/// - `random_sequence`: Whether to shuffle the full request sequence.
///
/// Returns: A stream of [`Candidate`] values wrapped in [`Result`].
///
/// Errors: Stream items fail if the target-word product overflows `usize` or if
/// URL generation fails.
///
/// Notes: Non-random mode preserves target-major order to match the streaming
/// dictionary path.
pub(super) fn target_word_stream(
    generators: Arc<[UrlGenerator]>,
    words: Arc<[String]>,
    random_sequence: bool,
) -> impl Stream<Item = Result<Candidate>> {
    try_stream! {
        if random_sequence {
            let mut sequence = target_word_indices(generators.len(), words.len())?;
            sequence.shuffle(&mut rand::rng());

            for (target_index, word_index) in sequence {
                let word = words[word_index].clone();
                let url = generators[target_index].build(&word)?;
                yield Candidate { word, url };
            }
        } else {
            for generator in generators.iter() {
                for word in words.iter() {
                    let url = generator.build(word)?;
                    yield Candidate {
                        word: word.clone(),
                        url,
                    };
                }
            }
        }
    }
}

/// Signature: `async fn open_dictionary(path, output) -> Result<File>`
///
/// Purpose: Opens a dictionary file and guards against writing scanner output
/// to the same filesystem object.
///
/// Parameters:
/// - `path`: Dictionary file path.
/// - `output`: Optional output file path supplied by the user.
///
/// Returns: An open Tokio [`File`] for the dictionary.
///
/// Errors: Returns an error if the dictionary cannot be opened, if the output
/// file is the same as the dictionary, or if same-file detection fails for an
/// existing output path.
///
/// Notes: The same-file check catches hard links as well as identical paths.
pub(super) async fn open_dictionary(path: &Path, output: Option<&str>) -> Result<File> {
    let file = File::open(path)
        .await
        .with_context(|| format!("failed to read dictionary file `{}`", path.display()))?;

    if let Some(output) = output {
        match same_file::is_same_file(path, output) {
            Ok(true) => bail!("output file cannot be the same as the dictionary file"),
            Ok(false) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to check whether output file `{output}` would overwrite the dictionary"));
            }
        }
    }

    Ok(file)
}

/// Signature: `async fn read_targets(value) -> Result<Vec<String>>`
///
/// Purpose: Resolves the target argument into one or more target URL strings.
///
/// Parameters:
/// - `value`: A target URL, a target-list file path, or `-` to read a single
///   target from standard input.
///
/// Returns: A non-empty vector of target strings.
///
/// Errors: Returns an error when stdin provides zero or multiple targets, a
/// target-list path is a directory, a target-list file is empty or unreadable,
/// or filesystem inspection fails for a supplied path.
///
/// Notes: A nonexistent path is treated as a literal target URL so ordinary URL
/// strings do not require disambiguation.
pub(super) async fn read_targets(value: &str) -> Result<Vec<String>> {
    if value == "-" {
        let mut input = String::new();
        tokio::io::stdin()
            .read_to_string(&mut input)
            .await
            .context("failed to read target URL from stdin")?;
        let targets = parse_target_lines(&input);
        return match targets.as_slice() {
            [target] => Ok(vec![target.clone()]),
            [] => bail!("stdin did not provide a target URL"),
            _ => bail!("stdin must provide exactly one target URL"),
        };
    }

    if is_http_url_literal(value) {
        return Ok(vec![value.to_owned()]);
    }

    let path = Path::new(value);
    match tokio::fs::metadata(path).await {
        Ok(metadata) => {
            if metadata.is_dir() {
                bail!("target list file cannot be a directory");
            }
            let input = tokio::fs::read_to_string(path)
                .await
                .with_context(|| format!("failed to read target list file `{}`", path.display()))?;
            let targets = parse_target_lines(&input);
            if targets.is_empty() {
                bail!(
                    "target list file `{}` did not provide any targets",
                    path.display()
                );
            }
            Ok(targets)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(vec![value.to_owned()]),
        Err(error) => {
            Err(error).with_context(|| format!("failed to inspect target `{}`", path.display()))
        }
    }
}

/// Signature: `fn is_http_url_literal(value) -> bool`
///
/// Purpose: Identifies target arguments that should be treated as URL text
/// without probing the filesystem first.
///
/// Parameters:
/// - `value`: Raw target argument.
///
/// Returns: `true` for HTTP(S) URL-looking input.
///
/// Notes: Windows rejects URL strings such as `https://host:443` as invalid
/// path syntax during metadata lookup, so URL detection must happen first.
fn is_http_url_literal(value: &str) -> bool {
    let value = value.trim_start();
    value
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
        || value
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
}

/// Signature: `fn normalize_extensions(extensions) -> Vec<String>`
///
/// Purpose: Normalizes user-provided extension suffixes before dictionary
/// expansion.
///
/// Parameters:
/// - `extensions`: Raw extension strings from CLI parsing.
///
/// Returns: Trimmed extensions without leading dots, preserving first-seen
/// order and removing empty or duplicate values.
///
/// Notes: Returned values do not include the separator dot; expansion adds it
/// when building candidate words.
pub(super) fn normalize_extensions(extensions: &[String]) -> Vec<String> {
    let mut seen = HashSet::with_capacity(extensions.len());
    let mut normalized = Vec::with_capacity(extensions.len());
    for extension in extensions {
        let extension = extension.trim().trim_start_matches('.').to_owned();
        if !extension.is_empty() && seen.insert(extension.clone()) {
            normalized.push(extension);
        }
    }
    normalized
}

/// Signature: `fn parse_target_lines(input) -> Vec<String>`
///
/// Purpose: Extracts target entries from raw target-list text.
///
/// Parameters:
/// - `input`: Raw text read from stdin or a target-list file.
///
/// Returns: Trimmed, non-empty target strings in original order.
///
/// Notes: This function performs only line cleanup; URL validation happens when
/// constructing [`UrlGenerator`] values.
fn parse_target_lines(input: &str) -> Vec<String> {
    input
        .lines()
        .map(str::trim)
        .filter(|target| !target.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Signature: `fn target_word_indices(target_count, word_count) -> Result<Vec<(usize, usize)>>`
///
/// Purpose: Builds the Cartesian product of target and word indexes.
///
/// Parameters:
/// - `target_count`: Number of target generators.
/// - `word_count`: Number of expanded dictionary words.
///
/// Returns: Target-major `(target_index, word_index)` pairs.
///
/// Errors: Returns an error if `target_count * word_count` overflows `usize`.
///
/// Notes: The caller may shuffle the returned vector to randomize the request
/// sequence without losing any target-word pair.
fn target_word_indices(target_count: usize, word_count: usize) -> Result<Vec<(usize, usize)>> {
    let total = target_count
        .checked_mul(word_count)
        .context("target and dictionary expansion is too large")?;
    let mut indices = Vec::with_capacity(total);

    for target_index in 0..target_count {
        for word_index in 0..word_count {
            indices.push((target_index, word_index));
        }
    }

    Ok(indices)
}

/// Signature: `fn expanded_words(word, extensions) -> impl Iterator<Item = String>`
///
/// Purpose: Produces a base dictionary word followed by extension variants.
///
/// Parameters:
/// - `word`: Trimmed dictionary word.
/// - `extensions`: Normalized extension suffixes without leading dots.
///
/// Returns: An iterator yielding owned candidate words.
///
/// Notes: The base word is always yielded first so extension expansion preserves
/// user-visible dictionary ordering.
fn expanded_words<'a>(
    word: &'a str,
    extensions: &'a [String],
) -> impl Iterator<Item = String> + 'a {
    std::iter::once(word.to_owned()).chain(extensions.iter().map(move |extension| {
        let mut candidate = String::with_capacity(word.len() + extension.len() + 1);
        candidate.push_str(word);
        candidate.push('.');
        candidate.push_str(extension);
        candidate
    }))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use futures::TryStreamExt;
    use rand::{SeedableRng, rngs::StdRng};

    use super::*;

    /// Signature: `fn expands_extensions_without_leading_dots()`
    ///
    /// Purpose: Verifies extension normalization integrates with word expansion.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: Extension variants are expected to include exactly one separator
    /// dot in the expanded word.
    #[test]
    fn expands_extensions_without_leading_dots() {
        let extensions = normalize_extensions(&[".php".to_owned(), "bak".to_owned()]);
        let words: Vec<String> = expanded_words("admin", &extensions).collect();

        assert!(words.contains(&"admin".to_owned()));
        assert!(words.contains(&"admin.php".to_owned()));
        assert!(words.contains(&"admin.bak".to_owned()));
    }

    /// Signature: `fn extension_expansion_preserves_argument_order_and_discards_duplicates()`
    ///
    /// Purpose: Verifies extension normalization preserves first-seen order while
    /// removing duplicates.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: Whitespace and leading dots are normalized before deduplication.
    #[test]
    fn extension_expansion_preserves_argument_order_and_discards_duplicates() {
        let extensions = normalize_extensions(&[
            "bak".to_owned(),
            ".php".to_owned(),
            "bak".to_owned(),
            " txt ".to_owned(),
        ]);

        assert_eq!(extensions, vec!["bak", "php", "txt"]);
    }

    /// Signature: `async fn refuses_to_overwrite_dictionary_through_a_hard_link()`
    ///
    /// Purpose: Verifies dictionary/output alias detection catches hard links.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: This protects users even when paths differ but point to the same
    /// filesystem object.
    #[tokio::test]
    async fn refuses_to_overwrite_dictionary_through_a_hard_link() {
        let directory = std::env::temp_dir().join(format!(
            "url_enum_test_{}_{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        std::fs::create_dir_all(&directory).expect("create temporary test directory");
        let dictionary = directory.join("dict.txt");
        let output = directory.join("result.csv");
        std::fs::write(&dictionary, "admin\n").expect("write dictionary");
        std::fs::hard_link(&dictionary, &output).expect("create hard link");

        let error = open_dictionary(&dictionary, output.to_str())
            .await
            .expect_err("same output file must fail");

        assert!(error.to_string().contains("output file cannot be the same"));
        std::fs::remove_dir_all(directory).expect("remove temporary test directory");
    }

    /// Signature: `fn parses_target_lines_and_ignores_blank_lines()`
    ///
    /// Purpose: Verifies target-list parsing trims whitespace and skips blanks.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: URL validation is intentionally outside this helper.
    #[test]
    fn parses_target_lines_and_ignores_blank_lines() {
        let targets = parse_target_lines("\n https://one.test \n\nhttps://two.test\n");

        assert_eq!(targets, vec!["https://one.test", "https://two.test"]);
    }

    /// Signature: `async fn reads_single_http_url_target_as_literal_url()`
    ///
    /// Purpose: Verifies single URL targets are not inspected as filesystem
    /// paths before URL validation.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: This covers Windows paths where `https://host:443` triggers an
    /// invalid path syntax error if passed to metadata lookup.
    #[tokio::test]
    async fn reads_single_http_url_target_as_literal_url() {
        let target = "https://token.telecomjs.com:443";

        let targets = read_targets(target).await.expect("literal URL target");

        assert_eq!(targets, vec![target]);
    }

    /// Signature: `fn recognizes_http_url_literals_case_insensitively()`
    ///
    /// Purpose: Verifies URL-looking targets are detected before filesystem
    /// inspection.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: URL schemes are case-insensitive.
    #[test]
    fn recognizes_http_url_literals_case_insensitively() {
        assert!(is_http_url_literal("http://example.test"));
        assert!(is_http_url_literal("HTTPS://example.test"));
        assert!(!is_http_url_literal("targets.txt"));
    }

    /// Signature: `fn creates_target_word_indices_in_target_major_order()`
    ///
    /// Purpose: Verifies Cartesian target-word indexes use target-major order.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: The order matches default non-random scanner output ordering.
    #[test]
    fn creates_target_word_indices_in_target_major_order() {
        let indices = target_word_indices(2, 3).expect("valid sequence");

        assert_eq!(
            indices,
            vec![(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2)]
        );
    }

    /// Signature: `fn shuffles_target_word_indices_as_the_full_product()`
    ///
    /// Purpose: Verifies shuffled sequencing keeps every target-word pair.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: Sorting after shuffle should recover the ordered product exactly.
    #[test]
    fn shuffles_target_word_indices_as_the_full_product() {
        let mut indices = target_word_indices(2, 3).expect("valid sequence");
        let ordered = indices.clone();
        indices.shuffle(&mut StdRng::seed_from_u64(7));

        assert_ne!(indices, ordered);
        indices.sort_unstable();
        assert_eq!(indices, ordered);
    }

    /// Signature: `async fn target_word_stream_uses_target_major_order_by_default()`
    ///
    /// Purpose: Verifies non-random preloaded word streams preserve
    /// target-major ordering.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: This keeps randomized and streaming paths behaviorally aligned
    /// when randomization is disabled.
    #[tokio::test]
    async fn target_word_stream_uses_target_major_order_by_default() {
        let generators = Arc::from(
            vec![
                UrlGenerator::new("https://one.test/base".to_owned(), None, false)
                    .expect("valid first target"),
                UrlGenerator::new("https://two.test/root".to_owned(), None, false)
                    .expect("valid second target"),
            ]
            .into_boxed_slice(),
        );
        let words = Arc::from(vec!["admin".to_owned(), "login".to_owned()].into_boxed_slice());

        let candidates: Vec<Candidate> = target_word_stream(generators, words, false)
            .try_collect()
            .await
            .expect("valid candidates");
        let urls: Vec<String> = candidates
            .into_iter()
            .map(|candidate| candidate.url.to_string())
            .collect();

        assert_eq!(
            urls,
            vec![
                "https://one.test/base/admin",
                "https://one.test/base/login",
                "https://two.test/root/admin",
                "https://two.test/root/login",
            ]
        );
    }
}
