#!/usr/bin/env python3
"""
nyquist-annotate-watch: polls NIC ring params and sysctl values every 2s.
When anything changes, posts a Grafana annotation so you can correlate
configuration changes with performance metrics on any dashboard panel.

Usage:
    python3 scripts/annotate-watch.py

    # One-shot: post a manual annotation (e.g. "started xfr test")
    python3 scripts/annotate-watch.py --note "started xfr -P 10"

Environment:
    GRAFANA_URL   (default http://localhost:3000)
    GRAFANA_USER  (default admin)
    GRAFANA_PASS  (default admin)
    WATCH_IFACES  (default ens1f0np0,ens1f1np1)
    POLL_SECS     (default 2)
"""
import argparse, base64, json, os, subprocess, sys, time, urllib.request

GRAFANA_URL  = os.environ.get("GRAFANA_URL",  "http://localhost:3000")
GRAFANA_USER = os.environ.get("GRAFANA_USER", "admin")
GRAFANA_PASS = os.environ.get("GRAFANA_PASS", "admin")
IFACES       = os.environ.get("WATCH_IFACES", "ens1f0np0,ens1f1np1").split(",")
POLL         = float(os.environ.get("POLL_SECS", "2"))

SYSCTLS = [
    "net.ipv4.tcp_rmem",
    "net.ipv4.tcp_wmem",
    "net.core.rmem_max",
    "net.core.wmem_max",
    "net.core.netdev_max_backlog",
    "net.ipv4.tcp_congestion_control",
    "net.ipv4.tcp_slow_start_after_idle",
]


# ─── collectors ───────────────────────────────────────────────────────────────

def read_sysctl(key):
    try:
        return subprocess.check_output(
            ["sysctl", "-n", key], text=True, stderr=subprocess.DEVNULL
        ).strip()
    except Exception:
        return None


def read_ring(iface):
    try:
        out = subprocess.check_output(
            ["ethtool", "-g", iface], text=True, stderr=subprocess.DEVNULL
        )
    except Exception:
        return {}
    result, in_current = {}, False
    for line in out.splitlines():
        if "Current hardware" in line:
            in_current = True
            continue
        if not in_current or not line.strip():
            continue
        key, _, val = line.partition(":")
        k = key.strip().lower().replace(" ", "_")
        if k in ("rx", "tx"):
            result[k] = val.strip()
    return result


def read_channels(iface):
    try:
        out = subprocess.check_output(
            ["ethtool", "-l", iface], text=True, stderr=subprocess.DEVNULL
        )
    except Exception:
        return {}
    result, in_current = {}, False
    for line in out.splitlines():
        if "Current hardware" in line:
            in_current = True
            continue
        if not in_current or not line.strip():
            continue
        key, _, val = line.partition(":")
        k = key.strip().lower()
        if k in ("combined", "rx", "tx"):
            result[k] = val.strip()
    return result


def read_coalesce(iface):
    try:
        out = subprocess.check_output(
            ["ethtool", "-c", iface], text=True, stderr=subprocess.DEVNULL
        )
    except Exception:
        return {}
    result = {}
    for line in out.splitlines():
        if ":" not in line:
            continue
        key, _, val = line.partition(":")
        k = key.strip().lower().replace("-", "_")
        if k in ("rx_usecs", "tx_usecs", "adaptive_rx", "adaptive_tx"):
            result[k] = val.strip()
    return result


def read_mtu(iface):
    try:
        return open(f"/sys/class/net/{iface}/mtu").read().strip()
    except Exception:
        return None


def snapshot():
    state = {}
    for key in SYSCTLS:
        v = read_sysctl(key)
        if v is not None:
            state[f"sysctl:{key}"] = v
    for iface in IFACES:
        for k, v in read_ring(iface).items():
            state[f"ring:{iface}:{k}"] = v
        for k, v in read_channels(iface).items():
            state[f"channels:{iface}:{k}"] = v
        for k, v in read_coalesce(iface).items():
            state[f"coalesce:{iface}:{k}"] = v
        m = read_mtu(iface)
        if m:
            state[f"mtu:{iface}"] = m
    return state


