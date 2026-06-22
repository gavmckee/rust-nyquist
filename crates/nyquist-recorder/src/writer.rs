use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;
use parquet::basic::Compression;
use crate::schema::{nyquist_schema, RowAccumulator};

#[derive(Debug, thiserror::Error)]
pub enum RecorderError {
    #[error("recorder I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),
}

/// Writes Parquet files with time-based rotation.
///
/// Each `flush()` creates one complete, self-contained Parquet file.
/// Files are named `nyquist-{rotation_ts_ms}-{seq}.parquet`.
/// `seq` is globally monotonic — never resets — so filenames are always unique
/// even when rotation fires within the same millisecond as the previous file.
pub struct ParquetWriter {
    output_dir:        PathBuf,
    rotation_interval: Duration,
    rotation_due:      Instant,
    rotation_ts_ms:    u64,
    seq:               u64,
}

impl ParquetWriter {
    pub fn new(output_dir: PathBuf, rotation_interval: Duration) -> Result<Self, RecorderError> {
        fs::create_dir_all(&output_dir)?;
        Ok(ParquetWriter {
            output_dir,
            rotation_interval,
            rotation_due: Instant::now() + rotation_interval,
            rotation_ts_ms: unix_ms(),
            seq: 0,
        })
    }

    /// Rotate to a new period if the rotation interval has elapsed.
    pub fn maybe_rotate(&mut self) -> Result<(), RecorderError> {
        if Instant::now() >= self.rotation_due {
            self.rotation_ts_ms = unix_ms();
            self.rotation_due = Instant::now() + self.rotation_interval;
        }
        Ok(())
    }

    /// Write all buffered rows to a new complete Parquet file and clear the buffer.
    /// If the accumulator is empty, this is a no-op.
    pub fn flush(&mut self, acc: &mut RowAccumulator) -> Result<(), RecorderError> {
        if acc.is_empty() { return Ok(()); }
        let batch = acc.drain();
        let path = self.output_dir.join(
            format!("nyquist-{}-{}.parquet", self.rotation_ts_ms, self.seq)
        );
        self.seq += 1;
        let file = std::fs::File::create(&path)?;
        let props = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build();
        let mut writer = ArrowWriter::try_new(file, nyquist_schema(), Some(props))?;
        writer.write(&batch)?;
        writer.close()?;
        Ok(())
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyquist_core::model::{Kind, Labels, Unit};
    use nyquist_core::snapshot::MetricSnapshot;
    use tempfile::tempdir;

    fn make_metric() -> MetricSnapshot {
        MetricSnapshot {
            name: "cpu/usage/user".to_string(),
            kind: Kind::Counter,
            unit: Unit::Count,
            labels: Labels::new(),
            raw: 100,
            buckets: vec![(50, 5), (90, 3), (99, 1), (100, 1)],
        }
    }

    #[test]
    fn flush_creates_parquet_file() {
        let dir = tempdir().unwrap();
        let mut writer = ParquetWriter::new(dir.path().to_path_buf(), Duration::from_secs(3600)).unwrap();
        let mut acc = RowAccumulator::new();
        acc.push(1_000_000, &make_metric());
        writer.flush(&mut acc).unwrap();
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("parquet"))
            .collect();
        assert_eq!(files.len(), 1, "expected exactly 1 parquet file");
        assert!(files[0].metadata().unwrap().len() > 0);
    }

    #[test]
    fn flush_empty_accumulator_is_noop() {
        let dir = tempdir().unwrap();
        let mut writer = ParquetWriter::new(dir.path().to_path_buf(), Duration::from_secs(3600)).unwrap();
        let mut acc = RowAccumulator::new();
        writer.flush(&mut acc).unwrap();
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(files.is_empty(), "no files expected for empty flush");
    }

    #[test]
    fn rotation_creates_new_file() {
        let dir = tempdir().unwrap();
        let mut writer = ParquetWriter::new(dir.path().to_path_buf(), Duration::from_millis(1)).unwrap();
        let mut acc = RowAccumulator::new();
        acc.push(1000, &make_metric());
        writer.flush(&mut acc).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        writer.maybe_rotate().unwrap();
        acc.push(2000, &make_metric());
        writer.flush(&mut acc).unwrap();
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("parquet"))
            .collect();
        assert_eq!(files.len(), 2, "expected 2 parquet files after rotation");
    }
}
