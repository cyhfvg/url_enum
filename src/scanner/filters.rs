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
    /// Signature: `fn parse(values) -> Result<Self>`
    ///
    /// Purpose: Parses blocked response sizes and merges overlapping or adjacent
    /// ranges.
    ///
    /// Parameters:
    /// - `values`: Raw size filters from CLI parsing.
    ///
    /// Returns: A sorted, compact range set for efficient membership checks.
    ///
    /// Errors: Returns an error for empty values, malformed ranges, invalid
    /// integers, or descending ranges.
    ///
    /// Notes: Merging adjacent ranges keeps lookup logic simple and predictable.
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

    /// Signature: `fn contains(&self, size) -> bool`
    ///
    /// Purpose: Checks whether a response size is blocked.
    ///
    /// Parameters:
    /// - `size`: Response size in bytes.
    ///
    /// Returns: `true` when `size` falls inside any blocked range.
    ///
    /// Notes: Uses `partition_point` over sorted ranges for logarithmic lookup.
    fn contains(&self, size: u64) -> bool {
        let index = self.0.partition_point(|range| range.start <= size);
        index > 0 && self.0[index - 1].end >= size
    }
}

impl Filters {
    /// Signature: `fn new(args) -> Result<Self>`
    ///
    /// Purpose: Builds all status and size filters from CLI arguments.
    ///
    /// Parameters:
    /// - `args`: Parsed CLI settings containing allowlists and blocklists.
    ///
    /// Returns: A [`Filters`] value ready to apply to probe results.
    ///
    /// Errors: Returns an error when blocked size filters cannot be parsed.
    ///
    /// Notes: Status filters are stored as sets because membership checks happen
    /// for every response.
    pub(super) fn new(args: &Args) -> Result<Self> {
        Ok(Self {
            accepted_status: args.filter_http_code.iter().copied().collect(),
            blocked_status: args.black_http_code.iter().copied().collect(),
            blocked_size: BlockedSizes::parse(&args.black_size)?,
        })
    }

    /// Signature: `fn accepts_status(&self, status) -> bool`
    ///
    /// Purpose: Determines whether an HTTP status code should be emitted.
    ///
    /// Parameters:
    /// - `status`: HTTP status code from a response.
    ///
    /// Returns: `true` when the status passes the optional allowlist and the
    /// blocklist.
    ///
    /// Notes: An empty allowlist means all statuses are allowed unless blocked.
    pub(super) fn accepts_status(&self, status: u16) -> bool {
        (self.accepted_status.is_empty() || self.accepted_status.contains(&status))
            && !self.blocked_status.contains(&status)
    }

    /// Signature: `fn accepts_failure(&self) -> bool`
    ///
    /// Purpose: Decides whether failed network probes should be emitted.
    ///
    /// Parameters: None.
    ///
    /// Returns: `true` when no explicit status allowlist is configured.
    ///
    /// Notes: Failures have no status code, so they cannot satisfy a non-empty
    /// status allowlist.
    pub(super) fn accepts_failure(&self) -> bool {
        self.accepted_status.is_empty()
    }

    /// Signature: `fn accepts_size(&self, size) -> bool`
    ///
    /// Purpose: Determines whether a response body size should be emitted.
    ///
    /// Parameters:
    /// - `size`: Response size in bytes.
    ///
    /// Returns: `true` when `size` is not blocked.
    ///
    /// Notes: Size filtering only applies when a size is known.
    pub(super) fn accepts_size(&self, size: u64) -> bool {
        !self.blocked_size.contains(size)
    }
}

/// Signature: `fn parse_size_range(value) -> Result<SizeRange>`
///
/// Purpose: Parses a single blocked-size value or inclusive range.
///
/// Parameters:
/// - `value`: Raw value such as `612` or `612-614`.
///
/// Returns: A normalized inclusive [`SizeRange`].
///
/// Errors: Returns an error for empty input, malformed ranges, invalid integers,
/// or ranges where the start is greater than the end.
///
/// Notes: A single integer is represented as a range whose start and end match.
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

/// Signature: `fn parse_size_value(value, input) -> Result<u64>`
///
/// Purpose: Parses one numeric bound from a blocked-size filter.
///
/// Parameters:
/// - `value`: Raw numeric bound to parse.
/// - `input`: Original filter string used in error messages.
///
/// Returns: The parsed non-negative byte count.
///
/// Errors: Returns an error when `value` is not a valid `u64`.
///
/// Notes: The caller trims whitespace before parsing to permit readable CLI
/// input around range separators.
fn parse_size_value(value: &str, input: &str) -> Result<u64> {
    value.trim().parse().with_context(|| {
        format!("invalid response size `{input}`, expected a non-negative integer or `START-END`")
    })
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    /// Signature: `fn parses_and_merges_blocked_size_values_and_ranges()`
    ///
    /// Purpose: Verifies blocked-size parsing, sorting, merging, and lookup.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: Covers both repeated CLI flags and comma-delimited values.
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

    /// Signature: `fn rejects_descending_blocked_size_range()`
    ///
    /// Purpose: Ensures ranges with `start > end` produce a validation error.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: This protects users from silently inverted filters.
    #[test]
    fn rejects_descending_blocked_size_range() {
        let error =
            BlockedSizes::parse(&["614-612".to_owned()]).expect_err("descending range must fail");

        assert!(error.to_string().contains("start cannot exceed end"));
    }
}
