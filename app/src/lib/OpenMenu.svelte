<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Navneet Joneja -->

<script lang="ts">
  // Header split button: "Open" shows the file dialog; the chevron opens a menu of recent
  // songs (same data as the start screen's list) plus "Open file…".
  import { tick } from "svelte";
  import Icon from "./Icon.svelte";
  import { app } from "./store.svelte";
  import { formatTime } from "./meter";
  import { recentStatus } from "./recent";
  import type { LibraryEntry } from "./types";

  const MAX = 8;

  let open = $state(false);
  let wrap = $state<HTMLElement>();
  let chevron = $state<HTMLButtonElement>();
  let menu = $state<HTMLElement>();

  const items = $derived(app.library.slice(0, MAX));

  function menuItems() {
    return Array.from(menu?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? []);
  }

  async function show(focus: "first" | "last" = "first") {
    open = true;
    app.refreshLibrary();
    await tick();
    const all = menuItems();
    (focus === "first" ? all[0] : all[all.length - 1])?.focus();
  }

  function hide(refocus = true) {
    open = false;
    if (refocus) chevron?.focus();
  }

  function onChevronKey(e: KeyboardEvent) {
    if (e.key === "Enter" || e.key === " " || e.key === "ArrowDown" || e.key === "ArrowUp") {
      // Handled here so the app's Space/arrow shortcuts (play, seek, speed) don't also fire,
      // and so the native keyup "click" doesn't toggle the menu shut again.
      e.preventDefault();
      e.stopPropagation();
      if (open && e.key !== "ArrowDown" && e.key !== "ArrowUp") hide();
      else show(e.key === "ArrowUp" ? "last" : "first");
    }
  }

  function onMenuKey(e: KeyboardEvent) {
    if (e.key === "Tab") {
      hide(false);
      return;
    }
    // Keep every other key inside the menu (Space/arrows/digits are app shortcuts).
    e.stopPropagation();
    const all = menuItems();
    const i = all.indexOf(document.activeElement as HTMLElement);
    let next = -1;
    if (e.key === "ArrowDown") next = (i + 1) % all.length;
    else if (e.key === "ArrowUp") next = (i - 1 + all.length) % all.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = all.length - 1;
    else if (e.key === "Escape") {
      e.preventDefault();
      hide();
      return;
    } else if (e.key === "Delete" && i >= 0 && all[i].dataset.song) {
      e.preventDefault();
      const entry = items.find((x) => x.song_id === all[i].dataset.song);
      if (entry) remove(null, entry);
      return;
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      if (i >= 0) all[i].click();
      return;
    } else return;
    e.preventDefault();
    all[next]?.focus();
  }

  function onWindowClick(e: MouseEvent) {
    // A click in the remove dialog reopens the menu (keyboard); don't close it again here.
    if (e.target instanceof Element && e.target.closest('[role="dialog"]')) return;
    if (open && wrap && !wrap.contains(e.target as Node)) hide(false);
  }

  function choose(entry: LibraryEntry) {
    hide();
    // The current song: just close the menu (re-opening would reset playback and the mix).
    if (entry.song_id === app.song?.song_id) return;
    app.openPath(entry.path);
  }

  // Asks first (RemoveRecentDialog: remove from recent, optionally delete the stems). The menu
  // closes meanwhile; from the keyboard (Delete, or Enter on ✕) it reopens afterwards with
  // focus where it was, or on the next row if the song went.
  function remove(ev: MouseEvent | null, entry: LibraryEntry) {
    ev?.stopPropagation();
    const keyboard = !ev || ev.detail === 0;
    const all = menuItems();
    const at = Math.max(all.indexOf(document.activeElement as HTMLElement), 0);
    const row = Math.floor(all.findIndex((el) => el.dataset.song === entry.song_id) / 2);
    hide(false);
    app.askRemoveRecent(entry, async (removed) => {
      if (!keyboard) {
        chevron?.focus();
        return;
      }
      await show();
      if (!removed) {
        menuItems()[at]?.focus();
        return;
      }
      const rest = menuItems().filter((el) => el.dataset.song || el.classList.contains("file"));
      const songsLeft = rest.filter((el) => el.dataset.song);
      (songsLeft[Math.min(Math.max(row, 0), songsLeft.length - 1)] ?? rest[rest.length - 1])?.focus();
    });
  }

  function openFile() {
    hide();
    app.pickAndOpen();
  }
</script>

<svelte:window onclick={onWindowClick} />

