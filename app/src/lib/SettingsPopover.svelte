<script lang="ts">
  import { app } from "./store.svelte";
  import { THEMES, type ModelInfo, type Quality } from "./types";
  import { THEME_NAME, isTheme } from "./theme";

  let { onclose }: { onclose: () => void } = $props();

  const QUALITIES: { v: Quality; label: string }[] = [
    { v: "standard", label: "Standard (1 pass)" },
    { v: "high", label: "High (2 passes)" },
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
    app.saveAppSettings({ quality: q === st.default_quality ? null : q });
  }

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
  {#if !st}
    <p class="muted">Settings are not available.</p>
  {:else}
    <div class="field">
      <div class="label">Separation quality</div>
      <div class="options" role="radiogroup" aria-label="Separation quality">
        {#each QUALITIES as q (q.v)}
          <button role="radio" aria-checked={st.quality === q.v} disabled={app.savingSettings} onclick={() => pickQuality(q.v)}>
            <span>{q.label}</span>
            {#if q.v === st.default_quality}<span class="muted note">default on {st.device_label}</span>{/if}
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
  .options {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .options button {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 8px;
    padding: 7px 10px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    text-align: left;
  }
  .options button[aria-checked="true"] {
    border-color: var(--accent);
    background: var(--accent-soft);
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
  /* Readable native option list in every theme (color-scheme follows the theme too). */
  option {
    background: var(--surface);
    color: var(--fg);
  }
</style>
