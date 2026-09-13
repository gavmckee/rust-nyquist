const $ = (id) => document.getElementById(id);
const colors = [
  "#6aebc5",
  "#76b9ff",
  "#e9b975",
  "#cd9bff",
  "#ff8c9b",
  "#ade080",
  "#87deed",
  "#dfb8a0",
];
const state = {
  rows: [],
  files: new Set(),
  selected: "",
  enabled: new Set(),
  page: 0,
  filtered: [],
  groups: [],
  points: [],
  busy: false,
};
const key = (r) => JSON.stringify([r.name, r.labels, r.kind, r.unit]);
const label = (r) =>
  Object.entries(r.labels)
    .map(([k, v]) => `${k}=${v}`)
    .join(" · ") || "(no labels)";
const utc = (ts) =>
  new Date(ts).toISOString().replace("T", " ").replace("Z", "");
const inputTime = (ts) => new Date(ts).toISOString().slice(0, 23);
const format = (n) =>
  Intl.NumberFormat("en", {
    notation: "compact",
    maximumFractionDigits: 2,
  }).format(n);
function status(message, error = false) {
  $("status").textContent = message;
  $("status").classList.toggle("error", error);
}
function el(tag, text, className) {
  const e = document.createElement(tag);
  if (text !== undefined) e.textContent = text;
  if (className) e.className = className;
  return e;
}
async function api(url, options) {
  const response = await fetch(url, options);
  if (!response.ok) {
    const text = await response.text();
    let message = text;
    try {
      message = JSON.parse(text).error || text;
    } catch {}
    throw new Error(message || `Request failed (${response.status})`);
  }
  return response.json();
}
async function refresh() {
  try {
    const files = await api("/api/files");
    $("server-file").replaceChildren(
      new Option(
        files.length ? "Choose a recording…" : "No server recordings",
        "",
      ),
    );
    for (const f of files)
      $("server-file").add(
        new Option(`${f.name} · ${format(f.bytes)}B`, f.name),
      );
    $("server-hint").textContent = files.length
      ? `${files.length} recordings available. Open several to compare time ranges.`
      : "Use --recordings <directory> to browse server files, or upload a downloaded file.";
    $("open-server").disabled = true;
  } catch (e) {
    status(e.message, true);
  }
}
async function load(name, id, request) {
  if (state.files.has(id)) {
    status(`${name} is already loaded.`);
    return;
  }
  const result = await request();
  // Exact duplicate observations across overlapping recordings are included once.
  const known = new Set(state.rows.map((r) => JSON.stringify(r)));
  const additions = [];
  for (const row of result.rows) {
    const fingerprint = JSON.stringify(row);
    if (!known.has(fingerprint)) {
      additions.push(row);
      known.add(fingerprint);
    }
  }
  if (state.rows.length + additions.length > 200000)
    throw new Error(
      "Session exceeds 200,000 rows. Clear the session and open fewer files.",
    );
  for (const row of additions) state.rows.push(row);
  state.rows.sort((a, b) => a.ts - b.ts);
  state.files.add(id);
  $("file-chips").append(el("span", name, "chip"));
  $("clear").disabled = false;
  overview();
  metricList();
  if (!state.selected && state.rows.length) choose(state.rows[0].name);
  else if (state.selected) choose(state.selected);
  status(
    `Loaded ${name} · ${result.rows.length.toLocaleString()} observations. ${state.files.size} file(s) in this session.`,
  );
}
async function withBusy(fn) {
  if (state.busy) return;
  state.busy = true;
  $("dropzone").setAttribute("aria-busy", "true");
  $("open-server").disabled = true;
  $("clear").disabled = true;
  try {
    await fn();
  } catch (e) {
    status(e.message, true);
  } finally {
    state.busy = false;
    $("dropzone").setAttribute("aria-busy", "false");
    $("open-server").disabled = !$("server-file").value;
    $("clear").disabled = !state.files.size;
  }
}
function upload(files) {
  withBusy(async () => {
    for (const file of files) {
      if (!file.name.toLowerCase().endsWith(".parquet"))
        throw new Error("Choose .parquet recordings.");
      if (file.size > 128 * 1024 * 1024)
        throw new Error(`${file.name} exceeds 128 MiB.`);
      status(`Reading ${file.name}…`);
      await load(
        file.name,
        `upload:${file.name}:${file.size}:${file.lastModified}`,
        () =>
          api("/api/upload", {
            method: "POST",
            body: file,
            headers: { "Content-Type": "application/octet-stream" },
          }),
      );
    }
  });
}
function overview() {
  const n = state.rows.length;
  $("row-count").textContent = n.toLocaleString();
  $("metric-count").textContent = new Set(
    state.rows.map((r) => r.name),
  ).size.toLocaleString();
  $("series-count").textContent = new Set(
    state.rows.map(key),
  ).size.toLocaleString();
  $("span").textContent = n
    ? `${utc(state.rows[0].ts)} → ${utc(state.rows[n - 1].ts)}`
    : "—";
  $("empty").hidden = !!n;
  $("workspace").hidden = !n;
}
function metricList() {
  const query = $("search").value.toLowerCase();
  $("metric-list").replaceChildren();
  for (const name of [...new Set(state.rows.map((r) => r.name))]
    .sort()
    .filter((n) => n.toLowerCase().includes(query))) {
    const b = el("button", name, "metric-item");
    b.classList.toggle("active", name === state.selected);
    b.onclick = () => choose(name);
    $("metric-list").append(b);
  }
}
function choose(name) {
  state.selected = name;
  state.page = 0;
  metricList();
  const rows = state.rows.filter((r) => r.name === name);
  if (!rows.length) return;
  $("metric-title").textContent = name;
  $("metric-kind").textContent = [
    ...new Set(rows.map((r) => `${r.kind} · ${r.unit}`)),
  ]
    .join(" / ")
    .toUpperCase();
  $("label-filters").replaceChildren();
  const shapes = [
    ...new Set(rows.map((r) => JSON.stringify([r.kind, r.unit]))),
  ];
  const shapeLabel = el("label", "KIND / UNIT");
  const shapeSelect = el("select");
  shapeSelect.id = "shape-filter";
  shapeSelect.setAttribute("aria-label", "Metric kind and unit");
  for (const shape of shapes)
    shapeSelect.add(new Option(JSON.parse(shape).join(" · "), shape));
  shapeLabel.hidden = shapes.length === 1;
  shapeLabel.append(shapeSelect);
  $("label-filters").append(shapeLabel);
  shapeSelect.onchange = () => {
    state.enabled = new Set(
      [
        ...new Set(
          rows
            .filter(
              (r) => JSON.stringify([r.kind, r.unit]) === shapeSelect.value,
            )
            .map(key),
        ),
      ].slice(0, 8),
    );
    state.page = 0;
    render();
  };
  const keys = [...new Set(rows.flatMap((r) => Object.keys(r.labels)))].sort();
  for (const k of keys) {
    const l = el("label", k.toUpperCase());
    const s = el("select");
    s.dataset.key = k;
    s.setAttribute("aria-label", `Filter ${k}`);
    s.add(new Option(`All ${k}`, ""));
    for (const v of [
      ...new Set(rows.map((r) => r.labels[k]).filter((v) => v !== undefined)),
    ].sort())
      s.add(new Option(v, JSON.stringify(v)));
    s.onchange = () => {
      state.page = 0;
      render();
    };
    l.append(s);
    $("label-filters").append(l);
  }
  state.enabled = new Set(
    [
      ...new Set(
        rows
          .filter((r) => JSON.stringify([r.kind, r.unit]) === shapeSelect.value)
          .map(key),
      ),
    ].slice(0, 8),
  );
  resetRange();
}
function resetRange() {
  const rows = state.rows.filter((r) => r.name === state.selected);
  if (!rows.length) return;
  $("from").value = inputTime(rows[0].ts);
  $("to").value = inputTime(rows.at(-1).ts);
  state.page = 0;
  render();
}
function render() {
  if (!state.selected) return;
  const from = $("from").value ? Date.parse($("from").value + "Z") : -Infinity;
  const to = $("to").value ? Date.parse($("to").value + "Z") : Infinity;
  const shape = $("shape-filter").value;
  const isCounter = JSON.parse(shape)[0] === "counter";
  $("measure").querySelector('[value="rate"]').disabled = !isCounter;
  if (!isCounter && $("measure").value === "rate") $("measure").value = "p99";
  const filters = [...$("label-filters").querySelectorAll("select[data-key]")]
    .filter((s) => s.value)
    .map((s) => [s.dataset.key, JSON.parse(s.value)]);
  const groups = new Map();
  for (const r of state.rows) {
    if (
      r.name !== state.selected ||
      JSON.stringify([r.kind, r.unit]) !== shape ||
      filters.some(([k, v]) => r.labels[k] !== v)
    )
      continue;
    const k = key(r);
    if (!groups.has(k)) groups.set(k, []);
    groups.get(k).push(r);
  }
  $("legend").replaceChildren();
  let i = 0;
  for (const [k, rows] of groups) {
    const b = el("button", label(rows[0]));
    const color = colors[i++ % colors.length];
    b.style.setProperty("--series", color);
    b.classList.toggle("active", state.enabled.has(k));
    b.setAttribute("aria-pressed", state.enabled.has(k));
    b.title = `Toggle ${label(rows[0])}`;
    b.onclick = () => {
      if (state.enabled.has(k)) state.enabled.delete(k);
      else if (state.enabled.size < 20) state.enabled.add(k);
      else {
        status(
          "Plot up to 20 series at a time. Deselect a series first.",
          true,
        );
        return;
      }
      state.page = 0;
      render();
    };
    $("legend").append(b);
  }
  const measure = $("measure").value;
  state.groups = [];
  state.filtered = [];
  i = 0;
  for (const [k, rows] of groups) {
    const color = colors[i++ % colors.length];
    if (!state.enabled.has(k)) continue;
    let prev = null;
    const points = [];
    for (const row of rows) {
      let value = row[measure];
      if (measure === "rate") {
        value = null;
        if (prev && row.ts > prev.ts) {
          const delta = BigInt(row.raw) - BigInt(prev.raw);
          if (delta >= 0n) value = Number(delta) / ((row.ts - prev.ts) / 1000);
        }
        prev = row;
      }
      if (row.ts < from || row.ts > to) continue;
      state.filtered.push(row);
      points.push({
        ts: row.ts,
        value: value === null ? null : Number(value),
        row,
      });
    }
    state.groups.push({ name: label(rows[0]), color, points });
  }
  state.filtered.sort((a, b) => a.ts - b.ts);
  const kinds = [...new Set(state.filtered.map((r) => r.kind))];
  let note =
    measure === "rate"
      ? "Rate = raw counter delta / elapsed seconds. Resets and first observations have no rate."
      : measure === "raw"
        ? "Raw counters and distribution counts are cumulative; gauges are instantaneous readings."
        : `Percentiles reflect the recorded sliding window${kinds.includes("counter") ? "; counter percentiles are rates per second" : ""}. Missing histograms are gaps.`;
  note += ` Showing ${state.groups.length} of ${groups.size} labeled series. Toggle series below.`;
  if (from > to) note = "Start time must be before end time.";
  $("plot-note").textContent = note;
  draw();
  table();
}
function table() {
  const rows = state.filtered;
  const pages = Math.max(1, Math.ceil(rows.length / 50));
  state.page = Math.min(state.page, pages - 1);
  $("rows").replaceChildren();
  for (const r of rows.slice(state.page * 50, (state.page + 1) * 50)) {
    const tr = el("tr");
    for (const v of [utc(r.ts), label(r), r.raw, r.p50, r.p90, r.p99, r.p999])
      tr.append(el("td", v ?? "—"));
    $("rows").append(tr);
  }
  $("filtered-count").textContent = `${rows.length.toLocaleString()} rows`;
  $("page").textContent = `Page ${state.page + 1} of ${pages}`;
  $("prev").disabled = state.page === 0;
  $("next").disabled = state.page >= pages - 1;
  $("export").disabled = !rows.length;
}
function draw() {
  $("tooltip").hidden = true;
  const canvas = $("chart"),
    rect = canvas.getBoundingClientRect(),
    dpr = devicePixelRatio || 1;
  canvas.width = rect.width * dpr;
  canvas.height = rect.height * dpr;
  const c = canvas.getContext("2d");
  c.scale(dpr, dpr);
  const w = rect.width,
    h = rect.height,
    pad = { l: 65, r: 15, t: 15, b: 35 };
  c.clearRect(0, 0, w, h);
  state.points = [];
  const points = state.groups
    .flatMap((g) => g.points)
    .filter((p) => p.value !== null && Number.isFinite(p.value));
  $("chart-empty").hidden = !!points.length;
  if (!points.length) return;
  let minX = Infinity,
    maxX = -Infinity,
    minY = Infinity,
    maxY = -Infinity;
  for (const p of points) {
    minX = Math.min(minX, p.ts);
    maxX = Math.max(maxX, p.ts);
    minY = Math.min(minY, p.value);
    maxY = Math.max(maxY, p.value);
  }
  if (minX === maxX) {
    minX -= 1000;
    maxX += 1000;
  }
  if (minY === maxY) {
    const delta = Math.abs(minY) * 0.1 || 1;
    minY = Math.max(0, minY - delta);
    maxY += delta;
  }
  const x = (t) => pad.l + ((t - minX) / (maxX - minX)) * (w - pad.l - pad.r),
    y = (v) => h - pad.b - ((v - minY) / (maxY - minY)) * (h - pad.t - pad.b);
  c.font = "10px system-ui";
  for (let j = 0; j <= 4; j++) {
    const yy = pad.t + ((h - pad.t - pad.b) * j) / 4;
    c.strokeStyle = "#273345";
    c.beginPath();
    c.moveTo(pad.l, yy);
    c.lineTo(w - pad.r, yy);
    c.stroke();
    c.fillStyle = "#91a2b7";
    c.textAlign = "right";
    c.fillText(format(maxY - ((maxY - minY) * j) / 4), pad.l - 10, yy + 4);
  }
  for (let j = 0; j <= 3; j++) {
    const t = minX + ((maxX - minX) * j) / 3;
    c.textAlign = j === 0 ? "left" : j === 3 ? "right" : "center";
    c.fillText(new Date(t).toISOString().slice(11, 23), x(t), h - 10);
  }
  for (const g of state.groups) {
    c.strokeStyle = g.color;
    c.fillStyle = g.color;
    c.lineWidth = 1.8;
    c.beginPath();
    let started = false;
    for (const p of g.points) {
      if (p.value === null || !Number.isFinite(p.value)) {
        started = false;
        continue;
      }
      const px = x(p.ts),
        py = y(p.value);
      if (started) c.lineTo(px, py);
      else c.moveTo(px, py);
      started = true;
      state.points.push({ ...p, x: px, y: py, name: g.name, color: g.color });
    }
    c.stroke();
    if (g.points.length <= 60)
      for (const p of g.points) {
        if (p.value === null) continue;
        c.beginPath();
        c.arc(x(p.ts), y(p.value), 2.6, 0, Math.PI * 2);
        c.fill();
      }
  }
}
$("chart").onmousemove = (e) => {
  if (!state.points.length) return;
  const rect = e.currentTarget.getBoundingClientRect(),
    x = e.clientX - rect.left,
    y = e.clientY - rect.top;
  let nearest = null,
    distance = Infinity;
  for (const p of state.points) {
    const d = Math.hypot(p.x - x, p.y - y);
    if (d < distance) {
      distance = d;
      nearest = p;
    }
  }
  const t = $("tooltip");
  if (!nearest || distance > 70) {
    t.hidden = true;
    return;
  }
  const value =
    $("measure").value === "rate"
      ? nearest.value.toPrecision(6)
      : nearest.row[$("measure").value];
  t.textContent = `${utc(nearest.ts)} UTC\n${nearest.name}\n${$("measure").selectedOptions[0].text}: ${value}`;
  t.hidden = false;
  t.style.left = `${Math.max(0, Math.min(x + 12, rect.width - 260))}px`;
  t.style.top = `${Math.max(0, Math.min(y + 12, rect.height - 100))}px`;
};
$("chart").onmouseleave = () => ($("tooltip").hidden = true);
function csvCell(value) {
  let s = String(value ?? "");
  if (/^[=+@-]/.test(s)) s = "'" + s;
  return '"' + s.replaceAll('"', '""') + '"';
}
$("export").onclick = () => {
  const fields = [
    "ts_unix_ms",
    "time_utc",
    "name",
    "labels_json",
    "kind",
    "unit",
    "raw",
    "p50",
    "p90",
    "p99",
    "p999",
  ];
  const lines = [fields.join(",")];
  for (const r of state.filtered)
    lines.push(
      [
        r.ts,
        utc(r.ts),
        r.name,
        JSON.stringify(r.labels),
        r.kind,
        r.unit,
        r.raw,
        r.p50,
        r.p90,
        r.p99,
        r.p999,
      ]
        .map(csvCell)
        .join(","),
    );
  const url = URL.createObjectURL(
    new Blob([lines.join("\r\n")], { type: "text/csv;charset=utf-8" }),
  );
  const a = el("a");
  a.href = url;
  a.download = "nyquist-selection.csv";
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
};
$("upload").onchange = (e) => {
  upload([...e.target.files]);
  e.target.value = "";
};
$("dropzone").onclick = () => $("upload").click();
$("dropzone").onkeydown = (e) => {
  if (e.key === "Enter" || e.key === " ") {
    e.preventDefault();
    $("upload").click();
  }
};
for (const type of ["dragover", "dragleave", "drop"])
  $("dropzone").addEventListener(type, (e) => {
    e.preventDefault();
    $("dropzone").classList.toggle("dragging", type === "dragover");
    if (type === "drop") upload([...e.dataTransfer.files]);
  });
$("server-file").onchange = () =>
  ($("open-server").disabled = !$("server-file").value || state.busy);
$("open-server").onclick = () =>
  withBusy(async () => {
    const name = $("server-file").value;
    status(`Reading ${name}…`);
    await load(name, `server:${name}`, () =>
      api("/api/recording?name=" + encodeURIComponent(name)),
    );
  });
$("refresh").onclick = refresh;
$("search").oninput = metricList;
$("measure").onchange = render;
for (const id of ["from", "to"])
  $(id).onchange = () => {
    state.page = 0;
    render();
  };
$("reset-range").onclick = resetRange;
$("prev").onclick = () => {
  state.page--;
  table();
};
$("next").onclick = () => {
  state.page++;
  table();
};
$("clear").onclick = () => {
  state.rows = [];
  state.files.clear();
  state.selected = "";
  state.enabled.clear();
  state.filtered = [];
  state.groups = [];
  state.points = [];
  state.page = 0;
  $("search").value = "";
  $("tooltip").hidden = true;
  $("file-chips").replaceChildren();
  $("clear").disabled = true;
  overview();
  metricList();
  status("Session cleared. Open another recording to explore.");
};
new ResizeObserver(() => {
  if (state.selected) draw();
}).observe($("chart"));
refresh();
