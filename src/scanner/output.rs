use std::fs::File as StdFile;
use std::io::{self, BufWriter, Write};

use anyhow::{Context, Result};

use crate::cli::OutputFormat;

use super::ProbeResult;

pub(super) enum ResultWriter {
    Csv(Box<csv::Writer<Box<dyn Write>>>),
    Jsonl(BufWriter<Box<dyn Write>>),
}

impl ResultWriter {
    /// Signature: `fn new(format, output) -> Result<Self>`
    ///
    /// Purpose: Creates a result writer for stdout or a user-specified file.
    ///
    /// Parameters:
    /// - `format`: Output serialization format.
    /// - `output`: Optional path to create for scan results.
    ///
    /// Returns: A writer configured for CSV or JSON Lines output.
    ///
    /// Errors: Returns an error if the output file cannot be created.
    ///
    /// Notes: The caller is responsible for preventing output from aliasing the
    /// dictionary file before constructing the writer.
    pub(super) fn new(format: OutputFormat, output: Option<&str>) -> Result<Self> {
        let destination: Box<dyn Write> = match output {
            Some(path) => Box::new(
                StdFile::create(path)
                    .with_context(|| format!("failed to create output file `{path}`"))?,
            ),
            None => Box::new(io::stdout()),
        };

        Ok(match format {
            OutputFormat::Csv => {
                Self::Csv(Box::new(csv::WriterBuilder::new().from_writer(destination)))
            }
            OutputFormat::Jsonl => Self::Jsonl(BufWriter::new(destination)),
        })
    }

    /// Signature: `fn write(&mut self, result) -> Result<()>`
    ///
    /// Purpose: Serializes one probe result to the configured output format.
    ///
    /// Parameters:
    /// - `result`: Probe result to emit.
    ///
    /// Returns: `Ok(())` after the result has been written to the buffered
    /// destination.
    ///
    /// Errors: Returns an error if CSV or JSONL serialization or writing fails.
    ///
    /// Notes: JSONL output writes one serialized object followed by one newline.
    pub(super) fn write(&mut self, result: &ProbeResult) -> Result<()> {
        match self {
            Self::Csv(writer) => writer
                .serialize(result)
                .context("failed to write CSV result"),
            Self::Jsonl(writer) => {
                serde_json::to_writer(&mut *writer, result)
                    .context("failed to write JSONL result")?;
                writeln!(writer).context("failed to write JSONL newline")
            }
        }
    }

    /// Signature: `fn flush(&mut self) -> Result<()>`
    ///
    /// Purpose: Flushes any buffered result output.
    ///
    /// Parameters: None.
    ///
    /// Returns: `Ok(())` when all buffered bytes have reached the underlying
    /// writer.
    ///
    /// Errors: Returns an error if the underlying writer fails to flush.
    ///
    /// Notes: Call once after the scanner has consumed all candidates.
    pub(super) fn flush(&mut self) -> Result<()> {
        match self {
            Self::Csv(writer) => writer.flush().context("failed to flush CSV output"),
            Self::Jsonl(writer) => writer.flush().context("failed to flush JSONL output"),
        }
    }
}