<div class="split" bind:this={wrap}>
  <button class="btn main" onclick={() => app.pickAndOpen()} disabled={app.opening}>
    <Icon name="open" /> Open
  </button>
  <!-- Not disabled while opening (openPath ignores re-entry): disabling would drop the
       focus that hide() just returned to it. -->
  <button
    bind:this={chevron}
    class="btn chev"
    aria-haspopup="menu"
    aria-expanded={open}
    aria-controls={open ? "open-menu" : undefined}
    aria-label="Recent songs"
    title="Recent songs"
    onclick={() => (open ? hide() : show())}
    onkeydown={onChevronKey}
  >
    <Icon name="chevron" size={14} />
  </button>

  {#if open}
    <!-- svelte-ignore a11y_interactive_supports_focus -->
    <div class="menu" id="open-menu" role="menu" aria-label="Recent songs" bind:this={menu} onkeydown={onMenuKey}>
      <div class="head" role="presentation">Recent songs</div>
      {#each items as e (e.song_id)}
        {@const current = e.song_id === app.song?.song_id}
        <div class="row" role="none">
          <button
            class="item"
            class:current
            role="menuitem"
            tabindex="-1"
            aria-current={current ? "true" : undefined}
            aria-keyshortcuts="Delete"
            title={e.path}
            data-song={e.song_id}
            onclick={() => choose(e)}
          >
            <span class="t">{e.title}</span>
            <span class="badge" class:ready={e.stems_cached}>{recentStatus(e)}</span>
            <span class="meta muted">{[e.artist, formatTime(e.duration_sec)].filter(Boolean).join(" · ")}</span>
            {#if current}<span class="now">Open now</span>{/if}
          </button>
          <button
            class="x"
            role="menuitem"
            tabindex="-1"
            aria-label={`Remove ${e.title} from recent`}
            title="Remove from recent…"
            onclick={(ev) => remove(ev, e)}
          >
            <Icon name="close" size={12} />
          </button>
        </div>
      {:else}
        <div class="none muted" role="presentation">No recent songs</div>
      {/each}
      <div class="sep" role="separator"></div>
      <button class="item file" role="menuitem" tabindex="-1" onclick={openFile}>
        <Icon name="open" size={15} /> Open file…
      </button>
    </div>
  {/if}
</div>

<style>
  .split {
    position: relative;
    display: inline-flex;
  }
  .main {
    border-top-right-radius: 0;
    border-bottom-right-radius: 0;
  }
  .chev {
    width: 22px;
    padding: 0;
    justify-content: center;
    margin-left: -1px;
    border-top-left-radius: 0;
    border-bottom-left-radius: 0;
  }
  .chev[aria-expanded="true"] {
    background: var(--surface-3);
  }
  .menu {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 55;
    width: 330px;
    max-width: calc(100vw - 32px);
    display: flex;
    flex-direction: column;
    padding: 6px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 10px;
    box-shadow: var(--shadow);
  }
  .head {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted);
    padding: 4px 8px 6px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .row .item {
    flex: 1;
    min-width: 0;
  }
  .x {
    flex: none;
    width: 24px;
    height: 24px;
    display: grid;
    place-items: center;
    padding: 0;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--muted);
    opacity: 0;
  }
  .row:hover .x,
  .row:focus-within .x {
    opacity: 1;
  }
  .x:hover,
  .x:focus {
    background: var(--surface-2);
    color: var(--fg);
  }
  .x:focus:not(:focus-visible) {
    outline: none;
  }
  .item {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    column-gap: 10px;
    row-gap: 1px;
    align-items: center;
    width: 100%;
    text-align: left;
    background: none;
    border: 0;
    border-radius: var(--radius-sm);
    padding: 6px 8px;
    font-size: 13px;
  }
  .item:hover,
  .item:focus {
    background: var(--surface-2);
  }
  .item:focus:not(:focus-visible) {
    outline: none;
  }
  .item.current {
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .t {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meta {
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .now {
    justify-self: end;
    font-size: 11px;
    color: var(--accent);
  }
  .badge {
    justify-self: end;
    font-size: 11px;
    padding: 0 7px;
    border-radius: 10px;
    border: 1px solid var(--border);
    color: var(--muted);
  }
  .badge.ready {
    border-color: var(--ok);
    color: var(--ok);
  }
  .none {
    font-size: 12.5px;
    padding: 4px 8px 8px;
  }
  .sep {
    height: 1px;
    margin: 4px 2px;
    background: var(--border);
  }
  .item.file {
    display: flex;
    gap: 8px;
    font-weight: 500;
  }
</style>
