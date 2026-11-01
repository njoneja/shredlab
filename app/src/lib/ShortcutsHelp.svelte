<script lang="ts">
  import Icon from "./Icon.svelte";
  let { onclose }: { onclose: () => void } = $props();

  const KEYS: [string[], string][] = [
    [["Space"], "Play / pause"],
    [["L"], "Loop on / off"],
    [["["], "Set A at the playhead"],
    [["]"], "Set B at the playhead"],
    [["←", "→"], "Seek 5 s"],
    [["↑", "↓"], "Speed ±5%"],
    [["Shift", "↑ ↓"], "Pitch ±1 semitone"],
    [["C"], "Click on / off"],
    [["1–6"], "Toggle stem on / off"],
    [["Shift", "1–6"], "Only this stem"],
    [["Alt", "1–6"], "All but this stem"],
  ];
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && onclose()} />

<div class="backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div class="dialog" role="dialog" aria-modal="true" aria-label="Keyboard shortcuts">
    <div class="head">
      <h2>Keyboard shortcuts</h2>
      <button class="btn ghost small icon-only" onclick={onclose} aria-label="Close"><Icon name="close" size={14} /></button>
    </div>
    <dl>
      {#each KEYS as [keys, what] (what)}
        <dt>{#each keys as k, i (i)}{#if i}<span class="plus">+</span>{/if}<kbd>{k}</kbd>{/each}</dt>
        <dd>{what}</dd>
      {/each}
    </dl>
    <p class="muted">Stems are numbered top to bottom: 1 vocals, 2 guitar, 3 bass, 4 drums, 5 piano, 6 other.</p>
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
    width: 420px;
    max-width: 100%;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 16px 18px;
  }
  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 8px;
  }
  h2 {
    margin: 0;
    font-size: 17px;
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px 16px;
    margin: 0 0 10px;
    align-items: center;
  }
  dt {
    display: flex;
    gap: 3px;
    align-items: center;
  }
  dd {
    margin: 0;
  }
  .plus {
    color: var(--faint);
    font-size: 11px;
  }
  p {
    font-size: 12px;
    margin: 0;
  }
</style>
