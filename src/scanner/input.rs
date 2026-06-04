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

fn parse_target_lines(input: &str) -> Vec<String> {
    input
        .lines()
        .map(str::trim)
        .filter(|target| !target.is_empty())
        .map(str::to_owned)
        .collect()
}

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

    #[test]
    fn expands_extensions_without_leading_dots() {
        let extensions = normalize_extensions(&[".php".to_owned(), "bak".to_owned()]);
        let words: Vec<String> = expanded_words("admin", &extensions).collect();

        assert!(words.contains(&"admin".to_owned()));
        assert!(words.contains(&"admin.php".to_owned()));
        assert!(words.contains(&"admin.bak".to_owned()));
    }

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

    #[test]
    fn parses_target_lines_and_ignores_blank_lines() {
        let targets = parse_target_lines("\n https://one.test \n\nhttps://two.test\n");

        assert_eq!(targets, vec!["https://one.test", "https://two.test"]);
    }

    #[test]
    fn creates_target_word_indices_in_target_major_order() {
        let indices = target_word_indices(2, 3).expect("valid sequence");

        assert_eq!(
            indices,
            vec![(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2)]
        );
    }

    #[test]
    fn shuffles_target_word_indices_as_the_full_product() {
        let mut indices = target_word_indices(2, 3).expect("valid sequence");
        let ordered = indices.clone();
        indices.shuffle(&mut StdRng::seed_from_u64(7));

        assert_ne!(indices, ordered);
        indices.sort_unstable();
        assert_eq!(indices, ordered);
    }

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
