<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Navneet Joneja -->

<!--
  Chords panel: a few bars of the song with the chord of each stretch, under
  the waveform. Fits the loop when one is on, otherwise follows the playhead.
  Names come from lib/chords.ts via the store (shape in the confirmed song
  tuning, and what you hear when that differs). The tuning line at the end of
  the header row is always there, on one line, so the strip never moves.
-->
<script lang="ts">
  import { app, CHORD_ZOOMS } from "./store.svelte";
  import { barLength, chordIndexAt, isDark, rootFill, rootHighlight, shortNames, type ChordView, type PassingView } from "./chords";
  import { barAt } from "./meter";

  let canvas: HTMLCanvasElement;
  let wrap: HTMLDivElement;
  let width = $state(800);
  // The frame is 150 px when there is room and shrinks to 78 px in a small
  // window: the lane keeps its height and the waveform strip gives way.
  let height = $state(150);
  const RULER = 18;
  const LANE_H = 52;
  const laneTop = $derived(height - 6 - LANE_H);

  type Hit = { v: ChordView; x0: number; x1: number };
  let hits: Hit[] = [];
  /** Passing-chord tags as drawn, for hover and click. */
  let tags: { p: PassingView; x0: number; x1: number; y0: number; y1: number }[] = [];
  let tip = $state<{ x: number; y: number; title: string; sub: string } | null>(null);

  const win = $derived(app.chordWindow);
  const line = $derived(app.chordTuningLine);
  const loopOn = $derived(app.loop.enabled && app.loop.a_sec != null && app.loop.b_sec != null);
  const barsShown = $derived(
    win.fitting ? Math.max(1, Math.round((win.end - win.start) / barLength(app.downbeats))) : app.chordsBars,
  );

  function countBars(a: number, b: number): number {
    return app.downbeats.filter((d) => d >= a && d < b).length;
  }

  function cssVar(name: string) {
    return getComputedStyle(wrap).getPropertyValue(name).trim() || "#888";
  }

  $effect(() => {
    const ro = new ResizeObserver(([e]) => {
      width = Math.max(100, e.contentRect.width);
      height = Math.max(LANE_H + RULER + 10, Math.floor(e.contentRect.height));
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
      app.chordViews,
      app.detailPeaks ?? app.peaks,
      app.grid?.beats ?? [],
      app.downbeats,
      app.loop.a_sec,
      app.loop.b_sec,
      app.loop.enabled,
      width,
      height,
      app.passingViews,
      app.chords?.segments.map((s) => s.root ?? null) ?? [],
      app.chordFrame.main,
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
    views: ChordView[],
    peaks: number[] | null,
    beats: number[],
    downbeats: number[],
    a: number | null,
    b: number | null,
    loopEnabled: boolean,
    w: number,
    h: number,
    passing: PassingView[],
    roots: (number | null)[],
    shift: number,
  ) {
    if (!canvas) return;
    const LANE_TOP = h - 6 - LANE_H;
    const WAVE = Math.max(0, LANE_TOP - 4 - RULER);
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

    // Loop
    if (a != null && b != null) {
      ctx.fillStyle = loopEnabled ? cssVar("--loop-soft") : cssVar("--loop-idle");
      ctx.fillRect(X(a), 0, X(b) - X(a), h);
    }

    // Beats and bars. Beat lines only while they are at least 6 px apart.
    const bars = Math.max(1, countBars(t0, t1));
    const showBeats = beats.length > 1 && (w / span) * (span / Math.max(1, beats.filter((x) => x >= t0 && x < t1).length)) >= 6;
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
          ctx.fillStyle = cssVar("--muted");
          ctx.fillText(String(n), x + 4, RULER / 2);
        }
      }
    }

    // Waveform (peaks over the whole song, sampled for the window)
    const mid = RULER + WAVE / 2;
    const amp = WAVE / 2 - 2;
    const dur = app.song?.duration_sec ?? 1;
    if (peaks && peaks.length && WAVE >= 12) {
      const n = peaks.length;
      ctx.globalAlpha = 0.75;
      for (let x = 0; x < w; x += 2) {
        const ta = t0 + (x / w) * span;
        const tb = t0 + ((x + 2) / w) * span;
        const i0 = Math.max(0, Math.floor((ta / dur) * n));
        const i1 = Math.min(n, Math.max(i0 + 1, Math.ceil((tb / dur) * n)));
        let m = 0;
        for (let i = i0; i < i1; i++) m = Math.max(m, peaks[i]);
        const hh = Math.max(1, m * amp);
        ctx.fillStyle = ta < pos ? cssVar("--wave-played") : cssVar("--wave");
        ctx.fillRect(x, mid - hh, 1.25, hh * 2);
      }
      ctx.globalAlpha = 1;
    }

    // Chords
    hits = [];
    const cur = chordIndexAt(views, pos);
    const accent = cssVar("--accent");
    const fg = cssVar("--fg");
    const muted = cssVar("--muted");
    const dark = isDark(cssVar("--bg"));
    let i = Math.max(0, chordIndexAt(views, t0));
    for (; i < views.length && views[i].start < t1; i++) {
      const v = views[i];
      const x0 = X(v.start) + 1;
      const x1 = X(v.end) - 1;
      const bw = x1 - x0;
      if (bw <= 0) continue;
      hits.push({ v, x0, x1 });
      if (!v.main) {
        if (bw > 40) {
          ctx.fillStyle = cssVar("--faint");
          ctx.font = `12px ${font}`;
          ctx.fillText("N.C.", Math.max(x0, 0) + 8, LANE_TOP + LANE_H / 2);
        }
        continue;
      }
      const isCur = i === cur;
      roundRect(ctx, x0, LANE_TOP, bw, LANE_H, 4);
      // Same root color as the waveform's lane; the current chord is a stronger shade of it.
      const hi = isCur ? rootHighlight(roots[i] ?? null, shift, dark) : null;
      ctx.fillStyle = hi ? hi.fill : rootFill(roots[i] ?? null, shift, dark, true);
      ctx.fill();
      ctx.strokeStyle = hi ? hi.edge : cssVar("--control-border");
      ctx.lineWidth = 1;
      ctx.stroke();
      if (v.push && bw > 6) {
        // Notch: this chord starts off the beat.
        ctx.fillStyle = cssVar("--warn");
        ctx.beginPath();
        ctx.moveTo(x0, LANE_TOP);
        ctx.lineTo(x0 + 8, LANE_TOP);
        ctx.lineTo(x0, LANE_TOP + 8);
        ctx.closePath();
        ctx.fill();
      }
      // Labels: two lines, else the main name, else shorter forms. Kept in view when the block starts off screen.
      const lx = Math.max(x0, 0) + 8;
      const room = x1 - lx - 6;
      ctx.font = `650 15px ${font}`;
      const wm = ctx.measureText(v.main).width;
      ctx.font = `12.5px ${font}`;
      const ws = v.second ? ctx.measureText(v.second).width : 0;
      const col = fg;
      if (v.second && room >= Math.max(wm, ws)) {
        ctx.font = `650 15px ${font}`;
        ctx.fillStyle = col;
        ctx.fillText(v.main, lx, LANE_TOP + LANE_H / 2 - 8);
        ctx.font = `12.5px ${font}`;
        ctx.fillStyle = muted;
        ctx.fillText(v.second, lx, LANE_TOP + LANE_H / 2 + 10);
      } else if (room >= wm) {
        ctx.font = `650 15px ${font}`;
        ctx.fillStyle = col;
        ctx.fillText(v.main, lx, LANE_TOP + LANE_H / 2);
      } else {
        ctx.font = `650 13px ${font}`;
        const s = shortNames(v.main).find((n) => bw - 6 >= ctx.measureText(n).width);
        if (s) {
          ctx.fillStyle = col;
          ctx.fillText(s, Math.max(x0, 0) + 3, LANE_TOP + LANE_H / 2);
        }
      }
    }

    // Beat lines over the lane, so a push visibly starts before its beat.
    for (const t of beats) {
      if (t < t0 || t > t1) continue;
      const isBar = dbSet.has(t);
      if (!isBar && !showBeats) continue;
      ctx.fillStyle = isBar ? cssVar("--grid-line") : cssVar("--grid-line-faint");
      ctx.fillRect(Math.round(X(t)), isBar ? LANE_TOP : LANE_TOP + LANE_H - 8, 1, isBar ? LANE_H : 8);
    }

    // Passing chords: small tags over their stretch, just above the lane
    // (inside its bottom edge when the panel is too short for that).
    tags = [];
    const tagH = 15;
    const tagY = WAVE >= tagH + 2 ? LANE_TOP - tagH - 2 : LANE_TOP + LANE_H - tagH - 2;
    ctx.font = `600 11px ${font}`;
    for (const p of passing) {
      if (p.end < t0 || p.start > t1) continue;
      const label = p.second ? `${p.main} ${p.second}` : p.main;
      const tw = ctx.measureText(label).width + 10;
      const xa = X(p.start);
      const xb = X(p.end);
      const x0 = Math.max(xa, (xa + xb) / 2 - tw / 2);
      const x1 = Math.max(xb, x0 + tw);
      roundRect(ctx, x0, tagY, x1 - x0, tagH, 3);
      ctx.fillStyle = cssVar("--surface-3");
      ctx.fill();
      // Passing chords: dashed outline. Figures (A to A6): solid.
      if (!p.figure) ctx.setLineDash([2, 2]);
      ctx.strokeStyle = muted;
      ctx.stroke();
      ctx.setLineDash([]);
      ctx.fillStyle = pos >= p.start && pos < p.end ? accent : fg;
      ctx.fillText(p.main, x0 + 5, tagY + tagH / 2 + 0.5);
      if (p.second) {
        const mw = ctx.measureText(p.main + " ").width;
        ctx.fillStyle = muted;
        ctx.fillText(p.second, x0 + 5 + mw, tagY + tagH / 2 + 0.5);
      }
      tags.push({ p, x0, x1, y0: tagY, y1: tagY + tagH });
    }

    // Playhead
    if (pos >= t0 && pos <= t1) {
      ctx.fillStyle = cssVar("--playhead");
      ctx.fillRect(Math.round(X(pos)) - 1, 0, 2, h);
    }
  }

  function hitAt(e: PointerEvent | MouseEvent): Hit | null {
    const r = canvas.getBoundingClientRect();
    const x = e.clientX - r.left;
    const y = e.clientY - r.top;
    if (y < laneTop || y > laneTop + LANE_H) return null;
    return hits.find((h) => x >= h.x0 && x <= h.x1) ?? null;
  }

  /** "Bar 12, beat 3" or "Bar 12, pushed: an eighth before beat 3". */
  function where(t: number, push: boolean): string {
    const beats = app.grid?.beats ?? [];
    const bar = barAt(app.downbeats, t + 1e-3);
    if (!beats.length || bar < 1) return "";
    let i = beats.findIndex((b) => b > t + 1e-3);
    if (i < 0) i = beats.length;
    const next = beats[i];
    const prev = beats[i - 1];
    if (push && next != null) {
      const frac = prev != null ? (next - t) / Math.max(1e-3, next - prev) : 0.5;
      const early = frac > 0.375 ? "an eighth" : "a sixteenth";
      const nb = barAt(app.downbeats, next + 1e-3);
      const beatNo = beats.filter((b) => b >= app.downbeats[nb - 1] && b <= next + 1e-3).length;
      return `Bar ${nb}, pushed: starts ${early} before beat ${beatNo}`;
    }
    const beatNo = beats.filter((b) => b >= app.downbeats[bar - 1] && b <= t + 1e-3).length;
    return `Bar ${bar}, beat ${Math.max(1, beatNo)}`;
  }

  function tagAt(e: PointerEvent | MouseEvent) {
    const r = canvas.getBoundingClientRect();
    const x = e.clientX - r.left;
    const y = e.clientY - r.top;
    return tags.find((t) => x >= t.x0 && x <= t.x1 && y >= t.y0 && y <= t.y1) ?? null;
  }

  function onMove(e: PointerEvent) {
    const t = tagAt(e);
    if (t) {
      const r = wrap.getBoundingClientRect();
      tip = {
        x: Math.min(e.clientX - r.left + 12, r.width - 260),
        y: e.clientY - r.top - 54,
        title: `${t.p.main}${t.p.second ? ` · sounds ${t.p.second}` : ""} (${t.p.figure ? "figure" : "passing"})`,
        sub: `${where(t.p.start, false)} · ${Math.round((t.p.end - t.p.start) * 100) / 100} s, ${
          t.p.figure ? "the chord moves to this and back" : "inside the chord around it"
        }`,
      };
      return;
    }
    const h = hitAt(e);
    if (!h || !h.v.main) {
      tip = null;
      return;
    }
    const r = wrap.getBoundingClientRect();
    const beats = (h.v.end - h.v.start) / (60 / Math.max(1, app.grid?.bpm ?? 120));
    tip = {
      x: Math.min(e.clientX - r.left + 12, r.width - 260),
      y: e.clientY - r.top - 54,
      title: h.v.main + (h.v.second ? ` · sounds ${h.v.second}` : ""),
      sub: [where(h.v.start, h.v.push), `${Math.round(beats * 2) / 2} beats`].filter(Boolean).join(" · "),
    };
  }

  function onClick(e: MouseEvent) {
    const t = tagAt(e);
    if (t) {
      app.seek(Math.max(0, t.p.start));
      return;
    }
    const h = hitAt(e);
    if (h) {
      app.seek(Math.max(0, h.v.start));
      return;
    }
    const r = canvas.getBoundingClientRect();
    app.seek(win.start + ((e.clientX - r.left) / r.width) * (win.end - win.start));
  }

  function onWheel(e: WheelEvent) {
    if (!e.ctrlKey) return;
    e.preventDefault();
    app.zoomChords(e.deltaY < 0 ? 1 : -1);
  }
