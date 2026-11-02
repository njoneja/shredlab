// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

export default {
  // Enables <script lang="ts"> and other preprocessing in .svelte files.
  preprocess: vitePreprocess(),
};
