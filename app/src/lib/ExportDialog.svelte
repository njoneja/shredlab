<script lang="ts">
  import Icon from "./Icon.svelte";
  import { app } from "./store.svelte";
  import type { ExportFormat } from "./types";

  const FORMATS: { v: ExportFormat; label: string; note: string }[] = [
    { v: "wav", label: "WAV", note: "Lossless, large" },
    { v: "flac", label: "FLAC", note: "Lossless, smaller" },
    { v: "mp3", label: "MP3", note: "Small, plays anywhere" },
  ];
  const changedTempo = $derived(app.speed !== 100 || app.semitones !== 0 || app.cents !== 0);
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && app.closeExport()} />

<div class="backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && app.closeExport()}>
  <div class="dialog" role="dialog" aria-modal="true" aria-label="Export what you hear">
    <div class="head">
      <h2>Export what you hear</h2>
      <button class="btn ghost small icon-only" onclick={() => app.closeExport()} disabled={app.exporting} aria-label="Close"><Icon name="close" size={14} /></button>
    </div>
    <p class="muted">{app.exportSummary}. Volumes as set; solo is ignored.</p>
    {#if app.exportFromOriginal}<p class="muted caption">Backing track built from the original</p>{/if}
    {#if changedTempo}
      <p class="muted">
        Includes speed {app.speed}% and pitch {app.semitones > 0 ? "+" : ""}{app.semitones} st {app.cents ? `${app.cents > 0 ? "+" : ""}${app.cents}¢` : ""}.
      </p>
    {/if}
    <div class="formats" role="radiogroup" aria-label="Format">
      {#each FORMATS as f (f.v)}
        <button role="radio" aria-checked={app.exportFormat === f.v} disabled={app.exporting} onclick={() => (app.exportFormat = f.v)}>
          <strong>{f.label}</strong>
          <span class="muted">{f.note}</span>
        </button>
      {/each}
    </div>

    {#if app.exporting}
      <div class="bar"><div class="fill" style:width="{Math.round(app.exportFraction * 100)}%"></div></div>
      <div class="row">
        <span class="muted">Exporting… {Math.round(app.exportFraction * 100)}%</span>
        <button class="btn small" onclick={() => app.cancelExport()}>Cancel</button>
      </div>
    {:else if app.exportDonePath}
      <p class="ok">Saved to <span class="mono">{app.exportDonePath}</span></p>
      <div class="row end"><button class="btn" onclick={() => app.closeExport()}>Done</button></div>
    {:else}
      {#if app.exportError}<p class="err">{app.exportError}</p>{/if}
      <div class="row end">
        <button class="btn" onclick={() => app.closeExport()}>Cancel</button>
        <button class="btn primary" onclick={() => app.runExport()}><Icon name="export" /> Choose file and export</button>
      </div>
    {/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 60;
    background: rgba(0, 0, 0, 0.55);
    display: grid;
    place-items: center;
    padding: 20px;
  }
  .dialog {
    width: 460px;
    max-width: 100%;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 16px 18px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    box-shadow: 0 20px 50px rgba(0, 0, 0, 0.5);
  }
  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  h2 {
    margin: 0;
    font-size: 17px;
  }
  p {
    margin: 0;
    font-size: 13px;
  }
  .caption {
    font-size: 12px;
    margin-top: -6px;
  }
  .formats {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 8px;
  }
  .formats button {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 10px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    text-align: left;
  }
  .formats button span {
    font-size: 11.5px;
  }
  .formats button[aria-checked="true"] {
    border-color: var(--accent);
    background: var(--accent-soft);
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .row.end {
    justify-content: flex-end;
  }
  .bar {
    height: 6px;
    border-radius: 3px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
  }
  .ok {
    color: var(--ok);
    word-break: break-all;
  }
  .err {
    color: var(--danger);
  }
</style>
