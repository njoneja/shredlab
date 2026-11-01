<script lang="ts">
  import Icon from "./Icon.svelte";
  import { app } from "./store.svelte";
  import { formatTime } from "./meter";
  import Wordmark from "./Wordmark.svelte";
  import { ago, recentStatus } from "./recent";

  let dragOver = $state(false);
</script>

<div class="empty">
  <div
    class="drop"
    class:over={dragOver}
    role="region"
    aria-label="Drop zone"
    ondragenter={() => (dragOver = true)}
    ondragleave={() => (dragOver = false)}
    ondrop={() => (dragOver = false)}
  >
    <div class="mark"><Wordmark size={44} /></div>
    <p class="tagline muted">Split songs into stems to play along or transcribe</p>
    <h2>Open a song to get started</h2>
    <p class="muted">Drop an audio file here (MP3, WAV, FLAC, M4A, OGG) or choose one.</p>
    <button class="btn primary" onclick={() => app.pickAndOpen()} disabled={app.opening}>
      <Icon name="open" /> Open…
    </button>
    {#if app.opening}<p class="muted small">Opening…</p>{/if}
  </div>

  {#if app.library.length}
    <div class="recent">
      <h3>Recent songs</h3>
      <ul>
        {#each app.library as e (e.song_id)}
          <li>
            <button onclick={() => app.openPath(e.path)} title={e.path} disabled={app.opening}>
              <span class="t">{e.title}</span>
              <span class="muted a">{e.artist ?? ""}</span>
              <span class="badge" class:ready={e.stems_cached}>{recentStatus(e)}</span>
              <span class="muted mono d">{formatTime(e.duration_sec)}</span>
              <span class="muted w">{ago(e.last_opened_ms)}</span>
            </button>
            <button
              class="x"
              aria-label={`Remove ${e.title} from recent`}
              title="Remove from recent (stems are kept)"
              onclick={() => app.hideRecent(e.song_id)}
            >
              <Icon name="close" size={12} />
            </button>
          </li>
        {/each}
      </ul>
    </div>
  {/if}
</div>

<style>
  .empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 20px;
    max-width: 760px;
    width: 100%;
    margin: 0 auto;
    padding-top: 4vh;
    min-height: 0;
  }
  .drop {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    padding: 40px 24px;
    border: 2px dashed var(--border);
    border-radius: 14px;
    background: var(--surface);
    text-align: center;
    transition: border-color 0.12s, background 0.12s;
  }
  .drop.over {
    border-color: var(--accent);
    background: var(--accent-soft);
  }
  /* The treble clef hangs below the baseline; leave room for it. */
  .mark {
    padding: 4px 0 18px;
  }
  .drop p.tagline {
    margin: 0 0 14px;
    font-size: 13px;
  }
  h2 {
    margin: 4px 0 0;
    font-size: 20px;
  }
  .drop p {
    margin: 0 0 8px;
  }
  .small {
    font-size: 12px;
  }
  .recent {
    min-height: 0;
    overflow: auto;
  }
  h3 {
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted);
    margin: 0 0 6px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  li {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .x {
    flex: none;
    width: 26px;
    height: 26px;
    display: grid;
    place-items: center;
    padding: 0;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--muted);
    opacity: 0;
  }
  li:hover .x,
  li:focus-within .x {
    opacity: 1;
  }
  li .x:hover {
    background: var(--surface-2);
    color: var(--fg);
  }
  li button:not(.x) {
    flex: 1;
    min-width: 0;
    display: grid;
    grid-template-columns: minmax(0, 2fr) minmax(0, 1.3fr) auto 48px 84px;
    gap: 12px;
    align-items: center;
    text-align: left;
    background: none;
    border: 0;
    padding: 8px 10px;
    border-radius: var(--radius-sm);
  }
  li button:not(.x):hover:not(:disabled) {
    background: var(--surface-2);
  }
  .t {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .a {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .d,
  .w {
    text-align: right;
    font-size: 12px;
  }
  .badge {
    font-size: 11px;
    padding: 1px 8px;
    border-radius: 10px;
    border: 1px solid var(--border);
    color: var(--muted);
  }
  .badge.ready {
    border-color: var(--ok);
    color: var(--ok);
  }
</style>
