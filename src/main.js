const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// The firmware sends 0 (no force) .. 1023. The sensor sits between A0 and GND with A0's
// internal pull-up (20-50 kΩ, nominal value below) on top, so
//   value = 1023 * R_pullup / (R_pullup + R_sensor)
const PULLUP_OHMS = 35000;
const FULL_SCALE = 1023;

// Typical thin-film "5 kg" sensor (grams -> ohms), used until the user calibrates.
const TYPICAL_SENSOR = [
  [50, 60000],
  [100, 30000],
  [250, 12000],
  [500, 6500],
  [1000, 3500],
  [2000, 2000],
  [5000, 900],
];

// TE FX29 I2C load cell (datasheet): 1000 counts = no force, 15000 = full scale.
const FX29_ZERO = 1000;
const FX29_SPAN = 14000;
const GRAMS_PER_LBF = 453.59237;

const WINDOW_S = 15;   // seconds of history in the chart
const CAPTURE_S = 1;   // seconds averaged when capturing a calibration point
const AVERAGE_S = 2;   // seconds in the steady "average" readout

const UNITS = {
  g: { perGram: 1, decimals: 0 },
  kg: { perGram: 0.001, decimals: 3 },
  N: { perGram: 0.00980665, decimals: 2 },
  lbf: { perGram: 0.00220462, decimals: 3 },
};

const $ = (id) => document.getElementById(id);

// ---------- settings (per machine, best effort) ----------
function load(key, fallback) {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : JSON.parse(v);
  } catch {
    return fallback;
  }
}
function save(key, value) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* storage unavailable: settings just won't persist */
  }
}

const state = {
  device: { state: "searching" },
  sensor: null,         // "fx29" or "fsr", from the readings
  fx29: {
    rangeLbf: load("dynamis.fx29.range", 10),
    zero: load("dynamis.fx29.zero", FX29_ZERO), // counts at no force
    span: load("dynamis.fx29.span", 1),         // optional correction from a known weight
  },
  samples: [],          // { t, value } within the chart window
  latest: null,
  peak: null,
  zeroG: 0,
  unit: load("dynamis.unit", "g"),
  calib: load("dynamis.calib", []), // [{ g, value }]
  paused: false,
  recording: null,      // { startT, rows: [[t, value]], startedAt }
  savedPath: null,
  dirty: true,
};
if (!UNITS[state.unit]) state.unit = "g";

// ---------- sensor math ----------
function conductance(value) {
  // FSR force is close to linear in conductance (1/R), so interpolate in that space.
  if (value <= 0) return 0;
  const v = Math.min(value, FULL_SCALE - 0.5);
  return (1e6 * v) / (PULLUP_OHMS * (FULL_SCALE - v)); // µS
}

function isFx29() {
  return state.sensor === "fx29";
}

// The FX29 is factory-calibrated; the film sensor needs the user's calibration points.
function isCalibrated() {
  return isFx29() || state.calib.length > 0;
}

function fx29Grams(counts, span = state.fx29.span) {
  return ((counts - state.fx29.zero) / FX29_SPAN) * state.fx29.rangeLbf * GRAMS_PER_LBF * span;
}

function curvePoints() {
  const pts = isCalibrated()
    ? state.calib.map((p) => ({ c: conductance(p.value), g: p.g }))
    : TYPICAL_SENSOR.map(([g, ohms]) => ({ c: 1e6 / ohms, g }));
  return [{ c: 0, g: 0 }, ...pts].sort((a, b) => a.c - b.c);
}

// FX29: linear from its counts. Film sensor: piecewise-linear through the calibration
// curve, extrapolating the last segment.
function valueToGrams(value) {
  if (isFx29()) return fx29Grams(value);
  const pts = curvePoints();
  const c = conductance(value);
  let i = 1;
  while (i < pts.length - 1 && c > pts[i].c) i++;
  const p0 = pts[i - 1];
  const p1 = pts[i];
  const dc = p1.c - p0.c;
  if (dc <= 0) return p1.g;
  return p0.g + ((c - p0.c) * (p1.g - p0.g)) / dc;
}

function forceGrams(value) {
  return valueToGrams(value) - state.zeroG;
}

function formatForce(grams) {
  const u = UNITS[state.unit];
  return (grams * u.perGram).toFixed(u.decimals);
}

