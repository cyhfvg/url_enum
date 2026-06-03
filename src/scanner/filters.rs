use std::collections::HashSet;

use anyhow::{Context, Result, bail};

use crate::cli::Args;

#[derive(Debug)]
pub(super) struct Filters {
    accepted_status: HashSet<u16>,
    blocked_status: HashSet<u16>,
    blocked_size: BlockedSizes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SizeRange {
    start: u64,
    end: u64,
}

#[derive(Debug, Default)]
struct BlockedSizes(Vec<SizeRange>);

impl BlockedSizes {
    fn parse(values: &[String]) -> Result<Self> {
        let mut ranges = Vec::with_capacity(values.len());
        for value in values {
            ranges.push(parse_size_range(value)?);
        }
        ranges.sort_unstable_by_key(|range| range.start);

        let mut merged: Vec<SizeRange> = Vec::with_capacity(ranges.len());
        for range in ranges {
            if let Some(previous) = merged.last_mut()
                && range.start <= previous.end.saturating_add(1)
            {
                previous.end = previous.end.max(range.end);
                continue;
            }
            merged.push(range);
        }
        Ok(Self(merged))
    }

    fn contains(&self, size: u64) -> bool {
        let index = self.0.partition_point(|range| range.start <= size);
        index > 0 && self.0[index - 1].end >= size
    }
}

impl Filters {
    pub(super) fn new(args: &Args) -> Result<Self> {
        Ok(Self {
            accepted_status: args.filter_http_code.iter().copied().collect(),
            blocked_status: args.black_http_code.iter().copied().collect(),
            blocked_size: BlockedSizes::parse(&args.black_size)?,
        })
    }

    pub(super) fn accepts_status(&self, status: u16) -> bool {
        (self.accepted_status.is_empty() || self.accepted_status.contains(&status))
            && !self.blocked_status.contains(&status)
    }

    pub(super) fn accepts_failure(&self) -> bool {
        self.accepted_status.is_empty()
    }

    pub(super) fn accepts_size(&self, size: u64) -> bool {
        !self.blocked_size.contains(size)
    }
}

fn parse_size_range(value: &str) -> Result<SizeRange> {
    let value = value.trim();
    if value.is_empty() {
        bail!("blocked response size value cannot be empty");
    }

    if let Some((start, end)) = value.split_once('-') {
        if start.trim().is_empty() || end.trim().is_empty() || end.contains('-') {
            bail!("invalid response size range `{value}`, expected `START-END`");
        }
        let start = parse_size_value(start, value)?;
        let end = parse_size_value(end, value)?;
        if start > end {
            bail!("invalid response size range `{value}`, start cannot exceed end");
        }
        return Ok(SizeRange { start, end });
    }

    let size = parse_size_value(value, value)?;
    Ok(SizeRange {
        start: size,
        end: size,
    })
}

fn parse_size_value(value: &str, input: &str) -> Result<u64> {
    value.trim().parse().with_context(|| {
        format!("invalid response size `{input}`, expected a non-negative integer or `START-END`")
    })
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn parses_and_merges_blocked_size_values_and_ranges() {
        let args = Args::try_parse_from([
            "url_enum",
            "-t",
            "https://example.test",
            "-d",
            "dict.txt",
            "--black-size",
            "612",
            "--black-size",
            "613,614",
            "--black-size",
            "700-702",
        ])
        .expect("valid size arguments");
        let blocked = BlockedSizes::parse(&args.black_size).expect("valid ranges");

        assert_eq!(
            blocked.0,
            vec![
                SizeRange {
                    start: 612,
                    end: 614
                },
                SizeRange {
                    start: 700,
                    end: 702
                }
            ]
        );
        assert!(blocked.contains(613));
        assert!(blocked.contains(702));
        assert!(!blocked.contains(615));
    }

    #[test]
    fn rejects_descending_blocked_size_range() {
        let error =
            BlockedSizes::parse(&["614-612".to_owned()]).expect_err("descending range must fail");

        assert!(error.to_string().contains("start cannot exceed end"));
    }
}
