<script lang="ts">
  import { app } from "./store.svelte";
  import { STEMS, type StemName } from "./types";
  import { SLIDER_MAX, formatDb, gainToSlider, sliderToGain } from "./mix";

  /** Full names: tooltips and every accessible name (never the tape abbreviation). */
  const LABEL: Record<StemName, string> = {
    vocals: "Vocals",
    guitar: "Guitar",
    bass: "Bass",
    drums: "Drums",
    piano: "Piano / keys",
    other: "Other",
  };
  /** Scribble-strip abbreviations written on the tape (brand guide). */
  const SHORT: Record<StemName, string> = {
    vocals: "VOX",
    guitar: "GTR",
    bass: "BASS",
    drums: "DRUMS",
    piano: "KEYS",
    other: "MISC",
  };
  /** Small fixed tilt per strip, so they look hand-stuck. */
  const TILT: Record<StemName, number> = {
    vocals: -2,
    guitar: 1.5,
    bass: -1,
    drums: 1,
    piano: -1.5,
    other: 2,
  };

  let menu = $state<{ stem: StemName; x: number; y: number } | null>(null);

  function openMenu(e: MouseEvent, stem: StemName) {
    e.preventDefault();
    menu = { stem, x: e.clientX, y: e.clientY };
  }

  const disabled = $derived(!app.stemsReady);

  // Arrow/Page/Home/End on a focused volume slider move it (0.5 dB per arrow
  // step) instead of triggering the global seek/speed shortcuts.
  const SLIDER_KEYS = new Set(["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "PageUp", "PageDown", "Home", "End"]);
  function sliderKeys(e: KeyboardEvent) {
    if (SLIDER_KEYS.has(e.key) && !e.shiftKey && !e.altKey) e.stopPropagation();
  }
</script>

<svelte:window onclick={() => (menu = null)} onkeydown={(e) => e.key === "Escape" && (menu = null)} />

<section class="mixer" class:disabled aria-label="Stem mixer">
  <!-- Same columns as the rows: All on sits above S and ⊘, the heading above the labels. -->
  <div class="head">
    <button
      class="btn small all-on"
      aria-pressed={app.allOnAlready}
      {disabled}
      onclick={() => app.allOn()}
      title={app.allOnAlready
        ? "All stems are on"
        : "Turn every stem on and clear solo (volumes stay as set)"}
    >
      All on
    </button>
    <h2>Stems</h2>
  </div>
  {#if disabled}
    <div class="notice">
      {#if app.separating}
        Stems appear here when separation finishes. Until then you hear the original mix.
      {:else}
        Stems are not separated yet. You hear the original mix.
      {/if}
    </div>
  {/if}
  {#each STEMS as stem, i (stem)}
    {@const m = app.mix[stem]}
    {@const audible = app.audible(stem)}
    <div
      class="row"
      class:off={!audible}
      class:soloed={m.solo}
      style:--stem-color="var(--stem-{stem})"
      oncontextmenu={(e) => !disabled && openMenu(e, stem)}
      role="group"
      aria-label={LABEL[stem]}
    >
      <label class="switch" title="Turn {LABEL[stem]} on/off ({i + 1})">
        <input
          type="checkbox"
          checked={m.on}
          {disabled}
          aria-label="{LABEL[stem]} on"
          onchange={(e) => app.setStemOn(stem, e.currentTarget.checked)}
        />
        <span class="track"><span class="thumb"></span></span>
      </label>
      <button
        class="btn small solo"
        aria-pressed={m.solo}
        aria-label="Solo {LABEL[stem]}"
        {disabled}
        onclick={() => app.toggleSolo(stem)}
        title="Solo {LABEL[stem]}: hear just this stem, without changing the switches (Shift+{i + 1})"
      >
        S
      </button>
      <button
        class="all-but"
        {disabled}
        onclick={() => app.allButThis(stem)}
        aria-label="Play along: everything except {LABEL[stem]}"
        title="Play along: everything except {LABEL[stem]} (Alt+{i + 1})"
      >
        <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
          <circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" stroke-width="1.6" />
          <path d="M3.8 12.2 12.2 3.8" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
        </svg>
      </button>
      <span class="name">
        <span class="dot" aria-hidden="true"></span>
        <span class="tape" style:--tilt="{TILT[stem]}deg" title={LABEL[stem]} aria-hidden="true">{SHORT[stem]}</span>
        <span class="sr-only">{LABEL[stem]}</span>
        <kbd class="key" aria-hidden="true">{i + 1}</kbd>
      </span>
      <!-- dB-linear from −40 to +6 dB in 0.5 dB steps; the bottom position is off (−∞). -->
      <input
        class="vol"
        type="range"
        min="0"
        max={SLIDER_MAX}
        step="1"
        value={gainToSlider(m.volume)}
        {disabled}
        aria-label="{LABEL[stem]} volume"
        aria-valuetext={formatDb(m.volume)}
        title="{LABEL[stem]} volume: {formatDb(m.volume)} (double-click: 0 dB)"
        oninput={(e) => app.setVolume(stem, sliderToGain(Number(e.currentTarget.value)))}
        ondblclick={() => app.setVolume(stem, 1)}
        onkeydown={sliderKeys}
      />
      <span class="readout mono">{formatDb(m.volume)}</span>
    </div>
  {/each}
  {#if app.anySolo && !disabled}
    <div class="solo-note">Solo is on: only the soloed stem plays, and Export follows what you hear.</div>
  {/if}
</section>

{#if menu}
  <div class="menu" style:left="{menu.x}px" style:top="{menu.y}px" role="menu">
    <button role="menuitem" onclick={() => menu && app.toggleSolo(menu.stem)}>
      {menu && app.mix[menu.stem].solo ? "Unsolo" : "Solo"}
    </button>
    <button role="menuitem" onclick={() => menu && app.allButThis(menu.stem)}>All but this</button>
    <button role="menuitem" onclick={() => app.allOn()}>All on</button>
  </div>
{/if}

<style>
  .mixer {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 4px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 6px;
  }
  .head {
    display: grid;
    grid-template-columns: 40px 24px 24px minmax(120px, 160px) minmax(80px, 1fr) 62px;
    align-items: center;
    gap: 10px;
    /* Rows have a 3px stem-colour border plus 8px padding. */
    padding: 0 8px 2px 11px;
  }
  .all-on {
    grid-column: 2 / 4;
    justify-content: center;
    padding: 0;
  }
  /* Lit like a hardware button while every stem is on. */
  .all-on[aria-pressed="true"] {
    color: var(--lit);
    border-color: var(--lit);
    background: color-mix(in srgb, var(--lit) 16%, transparent);
    box-shadow: 0 0 8px color-mix(in srgb, var(--lit) 35%, transparent);
  }
  .all-on[aria-pressed="true"]:hover:not(:disabled) {
    background: color-mix(in srgb, var(--lit) 22%, transparent);
  }
  h2 {
    margin: 0;
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted);
  }
  .notice {
    font-size: 12.5px;
    color: var(--muted);
    padding: 4px 8px 6px;
  }
  .row {
    display: grid;
    grid-template-columns: 40px 24px 24px minmax(120px, 160px) minmax(80px, 1fr) 62px;
    align-items: center;
    gap: 10px;
    padding: 5px 8px;
    border-radius: var(--radius-sm);
    border-left: 3px solid var(--stem-color);
    background: var(--surface-2);
    transition: opacity 0.12s;
  }
  /* The whole row dims when off, tape included. */
  .row.off {
    opacity: 0.42;
  }
  .row.soloed {
    box-shadow: inset 0 0 0 1px var(--solo);
  }
  .mixer.disabled .row {
    opacity: 0.35;
  }
  .name {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--stem-color);
  }
  /* Scribble strip: masking tape written in marker. */
  .tape {
    display: inline-block;
    padding: 3px 10px 2px;
    font-family: var(--font-marker);
    font-size: 15px;
    line-height: 1.15;
    letter-spacing: 0.04em;
    background: var(--tape);
    color: var(--tape-ink);
    clip-path: polygon(2% 0, 98% 6%, 100% 48%, 97% 100%, 4% 94%, 0 52%);
    box-shadow: 0 1px 0 rgba(0, 0, 0, 0.25);
    transform: rotate(var(--tilt, 0deg));
    white-space: nowrap;
    user-select: none;
  }
  .key {
    margin-left: auto;
    opacity: 0.7;
  }
  .vol {
    width: 100%;
    accent-color: var(--stem-color);
  }
  .readout {
    text-align: right;
    font-size: 12px;
    color: var(--muted);
  }
  .solo {
    width: 24px;
    padding: 0;
    justify-content: center;
    font-weight: 700;
  }
  .solo[aria-pressed="true"] {
    background: var(--solo);
    border-color: var(--solo);
    color: var(--accent-fg);
  }
  .solo[aria-pressed="true"]:hover:not(:disabled) {
    background: var(--solo);
    filter: brightness(1.08);
  }
  /* "All but this": quieter than S (no fill, faint icon). */
  .all-but {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    padding: 0;
    border: 0;
    border-radius: 5px;
    background: transparent;
    color: var(--faint);
  }
  .all-but:hover:not(:disabled) {
    color: var(--muted);
    background: var(--surface-3);
  }
  .solo-note {
    font-size: 12px;
    color: var(--solo);
    padding: 2px 8px;
  }

  /* switch */
  .switch {
    position: relative;
    display: inline-flex;
    cursor: pointer;
  }
  .switch input {
    position: absolute;
    opacity: 0;
    width: 100%;
    height: 100%;
    margin: 0;
    cursor: inherit;
  }
  .track {
    width: 34px;
    height: 20px;
    border-radius: 10px;
    background: var(--surface-3);
    border: 1px solid var(--border);
    position: relative;
    transition: background 0.12s;
  }
  .thumb {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: var(--muted);
    transition: transform 0.12s, background 0.12s;
  }
  .switch input:checked + .track {
    background: var(--stem-color);
    border-color: var(--stem-color);
  }
  .switch input:checked + .track .thumb {
    transform: translateX(14px);
    background: #fff;
  }
  .switch input:focus-visible + .track {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .switch input:disabled {
    cursor: default;
  }

  .menu {
    position: fixed;
    z-index: 50;
    min-width: 150px;
    display: flex;
    flex-direction: column;
    padding: 4px;
    background: var(--surface-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    box-shadow: var(--shadow);
  }
  .menu button {
    text-align: left;
    background: none;
    border: 0;
    padding: 6px 10px;
    border-radius: 4px;
  }
  .menu button:hover {
    background: var(--surface-2);
  }

  @media (max-width: 960px) {
    .row,
    .head {
      grid-template-columns: 40px 24px 24px minmax(100px, 130px) minmax(60px, 1fr) 58px;
      gap: 8px;
    }
    .key {
      display: none;
    }
  }
</style>
