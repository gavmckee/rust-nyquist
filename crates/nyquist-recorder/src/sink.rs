use std::path::PathBuf;
use std::time::{Duration, Instant, UNIX_EPOCH};
use async_trait::async_trait;
use nyquist_core::sink::{Sink, SinkError};
use nyquist_core::snapshot::RegistrySnapshot;
use crate::schema::RowAccumulator;
use crate::writer::{ParquetWriter, RecorderError};

pub struct RecorderSink {
    writer:         ParquetWriter,
    acc:            RowAccumulator,
    flush_interval: Duration,
    last_flush:     Instant,
}

impl RecorderSink {
    pub fn new(
        output_dir:        PathBuf,
        rotation_interval: Duration,
        flush_interval:    Duration,
    ) -> Result<Self, RecorderError> {
        Ok(RecorderSink {
            writer:     ParquetWriter::new(output_dir, rotation_interval)?,
            acc:        RowAccumulator::new(),
            flush_interval,
            last_flush: Instant::now(),
        })
    }
}

#[async_trait]
impl Sink for RecorderSink {
    async fn export(&mut self, snapshot: &RegistrySnapshot) -> Result<(), SinkError> {
        let ts_ms = snapshot.captured
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        for m in &snapshot.metrics {
            self.acc.push(ts_ms, m);
        }
        if self.last_flush.elapsed() >= self.flush_interval {
            self.writer.maybe_rotate()?;
            self.writer.flush(&mut self.acc)?;
            self.last_flush = Instant::now();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyquist_core::model::{Kind, Labels, Unit};
    use nyquist_core::snapshot::{MetricSnapshot, RegistrySnapshot};
    use std::time::SystemTime;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use arrow_array::array::{Int64Array, StringArray};
    use arrow_array::Array;
    use tempfile::tempdir;

    fn make_snapshot(ts: SystemTime) -> RegistrySnapshot {
        RegistrySnapshot {
            captured: ts,
            metrics: vec![
                MetricSnapshot {
                    name: "cpu/usage/user".to_string(),
                    kind: Kind::Counter,
                    unit: Unit::Count,
                    labels: Labels::new().insert("cpu", "0"),
                    raw: 42,
                    percentiles: vec![(50.0, 10), (90.0, 20), (99.0, 30), (99.9, 35)],
                },
                MetricSnapshot {
                    name: "mem/used".to_string(),
                    kind: Kind::Gauge,
                    unit: Unit::Bytes,
                    labels: Labels::new(),
                    raw: 1024,
                    percentiles: vec![(50.0, 900), (90.0, 1000), (99.0, 1020), (99.9, 1024)],
                },
            ],
        }
    }

    #[tokio::test]
    async fn roundtrip_rows_readable() {
        let dir = tempdir().unwrap();
        let ts = SystemTime::UNIX_EPOCH + Duration::from_millis(1_000_000);
        let mut sink = RecorderSink::new(
            dir.path().to_path_buf(),
            Duration::from_secs(3600),
            Duration::ZERO,
        ).unwrap();
        sink.export(&make_snapshot(ts)).await.unwrap();
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("parquet"))
            .collect();
        assert_eq!(files.len(), 1, "expected one Parquet file");
        let file = std::fs::File::open(files[0].path()).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let reader = builder.build().unwrap();
        let batches: Vec<_> = reader.collect::<Result<_, _>>().unwrap();
        let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total_rows, 2, "two metrics → two rows");
        let batch = &batches[0];
        let names = batch.column(1).as_any().downcast_ref::<StringArray>().unwrap();
        let ts_col = batch.column(0).as_any().downcast_ref::<Int64Array>().unwrap();
        let names_set: std::collections::HashSet<&str> =
            (0..names.len()).map(|i| names.value(i)).collect();
        assert!(names_set.contains("cpu/usage/user"));
        assert!(names_set.contains("mem/used"));
        assert_eq!(ts_col.value(0), 1_000_000, "ts_unix_ms round-trips");
    }
}
