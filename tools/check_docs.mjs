#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

// Checks the developer docs for the kind of staleness a script can see:
//
//   - every source directory has a README.md (REQUIRED below, plus every
//     crate under crates/);
//   - every relative markdown link points at an existing file;
//   - every repo path in backticks (a token with a slash whose first segment
//     exists, outside fenced code blocks) exists relative to the doc or the
//     repo root, unless git ignores it (local files such as
//     tests/fixtures/local/);
//   - no published file (anything outside private/ and CLAUDE.md, code
//     included) mentions private/, which doesn't exist in the public repo.
//
//   node tools/check_docs.mjs        exit 1 on any problem
//
// Run by `pnpm check` and CI. Docs scanned: every tracked or new .md file
// outside private/, node_modules and third_party/.

import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join, posix } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
process.chdir(ROOT);

const REQUIRED = [
  "app/src",
  "app/src-tauri",
  "tools",
  "tools/reference",
  "scripts",
  "spikes",
  "tests/fixtures",
];

const git = (args, input) =>
  execFileSync("git", args, { encoding: "utf8", input }).split("\n").filter(Boolean);

const problems = [];

// READMEs present.
const crates = readdirSync("crates", { withFileTypes: true })
  .filter((d) => d.isDirectory() && existsSync(join("crates", d.name, "Cargo.toml")))
  .map((d) => `crates/${d.name}`);
for (const dir of [...crates, ...REQUIRED]) {
  if (!existsSync(join(dir, "README.md"))) problems.push(`${dir}: missing README.md`);
}

// Paths in docs.
const docs = git(["ls-files", "--cached", "--others", "--exclude-standard", "*.md"])
  .filter((f) => !/^private\/|(^|\/)(node_modules|third_party)\//.test(f))
  .filter((f) => existsSync(f));

const LINK = /\[[^\]]*\]\(([^)\s]+)(?:\s+"[^"]*")?\)/g;
const CODE = /`([^`\n]+)`/g;
// A backticked token is treated as a repo path if it has a slash and none of
// the characters that mark placeholders, URLs, commands or globs.
const PATHLIKE = /^[\w.\-/]+$/;

const unchecked = []; // [doc, token, candidates] resolved against .gitignore later

for (const doc of docs) {
  const base = posix.dirname(doc);
  const text = readFileSync(doc, "utf8").replace(/^(```|~~~)[\s\S]*?^\1/gm, "");

  for (const [, raw] of text.matchAll(LINK)) {
    if (/^([a-z]+:|#)/i.test(raw)) continue;
    const target = decodeURIComponent(raw.replace(/[#?].*$/, ""));
    if (!target) continue;
    if (!existsSync(posix.join(base, target))) problems.push(`${doc}: broken link ${raw}`);
  }

  for (const [, token] of text.matchAll(CODE)) {
    const t = token.trim();
    if (!t.includes("/") || !PATHLIKE.test(t) || /^\/|\/\//.test(t)) continue;
    // Only tokens whose first segment exists are paths; this skips `n_fft/2`
    // and other slashes that aren't paths.
    const head = t.split("/")[0];
    const bases = [base, "."].filter((b) => head === ".." || existsSync(posix.join(b, head)));
    if (!bases.length) continue;
    const candidates = bases.map((b) => posix.join(b, t).replace(/\/$/, ""));
    if (!candidates.some((p) => existsSync(p))) unchecked.push([doc, t, candidates]);
  }
}

// Missing paths are fine when git ignores them (local or generated files).
if (unchecked.length) {
  let ignored = new Set();
  try {
    const all = unchecked.flatMap(([, , c]) => c);
    ignored = new Set(git(["check-ignore", "--no-index", "--stdin"], all.join("\n")));
  } catch {
    // check-ignore exits 1 when nothing is ignored.
  }
  for (const [doc, t, candidates] of unchecked) {
    if (!candidates.some((p) => ignored.has(p))) problems.push(`${doc}: no such path \`${t}\``);
  }
}

// Published files never point into private/. The two check scripts name it
// to exclude it.
try {
  const hits = git([
    "grep", "--untracked", "-I", "-n", "private/", "--",
    ":!private/", ":!CLAUDE.md", ":!tools/check_spdx.sh", ":!tools/check_docs.mjs",
  ]);
  for (const h of hits) problems.push(`mentions private/: ${h}`);
} catch {
  // git grep exits 1 when nothing matches.
}

if (problems.length) {
  console.error("Docs check failed:\n  " + problems.join("\n  "));
  process.exit(1);
}
console.log(`Docs: ok (${docs.length} files)`);
