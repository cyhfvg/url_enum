use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_stream::try_stream;
use futures::Stream;
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

pub(super) async fn read_target(value: &str) -> Result<String> {
    if value != "-" {
        return Ok(value.to_owned());
    }

    let mut input = String::new();
    tokio::io::stdin()
        .read_to_string(&mut input)
        .await
        .context("failed to read target URL from stdin")?;
    let targets: Vec<&str> = input
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    match targets.as_slice() {
        [target] => Ok((*target).to_owned()),
        [] => bail!("stdin did not provide a target URL"),
        _ => bail!("stdin must provide exactly one target URL"),
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
}
