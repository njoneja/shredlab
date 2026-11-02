<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Navneet Joneja -->

<script lang="ts">
  import { app } from "./store.svelte";
  import { barAt, barEndingAt, formatTime, nearestBeatIndex } from "./meter";

  let canvas: HTMLCanvasElement;
  let wrap: HTMLDivElement;
  let width = $state(800);
  let height = $state(150);

  const RULER = 18; // bar-number strip at the top
  const HANDLE_PX = 7;

  type Drag =
    | { kind: "pending"; x0: number; t0: number }
    | { kind: "range"; t0: number }
    | { kind: "marker"; which: "a" | "b" }
    | null;
  let drag: Drag = null;
  let hoverCursor = $state("pointer");

  const duration = $derived(app.song?.duration_sec ?? 1);

  function xToT(x: number) {
    return Math.max(0, Math.min(duration, (x / width) * duration));
  }
  function tToX(t: number) {
    return (t / duration) * width;
  }

  function cssVar(name: string) {
    return getComputedStyle(wrap).getPropertyValue(name).trim() || "#888";
  }

  $effect(() => {
    const ro = new ResizeObserver(([e]) => {
      width = Math.max(100, e.contentRect.width);
      height = Math.max(80, e.contentRect.height);
    });
    ro.observe(wrap);
    return () => ro.disconnect();
  });

  // Redraw whenever anything visible changes (the theme changes the colours).
  $effect(() => {
    void app.theme;
    draw(app.peaks, app.position, app.loop.a_sec, app.loop.b_sec, app.loop.enabled, app.downbeats, width, height);
  });

  function draw(
    peaks: number[] | null,
    pos: number,
    a: number | null,
    b: number | null,
    loopOn: boolean,
    downbeats: number[],
    w: number,
    h: number,
  ) {
    if (!canvas) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);

    const top = RULER;
    const mid = top + (h - top) / 2;
    const amp = (h - top) / 2 - 4;

    // Loop region
    if (a != null && b != null) {
      const xa = tToX(a);
      const xb = tToX(b);
      ctx.fillStyle = loopOn ? cssVar("--loop-soft") : cssVar("--loop-idle");
      ctx.fillRect(xa, 0, xb - xa, h);
    }

    // Bar lines + numbers
    if (downbeats.length) {
      const pxPerBar = w / Math.max(1, downbeats.length) ;
      const step = [1, 2, 4, 8, 16, 32].find((s) => pxPerBar * s >= 26) ?? 64;
      ctx.font = `10px ${cssVar("--mono")}`;
      ctx.textBaseline = "middle";
      for (let i = 0; i < downbeats.length; i++) {
        const bar = i + 1;
        const x = Math.round(tToX(downbeats[i])) + 0.5;
        const labelled = (bar - 1) % step === 0;
        ctx.strokeStyle = labelled ? cssVar("--grid-line") : cssVar("--grid-line-faint");
        ctx.beginPath();
        ctx.moveTo(x, labelled ? 4 : top);
        ctx.lineTo(x, h);
        ctx.stroke();
        if (labelled) {
          ctx.fillStyle = cssVar("--faint");
          ctx.fillText(String(bar), x + 3, RULER / 2);
        }
      }
    }

    // Waveform
    const played = cssVar("--wave-played");
    const unplayed = cssVar("--wave");
    const px = tToX(pos);
    if (peaks && peaks.length) {
      const n = peaks.length;
      const barW = 2;
      const gap = 1;
      for (let x = 0; x < w; x += barW + gap) {
        const i0 = Math.floor((x / w) * n);
        const i1 = Math.max(i0 + 1, Math.floor(((x + barW + gap) / w) * n));
        let m = 0;
        for (let i = i0; i < i1 && i < n; i++) m = Math.max(m, peaks[i]);
        const hh = Math.max(1, m * amp);
        ctx.fillStyle = x + barW <= px ? played : unplayed;
        ctx.fillRect(x, mid - hh, barW, hh * 2);
      }
    } else {
      ctx.fillStyle = unplayed;
      ctx.fillRect(0, mid, w, 1);
    }

    // Loop markers
    if (a != null && b != null) {
      const col = loopOn ? cssVar("--loop") : cssVar("--faint");
      for (const [t, label] of [
        [a, "A"],
        [b, "B"],
      ] as const) {
        const x = Math.round(tToX(t)) + 0.5;
        ctx.strokeStyle = col;
        ctx.lineWidth = 1.5;
        ctx.beginPath();
        ctx.moveTo(x, 0);
        ctx.lineTo(x, h);
        ctx.stroke();
        ctx.fillStyle = col;
        const fx = label === "A" ? x : x - 14;
        ctx.fillRect(fx, 0, 14, 14);
        ctx.fillStyle = cssVar("--bg");
        ctx.font = `bold 10px ${cssVar("--font")}`;
        ctx.textBaseline = "middle";
        ctx.fillText(label, fx + 4, 7.5);
      }
      ctx.lineWidth = 1;
    }

    // Playhead
    ctx.fillStyle = cssVar("--playhead");
    ctx.fillRect(Math.round(px) - 1, 0, 2, h);
  }

  function localX(e: PointerEvent) {
    const r = canvas.getBoundingClientRect();
    return e.clientX - r.left;
  }

  function snap(t: number, e: PointerEvent) {
    if (!e.shiftKey || !app.grid) return t;
    const i = nearestBeatIndex(app.grid.beats, t);
    return i >= 0 ? app.grid.beats[i] : t;
  }

  function markerAt(x: number): "a" | "b" | null {
    const { a_sec, b_sec } = app.loop;
    if (a_sec == null || b_sec == null) return null;
    const da = Math.abs(x - tToX(a_sec));
    const db = Math.abs(x - tToX(b_sec));
    if (Math.min(da, db) > HANDLE_PX) return null;
    return da <= db ? "a" : "b";
  }

  function onDown(e: PointerEvent) {
    if (!app.song || e.button !== 0) return;
    canvas.setPointerCapture(e.pointerId);
    const x = localX(e);
    const m = markerAt(x);
    drag = m ? { kind: "marker", which: m } : { kind: "pending", x0: x, t0: snap(xToT(x), e) };
  }

  function onMove(e: PointerEvent) {
    const x = localX(e);
    if (!drag) {
      hoverCursor = markerAt(x) ? "ew-resize" : "pointer";
      return;
    }
    const t = snap(xToT(x), e);
    if (drag.kind === "pending" && Math.abs(x - drag.x0) > 4) drag = { kind: "range", t0: drag.t0 };
    if (drag.kind === "range") {
      app.loop = { a_sec: Math.min(drag.t0, t), b_sec: Math.max(drag.t0, t), enabled: true };
    } else if (drag.kind === "marker") {
      const { a_sec, b_sec } = app.loop;
      if (drag.which === "a") {
        if (b_sec != null && t > b_sec) {
          app.loop = { a_sec: b_sec, b_sec: t, enabled: app.loop.enabled };
          drag = { kind: "marker", which: "b" };
        } else app.loop.a_sec = t;
      } else {
        if (a_sec != null && t < a_sec) {
          app.loop = { a_sec: t, b_sec: a_sec, enabled: app.loop.enabled };
          drag = { kind: "marker", which: "a" };
        } else app.loop.b_sec = t;
      }
    }
  }

  function onUp(e: PointerEvent) {
    if (!drag) return;
    const d = drag;
    drag = null;
    if (d.kind === "pending") {
      app.seek(snap(xToT(localX(e)), e));
    } else {
      app.setLoop(app.loop.a_sec, app.loop.b_sec, app.loop.enabled);
      if (d.kind === "range" && app.loop.a_sec != null) app.seek(app.loop.a_sec);
    }
  }

  const loopText = $derived.by(() => {
    const { a_sec, b_sec } = app.loop;
    if (a_sec == null || b_sec == null) return null;
    let s = `${formatTime(a_sec, true)} – ${formatTime(b_sec, true)}`;
    if (app.downbeats.length) {
      const ba = barAt(app.downbeats, a_sec);
      const bb = barEndingAt(app.downbeats, b_sec);
      s += ba === bb ? ` · bar ${ba}` : ` · bars ${ba}–${bb}`;
    }
    return s;
  });