function displayValue(value) {
  return forceGrams(value) * UNITS[state.unit].perGram;
}

// ---------- UI helpers ----------
let toastTimer = null;
function toast(msg, ms = 3500) {
  const el = $("toast");
  el.textContent = msg;
  el.classList.remove("hidden");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.add("hidden"), ms);
}

function markDirty() {
  state.dirty = true;
}

// ---------- device ----------
function onDeviceStatus(s) {
  const was = state.device.state;
  state.device = s;
  const connected = s.state === "connected";

  if (connected && was !== "connected") {
    state.samples = [];
    state.latest = null;
    state.sensor = null;
  }
  if (!connected && was === "connected" && state.recording) stopRecording();

  $("setup").classList.toggle("hidden", connected);
  $("meter").classList.toggle("hidden", !connected);
  $("setup").classList.toggle("error", s.state === "error");
  $("install-bar").classList.toggle("hidden", s.state !== "installing");
  $("setup-actions").classList.toggle("hidden", s.state !== "error");
  $("setup-action-btn").textContent = "Try again";

  const status = $("status");
  const text = $("status-text");
  switch (s.state) {
    case "searching":
      status.className = "status";
      text.textContent = "No Arduino found";
      $("setup-title").textContent = "Plug in the Arduino";
      $("setup-text").textContent = "Dynamis finds the Uno by itself. Nothing to install or select.";
      break;
    case "connecting":
      status.className = "status waiting";
      text.textContent = `Connecting to ${s.board}…`;
      $("setup-title").textContent = "Connecting…";
      $("setup-text").textContent = `Found the ${s.board} on ${s.port}. Checking its firmware.`;
      break;
    case "installing": {
      const pct = Math.round(s.progress * 100);
      status.className = "status waiting";
      text.textContent = `Installing firmware ${pct}%`;
      $("setup-title").textContent = "Setting up the Arduino…";
      $("setup-text").textContent = `${s.step} (${s.board}). Keep it plugged in.`;
      $("install-fill").style.width = `${pct}%`;
      break;
    }
    case "connected":
      status.className = "status connected";
      text.textContent = `Connected · ${s.port}`;
      $("board-value").textContent = s.board;
      break;
    case "error":
      status.className = "status error";
      text.textContent = "Problem";
      $("setup-title").textContent = "Couldn't connect";
      $("setup-text").textContent = s.message;
      break;
  }
  updateControls();
  markDirty();
}

function onReading({ t, sensor, value }) {
  if (sensor !== state.sensor) {
    // First reading, or the sensor was swapped: start the display afresh.
    state.sensor = sensor;
    state.samples = [];
    state.latest = null;
    state.peak = null;
    state.zeroG = 0;
    renderCalibTable();
  }
  const s = { t, value };
  const first = !state.latest;
  state.latest = s;
  if (first) updateControls();
  state.samples.push(s);
  const cutoff = t - WINDOW_S - 1;
  while (state.samples.length && state.samples[0].t < cutoff) state.samples.shift();

  if (state.peak === null || value > state.peak) state.peak = value;

  if (state.recording) {
    if (state.recording.startT === null) state.recording.startT = t;
    state.recording.rows.push([t - state.recording.startT, sensor, value]);
  }
  markDirty();
}

function recentAverage(seconds) {
  if (!state.latest) return null;
  const recent = state.samples.filter((s) => s.t >= state.latest.t - seconds);
  return recent.reduce((sum, s) => sum + s.value, 0) / recent.length;
}

// ---------- FX29 ----------
function zeroFx29() {
  const counts = recentAverage(0.5);
  if (counts === null) return;
  state.fx29.zero = counts;
  save("dynamis.fx29.zero", counts);
  toast("Zeroed. This is remembered for next time.");
  markDirty();
}

function checkFx29() {
  const grams = parseFloat($("fx29-weight").value);
  if (!(grams > 0)) {
    toast("Type the known weight in grams first.");
    return;
  }
  const measured = fx29Grams(recentAverage(CAPTURE_S), 1);
  if (!(measured > grams * 0.2)) {
    toast("The sensor reads almost nothing. Is the weight centered on the button, and did you Zero it with nothing on it?", 7000);
    return;
  }
  state.fx29.span = grams / measured;
  save("dynamis.fx29.span", state.fx29.span);
  $("fx29-weight").value = "";
  toast(`Corrected by ×${state.fx29.span.toFixed(3)}.`);
  renderCalibTable();
  markDirty();
}

