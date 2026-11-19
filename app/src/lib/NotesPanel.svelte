<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Navneet Joneja -->

<!--
  Notes panel: the single notes of one stem as a piano roll, under the
  chords panel and on its window (Fit loop / Follow playhead and the zoom
  are shared, so bar lines line up). Names come from lib/notes.ts via the
  store, in the same frame as the chord names. The other notes the model
  heard (not in the lead line) are drawn faintly behind the lead notes
  unless the header's "Also heard" box is cleared. The header's Fast runs
  (Fewer / Normal / More) and Quiet notes (Skip / Include) set how the
  song's notes are found, and Reset puts both back to Normal and Skip in
  one change (store.setNotesOptions). While a loop is on they apply to
  the loop only (the label next to them says which): the loop's notes are
  found for them and drawn inside the loop, the song's outside it, and
  Reset puts the loop back on the song's settings.
  While the chords panel is closed, the header carries Fit/Follow and the
  tuning line itself.
-->
<script lang="ts">
  import { app, CHORD_ZOOMS } from "./store.svelte";
  import { barLength, isDark, rootFill, rootHighlight, spellingFor, type ChordView } from "./chords";
  import { bendWords, defaultRange, firstNoteFrom, loopBetween, nameIn, NOTE_STEM_LABEL, noteIndexAt, octaveName, refinedSide, rollRange, type HeardView, type NoteView } from "./notes";
  import { barAt } from "./meter";
  import type { NotesFast, StemName } from "./types";

  let canvas: HTMLCanvasElement;
  let wrap: HTMLDivElement;
  let width = $state(800);
  let height = $state(200);
  const RULER = 18;
  /** Width of the keyboard strip at the left edge (drawn over the roll, so
   * the timeline stays aligned with the chords panel). */
  const KEYS = 34;
  const FASTS: [NotesFast, string][] = [
    ["fewer", "Fewer"],
    ["normal", "Normal"],
    ["more", "More"],
  ];
  const QUIETS: [boolean, string][] = [
    [false, "Skip"],
    [true, "Include"],
  ];

  type Hit = { v: NoteView; i: number; x0: number; x1: number; y0: number; y1: number };
  let hits: Hit[] = [];
  type HeardHit = { v: HeardView; x0: number; x1: number; y0: number; y1: number };
  let heardHits: HeardHit[] = [];
  let stretchHits: { x0: number; x1: number; y0: number; y1: number }[] = [];
  let tip = $state<{ x: number; y: number; title: string; lines: string[] } | null>(null);
  let menuOpen = $state(false);
  /** First note of a Shift-click loop (a lead note or another note heard), waiting for the second. */
  type Anchor = { start: number; end: number; midi: number };
  let anchor = $state<Anchor | null>(null);

  const win = $derived(app.chordWindow);
  const line = $derived(app.chordTuningLine);
  const loopOn = $derived(app.loop.enabled && app.loop.a_sec != null && app.loop.b_sec != null);
  const barsShown = $derived(
    win.fitting ? Math.max(1, Math.round((win.end - win.start) / barLength(app.downbeats))) : app.chordsBars,
  );
  const stem = $derived(app.notesStem);
  const data = $derived(app.shownNotes);
  const pending = $derived(app.notesStemPending);
  const opts = $derived(app.notesPanel);
  /** Where the loop's notes are being found, in % of the panel's width. */
  const loopBusy = $derived.by(() => {
    const l = app.notesLoopSpan;
    if (!app.notesLoopPending || !l || pending) return null;
    const span = Math.max(1e-6, win.end - win.start);
    const x0 = Math.max(0, (l.start - win.start) / span);
    const x1 = Math.min(1, (l.end - win.start) / span);
    return x1 > x0 ? { left: 100 * x0, width: 100 * (x1 - x0) } : null;
  });
  const legend = $derived.by(() => {
    const v = app.noteViews.find((n) => n.second);
    return v ? { main: v.main, heard: v.second! } : line.legend;
  });

  function label(s: StemName): string {
    return NOTE_STEM_LABEL[s];
  }
  function side(s: StemName): string | null {
    return refinedSide(app.refine[s]?.pan);
  }
  function status(s: StemName): string {
    if (app.notesRunning === s) return "finding notes";
    if (app.notesPending.includes(s)) return "waiting";
    const n = app.notes[s];
    return n && (n.notes.length || n.chordal?.length) ? "ready" : "no notes";
  }

  function cssVar(name: string) {
    return getComputedStyle(wrap).getPropertyValue(name).trim() || "#888";
  }

  $effect(() => {
    const ro = new ResizeObserver(([e]) => {
      width = Math.max(100, e.contentRect.width);
      height = Math.max(RULER + 40, Math.floor(e.contentRect.height));
    });
    ro.observe(wrap);
    return () => ro.disconnect();
  });

  $effect(() => {
    void app.theme;
    draw(
      win.start,
      win.end,
      app.position,
      pending ? [] : app.noteViews,
      pending ? [] : app.heardViews,
      pending ? [] : (data?.chordal ?? []),
      app.grid?.beats ?? [],
      app.downbeats,
      app.loop.a_sec,
      app.loop.b_sec,
      app.loop.enabled,
      width,
      height,
      app.chordViews,
      app.chordFrame.main,
      stem,
      anchor,
    );
  });

  function roundRect(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number) {
    r = Math.max(0, Math.min(r, w / 2, h / 2));
    ctx.beginPath();
    ctx.moveTo(x + r, y);
    ctx.arcTo(x + w, y, x + w, y + h, r);
    ctx.arcTo(x + w, y + h, x, y + h, r);
    ctx.arcTo(x, y + h, x, y, r);
    ctx.arcTo(x, y, x + w, y, r);
    ctx.closePath();
  }

  function draw(
    t0: number,
    t1: number,
    pos: number,
    views: NoteView[],
    heard: HeardView[],
    chordal: [number, number][],
    beats: number[],
    downbeats: number[],
    a: number | null,
    b: number | null,
    loopEnabled: boolean,
    w: number,
    h: number,
    chords: ChordView[],
    shift: number,
    shownStem: StemName | null,
    anchorNote: Anchor | null,
  ) {
    if (!canvas) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    const span = Math.max(1e-6, t1 - t0);
    const X = (t: number) => ((t - t0) / span) * w;
    const font = cssVar("--font");
    const mono = cssVar("--mono");
    const fg = cssVar("--fg");
    const muted = cssVar("--muted");
    const faint = cssVar("--faint");
    const dark = isDark(cssVar("--bg"));

    // Loop
    if (a != null && b != null) {
      ctx.fillStyle = loopEnabled ? cssVar("--loop-soft") : cssVar("--loop-idle");
      ctx.fillRect(X(a), 0, X(b) - X(a), h);
    }

    // Beats and bars, as in the chords panel.
    const bars = Math.max(1, downbeats.filter((d) => d >= t0 && d < t1).length);
    const showBeats = beats.length > 1 && w / Math.max(1, beats.filter((x) => x >= t0 && x < t1).length) >= 6;
    const dbSet = new Set(downbeats);
    const labelEvery = [1, 2, 4, 8, 16].find((s) => (w / bars) * s >= 28) ?? 32;
    ctx.font = `10.5px ${mono}`;
    ctx.textBaseline = "middle";
    for (const t of beats) {
      if (t < t0 || t > t1) continue;
      const isBar = dbSet.has(t);
      if (!isBar && !showBeats) continue;
      const x = Math.round(X(t)) + 0.5;
      ctx.strokeStyle = isBar ? cssVar("--grid-line") : cssVar("--grid-line-faint");
      ctx.beginPath();
      ctx.moveTo(x, isBar ? 0 : RULER - 5);
      ctx.lineTo(x, h);
      ctx.stroke();
      if (isBar) {
        const n = barAt(downbeats, t + 1e-6);
        if (n > 0 && (n - 1) % labelEvery === 0) {
          ctx.fillStyle = muted;
          ctx.fillText(String(n), x + 4, RULER / 2);
        }
      }
    }

    hits = [];
    heardHits = [];
    stretchHits = [];
    const first = firstNoteFrom(views, t0);
    const vis: NoteView[] = [];
    for (let i = first; i < views.length && views[i].start < t1; i++) vis.push(views[i]);
    // Other notes heard overlap, so they are searched in full (a few thousand at most).
    const heardVis = heard.filter((v) => v.end > t0 && v.start < t1);
    const { lo, hi } = rollRange(
      vis.map((v) => ({ midi: v.midi + shift, bend: v.bend })),
      shownStem ? defaultRange(shownStem) : { lo: 55, hi: 79 },
      8,
      heardVis.map((v) => ({ midi: v.midi + shift })),
    );
    const top = RULER + 8;
    const bottom = h - 10;
    const rows = hi - lo + 1;
    const rh = (bottom - top) / rows;
    // Pitch in the frame of the names (so the C lines are the named Cs).
    const Y = (m: number) => bottom - (m + shift - lo + 0.5) * rh;
    const th = Math.max(10, Math.min(18, rh * 1.4));
    // Names shrink with the bars in a short panel.
    const nameSize = th >= 13 ? 11 : 10;

    // C lines with octave labels.
    ctx.font = `9.5px ${mono}`;
    for (let m = Math.ceil(lo / 12) * 12; m <= hi; m += 12) {
      const y = Math.round(bottom - (m - lo + 0.5) * rh) + 0.5;
      ctx.fillStyle = cssVar("--grid-line");
      ctx.fillRect(0, y, w, 1);
    }

    // Chordal stretches: hatched, labeled, with the chord lane's names.
    for (const [s, e] of chordal) {
      if (e < t0 || s > t1) continue;
      const x0 = Math.max(-4, X(s) + 1);
      const x1 = Math.min(w + 4, X(e) - 1);
      const y0 = RULER + 4;
      const y1 = h - 4;
      if (x1 - x0 < 2) continue;
      ctx.save();
      roundRect(ctx, x0, y0, x1 - x0, y1 - y0, 4);
      ctx.fillStyle = cssVar("--surface-2");
      ctx.fill();
      ctx.clip();
      ctx.strokeStyle = cssVar("--grid-line");
      ctx.lineWidth = 1;
      for (let x = x0 - (y1 - y0); x < x1; x += 9) {
        ctx.beginPath();
        ctx.moveTo(x, y1);
        ctx.lineTo(x + (y1 - y0), y0);
        ctx.stroke();
      }
      ctx.restore();
      roundRect(ctx, x0, y0, x1 - x0, y1 - y0, 4);
      ctx.setLineDash([4, 3]);
      ctx.strokeStyle = cssVar("--control-border");
      ctx.stroke();
      ctx.setLineDash([]);
      const lx = Math.max(x0, 0) + 8;
      ctx.font = `700 11px ${font}`;
      ctx.fillStyle = muted;
      ctx.fillText("CHORDS", lx, y0 + 12);
      const lw = ctx.measureText("CHORDS").width;
      ctx.font = `11px ${font}`;
      const hint = "several notes at once, see the chord lane";
      if (lx + lw + 10 + ctx.measureText(hint).width < x1 - 6) {
        ctx.fillStyle = faint;
        ctx.fillText(hint, lx + lw + 10, y0 + 12);
      }
      ctx.font = `650 14px ${font}`;
      let lastX = -1e9;
      for (const c of chords) {
        if (!c.main || c.end <= Math.max(t0, s) || c.start >= Math.min(t1, e)) continue;
        const cx = Math.max(X(c.start), lx) + 6;
        const tw = ctx.measureText(c.main).width;
        if (cx < lastX + 8 || cx + tw > x1 - 6) continue;
        ctx.globalAlpha = 0.7;
        ctx.fillStyle = fg;
        ctx.fillText(c.main, cx, (y0 + y1) / 2 + 8);
        ctx.globalAlpha = 1;
        lastX = cx + tw;
      }
      stretchHits.push({ x0, x1, y0, y1 });
    }

    // Other notes heard: faint, thinner bars in the note's color, behind the
    // lead notes (and over the chordal hatching, where they show the chord
    // tones). No names; the tooltip has them.
    const hh = Math.max(4, th * 0.6);
    for (const v of heardVis) {
      const y = Y(v.midi);
      if (y < RULER || y > h) continue;
      const x0 = X(v.start) + 1;
      const x1 = Math.max(x0 + 2, X(v.end) - 1);
      roundRect(ctx, x0, y - hh / 2, x1 - x0, hh, 2);
      ctx.globalAlpha = dark ? 0.35 : 0.4;
      ctx.fillStyle = rootFill(v.midi, shift, dark, true);
      ctx.fill();
      ctx.globalAlpha = 0.25;
      ctx.strokeStyle = muted;
      ctx.lineWidth = 1;
      ctx.stroke();
      ctx.globalAlpha = 1;
      if (anchorNote && anchorNote.start === v.start && anchorNote.midi === v.midi) {
        ctx.strokeStyle = cssVar("--loop");
        ctx.lineWidth = 2;
        roundRect(ctx, x0 - 2, y - hh / 2 - 2, x1 - x0 + 4, hh + 4, 3);
        ctx.stroke();
      }
      heardHits.push({ v, x0, x1, y0: y - hh / 2, y1: y + hh / 2 });
    }

    // Notes
    const cur = noteIndexAt(views, pos);
    const two = vis.some((v) => v.second);
    const border = cssVar("--control-border");
    // Per row, where the last name drawn above a narrow note ends.
    const above = new Map<number, number>();
    for (let k = 0; k < vis.length; k++) {
      const v = vis[k];
      const i = first + k;
      const x0 = X(v.start) + 1;
      const x1 = Math.max(x0 + 2, X(v.end) - 1);
      const bw = x1 - x0;
      const y = Y(v.midi);
      const isCur = i === cur;
      const hl = isCur ? rootHighlight(v.midi, shift, dark) : null;
      const fill = hl ? hl.fill : rootFill(v.midi, shift, dark, true);
      const edge = hl ? hl.edge : border;
      ctx.lineWidth = isCur ? 1.5 : 1;
      let room = bw - 6;
      let yTop = y - th / 2;
      if (v.bend) {
        const bd = v.bend;
        const yt = Y(v.midi + bd.semitones);
        const clampX = (t: number) => Math.min(x1, Math.max(x0, X(t)));
        const xs = clampX(bd.start);
        const xp = Math.max(xs + 2, clampX(bd.peak));
        const rise = xp - xs;
        const xr = bd.release != null ? clampX(bd.release) : null;
        const xd = xr != null ? Math.max(xp, xr - rise) : x1;
        const curve = (xa: number, ya: number, xb: number, yb: number) => {
          ctx.beginPath();
          ctx.moveTo(xa - 2, ya);
          ctx.bezierCurveTo(xa + (xb - xa) * 0.6, ya, xa + (xb - xa) * 0.4, yb, xb + 2, yb);
        };
        // Ribbon from the note up to the target (and back down on a release).
        for (const [xa, ya, xb, yb] of xr != null ? [[xs, y, xp, yt], [xd, yt, xr, y]] : [[xs, y, xp, yt]]) {
          ctx.strokeStyle = fill;
          ctx.lineWidth = th * 0.55;
          curve(xa, ya, xb, yb);
          ctx.stroke();
          ctx.strokeStyle = edge;
          ctx.lineWidth = 1.2;
          curve(xa, ya, xb, yb);
          ctx.stroke();
        }
        ctx.lineWidth = isCur ? 1.5 : 1;
        roundRect(ctx, x0, y - th / 2, Math.max(2, xs - x0), th, 3);
        ctx.fillStyle = fill;
        ctx.fill();
        ctx.strokeStyle = edge;
        ctx.stroke();
        roundRect(ctx, xp, yt - th / 2, Math.max(2, xd - xp), th, 3);
        ctx.fill();
        ctx.setLineDash([3, 2]);
        ctx.stroke();
        ctx.setLineDash([]);
        if (xr != null && x1 - xr > 2) {
          roundRect(ctx, xr, y - th / 2, x1 - xr, th, 3);
          ctx.fill();
          ctx.stroke();
        }
        ctx.font = `600 10.5px ${font}`;
        ctx.fillStyle = cssVar("--warn");
        ctx.textBaseline = "alphabetic";
        ctx.fillText(bd.label, xp + 2, Math.max(RULER + 10, yt - th / 2 - 3));
        ctx.textBaseline = "middle";
        room = xs - x0 - 6;
        yTop = Math.min(yTop, yt - th / 2);
      } else {
        roundRect(ctx, x0, y - th / 2, bw, th, 3);
        ctx.fillStyle = fill;
        ctx.fill();
        ctx.strokeStyle = edge;
        ctx.stroke();
      }
      if (anchorNote && anchorNote.start === v.start && anchorNote.midi === v.midi) {
        ctx.strokeStyle = cssVar("--loop");
        ctx.lineWidth = 2;
        roundRect(ctx, x0 - 2, y - th / 2 - 2, bw + 4, th + 4, 4);
        ctx.stroke();
      }
      hits.push({ v, i, x0, x1, y0: yTop, y1: y + th / 2 });
      // Names: the shape on the bar, what you hear dimmer below it.
      ctx.font = `650 ${nameSize}px ${font}`;
      const named = ctx.measureText(v.main).width <= room;
      if (named) {
        ctx.fillStyle = fg;
        ctx.fillText(v.main, x0 + 4, y + 0.5);
      }
      if (!named && !v.bend) {
        // Too narrow for its name: put it above the bar if it doesn't run
        // into the previous one in this row.
        ctx.font = `600 ${nameSize - 1}px ${font}`;
        const tw = ctx.measureText(v.main).width;
        const lx = (x0 + x1) / 2 - tw / 2;
        const ly = y - th / 2 - 6;
        if (lx >= (above.get(v.midi) ?? -1e9) + 3 && lx >= KEYS + 2 && ly > RULER + 4) {
          ctx.fillStyle = muted;
          ctx.fillText(v.main, lx, ly);
          above.set(v.midi, lx + tw);
        }
      }
      if (named && v.second && two) {
        ctx.font = `${nameSize - 1}px ${font}`;
        if (ctx.measureText(v.second).width <= room + 4) {
          ctx.fillStyle = muted;
          ctx.fillText(v.second, x0 + 4, y + th / 2 + 7);
        }
      }
    }

    // Slides (a slanted line from the note before) and legato (an arc with h or p).
    for (let k = 0; k < vis.length; k++) {
      const v = vis[k];
      const i = first + k;
      if (!v.joinsPrev || i === 0) continue;
      const p = views[i - 1];
      const pEnd = p.midi + (p.bend && p.bend.release == null ? p.bend.semitones : 0);
      const ya = Y(pEnd);
      const yb = Y(v.midi);
      if (v.attack === "slide") {
        ctx.strokeStyle = fg;
        ctx.globalAlpha = 0.8;
        ctx.lineWidth = 2.5;
        ctx.lineCap = "round";
        ctx.beginPath();
        ctx.moveTo(Math.max(X(p.start) + 4, X(p.end) - 10), ya + (yb > ya ? 2 : -2));
        ctx.lineTo(Math.min(X(v.end) - 4, X(v.start) + 10), yb + (yb > ya ? -2 : 2));
        ctx.stroke();
        ctx.globalAlpha = 1;
        ctx.lineCap = "butt";
      } else {
        const xa = (X(p.start) + X(p.end)) / 2;
        const xb = (X(v.start) + X(v.end)) / 2;
        const ea = ya - th / 2 - 3;
        const eb = yb - th / 2 - 3;
        const peak = Math.max(RULER + 4, Math.min(ea, eb) - 12);
        ctx.strokeStyle = muted;
        ctx.lineWidth = 1.2;
        ctx.beginPath();
        ctx.moveTo(xa, ea);
        ctx.quadraticCurveTo((xa + xb) / 2, peak, xb, eb);
        ctx.stroke();
        ctx.font = `italic 600 10.5px ${font}`;
        ctx.fillStyle = fg;
        ctx.textAlign = "center";
        ctx.textBaseline = "alphabetic";
        ctx.fillText(v.midi > p.midi ? "h" : "p", (xa + xb) / 2, Math.max(RULER + 10, (peak + Math.min(ea, eb)) / 2 - 1));
        ctx.textAlign = "left";
        ctx.textBaseline = "middle";
      }
    }

    // Playhead
    if (pos >= t0 && pos <= t1) {
      ctx.fillStyle = cssVar("--playhead");
      ctx.fillRect(Math.round(X(pos)) - 1, 0, 2, h);
    }

    // Keyboard: one key per row, names on the white keys when the rows are
    // tall enough (only the Cs otherwise), the current note's key lit.
    const spelling = spellingFor(app.chords?.key ?? null, shift);
    const curRow = cur >= 0 ? views[cur].midi + shift : null;
    ctx.fillStyle = cssVar("--surface-3");
    ctx.fillRect(0, RULER, KEYS, h - RULER);
    ctx.font = `9.5px ${mono}`;
    for (let m = lo; m <= hi; m++) {
      const yb = bottom - (m - lo) * rh;
      const black = [1, 3, 6, 8, 10].includes(((m % 12) + 12) % 12);
      if (m === curRow) {
        ctx.fillStyle = rootHighlight(m - shift, shift, dark).fill;
        ctx.fillRect(0, yb - rh, KEYS, rh);
      } else if (black) {
        ctx.fillStyle = dark ? "rgba(0,0,0,0.7)" : "rgba(0,0,0,0.45)";
        ctx.fillRect(0, yb - rh, KEYS * 0.45, rh);
      }
      ctx.fillStyle = cssVar("--grid-line-faint");
      ctx.fillRect(0, Math.round(yb) + 0.5, KEYS, 0.5);
      const pc = ((m % 12) + 12) % 12;
      const isC = pc === 0;
      // Which white keys are named: all when rows are tall, else C, E and G,
      // else C and G, else only C (labels need about 10 px between them).
      const named = rh >= 9 ? !black : rh >= 3.4 ? [0, 4, 7].includes(pc) : rh >= 1.5 ? [0, 7].includes(pc) : isC;
      if (named) {
        ctx.fillStyle = isC || m === curRow ? fg : muted;
        ctx.textAlign = "right";
        ctx.fillText(octaveName(m, spelling), KEYS - 3, yb - rh / 2 + 0.5);
        ctx.textAlign = "left";
      }
    }
    ctx.fillStyle = border;
    ctx.fillRect(KEYS, RULER, 1, h - RULER);
  }

  function at(e: MouseEvent) {
    const r = canvas.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  }

  function noteAt(e: MouseEvent): Hit | null {
    const { x, y } = at(e);
    return hits.find((h) => x >= h.x0 && x <= h.x1 && y >= h.y0 - 2 && y <= h.y1 + 2) ?? null;
  }

  /** The other note heard under the pointer (lead notes win), the shortest when several overlap. */
  function heardAt(e: MouseEvent): HeardHit | null {
    const { x, y } = at(e);
    let best: HeardHit | null = null;
    for (const h of heardHits) {
      if (x >= h.x0 && x <= h.x1 && y >= h.y0 - 2 && y <= h.y1 + 2 && (!best || h.x1 - h.x0 < best.x1 - best.x0)) best = h;
    }
    return best;
  }

  /** "Bar 12, beat 3". */
  function where(t: number): string {
    const beats = app.grid?.beats ?? [];
    const bar = barAt(app.downbeats, t + 1e-3);
    if (!beats.length || bar < 1) return "";
    const beatNo = beats.filter((b) => b >= app.downbeats[bar - 1] && b <= t + 1e-3).length;
    return `Bar ${bar}, beat ${Math.max(1, beatNo)}`;
  }

  function beatsLong(v: { start: number; end: number }): string {
    const n = (v.end - v.start) / (60 / Math.max(1, app.grid?.bpm ?? 120));
    const r = Math.round(n * 4) / 4;
    return r < 0.25 ? "under a sixteenth" : `${r} ${r === 1 ? "beat" : "beats"}`;
  }

  function onMove(e: PointerEvent) {
    const h = noteAt(e);
    const r = wrap.getBoundingClientRect();
    const place = (n: number) => ({ x: Math.min(e.clientX - r.left + 12, r.width - 280), y: Math.max(0, e.clientY - r.top - 18 - 16 * n) });
    if (h) {
      const v = h.v;
      const lines = [[v.octave, where(v.start), beatsLong(v)].filter(Boolean).join(" · ")];
      const prev = app.noteViews[h.i - 1];
      if (v.joinsPrev && prev) {
        lines.push(v.attack === "slide" ? `Slid into from ${prev.main}` : `${v.midi > prev.midi ? "Hammer-on" : "Pull-off"} from ${prev.main}`);
      }
      if (v.bend) {
        const target = nameIn(v.midi + Math.round(v.bend.semitones), app.chordFrame, app.chords?.key);
        lines.push(`Bent ${bendWords(v.bend.semitones)} to ${target}${v.bend.release != null ? ", and back" : ""}`);
      }
      tip = { ...place(lines.length), title: v.main + (v.second ? ` · sounds ${v.second}` : ""), lines };
      return;
    }
    const o = heardAt(e);
    if (o) {
      const v = o.v;
      const lines = [[v.octave, where(v.start), beatsLong(v)].filter(Boolean).join(" · "), "Also heard (not in the lead line)"];
      tip = { ...place(lines.length), title: v.main + (v.second ? ` · sounds ${v.second}` : ""), lines };
      return;
    }
    const { x, y } = at(e);
    if (stretchHits.some((s) => x >= s.x0 && x <= s.x1 && y >= s.y0 && y <= s.y1)) {
      tip = { ...place(1), title: "Chords", lines: ["Several notes at once: no single notes guessed here. The chord lane names them."] };
      return;
    }
    tip = null;
  }

  function onClick(e: MouseEvent) {
    const v: Anchor | null = noteAt(e)?.v ?? heardAt(e)?.v ?? null;
    if (v && e.shiftKey) {
      if (anchor && !(anchor.start === v.start && anchor.midi === v.midi)) {
        const { a, b } = loopBetween(anchor, v);
        anchor = null;
        app.setLoop(a, b, true);
      } else {
        anchor = { start: v.start, end: v.end, midi: v.midi };
      }
      return;
    }
    anchor = null;
    if (v) {
      app.seek(Math.max(0, v.start));
      return;
    }
    const { x } = at(e);
    app.seek(win.start + (x / width) * (win.end - win.start));
  }

  function onWheel(e: WheelEvent) {
    if (!e.ctrlKey) return;
    e.preventDefault();
    app.zoomChords(e.deltaY < 0 ? 1 : -1);
  }

  function pick(s: StemName) {
    menuOpen = false;
    anchor = null;
    app.setNotesStem(s);
  }
