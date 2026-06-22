#!/usr/bin/env bash
# nyquist start/stop/restart/status script
#
# Usage:
#   ./scripts/nyquist.sh start
#   ./scripts/nyquist.sh stop
#   ./scripts/nyquist.sh restart
#   ./scripts/nyquist.sh status

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
BINARY="$PROJECT_DIR/target/release/nyquist"
CONFIG="$PROJECT_DIR/nyquist.toml"
PIDFILE="$PROJECT_DIR/nyquist.pid"
LOGFILE="$PROJECT_DIR/nyquist.log"

# ── capabilities required ──────────────────────────────────────────────────────
CAPS="cap_perfmon,cap_bpf,cap_net_admin,cap_dac_read_search"

# ── helpers ────────────────────────────────────────────────────────────────────
die()  { echo "ERROR: $*" >&2; exit 1; }
info() { echo "[nyquist] $*"; }

need_sudo() {
    if [[ $EUID -ne 0 ]]; then
        sudo "$@"
    else
        "$@"
    fi
}

check_binary() {
    [[ -x "$BINARY" ]] || die "binary not found at $BINARY — run: cargo build --release"
}

check_config() {
    [[ -f "$CONFIG" ]] || die "config not found at $CONFIG"
}

ensure_prereqs() {
    # perf_event_paranoid — needs to be <= 1 for hardware counters
    local paranoid
    paranoid=$(cat /proc/sys/kernel/perf_event_paranoid)
    if [[ $paranoid -gt 1 ]]; then
        info "setting kernel.perf_event_paranoid=1 (was $paranoid)"
        need_sudo sysctl -qw kernel.perf_event_paranoid=1
    fi

    # debugfs — needed by the eBPF tracepoint loader
    if ! mountpoint -q /sys/kernel/debug; then
        info "mounting debugfs at /sys/kernel/debug"
        need_sudo mount -t debugfs debugfs /sys/kernel/debug
    fi

    # file capabilities on the binary
    local current_caps
    current_caps=$(getcap "$BINARY" 2>/dev/null || true)
    if [[ "$current_caps" != *"cap_bpf"* ]]; then
        info "setting capabilities on binary: $CAPS"
        need_sudo setcap "${CAPS}=eip" "$BINARY"
    fi
}

get_pid() {
    if [[ -f "$PIDFILE" ]]; then
        local pid
        pid=$(<"$PIDFILE")
        if kill -0 "$pid" 2>/dev/null; then
            echo "$pid"
            return 0
        fi
        rm -f "$PIDFILE"
    fi
    # fall back to pgrep in case pidfile is stale
    pgrep -f "nyquist --config" 2>/dev/null | head -1 || true
}

# ── commands ───────────────────────────────────────────────────────────────────
cmd_start() {
    check_binary
    check_config

    local pid
    pid=$(get_pid)
    if [[ -n "$pid" ]]; then
        info "already running (PID $pid)"
        return 0
    fi

    ensure_prereqs

    info "starting nyquist → log: $LOGFILE"
    nohup "$BINARY" --config "$CONFIG" >> "$LOGFILE" 2>&1 &
    echo $! > "$PIDFILE"
    local pid=$!

    # wait briefly and confirm it stayed up
    sleep 2
    if ! kill -0 "$pid" 2>/dev/null; then
        rm -f "$PIDFILE"
        die "nyquist exited immediately — check $LOGFILE"
    fi

    info "started (PID $pid)"
    info "metrics: http://localhost:9100/metrics"
    info "grafana: http://localhost:3000"
}

cmd_stop() {
    local pid
    pid=$(get_pid)
    if [[ -z "$pid" ]]; then
        info "not running"
        return 0
    fi

    info "stopping nyquist (PID $pid)"
    kill "$pid"

    # wait up to 10s for clean exit
    local i=0
    while kill -0 "$pid" 2>/dev/null && [[ $i -lt 10 ]]; do
        sleep 1
        ((i++))
    done

    if kill -0 "$pid" 2>/dev/null; then
        info "still running after 10s — sending SIGKILL"
        kill -9 "$pid"
    fi

    rm -f "$PIDFILE"
    info "stopped"
}

cmd_restart() {
    cmd_stop
    sleep 1
    cmd_start
}

cmd_status() {
    local pid
    pid=$(get_pid)
    if [[ -z "$pid" ]]; then
        info "not running"
        return 1
    fi

    info "running (PID $pid)"

    # show fd limit of the live process
    local soft hard
    soft=$(awk '/Max open files/{print $4}' /proc/"$pid"/limits 2>/dev/null || echo "?")
    hard=$(awk '/Max open files/{print $5}' /proc/"$pid"/limits 2>/dev/null || echo "?")
    info "open-file limit: soft=$soft hard=$hard"

    # quick metrics health check
    local types
    types=$(curl -s --max-time 2 http://localhost:9100/metrics 2>/dev/null | grep -c "^# TYPE" || echo 0)
    info "metric types served: $types"

    # caps on binary
    local caps
    caps=$(getcap "$BINARY" 2>/dev/null || echo "none")
    info "binary caps: $caps"
}

# ── main ───────────────────────────────────────────────────────────────────────
CMD="${1:-help}"
case "$CMD" in
    start)   cmd_start   ;;
    stop)    cmd_stop    ;;
    restart) cmd_restart ;;
    status)  cmd_status  ;;
    *)
        echo "Usage: $0 {start|stop|restart|status}"
        echo ""
        echo "  start    — set prereqs (caps, perf_paranoid, debugfs) and launch"
        echo "  stop     — gracefully stop nyquist"
        echo "  restart  — stop then start"
        echo "  status   — show PID, fd limits, metric count, binary caps"
        echo ""
        echo "Config:  $CONFIG"
        echo "Log:     $LOGFILE"
        echo "Binary:  $BINARY"
        exit 1
        ;;
esac