</script>

<section class="wave">
  <div class="canvas-wrap" bind:this={wrap}>
    <canvas
      bind:this={canvas}
      style:width="{width}px"
      style:height="{height}px"
      style:cursor={hoverCursor}
      onpointerdown={onDown}
      onpointermove={onMove}
      onpointerup={onUp}
      onpointercancel={() => (drag = null)}
      title="Click to seek · drag to set the loop · drag A/B to adjust · hold Shift to snap to beats"
    ></canvas>
    {#if !app.peaks}
      <div class="loading muted">Reading waveform…</div>
    {/if}
  </div>
  <div class="readout">
    <span class="mono time">
      <strong>{formatTime(app.position, true)}</strong>
      <span class="muted"> / {formatTime(app.song?.duration_sec ?? 0)}</span>
      {#if app.downbeats.length}
        <span class="muted"> · {barAt(app.downbeats, app.position) ? `bar ${barAt(app.downbeats, app.position)}` : "pickup"}</span>
      {/if}
    </span>
    {#if loopText}
      <span class="mono loopinfo" class:off={!app.loop.enabled}>
        Loop {loopText}{app.loop.enabled ? "" : " (off)"}
      </span>
    {:else}
      <span class="muted hint">Drag on the waveform to set a loop</span>
    {/if}
  </div>
</section>

<style>
  .wave {
    flex: 1 1 140px;
    min-height: 0;
    max-height: 180px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .canvas-wrap {
    position: relative;
    flex: 1;
    min-height: 90px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow: hidden;
  }
  canvas {
    display: block;
    touch-action: none;
  }
  .loading {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    pointer-events: none;
  }
  .readout {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    font-size: 12.5px;
    flex-wrap: wrap;
  }
  .loopinfo {
    color: var(--loop);
  }
  .loopinfo.off {
    color: var(--muted);
  }
  .hint {
    font-size: 12px;
  }
</style>