</script>

<svelte:window
  onclick={(e) => {
    if (menuOpen && !(e.target instanceof Element && e.target.closest(".drop"))) menuOpen = false;
  }}
  onkeydown={(e) => {
    if (e.key === "Escape") {
      menuOpen = false;
      anchor = null;
    }
  }}
/>

<section class="notes" aria-label="Notes">
  <div class="phead">
    <span class="label" title="Single notes from one stem, in time. A best guess: check by ear at slow speed."
      >NOTES <span class="guess">· best guess</span></span
    >
    <span
      class="draft"
      role="note"
      title="Automatic note detection is a rough draft. It misses notes (more in fast runs, chords and busy mixes), sometimes names the wrong note or octave, and can pick up another part (a rhythm guitar under a solo). Check by ear at slow speed. Refine's stereo mask can help when two parts share a stem."
      >Rough draft: expect missed and wrong notes</span
    >
    <div class="drop">
      <button
        class="btn small stem-btn"
        disabled={!app.notesStems.length}
        aria-haspopup="menu"
        aria-expanded={menuOpen}
        title={stem && side(stem)
          ? `Notes follow Refine's stereo mask (${side(stem)}). Changing it finds this stem's notes again.`
          : "Stem to show (remembered for this song)"}
        onclick={() => (menuOpen = !menuOpen)}
      >
        {#if stem}
          <span class="dot" style:background="var(--stem-{stem})"></span>
          <span>{label(stem)}{#if side(stem)}<span class="r">, refined: {side(stem)}</span>{/if}</span>
        {:else}
          <span>No stems</span>
        {/if}
        <span class="faint">▾</span>
      </button>
      {#if menuOpen}
        <div class="menu" role="menu">
          {#each app.notesStems as s (s)}
            <button role="menuitemradio" aria-checked={s === stem} onclick={() => pick(s)}>
              <span class="dot" style:background="var(--stem-{s})"></span>
              <span>{label(s)}{#if side(s)}<span class="r">, refined: {side(s)}</span>{/if}</span>
              <span class="st">{status(s)}</span>
            </button>
          {/each}
          <div class="more faint">Drums later.</div>
        </div>
      {/if}
    </div>
    <label
      class="heard"
      title="Also draw, faintly behind the lead line, the other notes the model heard: notes ringing on under the next one, chord tones and other parts in the stem"
    >
      <input type="checkbox" checked={app.notesHeard} onchange={(e) => app.setNotesHeard(e.currentTarget.checked)} />
      Also heard
    </label>
    <div class="opt" title="More splits quick runs into separate notes but can chop held notes">
      <span class="opt-label" id="notes-fast-label">Fast runs</span>
      <div class="seg detail" role="group" aria-labelledby="notes-fast-label">
        {#each FASTS as [f, name] (f)}
          <button
            aria-pressed={opts.fast === f}
            disabled={!app.notesStems.length}
            onclick={() => app.setNotesOptions(f, opts.quiet)}>{name}</button
          >
        {/each}
      </div>
    </div>
    <div class="opt" title="Include lets softer notes into the lead line">
      <span class="opt-label" id="notes-quiet-label">Quiet notes</span>
      <div class="seg detail" role="group" aria-labelledby="notes-quiet-label">
        {#each QUIETS as [q, name] (name)}
          <button
            aria-pressed={opts.quiet === q}
            disabled={!app.notesStems.length}
            onclick={() => app.setNotesOptions(opts.fast, q)}>{name}</button
          >
        {/each}
      </div>
    </div>
    <span
      class="scope"
      title={opts.scope === "loop"
        ? "Fast runs and Quiet notes apply to the loop only; the rest of the song keeps its own"
        : "Fast runs and Quiet notes apply to the whole song. Set a loop to change them for the loop only."}
      >{opts.scope === "loop" ? "This loop" : "Whole song"}</span
    >
    {#if opts.scope === "loop" ? opts.own : app.notesFast !== "normal" || app.notesQuiet}
      <button
        class="reset"
        title={opts.scope === "loop" ? "Back to the song's settings for this loop" : "Back to Normal and Skip"}
        disabled={!app.notesStems.length}
        onclick={() => app.resetNotesOptions()}>Reset</button
      >
    {/if}
    {#if !app.chordsOpen}
      <div class="seg" role="group" aria-label="Notes window">
        <button
          aria-pressed={win.fitting}
          disabled={!loopOn}
          title={loopOn ? "Show the loop" : "Set a loop to fit it"}
          onclick={() => (app.chordsFit = true)}>Fit loop</button
        >
        <button aria-pressed={!win.fitting} onclick={() => (app.chordsFit = false)}>Follow playhead</button>
      </div>
    {/if}
    <div class="zoom">
      <button
        class="btn small icon-only"
        title="Show more bars (Ctrl+wheel). Zooms the chords panel too."
        aria-label="Show more bars"
        disabled={!win.fitting && app.chordsBars === CHORD_ZOOMS[CHORD_ZOOMS.length - 1]}
        onclick={() => app.zoomChords(-1)}>−</button
      >
      <span class="mono zl">{barsShown} {barsShown === 1 ? "bar" : "bars"}</span>
      <button
        class="btn small icon-only"
        title="Show fewer bars (Ctrl+wheel). Zooms the chords panel too."
        aria-label="Show fewer bars"
        disabled={!win.fitting && app.chordsBars === CHORD_ZOOMS[0]}
        onclick={() => app.zoomChords(1)}>+</button
      >
    </div>
    {#if !app.chordsOpen}
      <div class="tline" class:warn={line.warn}>
        {#if legend}
          <span class="legend" title="Shape you fret, then what you hear"><b>{legend.main}</b> <span class="muted">{legend.heard}</span></span>
        {/if}
        <span class="ttext" title={line.text}>{line.text}</span>
      </div>
    {/if}
  </div>
  <div class="frame" bind:this={wrap}>
    <canvas
      bind:this={canvas}
      style:width="{width}px"
      style:height="{height}px"
      title="Click a note to play from it · Shift-click two notes to loop between them"
      onpointermove={onMove}
      onpointerleave={() => (tip = null)}
      onclick={onClick}
      onwheel={onWheel}
    ></canvas>
    {#if pending && stem}
      <div class="state busy" role="status">
        <span class="spin"></span>
        <span>Finding notes in the {label(stem).toLowerCase()} stem{app.notesRunning && app.notesRunning !== stem ? ` (after the ${label(app.notesRunning).toLowerCase()} stem)` : ""}…</span>
      </div>
    {:else if !stem}
      <div class="state muted">
        {#if app.stemsReady && !app.separating}No stem with notes in this stem set.{:else}Notes appear once the song is separated.{/if}
      </div>
    {:else if loopBusy}
      <div class="state busy loop-busy" role="status" style:left="{loopBusy.left}%" style:width="{loopBusy.width}%">
        <span class="spin"></span>
        <span>Finding notes in the loop…</span>
      </div>
    {:else if data && !data.notes.length && !data.chordal?.length}
      <div class="state muted">No single notes found in the {label(stem).toLowerCase()} stem.</div>
    {/if}
    {#if anchor}
      <div class="anchor-hint">Shift-click another note to loop between them</div>
    {/if}
    {#if tip}
      <div class="tip" style:left="{tip.x}px" style:top="{tip.y}px">
        <b>{tip.title}</b>
        {#each tip.lines as l}<div class="muted">{l}</div>{/each}
      </div>
    {/if}
  </div>
</section>

<style>
  .notes {
    display: flex;
    flex-direction: column;
    gap: 6px;
    /* Gives way more slowly than the chords panel and the mixer: a piano
       roll needs the height more than a chord lane. */
    flex: 0 0.5 auto;
    min-height: 112px;
  }
  .phead {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: 10px;
    height: 28px;
    min-width: 0;
  }
  .label {
    flex-shrink: 0;
    white-space: nowrap;
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.08em;
    color: var(--muted);
  }
  .guess {
    font-weight: 500;
    letter-spacing: 0;
    color: var(--faint);
    white-space: nowrap;
  }
  .draft {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 11px;
    color: var(--warn);
    border: 1px solid color-mix(in srgb, var(--warn) 45%, transparent);
    border-radius: 999px;
    padding: 1px 8px;
    cursor: help;
  }
  .drop {
    position: relative;
    flex-shrink: 1;
    min-width: 0;
  }
  .stem-btn {
    max-width: 100%;
    overflow: hidden;
    white-space: nowrap;
  }
  .stem-btn > span:not(.dot):not(.faint) {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .r {
    color: var(--muted);
    font-weight: 400;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    display: inline-block;
    flex-shrink: 0;
  }
  .menu {
    position: absolute;
    top: 28px;
    left: 0;
    z-index: 20;
    min-width: 230px;
    background: var(--surface-3);
    border: 1px solid var(--control-border);
    border-radius: 6px;
    padding: 4px;
    box-shadow: var(--shadow);
  }
  .menu button {
    display: flex;
    width: 100%;
    align-items: center;
    gap: 8px;
    border: 0;
    background: none;
    padding: 5px 8px;
    border-radius: 4px;
    font-size: 12.5px;
    text-align: left;
    white-space: nowrap;
  }
  .menu button:hover {
    background: var(--surface-2);
  }
  .menu button[aria-checked="true"] {
    color: var(--accent);
  }
  .menu .st {
    margin-left: auto;
    padding-left: 12px;
    color: var(--faint);
    font-size: 11.5px;
  }
  .menu .more {
    padding: 4px 8px;
    font-size: 11.5px;
  }
  .heard {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    flex-shrink: 0;
    font-size: 12px;
    color: var(--muted);
    white-space: nowrap;
    cursor: pointer;
  }
  .heard input {
    margin: 0;
  }
  .seg {
    display: inline-flex;
    border: 1px solid var(--control-border);
    border-radius: 5px;
    overflow: hidden;
    flex-shrink: 0;
  }
  .seg button {
    border: 0;
    background: var(--surface-2);
    height: 24px;
    padding: 0 10px;
    font-size: 12px;
    white-space: nowrap;
  }
  .seg button + button {
    border-left: 1px solid var(--control-border);
  }
  .seg button[aria-pressed="true"] {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .seg button:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .detail button {
    padding: 0 7px;
  }
  .opt {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    flex-shrink: 0;
  }
  .opt-label {
    font-size: 12px;
    color: var(--muted);
    white-space: nowrap;
  }
  .scope {
    flex-shrink: 0;
    font-size: 11.5px;
    color: var(--faint);
    white-space: nowrap;
    cursor: help;
  }
  .reset {
    flex-shrink: 0;
    border: 0;
    background: none;
    padding: 0 2px;
    height: 24px;
    font-size: 12px;
    color: var(--accent);
    cursor: pointer;
  }
  .reset:hover:not(:disabled) {
    text-decoration: underline;
  }
  .reset:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .zoom {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
  }
  .zl {
    min-width: 54px;
    text-align: center;
    font-size: 12px;
  }
  .tline {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 8px;
    height: 24px;
    padding-left: 10px;
    border-left: 1px solid var(--border);
    font-size: 12px;
    color: var(--muted);
  }
  .tline.warn {
    color: var(--warn);
  }
  .ttext {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .legend {
    display: inline-flex;
    gap: 5px;
    align-items: baseline;
    padding: 0 6px;
    border-radius: 4px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    color: var(--fg);
    flex-shrink: 0;
  }
  .legend b {
    font-weight: 650;
  }
  .frame {
    position: relative;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow: hidden;
    flex: 0 1 200px;
    min-height: 78px;
  }
  canvas {
    display: block;
    cursor: pointer;
  }
  .state {
    position: absolute;
    inset: 18px 0 0 0;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 10px;
    pointer-events: none;
    font-size: 12.5px;
    color: var(--muted);
  }
  .loop-busy {
    right: auto;
    overflow: hidden;
    white-space: nowrap;
  }
  .spin {
    width: 16px;
    height: 16px;
    border: 2px solid var(--control-border);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
    flex-shrink: 0;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .anchor-hint {
    position: absolute;
    right: 8px;
    top: 2px;
    font-size: 11.5px;
    color: var(--loop);
    pointer-events: none;
  }
  .tip {
    position: absolute;
    pointer-events: none;
    background: var(--surface-3);
    border: 1px solid var(--control-border);
    border-radius: 5px;
    padding: 6px 8px;
    font-size: 12px;
    box-shadow: var(--shadow);
    white-space: nowrap;
    z-index: 5;
  }
</style>
