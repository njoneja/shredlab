<script lang="ts">
  import { onMount } from "svelte";
  import { hello, type Hello } from "./lib/api";

  let info = $state<Hello | null>(null);
  let error = $state<string | null>(null);

  onMount(async () => {
    try {
      info = await hello();
    } catch (e) {
      error = String(e);
    }
  });
</script>

<main>
  <h1>Stem Extractor</h1>

  {#if info}
    <p class="hello" data-testid="hello">{info.message}</p>
    <p class="muted">App version {info.app_version}</p>
    <ul class="muted">
      {#each info.crates as c (c.name)}
        <li>{c.name} {c.version}</li>
      {/each}
    </ul>
  {:else if error}
    <p class="error">Could not reach the Rust core: {error}</p>
  {:else}
    <p class="muted">Connecting to the Rust core…</p>
  {/if}
</main>

<style>
  main {
    max-width: 40rem;
    margin: 0 auto;
    padding: 3rem 1.5rem;
  }
  h1 {
    margin: 0 0 1rem;
    font-size: 2rem;
  }
  .hello {
    font-size: 1.25rem;
  }
  .muted {
    color: var(--muted);
  }
  ul {
    padding-left: 1.25rem;
    font-family: ui-monospace, monospace;
    font-size: 0.875rem;
  }
  .error {
    color: var(--error);
  }
</style>
