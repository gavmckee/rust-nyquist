# Telemetry export and delivery semantics

Prometheus emits one TYPE declaration per family and groups its labeled
samples together. Raw distribution observation counts use `<name>_count`
(counter), separating them from same-name gauges such as `tcp_rtt_us{port}`.
This changes the former unsuffixed raw BPF distribution series. Existing
`<name>_rate{percentile}` distribution percentile names remain available for
compatibility; their values are observations in the metric's units, not rates.
Both `_rate` and `_value` percentile families are gauges. JSON includes a
`labels` object alongside `name`, `raw`, and `percentiles`.

Userspace histogram slices retain a small raw buffer for ordinary sampler
loads. Once full, a slice promotes to fixed-size histogram buckets, preserving
all observations (including all TCP connections on a busy service). BPF
histogram freshness allows three configured sampler intervals, with a
one-second minimum for the application's 100 ms slices.

Perf counters accumulate scaled interval deltas using enabled/running time.
`nyquist_perf_running_percent{source,cpu,group}` reports the fraction of each
interval for which the group ran. Low coverage means the scaled estimate is
less representative; zero coverage means no estimate is available for that
interval and the cumulative count stays unchanged.

Both configuration watchers retain failed change inserts in a bounded,
in-memory queue of 10,000 events. Retries preserve observation timestamps and
order. Overflow drops newest events and logs a cumulative `dropped_total`.
The queue is not a durable spool: process crashes or an outage continuing
through shutdown can lose pending events. An ambiguous HTTP failure after a
server commits an insert can cause duplicate events on retry.

Syswatch retries database/table initialization and participates in worker
supervision. Its BPF poller receives cancellation on shutdown or when its
owning task exits, and normal shutdown joins the poller before a final bounded
attempt to send pending changes.