# ─── Grafana annotation ───────────────────────────────────────────────────────

def post_annotation(text, tags):
    auth = base64.b64encode(f"{GRAFANA_USER}:{GRAFANA_PASS}".encode()).decode()
    body = json.dumps({"text": text, "tags": tags}).encode()
    req = urllib.request.Request(
        f"{GRAFANA_URL}/api/annotations",
        data=body,
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Basic {auth}",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=5) as r:
            return json.loads(r.read()).get("id")
    except Exception as e:
        print(f"  [grafana annotation failed: {e}]", flush=True)
        return None


def fmt_change(state_key, old, new):
    """Return (human_text, tags_list) for a state change."""
    parts = state_key.split(":", 2)
    kind  = parts[0]

    if kind == "sysctl":
        param = parts[1]
        # Format multi-value sysctls (e.g. tcp_rmem) more readably
        def fmt(v):
            vals = v.split()
            if len(vals) == 3:
                labels = ["min", "default", "max"]
                return " / ".join(f"{l}={int(x)//1024}K" for l, x in zip(labels, vals))
            return v
        text = f"{param}:  {fmt(old)}  →  {fmt(new)}"
        tags = ["sysconfig", "sysctl"]
        # Add a more specific tag
        if "rmem" in param or "wmem" in param:
            tags.append("tcp-buffers")
        elif "congestion" in param:
            tags.append("congestion-control")

    elif kind == "ring":
        iface, param = parts[1], parts[2]
        text = f"ring.{param}: {old} → {new}  ({iface})"
        tags = ["sysconfig", "ring", iface]

    elif kind == "channels":
        iface, param = parts[1], parts[2]
        text = f"channels.{param}: {old} → {new}  ({iface})"
        tags = ["sysconfig", "channels", iface]

    elif kind == "coalesce":
        iface, param = parts[1], parts[2]
        text = f"coalesce.{param}: {old} → {new}  ({iface})"
        tags = ["sysconfig", "coalesce", iface]

    elif kind == "mtu":
        iface = parts[1]
        text = f"MTU: {old} → {new}  ({iface})"
        tags = ["sysconfig", "mtu", iface]

    else:
        text = f"{state_key}: {old} → {new}"
        tags = ["sysconfig"]

    return text, tags


# ─── main ─────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="Watch NIC/sysctl config, annotate Grafana on change.")
    parser.add_argument("--note", metavar="TEXT", help="Post a manual annotation and exit.")
    args = parser.parse_args()

    if args.note:
        ann_id = post_annotation(args.note, ["manual", "sysconfig"])
        if ann_id:
            print(f"Annotation posted: id={ann_id}  text={args.note!r}")
        else:
            print("Failed to post annotation.", file=sys.stderr)
            sys.exit(1)
        return

    print(f"nyquist-annotate-watch  ({GRAFANA_URL})")
    print(f"Interfaces: {', '.join(IFACES)}  |  Poll: {POLL}s")
    print()

    prev = snapshot()
    print(f"Baseline: {len(prev)} config values")
    for k, v in sorted(prev.items()):
        print(f"  {k} = {v}")
    print()
    print("Watching... (Ctrl+C to stop, or run --note 'text' for manual annotations)")
    print()

    try:
        while True:
            time.sleep(POLL)
            curr = snapshot()
            for key in curr:
                old = prev.get(key)
                new = curr[key]
                if old is not None and old != new:
                    text, tags = fmt_change(key, old, new)
                    ann_id = post_annotation(text, tags)
                    ts = time.strftime("%H:%M:%S")
                    marker = f"[ann:{ann_id}]" if ann_id else "[post failed]"
                    print(f"{ts}  {marker}  {text}", flush=True)
            prev = curr
    except KeyboardInterrupt:
        print("\nStopped.")


if __name__ == "__main__":
    main()
