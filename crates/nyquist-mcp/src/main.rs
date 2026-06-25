//! nyquist-mcp — an MCP server that exposes nyquist telemetry (via ClickHouse)
//! plus encoded troubleshooting knowledge (resources + prompts) to an assistant.
//!
//! Tools query the live `samples` table with sane defaults and inline interpretation.
//! Resources/prompts carry the hard-won context (see knowledge.rs) so the assistant
//! interprets results correctly instead of re-deriving it.

mod clickhouse;
mod knowledge;

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        AnnotateAble, CallToolResult, Content, GetPromptRequestParams, GetPromptResult, Implementation,
        ListPromptsResult, ListResourcesResult, PaginatedRequestParams, Prompt, PromptMessage,
        PromptMessageRole, ProtocolVersion, RawResource, ReadResourceRequestParams,
        ReadResourceResult, ResourceContents, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::{Deserialize, Serialize};

use clickhouse::ClickHouse;

#[derive(Clone)]
pub struct NyquistMcp {
    ch: ClickHouse,
    // Read by the `#[tool_handler]` macro expansion (dead-code analysis can't see that).
    #[allow(dead_code)]
    tool_router: ToolRouter<NyquistMcp>,
}

// ---------- tool argument structs ----------

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct BandwidthArgs {
    /// Interface to query (default "ens1f0np0").
    #[serde(default = "default_iface")]
    pub iface: String,
    /// Look-back window in minutes (default 5).
    #[serde(default = "default_minutes")]
    pub minutes: u32,
    /// "rx", "tx", or "both" (default "both").
    #[serde(default = "default_both")]
    pub direction: String,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct QueueArgs {
    #[serde(default = "default_iface")]
    pub iface: String,
    #[serde(default = "default_minutes")]
    pub minutes: u32,
    /// Optional: number of parallel flows in the workload, to compare observed
    /// active-queue count against the coupon-collector expectation.
    #[serde(default)]
    pub flows: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct CpuArgs {
    #[serde(default = "default_minutes")]
    pub minutes: u32,
    /// "procfs" (jiffies, authoritative — default) or "ebpf" (nanoseconds).
    #[serde(default = "default_procfs")]
    pub source: String,
    /// How many busiest cores to return (default 10).
    #[serde(default = "default_top")]
    pub top: u32,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct WindowArgs {
    #[serde(default = "default_minutes")]
    pub minutes: u32,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ListMetricsArgs {
    /// Substring filter on metric name (empty = all).
    #[serde(default)]
    pub filter: String,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SqlArgs {
    /// A read-only SELECT/WITH query against the `samples` table. Append your own
    /// FORMAT if you want something other than TabSeparated.
    pub sql: String,
}

fn default_iface() -> String { "ens1f0np0".to_string() }
fn default_minutes() -> u32 { 5 }
fn default_both() -> String { "both".to_string() }
fn default_procfs() -> String { "procfs".to_string() }
fn default_top() -> u32 { 10 }

fn ok(text: String) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::success(vec![Content::text(text)]))
}
fn fail(e: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(e.to_string(), None)
}

// ---------- tools ----------

#[tool_router]
impl NyquistMcp {
    pub fn new() -> Self {
        NyquistMcp { ch: ClickHouse::from_env(), tool_router: Self::tool_router() }
    }

    /// Bandwidth percentiles (Gbps) over time for an interface. p50 is the trustworthy
    /// central rate; p99/p999 are the sub-second tail (carry ~2% measurement overshoot,
    /// so a value slightly above line rate is an artifact, not real throughput).
    #[tool(description = "RX/TX bandwidth percentiles in Gbps over a recent window for a NIC")]
    async fn query_bandwidth(
        &self,
        Parameters(a): Parameters<BandwidthArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let sql = match a.direction.as_str() {
            "rx" | "tx" => {
                let name = if a.direction == "rx" { "network_receive_bytes" } else { "network_transmit_bytes" };
                format!(
                    "SELECT toUnixTimestamp64Milli(ts) AS t_ms, \
                       round(p50/1.25e8,2) p50_gbps, round(p90/1.25e8,2) p90_gbps, \
                       round(p99/1.25e8,2) p99_gbps, round(p999/1.25e8,2) p999_gbps \
                     FROM samples WHERE name='{name}' AND tags['iface']='{iface}' \
                       AND ts >= now() - INTERVAL {min} MINUTE \
                     ORDER BY t_ms FORMAT TSVWithNames",
                    iface = a.iface, min = a.minutes
                )
            }
            _ => format!(
                "SELECT toUnixTimestamp64Milli(ts) AS t_ms, \
                   round(maxIf(p50,name='network_receive_bytes')/1.25e8,2) rx_p50, \
                   round(maxIf(p99,name='network_receive_bytes')/1.25e8,2) rx_p99, \
                   round(maxIf(p50,name='network_transmit_bytes')/1.25e8,2) tx_p50, \
                   round(maxIf(p99,name='network_transmit_bytes')/1.25e8,2) tx_p99 \
                 FROM samples \
                 WHERE name IN ('network_receive_bytes','network_transmit_bytes') \
                   AND tags['iface']='{iface}' AND ts >= now() - INTERVAL {min} MINUTE \
                 GROUP BY t_ms ORDER BY t_ms FORMAT TSVWithNames",
                iface = a.iface, min = a.minutes
            ),
        };
        let rows = self.ch.query(&sql).await.map_err(fail)?;
        let note = "# Gbps = bytes_per_sec / 1.25e8. p50 = trustworthy central rate; \
                    p99/p999 = sub-second tail with ~2% overshoot (values slightly over line \
                    rate are a measurement artifact, not real throughput).\n\n";
        ok(format!("{note}{rows}"))
    }

    /// Per-queue RSS distribution analysis. Reports active/idle queue counts and, if `flows`
    /// is given, the coupon-collector expected active count N*(1-((N-1)/N)^flows).
    #[tool(description = "Analyze NIC hardware-queue (RSS) distribution: active/idle, spread, hotspots")]
    async fn analyze_queues(
        &self,
        Parameters(a): Parameters<QueueArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let sql = format!(
            "WITH q AS (SELECT toUInt16(tags['queue']) queue, avg(p50)/1.25e8 g \
               FROM samples WHERE name='nic_queue_rx_bytes' AND tags['iface']='{iface}' \
                 AND ts >= now() - INTERVAL {min} MINUTE GROUP BY queue) \
             SELECT countIf(g>0.01) active, countIf(g<=0.01) idle, count() total, \
               round(maxIf(g,g>0.01),2) hottest_gbps, round(minIf(g,g>0.01),2) coldest_gbps, \
               round(stddevPopIf(g,g>0.01)/nullIf(avgIf(g,g>0.01),0)*100,1) cv_pct \
             FROM q FORMAT TSVWithNames",
            iface = a.iface, min = a.minutes
        );
        let rows = self.ch.query(&sql).await.map_err(fail)?;

        // Parse total + active from the single data row to compute the coupon-collector expectation.
        let mut extra = String::new();
        if let Some(data) = rows.lines().nth(1) {
            let cols: Vec<&str> = data.split('\t').collect();
            if cols.len() >= 3 {
                let active: f64 = cols[0].parse().unwrap_or(0.0);
                let total: f64 = cols[2].parse().unwrap_or(0.0);
                if total > 0.0 {
                    if let Some(f) = a.flows {
                        let expected = total * (1.0 - ((total - 1.0) / total).powi(f as i32));
                        extra = format!(
                            "\n# Coupon-collector: with {f} flows over {total:.0} queues, \
                             EXPECTED ~{expected:.0} active; OBSERVED {active:.0}. \
                             Idle queues at low flow count are normal — lever is flow count \
                             (or a deterministic RSS indirection table), not config.\n"
                        );
                    } else {
                        extra = format!(
                            "\n# {active:.0}/{total:.0} queues active. Pass `flows` (the workload's \
                             stream count) to compare against the coupon-collector expectation \
                             N*(1-((N-1)/N)^flows).\n"
                        );
                    }
                }
            }
        }
        ok(format!("{rows}{extra}"))
    }

    /// Per-core CPU usage. Defaults to the procfs JIFFIES series (authoritative). The eBPF
    /// series (source=ebpf, nanoseconds) is a separate source — see known-issues #1.
    #[tool(description = "Per-core CPU usage (busiest cores). source=procfs (jiffies, default) or ebpf (ns)")]
    async fn query_cpu(
        &self,
        Parameters(a): Parameters<CpuArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let src_filter = if a.source == "ebpf" {
            "tags['source']='ebpf'"
        } else {
            "NOT mapContains(tags,'source')"
        };
        let unit = if a.source == "ebpf" { "nanoseconds/sec" } else { "jiffies/sec (USER_HZ)" };
        let sql = format!(
            "SELECT tags['cpu'] cpu, \
               maxIf(p99,name='cpu_usage_softirq') softirq_p99, \
               maxIf(p99,name='cpu_usage_system')  system_p99, \
               maxIf(p99,name='cpu_usage_user')    user_p99 \
             FROM samples \
             WHERE name IN ('cpu_usage_softirq','cpu_usage_system','cpu_usage_user') \
               AND {src} AND tags['cpu'] != 'cpu' AND ts >= now() - INTERVAL {min} MINUTE \
             GROUP BY cpu ORDER BY softirq_p99 DESC LIMIT {top} FORMAT TSVWithNames",
            src = src_filter, min = a.minutes, top = a.top
        );
        let rows = self.ch.query(&sql).await.map_err(fail)?;
        let note = format!(
            "# source={} ({}). If you ever see p99=549755813887 (2^39-1) the source/unit \
             collision regressed (known-issues #1). procfs is authoritative for CPU.\n\n",
            a.source, unit
        );
        ok(format!("{note}{rows}"))
    }

    /// TCP flow health: RTT and retransmit percentiles. Near-zero retransmits + a capped
    /// aggregate usually means flow-parallelism is the limit, not buffers/MTU.
    #[tool(description = "TCP flow health: RTT (us) and retransmit percentiles over a recent window")]
    async fn check_flow_health(
        &self,
        Parameters(a): Parameters<WindowArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let sql = format!(
            "SELECT name, round(avg(p50),0) avg_p50, round(max(p99),0) max_p99, count() samples \
             FROM samples WHERE name IN ('tcp_rtt_us','tcp_retransmits','tcp_retrans_segs') \
               AND ts >= now() - INTERVAL {min} MINUTE GROUP BY name ORDER BY name FORMAT TSVWithNames",
            min = a.minutes
        );
        let rows = self.ch.query(&sql).await.map_err(fail)?;
        let note = "# tcp_rtt_us in microseconds; tcp_retransmits per-connection. High retransmits \
                    + high RTT → investigate buffers/MTU. Near-zero + capped throughput → the \
                    limit is parallel-flow count (per-flow ~5 Gbps), not tuning.\n\n";
        ok(format!("{note}{rows}"))
    }

    /// List metric names recorded in the last 10 minutes, with sample counts and tag keys.
    #[tool(description = "List available nyquist metric names (optionally filtered by substring)")]
    async fn list_metrics(
        &self,
        Parameters(a): Parameters<ListMetricsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let sql = format!(
            "SELECT name, count() samples, anyLast(mapKeys(tags)) tag_keys \
             FROM samples WHERE name LIKE '%{f}%' AND ts >= now() - INTERVAL 10 MINUTE \
             GROUP BY name ORDER BY name FORMAT TSVWithNames",
            f = a.filter.replace('\'', "")
        );
        let rows = self.ch.query(&sql).await.map_err(fail)?;
        ok(rows)
    }

    /// Escape hatch: run an arbitrary READ-ONLY (SELECT/WITH) query against `samples`.
    #[tool(description = "Run a read-only SELECT/WITH SQL query against the nyquist samples table")]
    async fn run_sql(
        &self,
        Parameters(a): Parameters<SqlArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let trimmed = a.sql.trim_start();
        let head = trimmed.get(..6).unwrap_or("").to_ascii_uppercase();
        if !(head.starts_with("SELECT") || head.starts_with("WITH")) {
            return Err(ErrorData::invalid_params(
                "only read-only SELECT/WITH queries are allowed",
                None,
            ));
        }
        let rows = self.ch.query(&a.sql).await.map_err(fail)?;
        ok(rows)
    }
}

// ---------- resources, prompts, server info ----------

#[tool_handler]
impl ServerHandler for NyquistMcp {
    fn get_info(&self) -> ServerInfo {
        // ServerInfo / Implementation are #[non_exhaustive]; build via Default + assignment.
        let mut imp = Implementation::from_build_env();
        imp.name = "nyquist-mcp".to_string();
        imp.version = env!("CARGO_PKG_VERSION").to_string();

        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::LATEST;
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_prompts()
            .build();
        info.server_info = imp;
        info.instructions = Some(format!(
            "nyquist telemetry + troubleshooting MCP (ClickHouse {}). \
             Prefer these tools over shell commands. Read the resources \
             (nyquist://known-issues, nyquist://playbook) before interpreting odd metrics: \
             CPU has a procfs/eBPF source split, percentiles can exceed line rate by ~2%, \
             and throughput 'ceilings' are usually per-flow x stream count.",
            self.ch.endpoint()
        ));
        info
    }

    async fn list_resources(
        &self,
        _req: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let resources = knowledge::RESOURCES
            .iter()
            .map(|d| {
                let mut r = RawResource::new(d.uri, d.name);
                r.description = Some(d.desc.to_string());
                r.mime_type = Some("text/markdown".to_string());
                r.no_annotation()
            })
            .collect();
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        req: ReadResourceRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        match knowledge::RESOURCES.iter().find(|d| d.uri == req.uri) {
            Some(d) => Ok(ReadResourceResult::new(vec![ResourceContents::text(d.body, &req.uri)])),
            None => Err(ErrorData::resource_not_found(
                format!("unknown resource: {}", req.uri),
                None,
            )),
        }
    }

    async fn list_prompts(
        &self,
        _req: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        let prompts = knowledge::PROMPTS
            .iter()
            .map(|p| Prompt::new(p.name, Some(p.desc), None))
            .collect();
        Ok(ListPromptsResult::with_all_items(prompts))
    }

    async fn get_prompt(
        &self,
        req: GetPromptRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, ErrorData> {
        match knowledge::PROMPTS.iter().find(|p| p.name == req.name) {
            Some(p) => Ok(GetPromptResult::new(vec![PromptMessage::new_text(
                PromptMessageRole::User,
                p.body,
            )])),
            None => Err(ErrorData::invalid_params(
                format!("unknown prompt: {}", req.name),
                None,
            )),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = NyquistMcp::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
