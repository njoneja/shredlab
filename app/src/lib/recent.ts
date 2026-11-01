// Shared by the start screen's recent list, the header's Open menu and the storage dialogs.
import type { LibraryEntry } from "./types";

export function ago(ms: number) {
  const d = Date.now() - ms;
  const day = 86400000;
  if (d < 60000) return "just now";
  if (d < 3600000) return `${Math.floor(d / 60000)} min ago`;
  if (d < day) return `${Math.floor(d / 3600000)} h ago`;
  if (d < 2 * day) return "yesterday";
  return `${Math.floor(d / day)} days ago`;
}

export function recentStatus(e: LibraryEntry) {
  return e.stems_cached ? "Stems ready" : "Not separated";
}

export function formatBytes(n: number) {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
}
