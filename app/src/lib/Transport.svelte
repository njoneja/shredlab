<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Navneet Joneja -->

<script lang="ts">
  import Icon from "./Icon.svelte";
  import MeterPanel from "./MeterPanel.svelte";
  import { app, CENTS_MAX, SEMI_MAX, SPEED_MAX, SPEED_MIN } from "./store.svelte";
  import { formatCountIn } from "./meter";

  let meterOpen = $state(false);
  let volOpen = $state(false);
  let centsOpen = $state(false);
  const noSong = $derived(!app.song);
  const noGrid = $derived(!app.grid);
  const volIcon = $derived(app.masterVolume === 0 ? "volume-mute" : app.masterVolume < 0.5 ? "volume-low" : "volume");

  // Media-player layout: play controls and volume centered, click/count-in on
  // the left, speed/pitch on the right. When the sides no longer fit beside the
  // center at equal width (so the center stays centered), they drop to a
  // second row: left aligned and right aligned, so nothing changes sides.
  let bar: HTMLDivElement;
  let left: HTMLDivElement;
  let center: HTMLDivElement;
  let right: HTMLDivElement;
  let layout = $state<"wide" | "narrow" | "stacked">("wide");

  $effect(() => {
    const GAP = 18;
    const measure = () => {
      const w = bar.clientWidth;
      const l = left.offsetWidth;
      const r = right.offsetWidth;
      const c = center.offsetWidth;
      layout = w >= 2 * Math.max(l, r) + c + 2 * GAP ? "wide" : w >= l + r + GAP ? "narrow" : "stacked";
    };
    const ro = new ResizeObserver(measure);
    for (const el of [bar, left, center, right]) ro.observe(el);
    return () => ro.disconnect();
  });

  function onWindowDown(e: PointerEvent) {
    const t = e.target as Element;
    if (volOpen && !t.closest?.(".vol-anchor")) volOpen = false;
    if (centsOpen && !t.closest?.(".cents-anchor")) centsOpen = false;
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") volOpen = centsOpen = false;
  }

  function onVolWheel(e: WheelEvent) {
    e.preventDefault();
    const step = e.deltaY < 0 ? 0.05 : -0.05;
    app.setMasterVolume(Math.min(1, Math.max(0, Math.round((app.masterVolume + step) * 20) / 20)), true);
  }
</script>

<svelte:window onpointerdown={onWindowDown} onkeydown={onKey} />

