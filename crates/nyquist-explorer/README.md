# Nyquist Recording Explorer

A standalone web UI for the Parquet files produced by `nyquist-recorder`.
It does not need ClickHouse, Grafana, Node.js, or an external service to run.

```sh
cargo run -p nyquist-explorer -- --recordings /var/lib/nyquist/parquet
```

Open **http://127.0.0.1:9101**. Choose a file from the server list, or drag
one or more downloaded `.parquet` files into the upload area. Omit
`--recordings` to use uploads only. `--listen` changes the bind address/port.

For a remote server, run the explorer there and forward its loopback port:

```sh
ssh -L 9101:127.0.0.1:9101 user@server
```

Then open the same local URL. The explorer defaults to loopback and has no
built-in authentication. Uploaded files are parsed in memory by the explorer
process and are not saved; source recordings are never modified. The server
browser reads completed `.parquet` files directly inside its configured
directory, excluding temporary files and symlinks outside that directory.

## Working with recordings

- Load multiple recording files into one session. Identical observations
  in overlapping files are deduplicated.
- Search the metric library and filter labels such as CPU, interface, queue,
  or port. Toggle individual labeled series in the chart legend.
- Plot p50, p90, p99, p99.9, raw values, or counter rates. Hover over the chart
  to inspect an observation; narrow the UTC start/end times to zoom in.
- Inspect exact values in a paginated table and export the filtered selection
  as CSV. CSV contains raw and percentile columns; counter rates are a chart
  calculation, not an extra recorded column.

Counter percentiles are rates from the recorder's sliding-window histograms;
gauge and distribution percentiles are observed values. The chart's counter
rate option instead divides differences in raw cumulative counts by elapsed
time between snapshots. The first point and resets have no rate. Empty
histograms are missing values, not zero. Windows can overlap: do not add their
percentiles or interpret them as individual events.

UInt64 values are sent to the browser as decimal strings. Tables and CSV keep
those integers exact; charts use floating-point approximations for plotting.
Metric units are shown as stored. Unit `none` can require interpretation from
the metric name, for example `tcp/rtt_us` uses microseconds.

This first version supports up to 128 MiB and 100,000 rows per file, 200,000
rows per browser session, and 20 visible chart series (the first eight are
selected initially). Files above the limits are rejected rather than silently
truncated. The complete selected recordings are loaded into memory, so this
is intended for individual captures, not querying months of archive data.

## Try a synthetic recording

```sh
cargo run -p nyquist-explorer --example make_demo -- /tmp/nyquist-demo
cargo run -p nyquist-explorer -- --recordings /tmp/nyquist-demo
```

Demo series are explicitly labeled `data="synthetic demo"`.

## Validation

```sh
cargo test -p nyquist-explorer
cargo clippy -p nyquist-explorer --all-targets -- -D warnings
```

Tests write actual recordings with `nyquist-recorder` and verify upload and
server-file parity, UInt64 precision, percentile reconstruction, missing
histograms, malformed files, and path confinement.

Optional browser smoke tests require Node.js and Playwright. Generate the demo
in a fresh directory and run the explorer against it, then in a second shell:

```sh
cd crates/nyquist-explorer
npm ci
npx playwright install chromium
EXPLORER_URL=http://127.0.0.1:9101 \
EXPLORER_FIXTURES=/tmp/nyquist-demo npm run test:browser
```

These tests exercise server loading, upload, filtering, counter rates, CSV,
pagination, duplicate recordings, responsive layout, clearing a session, and
invalid-file feedback. Screenshots and CSV output go to a temporary directory.
