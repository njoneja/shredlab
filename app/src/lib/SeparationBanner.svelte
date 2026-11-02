<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Navneet Joneja -->

<script lang="ts">
  import { app, QUALITY_TEXT, STAGE_TEXT } from "./store.svelte";

  const pct = $derived(Math.round(app.sepFraction * 100));
  const target = $derived.by(() => {
    const st = app.appSettings;
    if (!st) return "";
    const name = st.models.find((m) => m.id === st.model_id)?.name ?? st.model_id ?? "";
    return `${name}, ${QUALITY_TEXT[st.quality].toLowerCase()} quality`;
  });
</script>

{#if app.separating}
  <div class="banner" role="status" aria-live="polite">
    <div class="top">
      <span class="stage">{STAGE_TEXT[app.sepStage]} <span class="mono">{pct}%</span></span>
      <span class="muted model">{app.sepModel}{app.sepDevice ? ` on ${app.sepDevice}` : ""}{app.sepQuality ? ` · ${QUALITY_TEXT[app.sepQuality]} quality` : ""}</span>
      <button class="btn small" onclick={() => app.cancelSeparation()}>Cancel</button>
    </div>
    <div class="bar"><div class="fill" style:width="{pct}%"></div></div>
    <div class="muted small">You can play the original mix while this runs.</div>
  </div>
{:else if app.showReseparate}
  <div class="banner idle" role="alertdialog" aria-label="Re-separate this song?">
    <span>Re-separate this song? <span class="muted small">Settings changed to {target}. Cached results load instantly.</span></span>
    <span class="buttons">
      <button class="btn small" onclick={() => app.keepCurrentStems()}>Keep current stems</button>
      <button class="btn small primary" onclick={() => app.reseparate()}>Re-separate</button>
    </span>
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
  .buttons {
    display: flex;
    gap: 6px;
    flex-shrink: 0;
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