</script>

<section class="chords" aria-label="Chords">
  <div class="phead">
    <span class="label" title="Detected from the guitar, bass, keys and other stems. A best guess: trust your ears."
      >CHORDS <span class="guess">· best guess</span></span
    >
    <div class="seg" role="group" aria-label="Chords view">
      <button
        aria-pressed={win.fitting}
        disabled={!loopOn}
        title={loopOn ? "Show the loop" : "Set a loop to fit it"}
        onclick={() => (app.chordsFit = true)}>Fit loop</button
      >
      <button aria-pressed={!win.fitting} onclick={() => (app.chordsFit = false)}>Follow playhead</button>
    </div>
    <div class="seg" role="group" aria-label="Chord names">
      <button aria-pressed={!app.chordsDetailed} title="Major, minor, power, 7th and sus chords" onclick={() => app.setChordsDetailed(false)}>Simple</button>
      <button
        aria-pressed={app.chordsDetailed}
        title="Also 6, add9, 9 and 7sus4 chords, and short passing chords"
        onclick={() => app.setChordsDetailed(true)}>Detailed</button
      >
    </div>
    <div class="zoom">
      <button
        class="btn small icon-only"
        title="Show more bars (Ctrl+wheel)"
        aria-label="Show more bars"
        disabled={!win.fitting && app.chordsBars === CHORD_ZOOMS[CHORD_ZOOMS.length - 1]}
        onclick={() => app.zoomChords(-1)}>−</button
      >
      <span class="mono zl">{barsShown} {barsShown === 1 ? "bar" : "bars"}</span>
      <button
        class="btn small icon-only"
        title="Show fewer bars (Ctrl+wheel)"
        aria-label="Show fewer bars"
        disabled={!win.fitting && app.chordsBars === CHORD_ZOOMS[0]}
        onclick={() => app.zoomChords(1)}>+</button
      >
    </div>
    <div class="tline" class:warn={line.warn}>
      {#if line.legend}
        <span class="legend" title="Shape you fret, then what you hear"><b>{line.legend.main}</b> <span class="muted">{line.legend.heard}</span></span>
      {/if}
      <span class="ttext" title={line.text}>{line.text}</span>
      <span class="tbtns">
        {#if line.useGuess}
          {@const g = line.useGuess}
          <button class="btn small" onclick={() => app.useGuessShapes(g.semitones)}>Use {g.label} shapes</button>
        {/if}
        {#if line.fixes}
          <button class="btn small" disabled={app.matchNow == null} title={app.matchNow == null ? "Needs a measured tuning" : ""} onclick={() => app.shiftToMyTuning()}
            >Shift song to my tuning</button
          >
          <button class="btn small" onclick={() => app.setTunedToHeard()}>{line.fixes.tunedLabel}</button>
        {/if}
      </span>
    </div>
  </div>
  <div class="frame" bind:this={wrap}>
    <canvas
      bind:this={canvas}
      style:width="{width}px"
      style:height="{height}px"
      onpointermove={onMove}
      onpointerleave={() => (tip = null)}
      onclick={onClick}
      onwheel={onWheel}
    ></canvas>
    {#if !app.chords}
      <div class="state muted">
        {#if app.chordsPending}Detecting chords…{:else}Chords appear once the song is separated.{/if}
      </div>
    {/if}
    {#if tip}
      <div class="tip" style:left="{tip.x}px" style:top="{tip.y}px">
        <b>{tip.title}</b>
        {#if tip.sub}<div class="muted">{tip.sub}</div>{/if}
      </div>
    {/if}
  </div>
</section>

<style>
  .chords {
    display: flex;
    flex-direction: column;
    gap: 6px;
    flex: 0 1 auto;
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
  /* The tuning line: always present, one line, fixed place. Long text
     truncates (full text in the tooltip); the buttons stay at the right. */
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
  .tbtns {
    display: inline-flex;
    gap: 6px;
    flex-shrink: 0;
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
    flex: 0 1 150px;
    min-height: 78px;
  }
  canvas {
    display: block;
    cursor: pointer;
  }
  .state {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 34px;
    text-align: center;
    pointer-events: none;
    font-size: 12.5px;
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
