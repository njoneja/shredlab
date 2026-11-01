<script lang="ts">
  import { app, STAGE_TEXT } from "./store.svelte";

  const pct = $derived(Math.round(app.sepFraction * 100));
</script>

{#if app.separating}
  <div class="banner" role="status" aria-live="polite">
    <div class="top">
      <span class="stage">{STAGE_TEXT[app.sepStage]} <span class="mono">{pct}%</span></span>
      <span class="muted model">{app.sepModel}{app.sepDevice ? ` on ${app.sepDevice}` : ""}</span>
      <button class="btn small" onclick={() => app.cancelSeparation()}>Cancel</button>
    </div>
    <div class="bar"><div class="fill" style:width="{pct}%"></div></div>
    <div class="muted small">You can play the original mix while this runs.</div>
  </div>
{:else if app.song && !app.stemsReady}
  <div class="banner idle">
    <span>{app.sepError ?? "Stems are not separated yet."}</span>
    <button class="btn small primary" onclick={() => app.startSeparation()}>Separate stems</button>
  </div>
{/if}

<style>
  .banner {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px 12px;
    background: var(--surface);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
  }
  .banner.idle {
    flex-direction: row;
    align-items: center;
    justify-content: space-between;
    border-color: var(--border);
  }
  .top {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .stage {
    font-weight: 600;
  }
  .model {
    flex: 1;
    font-size: 12.5px;
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
    transition: width 0.15s linear;
  }
  .small {
    font-size: 12px;
  }
</style>
