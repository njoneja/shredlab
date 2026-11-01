<script lang="ts">
  import Icon from "./Icon.svelte";
  import { app } from "./store.svelte";
  import { formatTime } from "./meter";

  let { onhelp }: { onhelp: () => void } = $props();

  const subline = $derived.by(() => {
    const s = app.song;
    if (!s) return [];
    const parts: string[] = [];
    if (s.artist) parts.push(s.artist);
    parts.push(formatTime(s.duration_sec));
    if (s.separated_with) parts.push(`${s.separated_with.model_name} on ${s.separated_with.device_label}`);
    else if (app.separating && app.sepModel) parts.push(`${app.sepModel} on ${app.sepDevice}`);
    if (s.bpm != null) parts.push(`${Math.round(s.bpm)} BPM`);
    return parts;
  });

  const canExport = $derived(app.stemsReady && !app.exportSummary.startsWith("Nothing"));
</script>

<header>
  <div class="title">
    {#if app.song}
      <h1 title={app.song.path}>{app.song.title}</h1>
      <div class="sub muted">{subline.join(" · ")}</div>
    {:else}
      <h1 class="brand"><Icon name="music" size={20} /> Stem Extractor</h1>
      <div class="sub muted">Split songs into stems to play along or transcribe</div>
    {/if}
  </div>
  <div class="actions">
    <button class="btn ghost icon-only" onclick={onhelp} title="Keyboard shortcuts" aria-label="Keyboard shortcuts">
      <Icon name="keyboard" />
    </button>
    <button class="btn" onclick={() => app.pickAndOpen()} disabled={app.opening}>
      <Icon name="open" /> Open
    </button>
    {#if app.song}
      <div class="export">
        <button class="btn primary" onclick={() => app.openExport()} disabled={!canExport} title={app.stemsReady ? app.exportSummary : "Available when stems are ready"}>
          <Icon name="export" /> Export what you hear
        </button>
        <div class="summary muted" title={app.exportSummary}>{app.stemsReady ? app.exportSummary : "Available when stems are ready"}</div>
      </div>
    {/if}
  </div>
</header>

<style>
  header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
  }
  .title {
    min-width: 0;
  }
  h1 {
    margin: 0;
    font-size: 20px;
    line-height: 1.25;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--accent);
  }
  .sub {
    font-size: 13px;
    margin-top: 2px;
  }
  .actions {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    flex-shrink: 0;
  }
  .export {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 3px;
  }
  .summary {
    font-size: 11.5px;
    max-width: 320px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
