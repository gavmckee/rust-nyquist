#!/usr/bin/env bash
# Capture the golden coverage baseline from a live agent (run on Linux).
# Usage: scripts/capture-golden-baseline.sh [ADDR] > crates/nyquist-exposition/tests/fixtures/golden_metrics.txt
#
# Assumes a nyquist agent is running and has been up > window seconds so all
# dynamic families (cpu/* per-CPU, memory/* per meminfo field, psi/*, softirq/*)
# have emitted at least once. The captured /metrics text is reduced to canonical
# tuples by nyquist_core::coverage::parse_prometheus_tuples in the parity gate.
set -euo pipefail
ADDR="${1:-127.0.0.1:9100}"
curl -fsS "http://${ADDR}/metrics"
