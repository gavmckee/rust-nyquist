//! Generate a real Parquet recording containing explicitly synthetic demo data.
use nyquist_core::{Kind, Labels, MetricSnapshot, Unit};
use nyquist_recorder::{schema::RowAccumulator, writer::ParquetWriter};
fn main() -> anyhow::Result<()> {
    let output = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/nyquist-demo".into());
    let mut rows = RowAccumulator::new();
    for tick in 0..120u64 {
        for (index, iface) in ["eth0", "eth1", "eth2"].iter().enumerate() {
            let burst = if (45..60).contains(&tick) { 3 } else { 1 };
            let value = (50_000_000
                + ((tick as f64 / 8.0).sin().abs() * 5_000_000.0) as u64
                + index as u64 * 12_000_000)
                * burst;
            rows.push(
                1_700_000_000_000 + tick as i64 * 10000,
                &MetricSnapshot {
                    name: "network/receive/bytes".into(),
                    kind: Kind::Counter,
                    unit: Unit::Bytes,
                    labels: Labels::new()
                        .insert("iface", *iface)
                        .insert("data", "synthetic demo"),
                    raw: 1_000_000_000 + tick * 500_000_000,
                    buckets: vec![(value, 900), (value * 2, 90), (value * 3, 10)],
                },
            );
            if index == 0 {
                rows.push(
                    1_700_000_000_000 + tick as i64 * 10000,
                    &MetricSnapshot {
                        name: "memory/used".into(),
                        kind: Kind::Gauge,
                        unit: Unit::Bytes,
                        labels: Labels::new().insert("data", "synthetic demo"),
                        raw: 8_000_000_000 + tick * 1_000_000,
                        buckets: vec![(8_000_000_000 + tick * 1_000_000, 100)],
                    },
                );
            }
        }
    }
    let mut writer =
        ParquetWriter::new(output.clone().into(), std::time::Duration::from_secs(3600))?;
    writer.flush(&mut rows)?;
    println!("Synthetic demo recording written to {output}");
    Ok(())
}
