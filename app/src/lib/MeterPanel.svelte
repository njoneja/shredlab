<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Navneet Joneja -->

<script lang="ts">
  import Icon from "./Icon.svelte";
  import { app } from "./store.svelte";
  import { barAt, formatTime } from "./meter";
  import type { Subdivision } from "./types";

  let { onclose }: { onclose: () => void } = $props();

  let panel: HTMLDivElement;
  const currentBar = $derived(Math.max(1, barAt(app.downbeats, app.position)));
  // Initialised from the playhead once, when the panel opens.
  let fromBar = $state(Math.max(1, barAt(app.downbeats, app.position)));
  let bpb = $state(app.beatsPerBar);

  const SUBDIVS: { v: Subdivision; label: string }[] = [
    { v: 1, label: "None" },
    { v: 2, label: "2" },
    { v: 3, label: "3" },
  ];

  function describe(i: number) {
    const o = app.overrides[i];
    const t = app.grid?.beats[o.beat_index] ?? 0;
    if (o.kind === "beat_one") return `Beat 1 at ${formatTime(t, true)}`;
    return `${o.beats_per_bar} beats per bar from ${formatTime(t, true)}`;
  }

  function openClickHelp() {
    onclose();
    app.openHelp("click");
  }

  function onWindowDown(e: PointerEvent) {
    const target = e.target as Node;
    if (panel && !panel.contains(target) && !(target as Element).closest?.(".meter-anchor")) onclose();
  }
</script>

<svelte:window onpointerdown={onWindowDown} onkeydown={(e) => e.key === "Escape" && onclose()} />

<div class="panel" bind:this={panel} role="dialog" aria-label="Click and beat grid">
  <div class="head">
    <strong>Click</strong>
    <span class="muted">
      {app.grid?.bpm ? `${Math.round(app.grid.bpm)} BPM · ` : ""}{app.beatsPerBar} beats per bar · bar {currentBar}
    </span>
    <button class="btn ghost small icon-only" onclick={onclose} aria-label="Close"><Icon name="close" size={14} /></button>
  </div>

  <div class="sect row">
    <span>Click subdivision</span>
    <div class="seg" role="radiogroup" aria-label="Click subdivision">
      {#each SUBDIVS as s (s.v)}
        <button role="radio" aria-checked={app.click.subdivision === s.v} onclick={() => app.setSubdivision(s.v)}>{s.label}</button>
      {/each}
    </div>
  </div>

  <label class="check" title="With the A–B loop and count-in on, the loop stops at B and counts in again before A">
    <input type="checkbox" checked={app.click.count_in_each_repeat} onchange={() => app.toggleCountInEachRepeat()} />
    Count in before each loop repeat
  </label>

  <div class="sub">Beat grid</div>

  <div class="sect">
    <button class="btn" onclick={() => app.beatOneHere()} title="Make the beat nearest the playhead the first beat of a bar">
      This is beat 1
    </button>
    <span class="muted small">Moves the downbeat to the beat at the playhead ({formatTime(app.position, true)}).</span>
  </div>

  <div class="sect row">
    <label>
      <input class="num" type="number" min="1" max="16" bind:value={bpb} />
      beats per bar from bar
    </label>
    <input class="num" type="number" min="1" max={app.downbeats.length || 1} bind:value={fromBar} />
    <button class="btn small" onclick={() => app.beatsPerBarFrom(fromBar, bpb)}>Apply</button>
  </div>

  {#if app.overrides.length}
    <div class="sect">
      <div class="row between">
        <span class="muted small">Overrides for this song</span>
        <button class="btn ghost small" onclick={() => app.clearOverrides()}>Reset all</button>
      </div>
      <ul>
        {#each app.overrides as _, i (i)}
          <li>
            <span>{describe(i)}</span>
            <button class="btn ghost small icon-only" onclick={() => app.removeOverride(i)} aria-label="Remove override"><Icon name="close" size={12} /></button>
          </li>
        {/each}
      </ul>
    </div>
  {/if}

  <button class="link small" onclick={openClickHelp} title="How to fix the click, and when beat detection gets it wrong">
    <Icon name="help" size={14} /> Click sounds off?
  </button>
</div>

<style>
  .panel {
    position: absolute;
    bottom: calc(100% + 8px);
    left: 0;
    z-index: 40;
    width: 360px;
    max-width: calc(100vw - 40px);
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    background: var(--surface-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .head .muted {
    font-size: 12px;
    flex: 1;
  }
  .sect {
    display: flex;
    flex-direction: column;
    gap: 6px;
    align-items: flex-start;
  }
  .row {
    flex-direction: row;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .sub {
    width: 100%;
    padding-top: 10px;
    border-top: 1px solid var(--border);
    color: var(--muted);
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  .between {
    justify-content: space-between;
    width: 100%;
  }
  .small {
    font-size: 12px;
  }
  .num {
    width: 52px;
    height: 26px;
    background: var(--surface-2);
    color: var(--fg);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0 6px;
    font: inherit;
  }
  .seg {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    overflow: hidden;
  }
  .seg button {
    border: 0;
    background: var(--surface-2);
    padding: 3px 12px;
    font-size: 12px;
  }
  .seg button + button {
    border-left: 1px solid var(--border);
  }
  .seg button[aria-checked="true"] {
    background: var(--accent);
    color: var(--accent-fg);
    font-weight: 600;
  }
  ul {
    margin: 0;
    padding: 0;
    list-style: none;
    width: 100%;
  }
  li {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 12.5px;
    padding: 2px 0;
  }
  .link {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    background: none;
    border: 0;
    padding: 0;
    color: var(--muted);
    align-self: flex-start;
  }
  .link:hover {
    color: var(--fg);
  }
</style>
