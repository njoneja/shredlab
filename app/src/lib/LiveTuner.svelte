<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Navneet Joneja -->

<script lang="ts">
  // The Tuning panel's Live tuner tab: listens to one input channel while it
  // is shown (mounting starts the input, unmounting stops it) and shows the
  // note, the cents gauge and which strings of My tuning are in tune.
  import { onDestroy, onMount, untrack } from "svelte";
  import { app } from "./store.svelte";
  import type { TunerReading } from "./types";
  import {
    IN_TUNE_CENTS,
    NO_INPUT_DB,
    NO_INPUT_MS,
    formatCents,
    formatHz,
    holdStep,
    noInputHint,
    strings,
    tuneAdvice,
    tunerTarget,
    type TuneHold,
    type TunerTarget,
  } from "./tuning";

  /** The last note stays lit this long after the sound stops (then dims). */
  const LIVE_MS = 600;
  /** Needle smoothing per reading (about 30 a second). */
  const SMOOTH = 0.35;
  /** What to check when the input stays silent, for this platform. */
  const NO_INPUT_HINT = noInputHint(navigator.userAgent);

  const my = $derived(app.myTuning);
  const strs = $derived(strings(my));
  const status = $derived(app.tunerStatus);
  const input = $derived(app.tunerInput);
  const channels = $derived(input?.channels ?? 1);
  const selectedDevice = $derived(input?.device_id ?? app.appSettings?.tuner_device_id ?? "");

  let shown = $state<TunerTarget | null>(null);
  let needle = $state(0);
  let live = $state(false);
  let level = $state(-120);
  let noInput = $state(false);
  let tuned = $state<number[]>([]);
  let hold: TuneHold | null = null;
  let lastHzAt = 0;
  let silentSince: number | null = null;

  const advice = $derived(shown ? tuneAdvice(needle) : null);
  const meter = $derived(Math.max(0, Math.min(1, (level + 60) / 60)));
  const pos = (c: number) => 50 + Math.max(-50, Math.min(50, c));

  // A new tuning (or chromatic) starts the strip over.
  $effect(() => {
    void strs;
    void app.tunerChromatic;
    untrack(() => {
      tuned = [];
      hold = null;
      shown = null;
    });
  });

  $effect(() => {
    const r = app.tunerReading;
    untrack(() => onReading(r));
  });

  function onReading(r: TunerReading | null) {
    const now = performance.now();
    if (!r) {
      // (Re)started or stopped: nothing heard yet.
      shown = null;
      hold = null;
      level = -120;
      silentSince = null;
      noInput = false;
      live = false;
      return;
    }
    level = r.level_db;
    if (r.level_db <= NO_INPUT_DB) {
      silentSince ??= now;
      noInput = now - silentSince >= NO_INPUT_MS;
    } else {
      silentSince = null;
      noInput = false;
    }
    const t = r.hz != null ? tunerTarget(r.hz, my, app.tunerChromatic) : null;
    if (t) {
      const same = live && shown && shown.midi === t.midi && shown.string === t.string;
      needle = same ? needle + SMOOTH * (t.cents - needle) : t.cents;
      shown = t;
      live = true;
      lastHzAt = now;
      const step = holdStep(hold, t.string, t.cents, now);
      hold = step.hold;
      if (step.settled) {
        const n = step.settled.string;
        if (step.settled.inTune && !tuned.includes(n)) tuned = [...tuned, n];
        else if (!step.settled.inTune && tuned.includes(n)) tuned = tuned.filter((x) => x !== n);
      }
    } else if (now - lastHzAt > LIVE_MS) {
      live = false;
      hold = null;
    }
  }

  onMount(() => {
    app.startTuner();
  });
  onDestroy(() => {
    app.stopTuner();
  });
</script>

