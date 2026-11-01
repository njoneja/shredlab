<script lang="ts">
  import Icon from "./Icon.svelte";
  import MeterPanel from "./MeterPanel.svelte";
  import { app, CENTS_MAX, SEMI_MAX, SPEED_MAX, SPEED_MIN } from "./store.svelte";
  import { formatCountIn } from "./meter";

  let meterOpen = $state(false);
  const noSong = $derived(!app.song);
  const noGrid = $derived(!app.grid);
</script>

<section class="transport" aria-label="Transport">
  <div class="group main">
    <button class="btn icon-only ghost" onclick={() => app.backToStart()} disabled={noSong} title="Back to loop start">
      <Icon name="back" />
    </button>
    <button class="play" onclick={() => app.togglePlay()} disabled={noSong} title="Play / pause (Space)" aria-label={app.playing ? "Pause" : "Play"}>
      <Icon name={app.playing ? "pause" : "play"} size={22} />
    </button>
    {#if app.countInLeft > 0}
      <span class="countdown mono" title="Count-in">{app.countInLeft}</span>
    {/if}
  </div>

  <div class="group">
    <button class="btn toggle" aria-pressed={app.loop.enabled} onclick={() => app.toggleLoop()} disabled={noSong} title="A–B loop on/off (L). [ and ] set A/B at the playhead.">
      <Icon name="loop" /> A–B
    </button>
    <button class="btn toggle" aria-pressed={app.click.enabled} onclick={() => app.toggleClick()} disabled={noSong || noGrid} title={noGrid ? "Waiting for beat detection" : "Click track on/off (C)"}>
      <Icon name="metronome" /> Click
    </button>
    <button class="btn toggle" aria-pressed={app.click.count_in_enabled} onclick={() => app.toggleCountIn()} disabled={noSong || noGrid} title="Count-in before playback starts">
      <Icon name="countin" /> Count-in
    </button>
    <div class="stepper" class:inactive={!app.click.count_in_enabled}>
      <button class="btn small icon-only" onclick={() => app.stepCountIn(-1)} disabled={noGrid || app.click.count_in_beats <= app.countInMin} aria-label="Shorter count-in">
        <Icon name="minus" size={14} />
      </button>
      <span class="mono countin" title="Count-in length; ends exactly at the playback or loop start">
        {noGrid ? "—" : formatCountIn(app.click.count_in_beats, app.beatsPerBar)}
      </span>
      <button class="btn small icon-only" onclick={() => app.stepCountIn(1)} disabled={noGrid || app.click.count_in_beats >= app.countInMax} aria-label="Longer count-in">
        <Icon name="plus" size={14} />
      </button>
    </div>
    <div class="meter-anchor">
      <button class="btn small" onclick={() => (meterOpen = !meterOpen)} disabled={noGrid} aria-expanded={meterOpen} title="Beat grid and meter overrides">
        {noGrid ? "Detecting beats…" : `${app.beatsPerBar} beats per bar`}
        {#if app.overrides.length}<span class="dot" title="Has overrides"></span>{/if}
        <span class="caret">▾</span>
      </button>
      {#if meterOpen}
        <MeterPanel onclose={() => (meterOpen = false)} />
      {/if}
    </div>
  </div>

  <div class="group practice">
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
    <div class="field" title="Pitch in semitones (Shift+↑ ↓)">
      <span class="lbl">Pitch</span>
      <button class="btn small icon-only" onclick={() => app.setPitch(app.semitones - 1, app.cents)} disabled={noSong || app.semitones <= -SEMI_MAX} aria-label="Pitch down a semitone">
        <Icon name="minus" size={14} />
      </button>
      <span class="mono val semi" class:changed={app.semitones !== 0}>{app.semitones > 0 ? "+" : ""}{app.semitones} st</span>
      <button class="btn small icon-only" onclick={() => app.setPitch(app.semitones + 1, app.cents)} disabled={noSong || app.semitones >= SEMI_MAX} aria-label="Pitch up a semitone">
        <Icon name="plus" size={14} />
      </button>
    </div>
    <label class="field" title="Fine pitch in cents. Double-click to reset.">
      <input
        class="cents"
        type="range"
        min={-CENTS_MAX}
        max={CENTS_MAX}
        step="1"
        value={app.cents}
        disabled={noSong}
        oninput={(e) => app.setPitch(app.semitones, Number(e.currentTarget.value))}
        ondblclick={() => app.setPitch(app.semitones, 0)}
      />
      <span class="mono val" class:changed={app.cents !== 0}>{app.cents > 0 ? "+" : ""}{app.cents}¢</span>
    </label>
  </div>
</section>

<style>
  .transport {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px 18px;
    padding: 10px 12px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .group {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .practice {
    margin-left: auto;
    gap: 16px;
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
    min-width: 28px;
    height: 28px;
    border-radius: 14px;
    display: grid;
    place-items: center;
    background: var(--accent-soft);
    color: var(--accent);
    font-weight: 700;
  }
  .stepper {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .stepper.inactive .countin {
    color: var(--muted);
  }
  .countin {
    min-width: 128px;
    text-align: center;
    font-size: 12.5px;
  }
  .stepper .btn.icon-only,
  .field .btn.icon-only {
    width: 26px;
  }
  .meter-anchor {
    position: relative;
  }
  .caret {
    font-size: 10px;
    opacity: 0.7;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
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
    width: 110px;
  }
  .field input.cents {
    width: 80px;
  }
  .val {
    font-size: 12.5px;
    min-width: 38px;
  }
  .val.semi {
    min-width: 44px;
    text-align: center;
  }
  .val.changed {
    color: var(--accent);
  }
</style>
