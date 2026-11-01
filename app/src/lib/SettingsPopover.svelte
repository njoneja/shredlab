<script lang="ts">
  import { tick } from "svelte";
  import { app } from "./store.svelte";
  import { THEMES, type ModelInfo, type Quality } from "./types";
  import { THEME_NAME, isTheme } from "./theme";

  let { onclose }: { onclose: () => void } = $props();

  const PASSES: { v: Quality; n: number }[] = [
    { v: "standard", n: 1 },
    { v: "high", n: 2 },
  ];

  const st = $derived(app.appSettings);

  function modelLabel(m: ModelInfo) {
    if (!m.personal) return m.name;
    const lic = !m.license || m.license.toLowerCase() === "unknown" ? "license unknown" : m.license;
    return `${m.name} (personal, ${lic})`;
  }

  // Picking the device default saves null, so the default keeps following the device.
  function pickQuality(q: Quality) {
    if (!st || app.savingSettings || q === st.quality) return;
    return app.saveAppSettings({ quality: q === st.default_quality ? null : q });
  }

  // Radiogroup keys: arrows move the choice (and focus), like native radios.
  async function passKeys(e: KeyboardEvent) {
    if (!st) return;
    const dir = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
    if (!dir) return;
    e.preventDefault();
    e.stopPropagation();
    const i = PASSES.findIndex((p) => p.v === st.quality);
    const next = PASSES[(i + dir + PASSES.length) % PASSES.length];
    const group = e.currentTarget as HTMLElement;
    await pickQuality(next.v);
    await tick(); // the buttons are disabled while saving; focus once they are back
    group.querySelector<HTMLElement>(`[data-q="${next.v}"]`)?.focus();
  }

  const passesTip = $derived(
    st ? `2 passes: cleaner stems, about twice as slow. Default on ${st.device_label}: ${st.default_quality === "high" ? 2 : 1}.` : "",
  );

  function pickModel(id: string) {
    if (!st || id === st.model_id) return;
    app.saveAppSettings({ model_id: id });
  }
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && onclose()} />

<div class="pop" role="dialog" aria-label="Settings">
  <h2>Settings</h2>
  <div class="row">
    <label class="label" for="theme-select">Theme</label>
    <select
      id="theme-select"
      class="compact"
      value={app.theme}
      onchange={(e) => isTheme(e.currentTarget.value) && app.setTheme(e.currentTarget.value)}
    >
      {#each THEMES as t (t)}
        <option value={t}>{THEME_NAME[t]}</option>
      {/each}
    </select>
  </div>
  <div class="row" title="With the count-in on, playing or exporting from the song start skips a quiet intro (studio talk, silence) and counts straight into the music.">
    <label class="label" for="music-start">Count in to where the music starts</label>
    <input id="music-start" type="checkbox" checked={app.countInToMusicStart} disabled={!st} onchange={(e) => app.setCountInToMusicStart(e.currentTarget.checked)} />
  </div>
  {#if !st}
    <p class="muted">Settings are not available.</p>
  {:else}
    <div class="row" title={passesTip}>
      <span class="label" id="passes-label">Passes</span>
      <!-- svelte-ignore a11y_interactive_supports_focus -->
      <div class="seg" role="radiogroup" aria-label="Separation passes" tabindex="-1" onkeydown={passKeys}>
        {#each PASSES as p (p.v)}
          <button
            role="radio"
            data-q={p.v}
            aria-checked={st.quality === p.v}
            tabindex={st.quality === p.v ? 0 : -1}
            disabled={app.savingSettings}
            onclick={() => pickQuality(p.v)}
          >
            {p.n}
          </button>
        {/each}
      </div>
    </div>

    {#if st.models.length > 1}
      <div class="field">
        <label class="label" for="model-select">Model</label>
        <select id="model-select" value={st.model_id ?? ""} disabled={app.savingSettings} onchange={(e) => pickModel(e.currentTarget.value)}>
          {#each st.models as m (m.id)}
            <option value={m.id}>{modelLabel(m)}</option>
          {/each}
        </select>
        {#if st.model_from_env}<p class="muted note">STEMX_MODEL is set and overrides this choice.</p>{/if}
      </div>
    {/if}

    <p class="muted note">Applies to the next separation. Results are kept per model and quality, so switching back is instant.</p>
  {/if}
  <button
    class="btn ghost small about"
    onclick={() => {
      onclose();
      app.openStorage();
    }}>Manage storage…</button
  >
  <button
    class="btn ghost small about"
    onclick={() => {
      onclose();
      app.openAbout();
    }}>About ShredLab</button
  >
</div>

<style>
  .pop {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 55;
    width: 370px;
    max-width: calc(100vw - 32px);
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 12px 14px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 10px;
    box-shadow: var(--shadow);
  }
  h2 {
    margin: 0;
    font-size: 15px;
  }
  p {
    margin: 0;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .label {
    font-size: 12px;
    font-weight: 600;
    color: var(--muted);
  }
  .note {
    font-size: 11.5px;
  }
  /* Segmented control: two joined native-size buttons, the chosen one accent-filled. */
  .seg {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 5px;
    overflow: hidden;
  }
  .seg button {
    width: 36px;
    height: 26px;
    border: 0;
    background: var(--surface-2);
    font-size: 13px;
    font-weight: 500;
  }
  .seg button + button {
    border-left: 1px solid var(--border);
  }
  .seg button:hover:not(:disabled):not([aria-checked="true"]) {
    background: var(--surface-3);
  }
  .seg button[aria-checked="true"] {
    background: var(--accent);
    color: var(--accent-fg);
  }
  .seg button:focus-visible {
    outline-offset: -2px;
  }
  /* Label left, control right. */
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }
  select {
    width: 100%;
    text-overflow: ellipsis;
    font: inherit;
    font-size: 13px;
    color: inherit;
    height: 28px;
    padding: 0 8px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
  }
  select.compact {
    width: 160px;
    height: 28px;
  }
  input[type="checkbox"] {
    width: 16px;
    height: 16px;
    margin: 0 4px 0 0;
    accent-color: var(--accent);
  }
  /* Readable native option list in every theme (color-scheme follows the theme too). */
  option {
    background: var(--surface);
    color: var(--fg);
  }
  .about {
    align-self: flex-start;
    margin-top: 2px;
    padding-left: 0;
  }
</style>