// ---------- calibration ----------
function captureCalibration() {
  const grams = parseFloat($("calib-weight").value);
  if (!(grams > 0)) {
    toast("Type the known weight in grams first.");
    return;
  }
  if (!state.latest) {
    toast("Waiting for readings from the sensor.");
    return;
  }
  const recent = state.samples.filter((s) => s.t >= state.latest.t - CAPTURE_S);
  const value = recent.reduce((sum, s) => sum + s.value, 0) / recent.length;
  if (value < 3) {
    toast("The sensor reads ~0. Make sure the weight rests on the round sensing area.");
    return;
  }
  state.calib = state.calib.filter((p) => p.g !== grams);
  state.calib.push({ g: grams, value: Math.round(value * 10) / 10 });
  state.calib.sort((a, b) => a.g - b.g);
  if (state.calib.some((p, i) => i > 0 && p.value <= state.calib[i - 1].value)) {
    toast("A heavier weight gave a lower reading. Check the placement and recapture that point.", 7000);
  }
  onCalibChanged();
  $("calib-weight").value = "";
}

function onCalibChanged() {
  save("dynamis.calib", state.calib);
  state.zeroG = 0;
  renderCalibTable();
  updateControls();
  markDirty();
}

function renderCalibTable() {
  $("calib-fsr").classList.toggle("hidden", isFx29());
  $("calib-fx29").classList.toggle("hidden", !isFx29());
  $("fx29-range").value = String(state.fx29.rangeLbf);
  $("fx29-span").textContent =
    Math.abs(state.fx29.span - 1) < 1e-6 ? "none (factory calibration)" : `×${state.fx29.span.toFixed(3)}`;
  const body = $("calib-body");
  body.innerHTML = "";
  if (!isCalibrated()) {
    body.innerHTML = '<tr><td colspan="3" class="empty">No points yet: using the typical-sensor estimate</td></tr>';
    return;
  }
  state.calib.forEach((p, i) => {
    const tr = document.createElement("tr");
    tr.innerHTML = `<td>${p.g}</td><td>${p.value.toFixed(1)}</td>`;
    const td = document.createElement("td");
    const btn = document.createElement("button");
    btn.className = "ghost small";
    btn.textContent = "Remove";
    btn.onclick = () => {
      state.calib.splice(i, 1);
      onCalibChanged();
    };
    td.appendChild(btn);
    tr.appendChild(td);
    body.appendChild(tr);
  });
}

// ---------- recording ----------
function startRecording() {
  state.recording = { startT: null, rows: [], startedAt: new Date() };
  updateControls();
}

async function stopRecording() {
  const rec = state.recording;
  state.recording = null;
  updateControls();
  if (!rec || rec.rows.length === 0) {
    toast("Nothing recorded.");
    return;
  }
  const lines = [`time_s,sensor,raw_reading,force_g,force_N,calibrated`];
  const cal = isCalibrated() ? "yes" : "no (typical-sensor estimate)";
  for (const [t, sensor, value] of rec.rows) {
    if (sensor !== state.sensor) continue; // sensor swapped mid-recording
    const g = forceGrams(value);
    lines.push(`${t.toFixed(3)},${sensor},${value},${g.toFixed(1)},${(g * UNITS.N.perGram).toFixed(3)},${cal}`);
  }
  const d = rec.startedAt;
  const pad = (n) => String(n).padStart(2, "0");
  const fileName = `force_${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}_${pad(d.getHours())}-${pad(d.getMinutes())}-${pad(d.getSeconds())}`;
  try {
    state.savedPath = await invoke("save_csv", { fileName, contents: lines.join("\n") + "\n" });
    $("saved-path").textContent = state.savedPath;
    $("saved-row").classList.remove("hidden");
    toast(`Saved ${rec.rows.length} readings.`);
  } catch (e) {
    toast(`Could not save the CSV: ${e}`, 6000);
  }
}

