<script lang="ts">
  import { onMount, tick } from "svelte";
  import Icon from "./Icon.svelte";
  import { api } from "./api";
  import { app } from "./store.svelte";
  import { ago, formatBytes } from "./recent";
  import type { LibraryListing, StoredSongInfo } from "./types";

  // Manage storage: every song in the library (hidden ones too) with the size of its stored
  // data. Opened from Settings (app.openStorage()). Sizes come from list_library, which walks
  // the song folders on a background thread, so the dialog shows "Calculating…" first.
  // The open song cannot be deleted (its stems are playing); close it first.

  type Sort = "size" | "opened";

  let listing = $state<LibraryListing | null>(null);
  let loadError = $state<string | null>(null);
  let sort = $state<Sort>("size");
  let selected = $state<Set<string>>(new Set());
  // Songs waiting for the delete confirmation.
  let confirm = $state<StoredSongInfo[] | null>(null);
  let deleting = $state(false);
  let closeBtn: HTMLButtonElement;
  let confirmBtn = $state<HTMLButtonElement>();

  const songs = $derived.by(() => {
    const s = [...(listing?.songs ?? [])];
    if (sort === "size") s.sort((a, b) => b.size_bytes - a.size_bytes);
    else s.sort((a, b) => b.last_opened_ms - a.last_opened_ms);
    return s;
  });
  const deletable = $derived(songs.filter((s) => !s.open));
  const chosen = $derived(songs.filter((s) => selected.has(s.song_id) && !s.open));
  const allChosen = $derived(deletable.length > 0 && chosen.length === deletable.length);
  const confirmBytes = $derived(confirm?.reduce((n, s) => n + s.size_bytes, 0) ?? 0);

  function fileName(path: string) {
    return path.split(/[\\/]/).pop() || path;
  }

  async function load() {
    try {
      listing = await api.listLibrary();
      loadError = null;
      const ids = new Set(listing.songs.map((s) => s.song_id));
      selected = new Set([...selected].filter((id) => ids.has(id)));
    } catch (e) {
      loadError = e instanceof Error ? e.message : String(e);
    }
  }

  function toggle(id: string, on: boolean) {
    const next = new Set(selected);
    if (on) next.add(id);
    else next.delete(id);
    selected = next;
  }

  function toggleAll(on: boolean) {
    selected = new Set(on ? deletable.map((s) => s.song_id) : []);
  }

  async function ask(list: StoredSongInfo[]) {
    if (!list.length) return;
    confirm = list;
    await tick();
    confirmBtn?.focus();
  }

  async function cancelConfirm() {
    confirm = null;
    await tick();
    closeBtn?.focus();
  }

  async function doDelete() {
    if (!confirm) return;
    deleting = true;
    let freed = 0;
    const failed: string[] = [];
    for (const s of confirm) {
      try {
        freed += await api.deleteSong(s.song_id);
      } catch (e) {
        failed.push(`${s.title}: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
    deleting = false;
    confirm = null;
    selected = new Set();
    await load();
    app.refreshLibrary();
    if (failed.length) app.error = `Could not delete: ${failed.join("; ")}`;
    else app.showToast(`Freed ${formatBytes(freed)}`);
    await tick();
    closeBtn?.focus();
  }

  function onKey(e: KeyboardEvent) {
    if (e.key !== "Escape") return;
    if (confirm) {
      if (!deleting) cancelConfirm();
    } else app.closeStorage();
  }

  onMount(() => {
    closeBtn.focus();
    load();
  });
</script>

<svelte:window onkeydown={onKey} />

<div class="backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && !confirm && app.closeStorage()}>
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="storage-title">
    <div class="head">
      <h2 id="storage-title">Manage storage</h2>
      <button class="btn ghost small icon-only" bind:this={closeBtn} onclick={() => app.closeStorage()} aria-label="Close"><Icon name="close" size={14} /></button>
    </div>

    <p class="total" aria-live="polite">
      {#if loadError}
        <span class="err">Could not read the library: {loadError}</span>
      {:else if !listing}
        <span class="muted">Calculating…</span>
      {:else}
        <strong>{formatBytes(listing.total_bytes)}</strong>
        <span class="muted">in {listing.songs.length} {listing.songs.length === 1 ? "song" : "songs"}</span>
      {/if}
    </p>
    {#if listing}
      <p class="where muted">Stored in <span class="mono path" title={listing.dir}>{listing.dir}</span></p>
    {/if}

    {#if confirm}
      <div class="confirm" role="alertdialog" aria-labelledby="confirm-title" aria-describedby="confirm-desc">
        <h3 id="confirm-title">
          Delete stems for {confirm.length === 1 ? `“${confirm[0].title}”` : `${confirm.length} songs`}?
        </h3>
        <p id="confirm-desc">
          Frees {formatBytes(confirmBytes)}. The stems, cached analysis and the song's settings (mix, Refine, strength) are
          removed. Opening {confirm.length === 1 ? "the song" : "these songs"} again will need separating again. Your original
          audio files are not touched.
        </p>
        <div class="actions">
          <button class="btn" onclick={cancelConfirm} disabled={deleting}>Cancel</button>
          <button class="btn danger" bind:this={confirmBtn} onclick={doDelete} disabled={deleting}>
            {deleting ? "Deleting…" : "Delete stems"}
          </button>
        </div>
      </div>
    {:else if listing && listing.songs.length}
      <div class="bar">
        <label class="all">
          <input type="checkbox" checked={allChosen} disabled={!deletable.length} onchange={(e) => toggleAll(e.currentTarget.checked)} />
          Select all
        </label>
        <div class="seg" role="radiogroup" aria-label="Sort by">
          <button role="radio" aria-checked={sort === "size"} onclick={() => (sort = "size")}>Size</button>
          <button role="radio" aria-checked={sort === "opened"} onclick={() => (sort = "opened")}>Last opened</button>
        </div>
        <button class="btn small danger" disabled={!chosen.length} onclick={() => ask(chosen)}>
          Delete selected{chosen.length ? ` (${chosen.length})` : ""}
        </button>
      </div>
      <ul class="list">
        {#each songs as s (s.song_id)}
          <li class:open={s.open}>
            <input
              type="checkbox"
              aria-label={`Select ${s.title}`}
              checked={selected.has(s.song_id)}
              disabled={s.open}
              onchange={(e) => toggle(s.song_id, e.currentTarget.checked)}
            />
            <div class="info">
              <span class="t" title={s.path}>{s.title}</span>
              <span class="sub muted" title={s.path}>
                {fileName(s.path)}
                {#if s.open}<span class="tag now">Open now</span>{/if}
                {#if s.hidden}<span class="tag">Not in recent</span>{/if}
                {#if !s.source_exists}<span class="tag warn" title="The original audio file is no longer at {s.path}">File missing</span>{/if}
              </span>
            </div>
            <span class="size mono">{formatBytes(s.size_bytes)}</span>
            <span class="when muted" title={new Date(s.last_opened_ms).toLocaleString()}>{ago(s.last_opened_ms)}</span>
            <span class="del" title={s.open ? "This song is open. Close it to delete its stems." : undefined}>
              <button class="btn small" disabled={s.open} aria-label={`Delete stems of ${s.title}`} onclick={() => ask([s])}>
                Delete stems
              </button>
            </span>
          </li>
        {/each}
      </ul>
    {:else if listing}
      <p class="muted none">No songs are stored.</p>
    {/if}
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
    width: 760px;
    max-width: 100%;
    max-height: calc(100vh - 40px);
    display: flex;
    flex-direction: column;
    gap: 8px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 16px 18px;
    box-shadow: var(--shadow);
  }
  .head {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .head h2 {
    flex: 1;
    margin: 0;
    font-size: 17px;
  }
  p {
    margin: 0;
  }
  .total {
    font-size: 14px;
    display: flex;
    gap: 6px;
    align-items: baseline;
  }
  .total strong {
    font-size: 18px;
  }
  .err {
    color: var(--danger);
  }
  .where {
    font-size: 12px;
    display: flex;
    gap: 6px;
    min-width: 0;
  }
  .path {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    user-select: text;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-top: 4px;
  }
  .all {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12.5px;
    flex: 1;
  }
  input[type="checkbox"] {
    width: 16px;
    height: 16px;
    margin: 0;
    accent-color: var(--accent);
  }
  .seg {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 5px;
    overflow: hidden;
  }
  .seg button {
    height: 26px;
    padding: 0 10px;
    border: 0;
    background: var(--surface-2);
    font-size: 12.5px;
  }
  .seg button + button {
    border-left: 1px solid var(--border);
  }
  .seg button[aria-checked="true"] {
    background: var(--accent);
    color: var(--accent-fg);
  }
  .seg button:focus-visible {
    outline-offset: -2px;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow: auto;
    min-height: 0;
    border-top: 1px solid var(--border);
  }
  li {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) 72px 92px auto;
    gap: 12px;
    align-items: center;
    padding: 7px 4px;
    border-bottom: 1px solid var(--border);
  }
  li.open {
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .info {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .t {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sub {
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tag {
    font-size: 11px;
    padding: 0 6px;
    margin-left: 4px;
    border-radius: 10px;
    border: 1px solid var(--border);
  }
  .tag.now {
    color: var(--accent);
    border-color: var(--accent);
  }
  .tag.warn {
    color: var(--danger);
    border-color: currentColor;
  }
  .size,
  .when {
    text-align: right;
    font-size: 12.5px;
  }
  .none {
    padding: 8px 0;
  }
  .confirm {
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 12px 14px;
    background: var(--surface-2);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .confirm h3 {
    margin: 0;
    font-size: 15px;
  }
  .confirm p {
    font-size: 13px;
    line-height: 1.45;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .btn.danger:not(:disabled) {
    color: var(--danger);
    border-color: var(--danger);
  }
</style>