<div class="live">
  <div class="input">
    <select
      class="field grow"
      aria-label="Input device"
      value={selectedDevice}
      disabled={!app.inputDevices.length}
      onchange={(e) => app.startTuner(e.currentTarget.value, 0)}
    >
      {#if !app.inputDevices.length}<option value="">No input device</option>{/if}
      {#each app.inputDevices as d (d.id)}<option value={d.id}>{d.name}</option>{/each}
    </select>
    {#if channels > 1 && input}
      <select class="field" aria-label="Input channel" value={input.channel} onchange={(e) => app.startTuner(input.device_id, Number(e.currentTarget.value))}>
        {#each Array.from({ length: channels }, (_, i) => i) as i (i)}<option value={i}>Input {i + 1}</option>{/each}
      </select>
    {/if}
    <div class="meter" role="meter" aria-label="Input level" aria-valuemin={-60} aria-valuemax={0} aria-valuenow={Math.round(Math.max(-60, level))} title="{Math.round(level)} dBFS">
      <div class="meter-fill" style:width="{meter * 100}%"></div>
    </div>
  </div>
  {#if app.tunerMissing && input}
    <p class="muted missing">{app.tunerMissing} isn't connected; using {input.device_name}.</p>
  {/if}

  <div class="readout" aria-live="off">
    {#if status === "paused"}
      <p class="msg">Paused while the song plays.</p>
      <button class="btn" onclick={() => app.startTuner()}>Listen again</button>
    {:else if status === "error"}
      <p class="msg">{app.tunerError}</p>
      <button class="btn" onclick={() => app.startTuner()}>Try again</button>
    {:else if status === "starting" || status === "off"}
      <p class="muted">Opening the input…</p>
    {:else}
      <div class="note" class:dim={!live}>
        {#if shown}
          <span class="name">{shown.name}</span><span class="mono muted oct">{shown.octave}</span>
        {:else}
          <span class="name faint">–</span>
        {/if}
      </div>
      <p class="mono muted freq">
        {#if shown && live && app.tunerReading?.hz}
          {app.tunerReading.hz.toFixed(2)} Hz · target {shown.hz.toFixed(2)} Hz{shown.string != null ? ` (string ${shown.string})` : ""}
        {:else if shown}
          target {shown.hz.toFixed(2)} Hz{shown.string != null ? ` (string ${shown.string})` : ""}
        {:else}
          Play a string
        {/if}
      </p>
      <div class="gauge" class:dim={!live} aria-hidden="true">
        <div class="track"></div>
        <div class="zone" style:left="{pos(-IN_TUNE_CENTS)}%" style:width="{2 * IN_TUNE_CENTS}%"></div>
        {#each [-50, -25, 0, 25, 50] as c (c)}<div class="tick" class:major={c % 50 === 0} style:left="{pos(c)}%"></div>{/each}
        {#if shown}<div class="needle" class:ok={advice?.state === "in-tune"} style:left="{pos(needle)}%"></div>{/if}
        <span class="mono lab" style:left="0">−50</span>
        <span class="mono lab mid" style:left="50%">0</span>
        <span class="mono lab end" style:right="0">+50</span>
      </div>
      <div class="advice" class:dim={!live} aria-live="polite" aria-atomic="true">
        {#if shown && advice}
          <span class="mono cents" class:ok={advice.state === "in-tune"} class:warn={advice.state !== "in-tune"}>{formatCents(needle)}</span>
          <span class="text">{advice.text}</span>
          <span class="sr">{advice.full}</span>
        {/if}
      </div>
      {#if noInput}
        <p class="msg small">No input. {NO_INPUT_HINT}</p>
      {/if}
    {/if}
  </div>

  {#if !app.tunerChromatic}
    <div class="strip" role="list" aria-label="Strings of my tuning, lowest first">
      {#each strs as s (s.number)}
        {@const current = live && shown?.string === s.number}
        {@const ok = current && advice?.state === "in-tune"}
        {@const done = tuned.includes(s.number)}
        <div class="sc" class:current class:ok role="listitem" aria-label="String {s.number}, {s.label}: {current ? advice?.full : done ? 'in tune' : 'not yet'}">
          <span class="sn">{s.name}</span>
          {#if current}
            <span class="mono st">{formatCents(needle)}</span>
          {:else if done}
            <span class="st good">✓ in tune</span>
          {:else}
            <span class="st faint">not yet</span>
          {/if}
        </div>
      {/each}
    </div>
  {/if}

  <div class="foot">
    <label class="chk"><input type="checkbox" checked={app.tunerChromatic} onchange={(e) => app.setTunerChromatic(e.currentTarget.checked)} /> Chromatic (ignore my tuning)</label>
    <span class="muted">{app.tunerChromatic ? `Notes at A = ${formatHz(my.refHz)} Hz. ` : ""}The song is paused while the tuner listens.</span>
  </div>
</div>

<style>
  .live {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .input {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .field {
    height: 28px;
    min-width: 0;
    border: 1px solid var(--control-border);
    border-radius: var(--radius-sm);
    background: var(--surface);
    color: var(--fg);
    padding: 0 8px;
    font: inherit;
    font-size: 13px;
  }
  .grow {
    flex: 1;
  }
  .meter {
    flex: none;
    width: 110px;
    height: 8px;
    border-radius: 4px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .meter-fill {
    height: 100%;
    background: var(--muted);
    transition: width 60ms linear;
  }
  p {
    margin: 0;
  }
  .missing,
  .foot {
    font-size: 12.5px;
  }
  .readout {
    min-height: 236px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg);
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    padding: 16px;
    text-align: center;
  }
  .msg {
    font-size: 14px;
    max-width: 420px;
  }
  .msg.small {
    font-size: 12.5px;
    color: var(--warn);
  }
  .note {
    display: flex;
    align-items: baseline;
    gap: 6px;
  }
  .note .name {
    font-size: 64px;
    font-weight: 600;
    line-height: 1;
  }
  .oct {
    font-size: 18px;
  }
  .faint {
    color: var(--faint);
  }
  .dim {
    opacity: 0.45;
  }
  .freq {
    font-size: 12.5px;
  }
  .gauge {
    position: relative;
    width: 100%;
    max-width: 420px;
    height: 46px;
    margin-top: 6px;
  }
  .track {
    position: absolute;
    left: 0;
    right: 0;
    top: 20px;
    height: 2px;
    background: var(--control-border);
  }
  .zone {
    position: absolute;
    top: 12px;
    height: 18px;
    border-radius: 3px;
    border: 1px solid var(--ok);
    background: color-mix(in srgb, var(--ok) 18%, transparent);
    box-sizing: border-box;
  }
  .tick {
    position: absolute;
    top: 16px;
    width: 1px;
    height: 10px;
    background: var(--faint);
  }
  .tick.major {
    top: 14px;
    height: 14px;
  }
  .needle {
    position: absolute;
    top: 2px;
    width: 3px;
    height: 36px;
    margin-left: -1.5px;
    border-radius: 2px;
    background: var(--warn);
    transition: left 40ms linear;
  }
  .needle.ok {
    background: var(--ok);
  }
  @media (prefers-reduced-motion: reduce) {
    .needle,
    .meter-fill {
      transition: none;
    }
  }
  .lab {
    position: absolute;
    top: 32px;
    font-size: 11px;
    color: var(--faint);
  }
  .lab.mid {
    transform: translateX(-50%);
  }
  .advice {
    display: flex;
    align-items: baseline;
    gap: 10px;
    min-height: 30px;
    margin-top: 6px;
  }
  .cents {
    font-size: 22px;
  }
  .warn {
    color: var(--warn);
  }
  .ok {
    color: var(--ok);
  }
  .text {
    font-size: 15px;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .strip {
    display: flex;
    gap: 6px;
  }
  .sc {
    flex: 1;
    min-width: 0;
    height: 52px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface);
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 1px;
    overflow: hidden;
  }
  .sc.current {
    border-color: var(--warn);
    background: color-mix(in srgb, var(--warn) 10%, transparent);
    color: var(--warn);
  }
  .sc.current.ok {
    border-color: var(--ok);
    background: color-mix(in srgb, var(--ok) 10%, transparent);
    color: var(--ok);
  }
  .sn {
    font-weight: 600;
  }
  .st {
    font-size: 11px;
    white-space: nowrap;
  }
  .good {
    color: var(--ok);
  }
  .foot {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px 16px;
  }
  .chk {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .chk input {
    accent-color: var(--accent);
    margin: 0;
  }
</style>
