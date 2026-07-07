# nyquist-mcp

An [MCP](https://modelcontextprotocol.io) server that exposes nyquist telemetry
(via ClickHouse) plus **encoded troubleshooting knowledge** to an assistant such
as Claude. The goal: let any session diagnose nyquist/network behavior without
re-deriving the hard-won context captured in `src/knowledge.rs`.

## Tools (live queries against the `samples` table)

| Tool | Purpose |
|------|---------|
| `query_bandwidth` | RX/TX bandwidth percentiles (Gbps) over a window; p50 vs tail annotated |
| `analyze_queues` | NIC hardware-queue (RSS) distribution; coupon-collector expected-vs-observed |
| `query_cpu` | Per-core CPU (procfs **jiffies** by default; `source=ebpf` for the ns series) |
| `check_flow_health` | TCP RTT + retransmit percentiles |
| `list_metrics` | Available metric names (optional substring filter) |
| `run_sql` | Escape hatch: read-only `SELECT`/`WITH` against `samples` |

## Resources (encoded knowledge)

- `nyquist://schema` — `samples` columns and percentile semantics
- `nyquist://known-issues` — CPU procfs/eBPF source split, percentiles > line rate,
  GRO aliasing, per-flow ceiling, switch-MTU black hole, startup dt inflation
- `nyquist://playbook` — step-by-step troubleshooting flows
- `nyquist://metric-catalog` — metric families, labels, units

## Prompts

- `diagnose-throughput` — guided "why is throughput wrong" workflow
- `interpret-percentiles` — how to read p50/p90/p99/p999 vs physical limits

## Configuration (env)

| Var | Default |
|-----|---------|
| `NYQUIST_CH_URL` | `http://localhost:18123` |
| `NYQUIST_CH_DB` | `nyquist_live` |
| `NYQUIST_CH_USER` | `default` |
| `NYQUIST_CH_PASS` | `nyquist` |

## Build & register

```bash
cargo build --release -p nyquist-mcp
```

Add to `.mcp.json` (transport: stdio):

```json
{
  "mcpServers": {
    "nyquist": {
      "command": "/abs/path/to/target/release/nyquist-mcp",
      "args": [],
      "env": { "NYQUIST_CH_DB": "nyquist_live", "NYQUIST_CH_PASS": "nyquist" }
    }
  }
}
```

Smoke-test without a client:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"x","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | target/release/nyquist-mcp
```
