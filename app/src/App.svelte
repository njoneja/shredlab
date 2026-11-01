<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "./lib/api";
  import { app } from "./lib/store.svelte";
  import { STEMS } from "./lib/types";
  import Header from "./lib/Header.svelte";
  import EmptyState from "./lib/EmptyState.svelte";
  import SeparationBanner from "./lib/SeparationBanner.svelte";
  import Waveform from "./lib/Waveform.svelte";
  import Mixer from "./lib/Mixer.svelte";
  import Transport from "./lib/Transport.svelte";
  import ExportDialog from "./lib/ExportDialog.svelte";
  import ShortcutsHelp from "./lib/ShortcutsHelp.svelte";
  import AboutDialog from "./lib/AboutDialog.svelte";
  import StorageDialog from "./lib/StorageDialog.svelte";

  onMount(() => {
    app.init();
  });

  function isTyping(t: EventTarget | null) {
    if (!(t instanceof HTMLElement)) return false;
    if (t.isContentEditable || t instanceof HTMLTextAreaElement || t instanceof HTMLSelectElement) return true;
    return t instanceof HTMLInputElement && !["range", "checkbox", "radio", "button"].includes(t.type);
  }

  // Keyboard shortcuts (listed in lib/ShortcutsHelp.svelte). Uses e.code for digits so Shift+1
  // works regardless of keyboard layout ("!" etc.).
  function onKeydown(e: KeyboardEvent) {
    if (!app.song || app.exportOpen || app.helpOpen || app.aboutOpen || app.storageOpen || isTyping(e.target)) return;
    if (e.ctrlKey || e.metaKey) return;
    const digit = /^Digit([1-6])$/.exec(e.code) ?? /^Numpad([1-6])$/.exec(e.code);
    let handled = true;
    if (digit) {
      const stem = STEMS[Number(digit[1]) - 1];
      if (e.altKey) app.allButThis(stem);
      else if (e.shiftKey) app.toggleSolo(stem);
      else app.toggleStem(stem);
    } else if (e.altKey) {
      handled = false;
    } else if (e.code === "Space") {
      app.togglePlay();
    } else if (e.code === "KeyL") {
      app.toggleLoop();
    } else if (e.code === "BracketLeft") {
      app.setAAtPlayhead();
    } else if (e.code === "BracketRight") {
      app.setBAtPlayhead();
    } else if (e.code === "ArrowLeft") {
      app.seekBy(-5);
    } else if (e.code === "ArrowRight") {
      app.seekBy(5);
    } else if (e.code === "ArrowUp") {
      if (e.shiftKey) app.setPitch(app.semitones + 1, app.cents);
      else app.setSpeed(app.speed + 5);
    } else if (e.code === "ArrowDown") {
      if (e.shiftKey) app.setPitch(app.semitones - 1, app.cents);
      else app.setSpeed(app.speed - 5);
    } else if (e.code === "KeyC") {
      if (app.grid) app.toggleClick();
    } else {
      handled = false;
    }
    if (handled) e.preventDefault();
  }

  // A focused button would also "click" on Space keyup; swallow it.
  function onKeyup(e: KeyboardEvent) {
    if (e.code === "Space" && app.song && !isTyping(e.target) && !app.exportOpen && !app.helpOpen && !app.aboutOpen && !app.storageOpen) e.preventDefault();
  }
</script>

<svelte:window onkeydown={onKeydown} onkeyup={onKeyup} />

<div class="shell">
  <Header onhelp={() => app.openHelp()} />

  {#if app.error}
    <div class="error" role="alert">
      <span>{app.error}</span>
      <button class="btn ghost small" onclick={() => (app.error = null)}>Dismiss</button>
    </div>
  {/if}

  {#if app.song}
    <main>
      <SeparationBanner />
      <Waveform />
      <div class="mixer-scroll"><Mixer /></div>
      <Transport />
    </main>
  {:else}
    <EmptyState />
  {/if}

  {#if api.isMock}
    <div class="mock-badge" title="Running in a browser without Tauri: the backend is simulated">Mock backend</div>
  {/if}
</div>

{#if app.exportOpen}<ExportDialog />{/if}
{#if app.helpOpen}<ShortcutsHelp />{/if}
{#if app.aboutOpen}<AboutDialog />{/if}
{#if app.storageOpen}<StorageDialog />{/if}
{#if app.toast}<div class="toast" role="status">{app.toast}</div>{/if}

<style>
  .shell {
    height: 100%;
    display: flex;
    flex-direction: column;
    gap: var(--gap);
    padding: 14px 18px;
    max-width: 1400px;
    margin: 0 auto;
    position: relative;
  }
  main {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gap);
  }
  .mixer-scroll {
    flex: 0 1 auto;
    min-height: 120px;
    overflow: auto;
  }
  .error {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 6px 12px;
    border: 1px solid var(--danger);
    color: var(--danger);
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--danger) 8%, transparent);
  }
  .mock-badge {
    position: fixed;
    left: 8px;
    bottom: 6px;
    font-size: 10.5px;
    color: var(--faint);
    pointer-events: auto;
  }
  .toast {
    position: fixed;
    left: 50%;
    bottom: 24px;
    transform: translateX(-50%);
    background: var(--surface-3);
    border: 1px solid var(--border);
    padding: 8px 16px;
    border-radius: 20px;
    box-shadow: var(--shadow);
    z-index: 70;
  }
</style>
