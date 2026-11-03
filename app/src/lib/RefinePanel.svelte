<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Navneet Joneja -->

<script lang="ts">
  import { onDestroy } from "svelte";
  import { app } from "./store.svelte";
  import type { BandParams, EndMode, EndParams, RefineParams, StemName } from "./types";
  import {
    BAND_MAX_DB,
    F_MAX,
    F_MIN,
    PAN_QUICK,
    SHELF_MAX_DB,
    exceptionSide,
    formatHz,
    freqToX,
    heardGain,
    isIdentity,
    logFreqs,
    presetOf,
    quickOf,
    responseDb,
    xToFreq,
    type PanQuick,
  } from "./refine";

  let { stem, label }: { stem: StemName; label: string } = $props();

  const p = $derived(app.refine[stem]);
  const pan = $derived(p.pan);
  const eq = $derived(p.eq);
  const quick = $derived(quickOf(pan));
  const preset = $derived(presetOf(eq, app.refinePresets));
  const id = $derived(`refine-${stem}`);
  /** The row's on switch; the stereo window does the opposite of it. */
  const sw = $derived(app.mix[stem].on);
  const side = $derived(exceptionSide(pan));
  /** Switch off, no exception (and not soloed): the row plays nothing, so the EQ does nothing. */
  const silent = $derived(!sw && !pan.enabled && !app.mix[stem].solo);
  /** How much of the stem at `v` (pan) is heard: what the field display lights. */
  const heard = (v: number) => (!pan.enabled && app.mix[stem].solo ? 1 : heardGain(pan, sw, v));

  // ---- updates (coalesced to one backend call per frame while dragging) ----
  let pending: RefineParams | null = null;
  let raf = 0;
  function update(f: (q: RefineParams) => void) {
    const q: RefineParams = $state.snapshot(pending ?? app.refine[stem]) as RefineParams;
    f(q);
    pending = q;
    app.refine[stem] = q; // immediate UI feedback
    if (!raf) {
      raf = requestAnimationFrame(() => {
        raf = 0;
        const send = pending;
        pending = null;
        if (send) app.setRefine(stem, send);
      });
    }
  }
  onDestroy(() => {
    if (raf) cancelAnimationFrame(raf);
    if (pending) app.setRefine(stem, pending);
  });

  const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
  const num = (v: string, d: number) => (Number.isFinite(parseFloat(v)) ? parseFloat(v) : d);

  // ---- stereo position ----
  function setQuick(k: PanQuick) {
    update((q) => {
      q.pan.enabled = true;
      q.pan.center = PAN_QUICK[k].center;
      q.pan.width = PAN_QUICK[k].width;
    });
  }
  function panText(c: number) {
    if (Math.abs(c) < 0.02) return "Center";
    return `${c < 0 ? "L" : "R"} ${Math.round(Math.abs(c) * 100)}%`;
  }

  // ---- canvases ----
  let fieldCanvas = $state<HTMLCanvasElement>();
  let eqCanvas = $state<HTMLCanvasElement>();
  let fieldW = $state(300);
  let eqW = $state(300);
  const FIELD_H = 64;
  const EQ_H = 112;

  function css(name: string, el: Element) {
    return getComputedStyle(el).getPropertyValue(name).trim() || "#888";
  }
  function setup(c: HTMLCanvasElement, w: number, h: number) {
    const dpr = window.devicePixelRatio || 1;
    if (c.width !== Math.round(w * dpr) || c.height !== Math.round(h * dpr)) {
      c.width = Math.round(w * dpr);
      c.height = Math.round(h * dpr);
    }
    const g = c.getContext("2d")!;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, w, h);
    return g;
  }

  // Smoothed field per pan bin (dB) and band shares, with a slowly falling reference.
  let disp: number[] = new Array(41).fill(-90);
  let shares: number[][] = Array.from({ length: 41 }, () => [0.33, 0.34, 0.33]);
  let ref = -60;
  let haveField = $state(false);
  $effect(() => {
    const f = app.field;
    if (!f || f.length !== 123) return;
    haveField = true;
    let mx = -120;
    for (let i = 0; i < 41; i++) {
      const lo = f[i], mid = f[41 + i], hi = f[82 + i];
      const tot = lo + mid + hi;
      const db = 10 * Math.log10(tot + 1e-12);
      disp[i] = db > disp[i] ? 0.5 * disp[i] + 0.5 * db : 0.85 * disp[i] + 0.15 * db;
      if (tot > 0) shares[i] = [lo / tot, mid / tot, hi / tot];
      mx = Math.max(mx, disp[i]);
    }
    ref = Math.max(mx, ref - 0.3);
    drawField();
  });

  const panToX = (v: number) => ((v + 1) / 2) * fieldW;
  const xToPan = (x: number) => clamp((x / fieldW) * 2 - 1, -1, 1);

  function drawField() {
    const c = fieldCanvas;
    if (!c) return;
    const w = fieldW, h = FIELD_H;
    const g = setup(c, w, h);
    const color = css(`--stem-${stem}`, c);
    const grid = css("--grid-line", c);
    const muted = css("--muted", c);
    // center and thirds
    g.strokeStyle = grid;
    g.lineWidth = 1;
    for (const v of [-1 / 3, 0, 1 / 3]) {
      g.beginPath();
      g.moveTo(Math.round(panToX(v)) + 0.5, 0);
      g.lineTo(Math.round(panToX(v)) + 0.5, h);
      g.stroke();
    }
    // energy bars, stacked by band (low darkest); what isn't heard is dimmed
    const bw = w / 41;
    if (haveField) {
      for (let i = 0; i < 41; i++) {
        const t = clamp((disp[i] - (ref - 36)) / 36, 0, 1);
        const lit = 0.2 + 0.8 * heard((i / 40) * 2 - 1);
        let y = h;
        const total = t * (h - 6);
        [0.45, 0.72, 1].forEach((a, b) => {
          const hh = total * shares[i][b];
          g.globalAlpha = a * 0.85 * lit;
          g.fillStyle = color;
          g.fillRect(i * bw + 1, y - hh, Math.max(1, bw - 2), hh);
          y -= hh;
        });
      }
      g.globalAlpha = 1;
    }
    // with an exception: what is heard (it flips with the switch) shaded
    // under a line, the window's edges and center as handles
    if (pan.enabled) {
      g.fillStyle = css("--accent-soft", c);
      g.strokeStyle = css("--accent", c);
      g.lineWidth = 1.5;
      g.beginPath();
      g.moveTo(0, h);
      for (let x = 0; x <= w; x += 2) g.lineTo(x, h - heard(xToPan(x)) * (h - 2));
      g.lineTo(w, h);
      g.closePath();
      g.fill();
      g.beginPath();
      for (let x = 0; x <= w; x += 2) {
        const y = h - heard(xToPan(x)) * (h - 2);
        if (x === 0) g.moveTo(x, y);
        else g.lineTo(x, y);
      }
      g.stroke();
      // handles: center and the two −6 dB edges
      g.fillStyle = css("--accent", c);
      for (const v of [pan.center - pan.width / 2, pan.center, pan.center + pan.width / 2]) {
        if (v < -1.001 || v > 1.001) continue;
        g.fillRect(Math.round(panToX(v)) - 1, v === pan.center ? 0 : h / 2 - 8, 2, v === pan.center ? h : 16);
      }
    }
    g.fillStyle = muted;
    g.font = "10px system-ui, sans-serif";
    g.textBaseline = "top";
    g.fillText("L", 3, 2);
    g.textAlign = "right";
    g.fillText("R", w - 3, 2);
    g.textAlign = "left";
  }

  // pointer on the field: drag the window (center) or an edge (width)
  let fieldDrag: "center" | "lo" | "hi" | null = null;
  function fieldDown(e: PointerEvent) {
    const r = fieldCanvas!.getBoundingClientRect();
    const x = e.clientX - r.left;
    const near = (v: number) => Math.abs(panToX(v) - x) < 7;
    if (pan.enabled && near(pan.center - pan.width / 2)) fieldDrag = "lo";
    else if (pan.enabled && near(pan.center + pan.width / 2)) fieldDrag = "hi";
    else fieldDrag = "center";
    fieldCanvas!.setPointerCapture(e.pointerId);
    fieldMove(e);
  }
  function fieldMove(e: PointerEvent) {
    if (!fieldDrag) return;
    const r = fieldCanvas!.getBoundingClientRect();
    const v = xToPan(e.clientX - r.left);
    update((q) => {
      q.pan.enabled = true;
      if (fieldDrag === "center") q.pan.center = Math.round(v * 100) / 100;
      else q.pan.width = clamp(Math.round(2 * Math.abs(v - q.pan.center) * 100) / 100, 0.05, 2);
    });
  }
  function fieldUp() {
    fieldDrag = null;
  }

  // ---- EQ curve ----
  const DB_RANGE = BAND_MAX_DB + 3; // ±18 dB on screen
  const fToX = (f: number) => freqToX(f) * eqW;
  const xToF = (x: number) => clamp(xToFreq(x / eqW), F_MIN, F_MAX);
  const dbToY = (d: number) => EQ_H / 2 - (d / DB_RANGE) * (EQ_H / 2 - 6);
  const yToDb = (y: number) => ((EQ_H / 2 - y) / (EQ_H / 2 - 6)) * DB_RANGE;

  type Node = { key: "low" | "high" | 0 | 1 | 2; f: number; db: number; on: boolean; vertical: boolean };
  const nodes = $derived.by((): Node[] => {
    const endNode = (key: "low" | "high", e: EndParams): Node => ({
      key,
      f: e.freq,
      db: e.mode === "shelf" ? e.gain_db : 0,
      on: e.enabled,
      vertical: e.mode === "shelf",
    });
    return [
      endNode("low", eq.low_cut),
      ...eq.bands.map((b, i): Node => ({ key: i as 0 | 1 | 2, f: b.freq, db: b.gain_db, on: b.enabled, vertical: true })),
      endNode("high", eq.high_cut),
    ];
  });

  function drawEq() {
    const c = eqCanvas;
    if (!c) return;
    const w = eqW, h = EQ_H;
    const g = setup(c, w, h);
    const grid = css("--grid-line", c);
    const faint = css("--grid-line-faint", c);
    const muted = css("--muted", c);
    const color = css(`--stem-${stem}`, c);
    g.lineWidth = 1;
    for (const f of [50, 100, 200, 500, 1000, 2000, 5000, 10000]) {
      g.strokeStyle = f === 100 || f === 1000 || f === 10000 ? grid : faint;
      const x = Math.round(fToX(f)) + 0.5;
      g.beginPath();
      g.moveTo(x, 0);
      g.lineTo(x, h);
      g.stroke();
    }
    for (const d of [-12, 0, 12]) {
      g.strokeStyle = d === 0 ? grid : faint;
      const y = Math.round(dbToY(d)) + 0.5;
      g.beginPath();
      g.moveTo(0, y);
      g.lineTo(w, y);
      g.stroke();
    }
    g.fillStyle = muted;
    g.font = "10px system-ui, sans-serif";
    g.textBaseline = "bottom";
    for (const [f, t] of [[100, "100"], [1000, "1k"], [10000, "10k"]] as const) g.fillText(t, fToX(f) + 3, h - 2);
    g.textBaseline = "top";
    g.fillText("+12", 3, dbToY(12) + 1);
    g.fillText("−12", 3, dbToY(-12) + 1);
    // the curve
    const freqs = logFreqs(Math.max(32, Math.round(w / 3)));
    const resp = responseDb(eq, freqs);
    g.strokeStyle = color;
    g.lineWidth = 2;
    g.beginPath();
    freqs.forEach((f, i) => {
      const y = clamp(dbToY(resp[i]), -2, h + 2);
      if (i === 0) g.moveTo(fToX(f), y);
      else g.lineTo(fToX(f), y);
    });
    g.stroke();
    // nodes
    for (const n of nodes) {
      const x = fToX(n.f), y = dbToY(n.db);
      g.beginPath();
      g.arc(x, y, n.on ? 5.5 : 4.5, 0, Math.PI * 2);
      g.fillStyle = n.on ? color : css("--surface-3", c);
      g.fill();
      g.lineWidth = 1.5;
      g.strokeStyle = n.on ? css("--fg", c) : muted;
      g.stroke();
      g.fillStyle = n.on ? css("--bg", c) : muted;
      g.font = "bold 8px system-ui, sans-serif";
      g.textAlign = "center";
      g.textBaseline = "middle";
      g.fillText(n.key === "low" ? "L" : n.key === "high" ? "H" : String(n.key + 1), x, y + 0.5);
      g.textAlign = "left";
    }
  }

  let eqDrag: Node["key"] | null = null;
  function eqDown(e: PointerEvent) {
    const r = eqCanvas!.getBoundingClientRect();
    const x = e.clientX - r.left, y = e.clientY - r.top;
    let best: Node | null = null;
    let bd = 14;
    for (const n of nodes) {
      const d = Math.hypot(fToX(n.f) - x, dbToY(n.db) - y);
      if (d < bd) [best, bd] = [n, d];
    }
    if (!best) return;
    eqDrag = best.key;
    tab = best.key;
    eqCanvas!.setPointerCapture(e.pointerId);
    eqMove(e);
  }
  function eqMove(e: PointerEvent) {
    if (eqDrag == null) return;
    const r = eqCanvas!.getBoundingClientRect();
    const f = Math.round(xToF(e.clientX - r.left));
    const db = Math.round(yToDb(e.clientY - r.top) * 2) / 2;
    const k = eqDrag;
    update((q) => {
      if (k === "low" || k === "high") {
        const end = k === "low" ? q.eq.low_cut : q.eq.high_cut;
        end.enabled = true;
        end.freq = f;
        if (end.mode === "shelf") end.gain_db = clamp(db, -SHELF_MAX_DB, SHELF_MAX_DB);
      } else {
        const b = q.eq.bands[k];
        b.enabled = true;
        b.freq = f;
        b.gain_db = clamp(db, -BAND_MAX_DB, BAND_MAX_DB);
      }
    });
  }
  function eqUp() {
    eqDrag = null;
  }

  // ---- field editing helpers ----
  function setEnd(which: "low" | "high", f: (e: EndParams) => void) {
    update((q) => f(which === "low" ? q.eq.low_cut : q.eq.high_cut));
  }
  function setBand(i: number, f: (b: BandParams) => void) {
    update((q) => f(q.eq.bands[i]));
  }
  function endMode(which: "low" | "high", m: EndMode) {
    setEnd(which, (e) => {
      e.mode = m;
      e.enabled = true;
      if (m === "shelf" && e.gain_db === 0) e.gain_db = which === "low" ? 3 : -3;
    });
  }

  // redraw on data, size and theme changes
  $effect(() => {
    void [pan.enabled, pan.center, pan.width, sw, app.mix[stem].solo, fieldW, app.theme];
    drawField();
  });
  $effect(() => {
    void [JSON.stringify(eq), eqW, app.theme];
    drawEq();
  });

  // Arrow keys on sliders move them, not the playhead/speed (global shortcuts).
  const SLIDER_KEYS = new Set(["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "PageUp", "PageDown", "Home", "End"]);
  function keys(e: KeyboardEvent) {
    const t = e.target as HTMLElement;
    if (t instanceof HTMLInputElement && t.type === "range" && SLIDER_KEYS.has(e.key)) e.stopPropagation();
    if (e.key === "Escape") {
      e.stopPropagation();
      app.closeRefine();
    }
  }

  // ---- EQ controls: one tab per node, each value as a slider plus a number ----
  type Key = Node["key"];
  const TABS: { key: Key; glyph: string; short: string; title: string }[] = [
    { key: "low", glyph: "L", short: "Low", title: "Low end" },
    { key: 0, glyph: "1", short: "Band 1", title: "Band 1" },
    { key: 1, glyph: "2", short: "Band 2", title: "Band 2" },
    { key: 2, glyph: "3", short: "Band 3", title: "Band 3" },
    { key: "high", glyph: "H", short: "High", title: "High end" },
  ];
  let tab = $state<Key>(0);
  const tabIdx = $derived(TABS.findIndex((t) => t.key === tab));
  const tabOn = (k: Key) => (k === "low" ? eq.low_cut.enabled : k === "high" ? eq.high_cut.enabled : eq.bands[k].enabled);

  /** Slider law and keyboard steps. Log specs step in octaves (×2^step). */
  interface Spec {
    min: number;
    max: number;
    log: boolean;
    small: number;
    big: number;
    round: (v: number) => number;
  }
  const POS = 1000;
  const FREQ: Spec = { min: F_MIN, max: F_MAX, log: true, small: 1 / 24, big: 1 / 3, round: Math.round };
  const QS: Spec = { min: 0.3, max: 8, log: true, small: 1 / 12, big: 1 / 2, round: (v) => Math.round(v * 100) / 100 };
  const gainSpec = (m: number): Spec => ({ min: -m, max: m, log: false, small: 0.5, big: 3, round: (v) => Math.round(v * 2) / 2 });
  const toPos = (sp: Spec, v: number) =>
    Math.round(sp.log ? (POS * Math.log(v / sp.min)) / Math.log(sp.max / sp.min) : (POS * (v - sp.min)) / (sp.max - sp.min));
  const fromPos = (sp: Spec, p: number) =>
    sp.round(sp.log ? sp.min * (sp.max / sp.min) ** (p / POS) : sp.min + ((sp.max - sp.min) * p) / POS);
  function stepBy(sp: Spec, v: number, dir: number, big: boolean) {
    const d = big ? sp.big : sp.small;
    let n = sp.round(sp.log ? v * 2 ** (dir * d) : v + dir * d);
    if (n === v) n = sp.round(v + dir * (sp.log ? 1 : d)); // always move
    return clamp(n, sp.min, sp.max);
  }
  /** Arrows (Shift: bigger), Page Up/Down (bigger), Home/End on a slider or number. */
  function stepKeys(e: KeyboardEvent, sp: Spec, v: number, set: (v: number) => void) {
    const dir = { ArrowUp: 1, ArrowRight: 1, PageUp: 1, ArrowDown: -1, ArrowLeft: -1, PageDown: -1 }[e.key];
    const isNum = (e.currentTarget as HTMLInputElement).type === "number";
    if (isNum && (e.key === "ArrowLeft" || e.key === "ArrowRight")) return; // caret
    if (dir) set(stepBy(sp, v, dir, e.shiftKey || e.key.startsWith("Page")));
    else if (e.key === "Home" && !isNum) set(sp.min);
    else if (e.key === "End" && !isNum) set(sp.max);
    else return;
    e.preventDefault();
    e.stopPropagation();
  }

  /** Frequency in the number field: Hz below 1 kHz, kHz above. */
  const kHz = (f: number) => f >= 1000;
  const freqField = (f: number) => (kHz(f) ? String(Math.round(f / 10) / 100) : String(Math.round(f)));
  /** Typed frequency: small numbers are kHz (20 Hz is the minimum anyway). */
  const parseFreq = (n: number, wasKHz: boolean) => (n < 20 || (wasKHz && n <= 20) ? n * 1000 : n);
  const fmtDb = (v: number) => `${v > 0 ? "+" : v < 0 ? "−" : ""}${Math.abs(v)} dB`;

  interface Row {
    name: "Frequency" | "Gain" | "Q";
    sp: Spec;
    v: number;
    /** Number-field text, step and unit. */
    field: string;
    step: string;
    unit: string;
    aria: string;
    text: string;
    parse: (n: number) => number;
    set: (v: number) => void;
  }
  const rows = $derived.by((): Row[] => {
    const k = tab;
    const end = k === "low" || k === "high";
    const title = TABS[tabIdx].title.toLowerCase();
    const who = `${label} ${title}`;
    const f = end ? (k === "low" ? eq.low_cut : eq.high_cut).freq : eq.bands[k].freq;
    const setF = (v: number) =>
      end
        ? setEnd(k, (x) => ((x.enabled = true), (x.freq = v)))
        : setBand(k, (x) => ((x.enabled = true), (x.freq = v)));
    const out: Row[] = [
      {
        name: "Frequency",
        sp: FREQ,
        v: f,
        field: freqField(f),
        step: kHz(f) ? "0.01" : "1",
        unit: kHz(f) ? "kHz" : "Hz",
        aria: `${who} frequency`,
        text: formatHz(f),
        parse: (n) => parseFreq(n, kHz(f)),
        set: setF,
      },
    ];
    if (end) {
      const e = k === "low" ? eq.low_cut : eq.high_cut;
      if (e.mode === "shelf") {
        out.push({
          name: "Gain",
          sp: gainSpec(SHELF_MAX_DB),
          v: e.gain_db,
          field: String(e.gain_db),
          step: "0.5",
          unit: "dB",
          aria: `${who} shelf gain`,
          text: fmtDb(e.gain_db),
          parse: (n) => n,
          set: (v) => setEnd(k, (x) => ((x.enabled = true), (x.gain_db = v))),
        });
      }
    } else {
      const b = eq.bands[k];
      out.push(
        {
          name: "Gain",
          sp: gainSpec(BAND_MAX_DB),
          v: b.gain_db,
          field: String(b.gain_db),
          step: "0.5",
          unit: "dB",
          aria: `${who} gain`,
          text: fmtDb(b.gain_db),
          parse: (n) => n,
          set: (v) => setBand(k, (x) => ((x.enabled = true), (x.gain_db = v))),
        },
        {
          name: "Q",
          sp: QS,
          v: b.q,
          field: String(b.q),
          step: "0.01",
          unit: "",
          aria: `${who} width (Q)`,
          text: `Q ${b.q}${b.q < 1 ? ", broad" : b.q > 3 ? ", narrow" : ""}`,
          parse: (n) => n,
          set: (v) => setBand(k, (x) => (x.q = v)),
        },
      );
    }
    return out;
  });
  /** Commits a typed number (clamped, rounded) and shows the stored value. */
  function commit(r: Row, el: HTMLInputElement) {
    const n = parseFloat(el.value);
    if (Number.isFinite(n)) r.set(clamp(r.sp.round(r.parse(n)), r.sp.min, r.sp.max));
    queueMicrotask(() => {
      const cur = rows.find((x) => x.name === r.name);
      if (cur) el.value = cur.field;
    });
  }
  function tabKeys(e: KeyboardEvent) {
    const d = { ArrowRight: 1, ArrowLeft: -1 }[e.key];
    let i = tabIdx;
    if (d) i = (tabIdx + d + TABS.length) % TABS.length;
    else if (e.key === "Home") i = 0;
    else if (e.key === "End") i = TABS.length - 1;
    else return;
    e.preventDefault();
    e.stopPropagation();
    tab = TABS[i].key;
    document.getElementById(`${id}-tab-${i}`)?.focus();
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<section class="panel" {id} aria-label="Refine {label}" style:--stem-color="var(--stem-{stem})" onkeydown={keys}>
  <div class="top">
    <h3>Refine <span class="who">{label}</span></h3>
    <label class="sr-only" for="{id}-preset">{label} EQ preset</label>
    <select id="{id}-preset" class="preset" value={preset} onchange={(e) => app.applyPreset(stem, e.currentTarget.value)} title="EQ preset (keeps the stereo selection)">
      {#if preset === ""}<option value="" disabled>Custom EQ</option>{/if}
      {#each app.refinePresets as pr (pr.id)}
        <option value={pr.id}>{pr.name}</option>
      {/each}
    </select>
    <button class="btn small" disabled={isIdentity(p)} onclick={() => app.resetRefine(stem)} title="Back to the stem as separated: no stereo selection, flat EQ">Reset</button>
    <button class="btn ghost small icon-only close" aria-label="Close the {label} Refine panel" title="Close (Esc)" onclick={() => app.closeRefine()}>✕</button>
  </div>

  <div class="cols">
    <div class="col">
      <div class="line">
        <label class="check" title="A part of the stereo field that does the opposite of the {label} switch: with {label} on you hear it without that part, with {label} off you hear only that part">
          <input
            type="checkbox"
            checked={pan.enabled}
            aria-label="{label} stereo exception"
            onchange={(e) => update((q) => (q.pan.enabled = e.currentTarget.checked))}
          />
          Stereo exception
        </label>
        <span class="except" id="{id}-except">Except:</span>
        <div class="seg" role="group" aria-labelledby="{id}-except" aria-label="{label}: except the left, center or right of the stereo field">
          {#each [["left", "Left"], ["center", "Center"], ["right", "Right"]] as [k, t] (k)}
            <button aria-pressed={quick === k} aria-label="Except the {t.toLowerCase()} third of the stereo field" onclick={() => setQuick(k as PanQuick)}>{t}</button>
          {/each}
        </div>
      </div>
      <div class="field-wrap" bind:clientWidth={fieldW}>
        <canvas
          bind:this={fieldCanvas}
          class="field"
          style:height="{FIELD_H}px"
          aria-hidden="true"
          onpointerdown={fieldDown}
          onpointermove={fieldMove}
          onpointerup={fieldUp}
          onpointercancel={fieldUp}
        ></canvas>
        {#if !haveField}
          <div class="hint">Play to see where {label.toLowerCase()} sits, left to right. Drag to place the exception.</div>
        {/if}
      </div>
      {#if pan.enabled}
        <p class="heard" aria-live="polite">
          {#if sw}{label} is on: you hear it without its {side} part.{:else}{label} is off: you hear only its {side} part.{/if}
          <span class="faint">Flip the switch to swap.</span>
        </p>
      {/if}
      <div class="line sliders">
        <label for="{id}-center">Position</label>
        <input
          id="{id}-center"
          type="range"
          min="-1"
          max="1"
          step="0.01"
          value={pan.center}
          aria-valuetext={panText(pan.center)}
          oninput={(e) => update((q) => ((q.pan.enabled = true), (q.pan.center = Number(e.currentTarget.value))))}
        />
        <span class="mono val">{panText(pan.center)}</span>
        <label for="{id}-width">Width</label>
        <input
          id="{id}-width"
          type="range"
          min="0.05"
          max="2"
          step="0.01"
          value={pan.width}
          aria-valuetext="{Math.round(pan.width * 50)}% of the field"
          oninput={(e) => update((q) => ((q.pan.enabled = true), (q.pan.width = Number(e.currentTarget.value))))}
        />
        <span class="mono val">{Math.round(pan.width * 50)}%</span>
      </div>
    </div>

    <div class="col eq-col" class:silent>
      {#if silent}
        <p class="silent-note">Turn {label} on, or except part of it, to hear the EQ.</p>
      {/if}
      <div class="eq-body">
        <div class="eq-wrap" bind:clientWidth={eqW}>
          <canvas
            bind:this={eqCanvas}
            class="eq"
            style:height="{EQ_H}px"
            aria-hidden="true"
            onpointerdown={eqDown}
            onpointermove={eqMove}
            onpointerup={eqUp}
            onpointercancel={eqUp}
          ></canvas>
        </div>
        <div class="tabs" role="tablist" aria-label="{label} EQ sections" tabindex="-1" onkeydown={tabKeys}>
          {#each TABS as t, i (t.key)}
            <button
              id="{id}-tab-{i}"
              role="tab"
              aria-selected={tab === t.key}
              aria-controls="{id}-tabpanel"
              tabindex={tab === t.key ? 0 : -1}
              title="{t.title}: {tabOn(t.key) ? 'on' : 'off'}"
              aria-label="{t.title}, {tabOn(t.key) ? 'on' : 'off'}"
              onclick={() => (tab = t.key)}
            >
              <span class="glyph" class:on={tabOn(t.key)} aria-hidden="true">{t.glyph}</span>{t.short}
            </button>
          {/each}
        </div>
        <div class="tabpanel" id="{id}-tabpanel" role="tabpanel" aria-labelledby="{id}-tab-{tabIdx}">
          <div class="tp-head">
            {#if tab === "low" || tab === "high"}
              {@const which = tab}
              {@const e = which === "low" ? eq.low_cut : eq.high_cut}
              {@const title = TABS[tabIdx].title}
              <label class="check"><input type="checkbox" checked={e.enabled} onchange={(ev) => setEnd(which, (x) => (x.enabled = ev.currentTarget.checked))} aria-label="{label} EQ {title.toLowerCase()} on" />{title}</label>
              <div class="seg tiny" role="radiogroup" aria-label="{label} {title.toLowerCase()}: cut or shelf">
                <button role="radio" aria-checked={e.mode === "cut"} title={which === "low" ? "Low cut: removes everything below (24 dB/oct)" : "High cut: removes everything above (24 dB/oct)"} onclick={() => endMode(which, "cut")}>Cut</button>
                <button role="radio" aria-checked={e.mode === "shelf"} title={which === "low" ? "Low shelf: turns everything below up or down" : "High shelf: turns everything above up or down"} onclick={() => endMode(which, "shelf")}>Shelf</button>
              </div>
            {:else}
              {@const i = tab}
              {@const b = eq.bands[i]}
              <label class="check"><input type="checkbox" checked={b.enabled} onchange={(ev) => setBand(i, (x) => (x.enabled = ev.currentTarget.checked))} aria-label="{label} EQ band {i + 1} on" />Band {i + 1}</label>
              <span class="faint small">Q: 0.7 broad … 4 narrow</span>
            {/if}
          </div>
          {#each rows as r (r.name)}
            <div class="ctl">
              <label for="{id}-n-{r.name}">{r.name}</label>
              <input
                class="sl"
                type="range"
                min="0"
                max={POS}
                step="1"
                value={toPos(r.sp, r.v)}
                aria-label={r.aria}
                aria-valuetext={r.text}
                oninput={(e) => r.set(fromPos(r.sp, Number(e.currentTarget.value)))}
                onkeydown={(e) => stepKeys(e, r.sp, r.v, r.set)}
              />
              <input
                id="{id}-n-{r.name}"
                class="n"
                type="number"
                step={r.step}
                value={r.field}
                aria-label="{r.aria}{r.unit ? `, ${r.unit}` : ''}"
                onchange={(e) => commit(r, e.currentTarget)}
                onkeydown={(e) => stepKeys(e, r.sp, r.v, r.set)}
              />
              <span class="unit">{r.unit}</span>
            </div>
          {/each}
        </div>
      </div>
    </div>
  </div>
</section>

<style>
  .panel {
    margin: -2px 0 2px 3px;
    padding: 8px 10px 10px;
    border-left: 3px solid var(--stem-color);
    border-radius: 0 0 var(--radius-sm) var(--radius-sm);
    background: var(--surface-2);
    box-shadow: inset 0 1px 0 var(--border);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .top {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  h3 {
    margin: 0 auto 0 0;
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted);
  }
  .who {
    color: var(--fg);
    text-transform: none;
    letter-spacing: 0;
    font-size: 13px;
  }
  .preset {
    height: 24px;
    max-width: 260px;
    font: inherit;
    font-size: 12px;
    color: inherit;
    padding: 0 6px;
    background: var(--surface);
    border: 1px solid var(--control-border);
    border-radius: var(--radius-sm);
  }
  option {
    background: var(--surface);
    color: var(--fg);
  }
  .close {
    font-size: 12px;
  }
  .cols {
    display: grid;
    grid-template-columns: minmax(260px, 1fr) minmax(340px, 1.25fr);
    gap: 14px;
  }
  .col {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  .line {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    font-size: 12px;
  }
  .check {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 12px;
    font-weight: 600;
    color: var(--muted);
    white-space: nowrap;
  }
  .check input {
    margin: 0;
    width: 14px;
    height: 14px;
    accent-color: var(--accent);
  }
  .seg {
    display: inline-flex;
    border: 1px solid var(--control-border);
    border-radius: 5px;
    overflow: hidden;
  }
  .seg button {
    height: 22px;
    padding: 0 8px;
    border: 0;
    background: var(--surface);
    font-size: 12px;
  }
  .seg button + button {
    border-left: 1px solid var(--control-border);
  }
  .seg button:hover:not([aria-checked="true"]):not([aria-pressed="true"]) {
    background: var(--surface-3);
  }
  .seg button[aria-checked="true"],
  .seg button[aria-pressed="true"] {
    background: var(--accent);
    color: var(--accent-fg);
  }
  .seg button:focus-visible {
    outline-offset: -2px;
  }
  .seg.tiny button {
    height: 20px;
    padding: 0 5px;
    font-size: 11px;
  }
  .field-wrap,
  .eq-wrap {
    position: relative;
    width: 100%;
  }
  canvas {
    display: block;
    width: 100%;
    border-radius: 4px;
    background: var(--surface);
    border: 1px solid var(--border);
    touch-action: none;
    cursor: crosshair;
  }
  .hint {
    position: absolute;
    left: 10px;
    right: 10px;
    top: 50%;
    transform: translateY(-50%);
    text-align: center;
    font-size: 11.5px;
    color: var(--faint);
    pointer-events: none;
  }
  .sliders label {
    color: var(--muted);
  }
  .sliders input[type="range"] {
    flex: 1 1 70px;
    min-width: 60px;
    accent-color: var(--stem-color);
  }
  .val {
    min-width: 46px;
    font-size: 11.5px;
    color: var(--muted);
  }
  .except {
    font-weight: 600;
    color: var(--muted);
  }
  .heard {
    margin: 0;
    font-size: 11.5px;
    color: var(--muted);
  }
  .faint {
    color: var(--faint);
  }
  .small {
    font-size: 11px;
  }
  .eq-body {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .eq-col.silent .eq-body {
    opacity: 0.45;
  }
  .silent-note {
    margin: 0;
    font-size: 11.5px;
    color: var(--muted);
  }
  .tabs {
    display: grid;
    grid-template-columns: repeat(5, minmax(0, 1fr));
    border: 1px solid var(--control-border);
    border-radius: 5px;
    overflow: hidden;
  }
  .tabs button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 4px;
    min-width: 0;
    height: 24px;
    padding: 0 4px;
    border: 0;
    background: var(--surface);
    font-size: 11.5px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tabs button + button {
    border-left: 1px solid var(--control-border);
  }
  .tabs button:hover:not([aria-selected="true"]) {
    background: var(--surface-3);
  }
  .tabs button[aria-selected="true"] {
    background: var(--surface-2);
    box-shadow: inset 0 -2px 0 var(--stem-color);
    font-weight: 600;
  }
  .tabs button:focus-visible {
    outline-offset: -2px;
  }
  /* Like the curve's nodes: filled when on, hollow when off. */
  .glyph {
    flex: none;
    display: inline-grid;
    place-items: center;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    border: 1.5px solid var(--muted);
    font-size: 8px;
    font-weight: 700;
    color: var(--muted);
  }
  .glyph.on {
    background: var(--stem-color);
    border-color: var(--fg);
    color: var(--bg);
  }
  .tabpanel {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 6px 8px 8px;
    border: 1px solid var(--border);
    border-top: 0;
    border-radius: 0 0 var(--radius-sm) var(--radius-sm);
    margin-top: -6px;
  }
  .tp-head {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 22px;
  }
  .ctl {
    display: grid;
    grid-template-columns: 64px minmax(60px, 1fr) 72px 28px;
    align-items: center;
    gap: 8px;
    font-size: 12px;
  }
  .ctl label {
    color: var(--muted);
  }
  .sl {
    width: 100%;
    min-width: 0;
    accent-color: var(--stem-color);
  }
  .unit {
    font-size: 11.5px;
    color: var(--muted);
  }
  .n {
    width: 100%;
    height: 22px;
    font: inherit;
    font-family: var(--mono);
    font-size: 11.5px;
    color: inherit;
    padding: 0 4px;
    background: var(--surface);
    border: 1px solid var(--control-border);
    border-radius: 4px;
  }
</style>
