<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Navneet Joneja -->

<script lang="ts">
  // The transport's icon-only Tuning button. Its look shows the song's tuning
  // state (measuring, in tune, couldn't measure, off pitch, matched); the
  // tooltip and label say it in words. Click or T opens the Tuning panel.
  import Icon from "./Icon.svelte";
  import { app } from "./store.svelte";
  import { formatCents, formatHz, tuningName } from "./tuning";

  type Look = "measuring" | "in-tune" | "unmeasured" | "off" | "matched";

  const look = $derived.by((): Look => (app.matched ? "matched" : app.tuningState));

  const text = $derived.by(() => {
    const t = app.tuning;
    switch (look) {
      case "measuring":
        return "Measuring tuning…";
      case "matched":
        return `Matched to ${tuningName(app.myTuning)}`;
      case "unmeasured":
        return t ? "Couldn't measure the tuning" : "Tuning is measured after separation";
      case "in-tune":
        return `Song is at A440 (${formatCents(t?.offset_cents ?? 0)})`;
      case "off": {
        const c = Math.round(t?.offset_cents ?? 0);
        const dir = c === 0 ? "at A440" : `${Math.abs(c)}¢ ${c < 0 ? "flat" : "sharp"}`;
        return `Song is ${dir} (A${formatHz(t?.reference_hz ?? 440)})${t?.drifts ? ", drifts" : ""}`;
      }
    }
  });
</script>

<button
  class="btn icon-only tuning {look}"
  aria-pressed={look === "matched" ? "true" : undefined}
  aria-label="Tuning: {text}"
  aria-keyshortcuts="T"
  title={text}
  disabled={!app.song}
  onclick={() => app.openTuning()}
>
  <Icon name="fork" size={16} />
  {#if look === "measuring"}
    <svg class="spin" width="28" height="28" viewBox="0 0 28 28" aria-hidden="true">
      <path d="M14 2 A12 12 0 0 1 26 14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
    </svg>
  {:else if look === "off"}
    <span class="badge" aria-hidden="true"></span>
  {/if}
</button>

<style>
  .tuning {
    position: relative;
    flex: none;
    color: var(--muted);
  }
  .tuning.off {
    color: var(--warn);
  }
  .tuning.matched {
    background: var(--accent-soft);
    border-color: var(--accent);
    color: var(--accent);
  }
  .spin {
    position: absolute;
    inset: -1px;
    color: var(--accent);
    animation: spin 1.1s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .spin {
      animation: none;
    }
  }
  .badge {
    position: absolute;
    top: 3px;
    right: 3px;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--warn);
    box-shadow: 0 0 0 2px var(--surface-2);
  }
</style>