// ---------- rendering ----------
function updateControls() {
  const connected = state.device.state === "connected";
  $("zero-btn").disabled = !state.latest;
  $("unzero-btn").disabled = isFx29() ? state.fx29.zero === FX29_ZERO : state.zeroG === 0;
  $("fx29-check-btn").disabled = !connected;
  $("calib-add-btn").disabled = !connected;
  $("calib-clear-btn").disabled = !isCalibrated();

  const rec = $("record-btn");
  rec.disabled = !connected && !state.recording;
  rec.textContent = state.recording ? "Stop & save" : "Start recording";
  rec.className = state.recording ? "danger" : "primary";
  if (!state.recording) $("record-info").textContent = "";

  for (const b of document.querySelectorAll("#unit-group button")) {
    b.classList.toggle("active", b.dataset.unit === state.unit);
  }
  $("calib-warning").classList.toggle("hidden", isCalibrated());
  $("force-approx").classList.toggle("hidden", isCalibrated());
}

function renderReadout() {
  $("force-unit").textContent = state.unit;
  $("peak-unit").textContent = state.unit;
  $("avg-unit").textContent = state.unit;
  const latest = state.latest;
  const avg = recentAverage(AVERAGE_S);
  $("force-value").textContent = latest ? formatForce(forceGrams(latest.value)) : "—";
  $("avg-value").textContent = avg === null ? "—" : formatForce(forceGrams(avg));
  $("raw-label").textContent = isFx29() ? "Raw counts" : "Sensor reading";
  $("raw-value").textContent = latest ? latest.value.toFixed(0) : "—";
  $("peak-value").textContent = state.peak === null ? "—" : formatForce(forceGrams(state.peak));
  if (state.device.state === "connected") {
    const sensor = isFx29() ? `FX29 ${state.fx29.rangeLbf} lbf` : state.sensor === "fsr" ? "film sensor" : "…";
    $("board-value").textContent = `${state.device.board} · ${sensor}`;
  }

  if (state.recording) {
    const rows = state.recording.rows;
    const secs = rows.length ? rows[rows.length - 1][0] : 0;
    $("record-info").textContent = `● ${secs.toFixed(1)} s, ${rows.length} readings`;
  }
}

function niceStep(range, targetTicks) {
  const raw = range / targetTicks;
  const mag = Math.pow(10, Math.floor(Math.log10(raw)));
  const norm = raw / mag;
  return (norm < 1.5 ? 1 : norm < 3.5 ? 2 : norm < 7.5 ? 5 : 10) * mag;
}

function renderChart() {
  const canvas = $("chart");
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth;
  const h = canvas.clientHeight;
  if (!w || !h) return;
  if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
  }
  const ctx = canvas.getContext("2d");
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);

  const css = getComputedStyle(document.documentElement);
  const color = (name) => css.getPropertyValue(name).trim();
  const left = 56, right = 12, top = 10, bottom = 26;
  const pw = w - left - right;
  const ph = h - top - bottom;
  if (pw <= 0 || ph <= 0) return;

  const tEnd = state.latest ? state.latest.t : WINDOW_S;
  const tStart = tEnd - WINDOW_S;
  const pts = state.samples.filter((s) => s.t >= tStart).map((s) => ({ t: s.t, v: displayValue(s.value) }));

  const minSpan = 100 * UNITS[state.unit].perGram;
  const dataMax = pts.reduce((m, p) => Math.max(m, p.v), 0);
  const span = Math.max(dataMax * 1.1, minSpan);
  const step = niceStep(span, 5);
  const yMax = Math.ceil(span / step) * step;
  const yMin = Math.min(0, Math.floor(pts.reduce((m, p) => Math.min(m, p.v), 0) / step) * step);
  const x = (t) => left + ((t - tStart) / WINDOW_S) * pw;
  const y = (v) => top + ph - ((v - yMin) / (yMax - yMin)) * ph;

  ctx.font = "12px Segoe UI, system-ui, sans-serif";
  ctx.fillStyle = color("--muted");
  ctx.strokeStyle = color("--grid");
  ctx.lineWidth = 1;
  ctx.textAlign = "right";
  ctx.textBaseline = "middle";
  const decimals = step < 1 ? Math.min(3, Math.ceil(-Math.log10(step))) : 0;
  for (let v = yMin; v <= yMax + step / 2; v += step) {
    const yy = Math.round(y(v)) + 0.5;
    ctx.beginPath();
    ctx.moveTo(left, yy);
    ctx.lineTo(left + pw, yy);
    ctx.stroke();
    ctx.fillText(v.toFixed(decimals), left - 8, yy);
  }
  ctx.textAlign = "center";
  ctx.textBaseline = "top";
  for (let s = 0; s <= WINDOW_S; s += 5) {
    ctx.fillText(s === WINDOW_S ? "now" : `-${WINDOW_S - s}s`, left + (s / WINDOW_S) * pw, top + ph + 6);
  }
  ctx.save();
  ctx.translate(12, top + ph / 2);
  ctx.rotate(-Math.PI / 2);
  ctx.textBaseline = "middle";
  ctx.fillText(state.unit, 0, 0);
  ctx.restore();

  if (state.peak !== null) {
    const pv = displayValue(state.peak);
    if (pv <= yMax) {
      ctx.strokeStyle = color("--peak");
      ctx.setLineDash([5, 4]);
      ctx.beginPath();
      ctx.moveTo(left, y(pv));
      ctx.lineTo(left + pw, y(pv));
      ctx.stroke();
      ctx.setLineDash([]);
    }
  }

  if (pts.length > 1) {
    ctx.strokeStyle = color("--line");
    ctx.lineWidth = 2;
    ctx.lineJoin = "round";
    ctx.beginPath();
    pts.forEach((p, i) => (i ? ctx.lineTo(x(p.t), y(p.v)) : ctx.moveTo(x(p.t), y(p.v))));
    ctx.stroke();
  }
}

