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

    pub(super) fn flush(&mut self) -> Result<()> {
        match self {
            Self::Csv(writer) => writer.flush().context("failed to flush CSV output"),
            Self::Jsonl(writer) => writer.flush().context("failed to flush JSONL output"),
        }
    }
}
