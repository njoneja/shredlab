<script lang="ts">
  import Icon from "./Icon.svelte";
  import { app } from "./store.svelte";
  import { STEMS, type StemName } from "./types";

  const LABEL: Record<StemName, string> = {
    vocals: "Vocals",
    guitar: "Guitar",
    bass: "Bass",
    drums: "Drums",
    piano: "Piano",
    other: "Other",
  };

  let menu = $state<{ stem: StemName; x: number; y: number } | null>(null);

  function openMenu(e: MouseEvent, stem: StemName) {
    e.preventDefault();
    menu = { stem, x: e.clientX, y: e.clientY };
  }

  const disabled = $derived(!app.stemsReady);
</script>

<svelte:window onclick={() => (menu = null)} onkeydown={(e) => e.key === "Escape" && (menu = null)} />

<section class="mixer" class:disabled aria-label="Stem mixer">
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
        <input type="checkbox" checked={m.on} {disabled} onchange={(e) => app.setStemOn(stem, e.currentTarget.checked)} />
        <span class="track"><span class="thumb"></span></span>
      </label>
      <span class="name">
        <span class="stem-icon"><Icon name={stem} size={18} /></span>
        {LABEL[stem]}
        <kbd class="key">{i + 1}</kbd>
      </span>
      <input
        class="vol"
        type="range"
        min="0"
        max="100"
        step="1"
        value={Math.round(m.volume * 100)}
        {disabled}
        aria-label="{LABEL[stem]} volume"
        oninput={(e) => app.setVolume(stem, Number(e.currentTarget.value) / 100)}
        ondblclick={() => app.setVolume(stem, 1)}
      />
      <span class="readout mono">{Math.round(m.volume * 100)}%</span>
      <div class="actions">
        <button class="btn small" {disabled} onclick={() => app.onlyThis(stem)} title="Turn all other stems off (Shift+{i + 1})">
          Only this
        </button>
        <button class="btn small" {disabled} onclick={() => app.allButThis(stem)} title="Turn only this stem off (Alt+{i + 1})">
          All but this
        </button>
        <button
          class="btn small solo"
          aria-pressed={m.solo}
          {disabled}
          onclick={() => app.toggleSolo(stem)}
          title="Solo: hear just this stem for now, without changing the switches"
        >
          S
        </button>
      </div>
    </div>
  {/each}
  {#if app.anySolo && !disabled}
    <div class="solo-note">Solo is on: only the soloed stem plays. Export still uses the switches.</div>
  {/if}
</section>

{#if menu}
  <div class="menu" style:left="{menu.x}px" style:top="{menu.y}px" role="menu">
    <button role="menuitem" onclick={() => menu && app.onlyThis(menu.stem)}>Only this</button>
    <button role="menuitem" onclick={() => menu && app.allButThis(menu.stem)}>All but this</button>
    <button role="menuitem" onclick={() => menu && app.toggleSolo(menu.stem)}>
      {menu && app.mix[menu.stem].solo ? "Unsolo" : "Solo"}
    </button>
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
  .notice {
    font-size: 12.5px;
    color: var(--muted);
    padding: 4px 8px 6px;
  }
  .row {
    display: grid;
    grid-template-columns: 40px minmax(110px, 150px) minmax(80px, 1fr) 44px auto;
    align-items: center;
    gap: 12px;
    padding: 4px 8px;
    border-radius: var(--radius-sm);
    border-left: 3px solid var(--stem-color);
    background: var(--surface-2);
    transition: opacity 0.12s;
  }
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
    font-weight: 600;
  }
  .stem-icon {
    color: var(--stem-color);
    display: inline-flex;
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
  .actions {
    display: flex;
    gap: 6px;
  }
  .solo {
    width: 28px;
    justify-content: center;
    font-weight: 700;
  }
  .solo[aria-pressed="true"] {
    background: var(--solo);
    border-color: var(--solo);
    color: #1b1600;
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
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4);
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
    .row {
      grid-template-columns: 40px minmax(90px, 120px) minmax(60px, 1fr) 40px auto;
      gap: 8px;
    }
    .key {
      display: none;
    }
  }
</style>
