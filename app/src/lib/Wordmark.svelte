<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Navneet Joneja -->

<script lang="ts">
  // The ShredLab wordmark (from the brand guide): built from text so it
  // takes the theme accent. Written "Shredlab" (lower-case l); the clefs
  // stand in for the S and the D. Screen readers read "ShredLab".
  let { size = 20 }: { size?: number } = $props();
</script>

<span class="wordmark" style:font-size="{size}px" role="img" aria-label="ShredLab"
  ><span class="clef treble" aria-hidden="true">𝄞</span><span aria-hidden="true">hre</span><span class="clef bass" aria-hidden="true"
    >𝄢</span
  ><span aria-hidden="true">lab</span></span
>
