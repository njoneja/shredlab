<script lang="ts">
  import { onMount, tick } from "svelte";
  import Icon from "./Icon.svelte";
  import { app, type HelpSection } from "./store.svelte";

  // Help & shortcuts. Opened from the header (app.openHelp()) or, scrolled to one section, from
  // other panels (app.openHelp("click") in MeterPanel). Public long form: docs/HELP.md.

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
    [["Shift", "1–6"], "Solo stem on / off"],
    [["Alt", "1–6"], "All but this stem (play along)"],
  ];

  const NAV: { id: HelpSection; label: string }[] = [
    { id: "practice", label: "Practice a part" },
    { id: "play-along", label: "Play along" },
    { id: "click", label: "When the click is off" },
    { id: "keys", label: "Shortcuts" },
  ];

  let body: HTMLDivElement;
  let closeBtn: HTMLButtonElement;
  let flash = $state<HelpSection | null>(null);

  function show(id: HelpSection) {
    const h = body.querySelector<HTMLElement>(`#help-${id}`);
    if (!h) return;
    body.scrollTo({ top: h.offsetTop - 4 });
    h.focus({ preventScroll: true });
    flash = id;
    setTimeout(() => flash === id && (flash = null), 1200);
  }

  onMount(async () => {
    await tick();
    if (app.helpSection) show(app.helpSection);
    else closeBtn.focus();
  });
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && app.closeHelp()} />

<div class="backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && app.closeHelp()}>
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="help-title">
    <div class="head">
      <h2 id="help-title">Help &amp; shortcuts</h2>
      <button class="btn ghost small icon-only" bind:this={closeBtn} onclick={() => app.closeHelp()} aria-label="Close"><Icon name="close" size={14} /></button>
    </div>
    <nav aria-label="Help sections">
      {#each NAV as n (n.id)}
        <button class="chip" onclick={() => show(n.id)}>{n.label}</button>
      {/each}
    </nav>

    <div class="body" bind:this={body}>
      <div class="guide">
        <section>
          <h3 id="help-practice" tabindex="-1" class:flash={flash === "practice"}>Practice a part</h3>
          <ul>
            <li><b>Loop:</b> drag across the waveform to loop a section, or press <kbd>[</kbd> and <kbd>]</kbd> to set A and B at the playhead.</li>
            <li><b>Speed</b> slows down without changing the key. <b>Pitch</b> moves the key in semitones (fine-tune in cents) without changing the speed.</li>
            <li><b>Count-in</b> length is in bars. For a pickup, set A on the pickup note and use a fractional count-in, e.g. 1¾ bars.</li>
            <li><b>Count in to where the music starts:</b> playing from the top skips a quiet intro (studio talk, silence) and counts you into the music. Turn it off in Settings.</li>
          </ul>
        </section>

        <section>
          <h3 id="help-play-along" tabindex="-1" class:flash={flash === "play-along"}>Play along</h3>
          <ul>
            <li><b>⊘</b> on a stem row plays everything except that stem, so you play the missing part.</li>
            <li><b>S</b> solos a stem to hear just that part. <b>All on</b> brings every stem back and clears solo.</li>
            <li><b>Export what you hear</b> writes exactly what plays: the same stems and volumes, speed and pitch, plus the click and count-in when they're on.</li>
          </ul>
        </section>

        <section>
          <h3 id="help-click" tabindex="-1" class:flash={flash === "click"}>When the click is off</h3>
          <p>The click follows the detected beats. To fix it, open the <b>beats per bar</b> menu in the transport (fixes are saved per song):</p>
          <ul>
            <li><b>This is beat 1:</b> put the playhead on a downbeat and press it to move the bar lines there.</li>
            <li><b>Beats per bar from bar…:</b> for a meter change, or a song detected in the wrong meter.</li>
            <li><b>Click subdivision:</b> 2 or 3 clicks per beat (3 for a 6/8 or swing feel).</li>
          </ul>
          <p class="muted">Detection is good but not perfect:</p>
          <ul class="muted">
            <li>Odd meters (5/4, 7/4) often get the beats right but put the downbeats in the wrong place. Use This is beat 1, then set the beats per bar.</li>
            <li>Additive meters (7/8 = 2+2+3) may track as an even pulse.</li>
            <li>3/4 and 6/8 can't be told apart; 6/8 usually shows as 2 beats per bar. Use subdivision 3.</li>
          </ul>
        </section>

        <section>
          <h3>Your songs</h3>
          <ul>
            <li><b>✕</b> on a recent song removes it from the list, after asking. Its stems are kept unless you tick <b>Also delete its stems and settings</b>: open the file again and it's back, instantly.</li>
            <li><b>Settings → Manage storage…</b> shows how much space each song's stems take. <b>Delete stems</b> frees it; opening the song again means separating it again. The open song can't be deleted.</li>
          </ul>
        </section>
      </div>

      <section class="keys">
        <h3 id="help-keys" tabindex="-1" class:flash={flash === "keys"}>Keyboard shortcuts</h3>
        <dl>
          {#each KEYS as [keys, what] (what)}
            <dt>{#each keys as k, i (i)}{#if i}<span class="plus">+</span>{/if}<kbd>{k}</kbd>{/each}</dt>
            <dd>{what}</dd>
          {/each}
        </dl>
        <p class="muted">Stems are numbered top to bottom: 1 vocals, 2 guitar, 3 bass, 4 drums, 5 piano / keys, 6 other.</p>
      </section>
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
    width: 780px;
    max-width: 100%;
    max-height: min(640px, calc(100vh - 40px));
    display: flex;
    flex-direction: column;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 16px 18px;
    box-shadow: var(--shadow);
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
  nav {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-bottom: 10px;
  }
  .chip {
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--muted);
    border-radius: 999px;
    padding: 2px 10px;
    font-size: 12px;
  }
  .chip:hover {
    color: var(--fg);
  }
  .body {
    position: relative;
    overflow-y: auto;
    display: grid;
    grid-template-columns: 1fr 280px;
    gap: 4px 24px;
    padding-right: 4px;
  }
  @media (max-width: 760px) {
    .body {
      grid-template-columns: 1fr;
    }
  }
  .keys {
    align-self: start;
    position: sticky;
    top: 0;
  }
  @media (max-width: 760px) {
    .keys {
      position: static;
    }
  }
  section {
    margin-bottom: 12px;
  }
  h3 {
    margin: 0 0 6px;
    font-size: 13.5px;
    border-radius: 4px;
    transition: background 0.4s;
  }
  h3:focus {
    outline: none;
  }
  h3:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  h3.flash {
    background: var(--accent-soft);
  }
  ul {
    margin: 0 0 6px;
    padding-left: 18px;
    font-size: 12.5px;
    line-height: 1.45;
  }
  li + li {
    margin-top: 3px;
  }
  .guide p {
    margin: 0 0 4px;
    font-size: 12.5px;
    line-height: 1.45;
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px 14px;
    margin: 0 0 10px;
    align-items: center;
    font-size: 12.5px;
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
  .keys p {
    font-size: 12px;
    margin: 0;
  }
</style>
