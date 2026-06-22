# Linux Handoff — Rezolus Alignment

This branch (`rezolus-alignment`) was started on macOS. The non-BPF, macOS-buildable
parts of **Plan A** are done and pushed. This file is the pickup point for the
Linux machine, where the eBPF work (Plan B) and the remaining sampler task (A3)
can actually build and run.

**Plans:**
- `docs/superpowers/plans/2026-06-22-rezolus-alignment-A-foundation.md` (Plan A)
- `docs/superpowers/plans/2026-06-22-rezolus-alignment-B-bpf-slice.md` (Plan B)
- Design: `docs/superpowers/specs/2026-06-22-rezolus-alignment-design.md`

## State (what's already committed)

| Task | Status |
|---|---|
| A1 — bucket-array `MetricSnapshot` + `percentiles_from_buckets` | ✅ done, tested on macOS |
| A2 — sinks compute percentiles from buckets; `spawn_sink`/`main.rs` rewired | ✅ done; `main.rs` **not yet compiled** (needs Linux — root bin pulls `aya`) |
| A4 — golden baseline fixture + parity gate | ✅ done; fixture is a **seed** to replace with a live capture |
| A5 — `docs/principles.md` | ✅ done |
| A6 — `docs/coverage-map.md` | ✅ done |
| **A3 — `linkme` SAMPLERS registration in `nyquist-samplers`** | ⛔ **not started** (was blocked on macOS by `inet_diag` netlink) |
| **Plan B (B0–B9) — libbpf-rs toolchain + TCP slice + aya removal** | ⛔ **not started** |

Note: a one-line `#[cfg(target_os="linux")]` gate was added to
`crates/nyquist-sysconfig/src/ethtool.rs` (`SOCK_CLOEXEC`) so the sink crates
built on macOS. Harmless on Linux.

## Prerequisites (install on the Linux box)

```bash
# Debian/Ubuntu example
sudo apt-get update
sudo apt-get install -y clang llvm libelf-dev linux-tools-common linux-tools-$(uname -r) build-essential pkg-config
clang --version && bpftool version    # both must succeed
rustup toolchain install stable && rustup default stable
```

## Step 0 — get the code

```bash
git clone https://github.com/gavmckee/rust-nyquist.git
cd rust-nyquist
git checkout rezolus-alignment
git pull
# Plan B ports verbatim files from rezolus. Clone it where the plan expects:
git clone https://github.com/iopsystems/rezolus.git ~/projects/rezolus
# (If the plan's absolute path differs from your clone location, tell Claude.)
```

## Step 1 — verify Plan A on Linux (validates what macOS couldn't)

```bash
cargo build --workspace          # must compile main.rs (aya present, pre-Plan-B)
cargo test --workspace           # full suite incl. samplers, recorder, exposition
```
Both must be green before continuing. This is the first real compile of the
`main.rs` wiring from Task A2.

## Step 2 — remaining work, in order

1. **Finish Plan A — Task A3** (`linkme` SAMPLERS registration). Plan A, Task A3.
2. **Capture the real golden baseline** to replace the seed fixture:
   ```bash
   cargo run --release -- --config nyquist.toml &   # let it run > window (60s)
   scripts/capture-golden-baseline.sh > crates/nyquist-exposition/tests/fixtures/golden_metrics.txt
   ```
   Commit it. Then run the gate with `NYQUIST_LIVE_METRICS=<dump>`.
3. **Plan B, Tasks B0 → B9** in order (toolchain, headers/vmlinux.h, build.rs,
   H2 conversion, Registry seam B6, samplers B4/B5, wiring B7, tests B8, aya
   removal B9). B9 deletes aya **only after the parity gate passes**.

Run BPF load/integration tests as root (or with `CAP_BPF`+`CAP_PERFMON`).

## How to resume with Claude Code

Start Claude Code in the repo root and paste the prompt in the project README
section below (or just: "Read LINUX-HANDOFF.md and continue from Step 1, using
the executing-plans skill").
