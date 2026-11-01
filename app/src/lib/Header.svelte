<script lang="ts">
  import Icon from "./Icon.svelte";
  import SettingsPopover from "./SettingsPopover.svelte";
  import Wordmark from "./Wordmark.svelte";
  import { app, QUALITY_TEXT } from "./store.svelte";
  import { formatTime } from "./meter";

  let { onhelp }: { onhelp: () => void } = $props();

  let settingsOpen = $state(false);
  let settingsWrap = $state<HTMLElement>();

  function onWindowClick(e: MouseEvent) {
    if (settingsOpen && settingsWrap && !settingsWrap.contains(e.target as Node)) settingsOpen = false;
  }

  const subline = $derived.by(() => {
    const s = app.song;
    if (!s) return [];
    const parts: string[] = [];
    if (s.artist) parts.push(s.artist);
    parts.push(formatTime(s.duration_sec));
    // While separating, show what is being used; otherwise what the loaded stems were made with.
    if (app.separating && app.sepModel) {
      parts.push(`${app.sepModel} on ${app.sepDevice}`);
      if (app.sepQuality) parts.push(QUALITY_TEXT[app.sepQuality]);
    } else if (s.separated_with) {
      parts.push(`${s.separated_with.model_name} on ${s.separated_with.device_label}`);
      if (s.separated_with.quality) parts.push(QUALITY_TEXT[s.separated_with.quality] ?? s.separated_with.quality);
    }
    if (s.bpm != null) parts.push(`${Math.round(s.bpm)} BPM`);
    return parts;
  });

  const canExport = $derived(app.stemsReady && !app.exportSummary.startsWith("Nothing"));
</script>

<svelte:window onclick={onWindowClick} />

<header>
  <div class="left">
    <div class="brand" class:divider={!!app.song}><Wordmark size={20} /></div>
    {#if app.song}
      <div class="title">
        <h1 title={app.song.path}>{app.song.title}</h1>
        <div class="sub muted">{subline.join(" · ")}</div>
      </div>
    {/if}
  </div>
  <div class="actions">
    <div class="settings" bind:this={settingsWrap}>
      <button
        class="btn ghost icon-only"
        aria-pressed={settingsOpen}
        onclick={() => (settingsOpen = !settingsOpen)}
        title="Settings"
        aria-label="Settings"
        aria-expanded={settingsOpen}
      >
        <Icon name="settings" />
      </button>
      {#if settingsOpen}<SettingsPopover onclose={() => (settingsOpen = false)} />{/if}
    </div>
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
  .left {
    display: flex;
    align-items: flex-start;
    gap: 16px;
    min-width: 0;
  }
  .brand {
    flex: none;
    height: 40px;
    display: flex;
    align-items: center;
  }
  .brand.divider {
    padding-right: 16px;
    border-right: 1px solid var(--border);
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
  .settings {
    position: relative;
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
