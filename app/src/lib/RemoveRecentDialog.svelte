<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "./api";
  import { app } from "./store.svelte";
  import { formatBytes } from "./recent";

  // "Remove from recent?" asked by the ✕ on a recent song (start screen list and the header's
  // Open menu; app.askRemoveRecent()). Unticked (the default) it only hides the song from the
  // list. Ticked it also deletes the song's stored data (delete_song). Focus starts on Cancel,
  // so Enter or Esc never deletes anything. The open song's stems are in use: no delete option.

  const entry = app.removeAsk!.entry;
  const isOpen = entry.song_id === app.song?.song_id;

  let alsoDelete = $state(false);
  let busy = $state(false);
  // Bytes stored for the song (song_size walks only its folder, off the UI thread).
  let size = $state<number | null>(null);
  let dialog: HTMLElement;
  let cancelBtn: HTMLButtonElement;

  function cancel() {
    if (!busy) app.closeRemoveRecent(false);
  }

  async function confirm() {
    if (busy) return;
    if (!(alsoDelete && !isOpen)) {
      // hideRecent drops the row synchronously, before the opener moves focus.
      const hidden = app.hideRecent(entry.song_id);
      app.closeRemoveRecent(true);
      await hidden;
      app.showToast("Removed from recent");
      return;
    }
    busy = true;
    const freed = await app.deleteSongData(entry.song_id);
    busy = false;
    app.closeRemoveRecent(freed !== null);
    if (freed !== null) app.showToast(`Removed, freed ${formatBytes(freed)}`);
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      cancel();
    } else if (e.key === "Tab") {
      // Keep focus inside the dialog.
      const all = Array.from(dialog.querySelectorAll<HTMLElement>("button, input")).filter((el) => !(el as HTMLButtonElement).disabled);
      const i = all.indexOf(document.activeElement as HTMLElement);
      const next = e.shiftKey ? (i <= 0 ? all.length - 1 : i - 1) : i === all.length - 1 ? 0 : i + 1;
      e.preventDefault();
      all[next]?.focus();
    }
  }

  onMount(() => {
    cancelBtn.focus();
    if (!isOpen)
      api
        .songSize(entry.song_id)
        .then((n) => (size = n))
        .catch(() => (size = null));
  });
</script>

<svelte:window onkeydown={onKey} />

<div class="backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && cancel()}>
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="rr-title" aria-describedby="rr-desc" bind:this={dialog}>
    <h2 id="rr-title">Remove “{entry.title}” from recent?</h2>
    <p id="rr-desc" class="muted small">Opening the file again brings it back.</p>

    <label class="opt" class:off={isOpen}>
      <input type="checkbox" bind:checked={alsoDelete} disabled={isOpen || busy} aria-describedby="rr-note" />
      <span>
        Also delete its stems and settings
        <span id="rr-note" class="note muted">
          {#if isOpen}
            Close the song first to delete its stems.
          {:else}
            {#if size !== null}Frees {formatBytes(size)}.{/if}
            {#if entry.stems_cached}Opening the song again will mean separating it again.{/if}
            Your audio file is not touched.
          {/if}
        </span>
      </span>
    </label>

    <div class="actions">
      <button class="btn" bind:this={cancelBtn} onclick={cancel} disabled={busy}>Cancel</button>
      <button class="btn" class:danger={alsoDelete && !isOpen} onclick={confirm} disabled={busy}>
        {busy ? "Removing…" : "Remove"}
      </button>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 60;
    background: var(--backdrop);
    display: grid;
    place-items: center;
    padding: 20px;
  }
  .dialog {
    width: 420px;
    max-width: 100%;
    display: flex;
    flex-direction: column;
    gap: 10px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 16px 18px;
    box-shadow: var(--shadow);
  }
  h2 {
    margin: 0;
    font-size: 16px;
    line-height: 1.35;
    overflow-wrap: anywhere;
  }
  p {
    margin: -4px 0 0;
  }
  .small {
    font-size: 12.5px;
  }
  .opt {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    font-size: 13.5px;
    line-height: 1.4;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface-2);
  }
  .opt.off > span {
    color: var(--muted);
  }
  input[type="checkbox"] {
    width: 16px;
    height: 16px;
    margin: 2px 0 0;
    flex: none;
    accent-color: var(--accent);
  }
  .note {
    display: block;
    font-size: 12px;
    margin-top: 2px;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 2px;
  }
  .btn.danger:not(:disabled) {
    color: var(--danger);
    border-color: var(--danger);
  }
</style>