<section class="transport" aria-label="Transport">
  <div class="bar {layout}" bind:this={bar}>
    <div class="zone left" bind:this={left}>
      <div class="pill">
        <button class="btn toggle" aria-pressed={app.click.enabled} onclick={() => app.toggleClick()} disabled={noSong || noGrid} title={noGrid ? "Waiting for beat detection" : "Click track on/off (C)"}>
          <Icon name="metronome" /> Click
        </button>
        <label class="field" title="Click and count-in volume (only the click, not the music). Double-click to reset.">
          <input
            class="click-vol"
            type="range"
            min="0"
            max="100"
            step="5"
            value={Math.round(app.click.volume * 100)}
            aria-label="Click volume"
            oninput={(e) => app.setClickVolume(Number(e.currentTarget.value) / 100)}
            onchange={(e) => app.setClickVolume(Number(e.currentTarget.value) / 100, true)}
            ondblclick={() => app.setClickVolume(0.8, true)}
          />
          <span class="mono val">{Math.round(app.click.volume * 100)}%</span>
        </label>
        <div class="meter-anchor">
          <button
            class="btn small icon-only ghost"
            onclick={() => (meterOpen = !meterOpen)}
            disabled={noGrid}
            aria-expanded={meterOpen}
            aria-label="Click and beat grid settings"
            title={noGrid ? "Detecting beats…" : `Click and beat grid settings (${app.beatsPerBar} beats per bar)`}
          >
            <Icon name="settings" size={16} />
            {#if app.overrides.length}<span class="dot" title="Has beat grid fixes"></span>{/if}
          </button>
          {#if meterOpen}
            <MeterPanel onclose={() => (meterOpen = false)} />
          {/if}
        </div>
      </div>
      <div class="pill" class:inactive={!app.click.count_in_enabled}>
        <button class="btn toggle icon-only" aria-pressed={app.click.count_in_enabled} aria-label="Count-in" onclick={() => app.toggleCountIn()} disabled={noSong || noGrid} title="Count-in: clicks before playback starts">
          <Icon name="countin" />
        </button>
        <button class="btn small icon-only ghost" onclick={() => app.stepCountIn(-1)} disabled={noGrid || app.click.count_in_beats <= app.countInMin} aria-label="Shorter count-in">
          <Icon name="minus" size={14} />
        </button>
        <span class="mono countin" title={noGrid ? "" : `Count-in: ${formatCountIn(app.click.count_in_beats, app.beatsPerBar)}. Ends exactly at the playback or loop start.`}>
          {noGrid ? "—" : app.click.count_in_beats}
        </span>
        <button class="btn small icon-only ghost" onclick={() => app.stepCountIn(1)} disabled={noGrid || app.click.count_in_beats >= app.countInMax} aria-label="Longer count-in">
          <Icon name="plus" size={14} />
        </button>
      </div>
    </div>

    <div class="zone center" bind:this={center}>
      <button class="btn icon-only ghost" onclick={() => app.backToStart()} disabled={noSong} title="Back to loop start">
        <Icon name="back" />
      </button>
      <button class="play" onclick={() => app.togglePlay()} disabled={noSong} title="Play / pause (Space)" aria-label={app.playing ? "Pause" : "Play"}>
        {#if app.countInLeft > 0}
          <span class="countdown mono" title="Count-in">{app.countInLeft}</span>
        {:else}
          <Icon name={app.playing ? "pause" : "play"} size={22} />
        {/if}
      </button>
      <div class="vol-anchor">
        <button
          class="btn icon-only ghost"
          class:changed={app.masterVolume !== 1}
          onclick={() => (volOpen = !volOpen)}
          onwheel={onVolWheel}
          aria-expanded={volOpen}
          aria-label="Master volume"
          title="Master volume {Math.round(app.masterVolume * 100)}%: everything you hear, stems and click. Not applied to exports. Scroll to change."
        >
          <Icon name={volIcon} />
        </button>
        {#if volOpen}
          <div class="pop vol-pop" role="dialog" aria-label="Master volume">
            <label class="field" title="Master volume: everything you hear, stems and click. Not applied to exports. Double-click to reset.">
              <Icon name={volIcon} size={16} />
              <input
                type="range"
                min="0"
                max="100"
                step="1"
                value={Math.round(app.masterVolume * 100)}
                aria-label="Master volume"
                oninput={(e) => app.setMasterVolume(Number(e.currentTarget.value) / 100)}
                onchange={(e) => app.setMasterVolume(Number(e.currentTarget.value) / 100, true)}
                ondblclick={() => app.setMasterVolume(1, true)}
              />
              <span class="mono val" class:changed={app.masterVolume !== 1}>{Math.round(app.masterVolume * 100)}%</span>
            </label>
          </div>
        {/if}
      </div>
      <button class="btn toggle" aria-pressed={app.loop.enabled} onclick={() => app.toggleLoop()} disabled={noSong} title="A–B loop on/off (L). [ and ] set A/B at the playhead.">
        <Icon name="loop" /> A–B
      </button>
    </div>

    <div class="zone right" bind:this={right}>
      <label class="field" title="Speed (↑ ↓). Double-click to reset.">
        <span class="lbl">Speed</span>
        <input
          type="range"
          min={SPEED_MIN}
          max={SPEED_MAX}
          step="5"
          value={app.speed}
          disabled={noSong}
          oninput={(e) => app.setSpeed(Number(e.currentTarget.value))}
          ondblclick={() => app.setSpeed(100)}
        />
        <span class="mono val" class:changed={app.speed !== 100}>{app.speed}%</span>
      </label>
      <div class="pill" title="Pitch in semitones (Shift+↑ ↓)">
        <span class="lbl">Pitch</span>
        <button class="btn small icon-only ghost" onclick={() => app.setPitch(app.semitones - 1, app.cents)} disabled={noSong || app.semitones <= -SEMI_MAX} aria-label="Pitch down a semitone">
          <Icon name="minus" size={14} />
        </button>
        <span class="mono val semi" class:changed={app.semitones !== 0}>{app.semitones > 0 ? "+" : ""}{app.semitones} st</span>
        <button class="btn small icon-only ghost" onclick={() => app.setPitch(app.semitones + 1, app.cents)} disabled={noSong || app.semitones >= SEMI_MAX} aria-label="Pitch up a semitone">
          <Icon name="plus" size={14} />
        </button>
        <div class="cents-anchor">
          <button class="btn small ghost cents-btn" class:changed={app.cents !== 0} onclick={() => (centsOpen = !centsOpen)} disabled={noSong} aria-expanded={centsOpen} title="Fine pitch in cents">
            <span class="mono">{app.cents > 0 ? "+" : ""}{app.cents}¢</span>
          </button>
          {#if centsOpen}
            <div class="pop" role="dialog" aria-label="Fine pitch">
              <label class="field" title="Double-click to reset.">
                <span class="lbl">Fine</span>
                <input
                  type="range"
                  min={-CENTS_MAX}
                  max={CENTS_MAX}
                  step="1"
                  value={app.cents}
                  aria-label="Fine pitch in cents"
                  oninput={(e) => app.setPitch(app.semitones, Number(e.currentTarget.value))}
                  ondblclick={() => app.setPitch(app.semitones, 0)}
                />
                <span class="mono val" class:changed={app.cents !== 0}>{app.cents > 0 ? "+" : ""}{app.cents}¢</span>
              </label>
            </div>
          {/if}
        </div>
      </div>
    </div>
  </div>
</section>

<style>
  .transport {
    padding: 10px 12px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .bar {
    display: grid;
    align-items: center;
    gap: 10px 18px;
  }
  .bar.wide {
    grid-template-columns: 1fr auto 1fr;
    grid-template-areas: "left center right";
  }
  .bar.narrow {
    grid-template-columns: 1fr 1fr;
    grid-template-areas: "left right" "center center";
  }
  .bar.stacked {
    grid-template-columns: 1fr;
    grid-template-areas: "left" "right" "center";
  }
  .zone {
    display: flex;
    align-items: center;
    gap: 8px;
    white-space: nowrap;
  }
  .left {
    grid-area: left;
    justify-self: start;
  }
  .center {
    grid-area: center;
    justify-self: center;
  }
  .right {
    grid-area: right;
    justify-self: end;
    gap: 10px;
  }
  .stacked .right {
    justify-self: start;
  }
  .play {
    width: 44px;
    height: 44px;
    border-radius: 50%;
    border: 0;
    background: var(--accent);
    color: var(--accent-fg);
    display: grid;
    place-items: center;
  }
  .play:hover:not(:disabled) {
    filter: brightness(1.08);
  }
  .countdown {
    font-size: 18px;
    font-weight: 700;
  }
  /* A pill groups a toggle or label with its own settings. */
  .pill {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 2px 4px 2px 2px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface-2);
  }
  .pill > .btn.toggle {
    border-color: transparent;
  }
  .pill > .btn.toggle[aria-pressed="true"] {
    border-color: var(--accent);
  }
  .pill > .lbl {
    padding-left: 8px;
  }
  .pill.inactive .countin {
    color: var(--muted);
  }
  .countin {
    min-width: 22px;
    text-align: center;
    font-size: 12.5px;
  }
  .btn.icon-only.small {
    width: 26px;
  }
  .meter-anchor,
  .cents-anchor,
  .vol-anchor {
    position: relative;
  }
  .meter-anchor .btn {
    position: relative;
  }
  .dot {
    position: absolute;
    top: 3px;
    right: 3px;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
  }
  .cents-btn {
    min-width: 44px;
    justify-content: center;
    font-size: 12.5px;
  }
  .pop {
    position: absolute;
    bottom: calc(100% + 8px);
    right: 0;
    z-index: 40;
    padding: 10px 12px;
    background: var(--surface-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
    color: var(--fg);
  }
  .vol-pop {
    right: auto;
    left: 50%;
    transform: translateX(-50%);
  }
  .pop .field input[type="range"] {
    width: 140px;
  }
  .field {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .lbl {
    color: var(--muted);
    font-size: 12.5px;
  }
  .field input[type="range"] {
    width: 84px;
  }
  .field input.click-vol {
    width: 48px;
  }
  .val {
    font-size: 12.5px;
    min-width: 38px;
  }
  .val.semi {
    min-width: 44px;
    text-align: center;
  }
  .val.changed,
  .btn.changed {
    color: var(--accent);
  }
</style>
