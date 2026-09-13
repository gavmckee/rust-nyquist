use anyhow::{bail, Context, Result};
use arrow_array::{Array, Int64Array, StringArray, UInt64Array};
use parquet::{arrow::arrow_reader::ParquetRecordBatchReaderBuilder, file::reader::ChunkReader};
use serde::Serialize;
use std::collections::BTreeMap;

pub const MAX_ROWS: usize = 100_000;
pub const MAX_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct Recording {
    pub rows: Vec<Row>,
}

#[derive(Debug, Serialize)]
pub struct Row {
    pub ts: i64,
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub kind: String,
    pub unit: String,
    // Decimal strings preserve UInt64 precision across JSON / JavaScript.
    pub raw: String,
    pub p50: Option<String>,
    pub p90: Option<String>,
    pub p99: Option<String>,
    pub p999: Option<String>,
}

pub fn read_recording<R: ChunkReader + 'static>(input: R) -> Result<Recording> {
    let builder =
        ParquetRecordBatchReaderBuilder::try_new(input).context("Not a readable Parquet file")?;
    if builder.metadata().file_metadata().num_rows() > MAX_ROWS as i64 {
        bail!("Recording exceeds {MAX_ROWS} rows; open a smaller recording");
    }
    let reader = builder.with_batch_size(1024).build()?;
    let mut rows = Vec::new();
    for batch in reader {
        let batch = batch?;
        let column = |name: &str| {
            batch
                .column_by_name(name)
                .with_context(|| format!("Missing Nyquist column: {name}"))
        };
        let text = |name: &str| -> Result<&StringArray> {
            column(name)?
                .as_any()
                .downcast_ref::<StringArray>()
                .with_context(|| format!("Column {name} must be UTF-8 text"))
        };
        let ts = column("ts_unix_ms")?
            .as_any()
            .downcast_ref::<Int64Array>()
            .context("ts_unix_ms must be Int64")?;
        let raw = column("raw")?
            .as_any()
            .downcast_ref::<UInt64Array>()
            .context("raw must be UInt64")?;
        let (names, labels, kinds, units, buckets) = (
            text("name")?,
            text("labels_json")?,
            text("kind")?,
            text("unit")?,
            text("buckets_json")?,
        );
        for i in 0..batch.num_rows() {
            if [ts as &dyn Array, raw, names, labels, kinds, units, buckets]
                .iter()
                .any(|c| c.is_null(i))
            {
                bail!("Null values are not supported in Nyquist recordings");
            }
            if !(-62_167_219_200_000..=253_402_300_799_999).contains(&ts.value(i)) {
                bail!("Timestamp is outside the supported date range");
            }
            if !matches!(kinds.value(i), "counter" | "gauge" | "distribution") {
                bail!("Unknown metric kind: {}", kinds.value(i));
            }
            let bucket_values: Vec<(u64, u64)> =
                serde_json::from_str(buckets.value(i)).context("Invalid buckets_json")?;
            if bucket_values.windows(2).any(|b| b[0].0 >= b[1].0) {
                bail!("Histogram bucket bounds must be strictly increasing");
            }
            let count = bucket_values
                .iter()
                .try_fold(0u64, |n, b| n.checked_add(b.1))
                .context("Histogram observation count overflows UInt64")?;
            let p = nyquist_core::percentiles::percentiles_from_buckets(
                &bucket_values,
                &[50.0, 90.0, 99.0, 99.9],
            );
            let pct = |index: usize| (count > 0).then(|| p[index].1.to_string());
            rows.push(Row {
                ts: ts.value(i),
                name: names.value(i).into(),
                labels: serde_json::from_str(labels.value(i)).context("Invalid labels_json")?,
                kind: kinds.value(i).into(),
                unit: units.value(i).into(),
                raw: raw.value(i).to_string(),
                p50: pct(0),
                p90: pct(1),
                p99: pct(2),
                p999: pct(3),
            });
            if rows.len() > MAX_ROWS {
                bail!("Recording exceeds {MAX_ROWS} rows");
            }
        }
    }
    Ok(Recording { rows })
}