function frame() {
  if (state.dirty) {
    state.dirty = false;
    renderReadout();
    if (!state.paused) renderChart();
  }
  requestAnimationFrame(frame);
}

// ---------- wiring ----------
window.addEventListener("DOMContentLoaded", async () => {
  $("window-secs").textContent = WINDOW_S;

  $("setup-action-btn").onclick = () => invoke("retry_device");

  for (const b of document.querySelectorAll("#unit-group button")) {
    b.onclick = () => {
      state.unit = b.dataset.unit;
      save("dynamis.unit", state.unit);
      updateControls();
      markDirty();
    };
  }

  $("peak-reset-btn").onclick = () => {
    state.peak = state.latest ? state.latest.value : null;
    markDirty();
  };
  $("zero-btn").onclick = () => {
    if (!state.latest) return;
    if (isFx29()) zeroFx29();
    else state.zeroG = valueToGrams(state.latest.value);
    updateControls();
    markDirty();
  };
  $("unzero-btn").onclick = () => {
    if (isFx29()) {
      state.fx29.zero = FX29_ZERO;
      save("dynamis.fx29.zero", FX29_ZERO);
    } else {
      state.zeroG = 0;
    }
    updateControls();
    markDirty();
  };
  $("fx29-range").onchange = () => {
    state.fx29.rangeLbf = Number($("fx29-range").value);
    save("dynamis.fx29.range", state.fx29.rangeLbf);
    markDirty();
  };
  $("fx29-check-btn").onclick = checkFx29;
  $("fx29-weight").addEventListener("keydown", (e) => {
    if (e.key === "Enter") checkFx29();
  });
  $("fx29-span-reset").onclick = () => {
    state.fx29.span = 1;
    save("dynamis.fx29.span", 1);
    renderCalibTable();
    markDirty();
  };
  $("pause-btn").onclick = () => {
    state.paused = !state.paused;
    $("pause-btn").textContent = state.paused ? "Resume" : "Pause";
    markDirty();
  };

  $("calib-add-btn").onclick = captureCalibration;
  $("calib-weight").addEventListener("keydown", (e) => {
    if (e.key === "Enter") captureCalibration();
  });
  $("calib-clear-btn").onclick = () => {
    if (confirm("Remove all calibration points?")) {
      state.calib = [];
      onCalibChanged();
    }
  };

  $("record-btn").onclick = () => (state.recording ? stopRecording() : startRecording());
  $("reveal-btn").onclick = async () => {
    try {
      await invoke("reveal_file", { path: state.savedPath });
    } catch (e) {
      toast(String(e));
    }
  };

  new ResizeObserver(markDirty).observe($("chart"));

  await listen("reading", (e) => onReading(e.payload));
  await listen("device-status", (e) => onDeviceStatus(e.payload));
  onDeviceStatus(await invoke("device_status"));

  renderCalibTable();
  requestAnimationFrame(frame);
});
