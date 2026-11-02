<script lang="ts" module>
  // Inline SVG icons (24x24, stroke-based). Hand-drawn simple shapes.
  const PATHS: Record<string, string> = {
    play: '<path d="M7 4.5v15l12.5-7.5z" fill="currentColor" stroke="none"/>',
    pause: '<rect x="6" y="4.5" width="4" height="15" rx="1" fill="currentColor" stroke="none"/><rect x="14" y="4.5" width="4" height="15" rx="1" fill="currentColor" stroke="none"/>',
    back: '<path d="M6 5v14"/><path d="M19 5.5v13L9 12z" fill="currentColor"/>',
    loop: '<path d="M4 12a5 5 0 0 1 5-5h9"/><path d="M15 4l3 3-3 3"/><path d="M20 12a5 5 0 0 1-5 5H6"/><path d="M9 20l-3-3 3-3"/>',
    volume: '<path d="M4 9.5h3.5L12 5.5v13l-4.5-4H4z" fill="currentColor"/><path d="M15.5 9a4 4 0 0 1 0 6"/><path d="M18 6.5a7.5 7.5 0 0 1 0 11"/>',
    "volume-low": '<path d="M4 9.5h3.5L12 5.5v13l-4.5-4H4z" fill="currentColor"/><path d="M15.5 9a4 4 0 0 1 0 6"/>',
    "volume-mute": '<path d="M4 9.5h3.5L12 5.5v13l-4.5-4H4z" fill="currentColor"/><path d="M16 9.5l5 5M21 9.5l-5 5"/>',
    metronome: '<path d="M8 3h8l3.5 18h-15z"/><path d="M12 15l5-9"/><path d="M7 15h10"/>',
    countin: '<circle cx="5" cy="12" r="1.6" fill="currentColor"/><circle cx="10" cy="12" r="1.6" fill="currentColor"/><circle cx="15" cy="12" r="1.6" fill="currentColor"/><path d="M19 7v10"/>',
    open: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
    export: '<path d="M12 3v12"/><path d="M7 8l5-5 5 5"/><path d="M5 15v4a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-4"/>',
    chevron: '<path d="M6 9l6 6 6-6"/>',
    close: '<path d="M6 6l12 12M18 6L6 18"/>',
    minus: '<path d="M5 12h14"/>',
    plus: '<path d="M12 5v14M5 12h14"/>',
    music: '<path d="M9 18V5l11-2v13"/><circle cx="6.5" cy="18" r="2.5"/><circle cx="17.5" cy="16" r="2.5"/>',
    help: '<circle cx="12" cy="12" r="9"/><path d="M9.5 9.5a2.5 2.5 0 1 1 3.5 2.3c-.7.3-1 .9-1 1.7"/><circle cx="12" cy="17" r=".8" fill="currentColor"/>',
    keyboard: '<rect x="2.5" y="6" width="19" height="12" rx="2"/><path d="M6 10h1M10 10h1M14 10h1M18 10h0M7 14h10"/>',
    settings: '<circle cx="12" cy="12" r="3"/><path d="M12 2.5v3M12 18.5v3M2.5 12h3M18.5 12h3M5.3 5.3l2.1 2.1M16.6 16.6l2.1 2.1M5.3 18.7l2.1-2.1M16.6 7.4l2.1-2.1"/><circle cx="12" cy="12" r="6.5"/>',
    // stems
    vocals: '<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5.5 11a6.5 6.5 0 0 0 13 0"/><path d="M12 17.5V21M9 21h6"/>',
    guitar: '<path d="M19.5 4.5l-7 7"/><path d="M17.5 2.5l4 4"/><path d="M11.5 9.5c-2-1-4.5-.5-5.5 1.5-.5 1-1.5 1-2.5 1.5-2 1.5-1.5 5 .5 7s5.5 2.5 7 .5c.5-1 .5-2 1.5-2.5 2-1 2.5-3.5 1.5-5.5"/><circle cx="8.5" cy="15.5" r="1.3"/>',
    bass: '<path d="M20 4l-7.5 7.5"/><path d="M18 2l4 4"/><path d="M12 10c-1.5-1.5-4-1.5-5 0-.4.6-.4 1.4-1.2 1.8C3.5 13 3 16 5 18.5S10.5 21.5 12 19c.4-.8 1.2-.8 1.8-1.2 1.5-1 1.5-3.5 0-5"/><path d="M8 15l1 1"/>',
    drums: '<ellipse cx="12" cy="9" rx="8" ry="3"/><path d="M4 9v7c0 1.7 3.6 3 8 3s8-1.3 8-3V9"/><path d="M7 3l4 5M17 3l-4 5"/>',
    piano: '<rect x="3" y="4" width="18" height="16" rx="1.5"/><path d="M8 20v-6M12 20v-6M16 20v-6"/><path d="M7 4v10M11 4v10M15 4v10" stroke-width="2.6"/>',
    other: '<circle cx="12" cy="12" r="8.5"/><path d="M8 13.5c1 1.5 2.3 2 4 2s3-.5 4-2"/><circle cx="9" cy="9.8" r=".9" fill="currentColor"/><circle cx="15" cy="9.8" r=".9" fill="currentColor"/>',
  };
</script>

<script lang="ts">
  let { name, size = 18 }: { name: string; size?: number } = $props();
</script>

<svg
  class="icon"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.8"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  {@html PATHS[name] ?? ""}
</svg>
