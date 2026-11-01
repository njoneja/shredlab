#!/usr/bin/env node
// Generates app/public/THIRD_PARTY_LICENSES.html: the third-party notices
// shipped inside ShredLab (shown by About > Third-party licences, and copied
// next to the executable by the installer).
//
//   node tools/third_party_licenses/generate.mjs           write the file
//   node tools/third_party_licenses/generate.mjs --check   exit 1 if stale
//
// Needs only Node, cargo (for `cargo metadata`, which also fetches crate
// sources into the cargo registry) and `pnpm install` in app/. The output is
// deterministic (no dates or local paths) and committed, so builds that
// don't touch dependencies never need to run this. `pnpm bundle` runs it
// first; CI runs --check.
//
// Rust crates: normal (non-dev, non-build) dependencies of the `shredlab`
// package, resolved for Windows and macOS targets and merged, so one file
// covers both platforms. Licence texts come from each crate's own LICENSE /
// COPYING / NOTICE files; crates without any fall back to the standard text
// in spdx/<ID>.txt. Everything that is not a crate (ONNX Runtime, models,
// fonts, vendored C++, JS) is listed in OTHER_COMPONENTS below, with its
// texts either vendored in texts/ or read from the repo.

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..", "..");
const OUT = join(ROOT, "app", "public", "THIRD_PARTY_LICENSES.html");
const APP_PACKAGE = "shredlab";
const TARGETS = ["x86_64-pc-windows-msvc", "aarch64-apple-darwin", "x86_64-apple-darwin"];
// Placeholder until a public repository exists.
const SOURCE_URL = "https://github.com/njoneja/shredlab";

const read = (p) => readFileSync(p, "utf8").replace(/^﻿/, "").replace(/\r\n?/g, "\n").trimEnd() + "\n";
const rel = (p) => relative(ROOT, p).replace(/\\/g, "/");
const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const hash = (s) => createHash("sha256").update(s.replace(/\s+/g, " ").trim()).digest("hex").slice(0, 10);

// ---------------------------------------------------------------- crates

function cargoMetadata(target) {
  const out = execFileSync(
    "cargo",
    ["metadata", "--format-version", "1", "--locked", "--filter-platform", target, "--manifest-path", join(ROOT, "Cargo.toml")],
    { encoding: "utf8", maxBuffer: 256 * 1024 * 1024 },
  );
  return JSON.parse(out);
}

function collectCrates() {
  const crates = new Map(); // "name@version" -> package
  for (const target of TARGETS) {
    const meta = cargoMetadata(target);
    const pkgs = new Map(meta.packages.map((p) => [p.id, p]));
    const nodes = new Map(meta.resolve.nodes.map((n) => [n.id, n]));
    const members = new Set(meta.workspace_members);
    const root = meta.packages.find((p) => p.name === APP_PACKAGE && members.has(p.id));
    if (!root) throw new Error(`package ${APP_PACKAGE} not found`);
    const seen = new Set();
    const stack = [root.id];
    while (stack.length) {
      const id = stack.pop();
      if (seen.has(id)) continue;
      seen.add(id);
      const pkg = pkgs.get(id);
      if (!members.has(id)) crates.set(`${pkg.name}@${pkg.version}`, pkg);
      for (const dep of nodes.get(id).deps) {
        if (dep.dep_kinds.some((k) => k.kind === null)) stack.push(dep.pkg);
      }
    }
  }
  return [...crates.values()].sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));
}

const LICENSE_FILE = /^(licen[cs]e|copying|copyright|notice|unlicense|ofl)([-._].*)?$/i;

function licenseFiles(dir) {
  return readdirSync(dir)
    .filter((f) => LICENSE_FILE.test(f) && !/\.(rs|toml|json|html?)$/i.test(f) && statSync(join(dir, f)).isFile())
    .sort();
}

// "MIT/Apache-2.0" (old cargo syntax) -> "MIT OR Apache-2.0".
// Pure OR lists are sorted so "Apache-2.0 OR MIT" and "MIT OR Apache-2.0" group together.
const normExpr = (e) => {
  const x = (e ?? "").replace(/\s*\/\s*/g, " OR ").trim();
  return /[()]|AND/.test(x) ? x : x.split(" OR ").sort().join(" OR ");
};
const spdxIds = (e) => [...new Set(e.match(/[A-Za-z0-9.+-]+/g).filter((t) => !/^(OR|AND|WITH)$/.test(t)))];

// ---------------------------------------------------------------- texts

const texts = new Map(); // hash -> { id, title, body, users: [] }
function addText(title, body, user) {
  const h = hash(body);
  if (!texts.has(h)) texts.set(h, { id: `t-${h}`, title, body, users: [] });
  const t = texts.get(h);
  if (!t.users.includes(user)) t.users.push(user);
  return t;
}

function spdxText(id) {
  const p = join(HERE, "spdx", `${id}.txt`);
  return existsSync(p) ? read(p) : null;
}

// ---------------------------------------------------------------- others

const fonts = join(ROOT, "app", "src", "assets", "fonts");
const lameDir = (crates) => {
  const sys = crates.find((c) => c.name === "mp3lame-sys");
  if (!sys) throw new Error("mp3lame-sys not in the dependency graph; update OTHER_COMPONENTS");
  const dir = readdirSync(dirname(sys.manifest_path)).find((d) => /^lame-/.test(d));
  return { version: dir.replace(/^lame-/, ""), copying: join(dirname(sys.manifest_path), dir, "COPYING"), sys };
};
const nodePkg = (name) => {
  const dir = join(ROOT, "app", "node_modules", ...name.split("/"));
  if (!existsSync(dir)) throw new Error(`app/node_modules/${name} missing: run pnpm install in app/`);
  return { dir, version: JSON.parse(readFileSync(join(dir, "package.json"), "utf8")).version };
};

function otherComponents(crates) {
  const lame = lameDir(crates);
  const tauriApi = nodePkg("@tauri-apps/api");
  const svelte = nodePkg("svelte");
  return [
    {
      name: "ONNX Runtime",
      what: "Neural network inference (statically linked through the ort crate; prebuilt by pyke.io).",
      license: "MIT",
      notes:
        "Copyright (c) Microsoft Corporation. Licence and third-party notices below are from the official " +
        "onnxruntime 1.24.4 release (vendored in tools/third_party_licenses/texts/). " +
        'Source: <a href="https://github.com/microsoft/onnxruntime">github.com/microsoft/onnxruntime</a>.',
      texts: [
        ["ONNX Runtime licence", join(HERE, "texts", "onnxruntime-LICENSE.txt")],
        ["ONNX Runtime third-party notices", join(HERE, "texts", "onnxruntime-ThirdPartyNotices.txt")],
      ],
    },
    {
      name: "Microsoft DirectML (Windows only)",
      what: "GPU acceleration on Windows. Shipped unmodified as DirectML.dll from the Microsoft.AI.DirectML 1.15.4 NuGet package.",
      license: "Microsoft Software License Terms for DirectML (proprietary, redistributable)",
      notes:
        "DirectML.dll is not covered by the GPL; it is distributed alongside ShredLab under Microsoft's own terms " +
        "(see the additional permission in ShredLab's licence above). The licence terms are published with the " +
        '<a href="https://www.nuget.org/packages/Microsoft.AI.DirectML">Microsoft.AI.DirectML package</a> ' +
        '(<a href="https://github.com/microsoft/DirectML">github.com/microsoft/DirectML</a>). ' +
        "Copyright (c) Microsoft Corporation.",
      texts: existsSync(join(HERE, "texts", "DirectML-LICENSE.txt"))
        ? [
            ["DirectML licence", join(HERE, "texts", "DirectML-LICENSE.txt")],
            ["DirectML third-party notices", join(HERE, "texts", "DirectML-ThirdPartyNotices.txt")],
          ]
        : [],
    },
    {
      name: `LAME ${lame.version}`,
      what: "MP3 encoding. Compiled from source and statically linked through the mp3lame-sys crate.",
      license: "LGPL-2.0-or-later",
      notes:
        "LAME is unmodified. Its complete source is included in the " +
        `<a href="https://crates.io/crates/mp3lame-sys/${lame.sys.version}">mp3lame-sys ${lame.sys.version}</a> crate ` +
        '(and at <a href="https://lame.sourceforge.io/">lame.sourceforge.io</a>). Because ShredLab\'s complete source ' +
        "is available under the GPL, you can rebuild and relink it against a modified LAME.",
      texts: [["GNU Library General Public License, version 2 (LAME COPYING)", lame.copying]],
    },
    {
      name: "Signalsmith Stretch 1.3.1",
      what: "Time stretching and pitch shifting (vendored C++).",
      license: "MIT",
      notes: 'Copyright (c) 2022 Geraint Luff / Signalsmith Audio Ltd. <a href="https://github.com/Signalsmith-Audio/signalsmith-stretch">Source</a>.',
      texts: [["Signalsmith Stretch licence", join(ROOT, "crates", "stemx-audio", "third_party", "signalsmith-stretch", "LICENSE.txt")]],
    },
    {
      name: "Signalsmith Linear",
      what: "FFT used by Signalsmith Stretch (vendored C++).",
      license: "MIT",
      notes: '<a href="https://github.com/Signalsmith-Audio/linear">Source</a>.',
      texts: [["Signalsmith Linear licence", join(ROOT, "crates", "stemx-audio", "third_party", "signalsmith-linear", "LICENSE.txt")]],
    },
    {
      name: "Demucs v4 (htdemucs_6s model weights)",
      what: "Stem separation model, converted to ONNX and bundled.",
      license: "MIT",
      notes: 'Copyright (c) Meta Platforms, Inc. and affiliates. <a href="https://github.com/facebookresearch/demucs">Source</a>.',
      texts: [["Demucs licence", join(HERE, "texts", "demucs-LICENSE.txt")]],
    },
    {
      name: "Beat This! (final0 model weights)",
      what: "Beat and downbeat tracking model, converted to ONNX and bundled.",
      license: "MIT",
      notes:
        "Copyright (c) 2024 Institute of Computational Perception, JKU Linz, Austria. " +
        '<a href="https://github.com/CPJKU/beat_this">Source</a>.',
      texts: [["Beat This! licence", join(HERE, "texts", "beat_this-LICENSE.txt")]],
    },
    {
      name: "Rock Salt and Permanent Marker fonts",
      what: "Wordmark and stem labels (subset woff2).",
      license: "Apache-2.0",
      notes: "Copyright (c) 2010 by Font Diner, Inc DBA Sideshow.",
      texts: [["Apache License 2.0", join(fonts, "LICENSE-Apache-2.0.txt")]],
    },
    {
      name: "Noto Music font",
      what: "Clefs in the wordmark and app icon (subset woff2).",
      license: "OFL-1.1",
      notes: "Copyright 2022 The Noto Project Authors.",
      texts: [["SIL Open Font License 1.1", join(fonts, "OFL-NotoMusic.txt")]],
    },
    {
      name: `@tauri-apps/api ${tauriApi.version}`,
      what: "JavaScript bridge to the Tauri backend (bundled in the UI).",
      license: "Apache-2.0 OR MIT",
      notes: "Copyright (c) 2017 - Present Tauri Apps Contributors.",
      texts: [
        ["MIT licence (@tauri-apps/api)", join(tauriApi.dir, "LICENSE-MIT")],
        ["Apache License 2.0 (@tauri-apps/api)", join(tauriApi.dir, "LICENSE-APACHE-2.0")],
      ],
    },
    {
      name: `Svelte ${svelte.version}`,
      what: "UI framework runtime (compiled into the UI).",
      license: "MIT",
      notes: "",
      texts: [["Svelte licence", join(svelte.dir, "LICENSE.md")]],
    },
  ];
}

// ---------------------------------------------------------------- render

const CSS = `
:root{color-scheme:light dark;--bg:#fbfaf7;--fg:#1d1c1a;--muted:#5f5b54;--line:#ddd8cf;--card:#fff;--link:#8a3b12;--code:#f2efe9}
@media (prefers-color-scheme:dark){:root{--bg:#16151a;--fg:#ece9e3;--muted:#a9a49b;--line:#34323a;--card:#1e1d23;--link:#f0a070;--code:#24232a}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.55 system-ui,-apple-system,"Segoe UI",sans-serif}
main{max-width:920px;margin:0 auto;padding:24px 16px 64px}
h1{font-size:1.6rem;margin:0 0 .25rem}h2{font-size:1.25rem;margin:2.2rem 0 .6rem;border-bottom:1px solid var(--line);padding-bottom:.3rem}
h3{font-size:1.05rem;margin:1.4rem 0 .4rem}
a{color:var(--link)}p,li{max-width:70ch}.muted{color:var(--muted)}
nav ul{padding-left:1.2rem}
table{border-collapse:collapse;width:100%;font-size:.92rem}
th,td{text-align:left;padding:.3rem .5rem;border-bottom:1px solid var(--line);vertical-align:top}
.card{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:.8rem 1rem;margin:.8rem 0}
.card h3{margin-top:0}
details{background:var(--card);border:1px solid var(--line);border-radius:8px;margin:.5rem 0}
summary{cursor:pointer;padding:.5rem .8rem;font-weight:600}
details .users{padding:0 .8rem;color:var(--muted);font-size:.88rem}
pre{margin:0;padding:.8rem;background:var(--code);border-top:1px solid var(--line);white-space:pre-wrap;word-break:break-word;font:12.5px/1.45 ui-monospace,Consolas,monospace;max-height:60vh;overflow:auto}
`;

function render(crates, others) {
  // Crate texts.
  const missing = [];
  const crateTexts = new Map(); // crate key -> [text]
  for (const c of crates) {
    const dir = dirname(c.manifest_path);
    const key = `${c.name} ${c.version}`;
    const files = licenseFiles(dir);
    const list = files.map((f) => addText(`${f} (${c.name})`, read(join(dir, f)), key));
    if (!list.length) {
      for (const id of spdxIds(normExpr(c.license))) {
        const holders = (c.authors ?? []).map((a) => a.replace(/\s*<[^>]*>/, "").trim()).filter(Boolean);
        const copyright = `Copyright (c) ${holders.length ? holders.join(", ") : `the ${c.name} authors`}`;
        const body = spdxText(id)?.replace("{{COPYRIGHT}}", copyright);
        if (body) list.push(addText(`${id} (standard text)`, body, key));
        else missing.push(`${key}: ${id}`);
      }
    }
    crateTexts.set(key, list);
  }
  if (missing.length) {
    throw new Error(`no licence text for:\n  ${missing.join("\n  ")}\nadd tools/third_party_licenses/spdx/<ID>.txt`);
  }

  // Group crates by licence expression.
  const groups = new Map();
  for (const c of crates) {
    const e = normExpr(c.license) || "(unspecified)";
    if (!groups.has(e)) groups.set(e, []);
    groups.get(e).push(c);
  }
  const groupList = [...groups.entries()].sort((a, b) => b[1].length - a[1].length || a[0].localeCompare(b[0]));

  // Other components' texts (registered after crates; dedupe works across both).
  const otherTexts = others.map((o) => o.texts.map(([title, p]) => addText(title, read(p), o.name)));

  const h = [];
  h.push(`<!doctype html><html lang="en"><head><meta charset="utf-8">`);
  h.push(`<meta name="viewport" content="width=device-width,initial-scale=1">`);
  h.push(`<title>ShredLab third-party licences</title><style>${CSS}</style></head><body><main>`);
  h.push(`<h1>ShredLab: licences and notices</h1>`);
  h.push(`<p class="muted">Generated by tools/third_party_licenses/generate.mjs. Do not edit by hand.</p>`);
  h.push(`<nav><ul><li><a href="#shredlab">ShredLab</a></li><li><a href="#components">Bundled components</a></li>`);
  h.push(`<li><a href="#crates">Rust crates (${crates.length})</a></li><li><a href="#texts">Licence texts</a></li></ul></nav>`);

  h.push(`<h2 id="shredlab">ShredLab</h2>`);
  h.push(`<p>Copyright (C) 2026 Navneet Joneja.</p>`);
  h.push(
    `<p>ShredLab is free software: you can redistribute it and/or modify it under the terms of the GNU General Public ` +
      `License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any ` +
      `later version, with an additional permission for combining it with Microsoft DirectML. ShredLab is distributed ` +
      `in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY ` +
      `or FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for more details.</p>`,
  );
  h.push(`<p>Source code: <a href="${SOURCE_URL}">${SOURCE_URL}</a></p>`);
  h.push(`<p>${esc(read(join(ROOT, "TRADEMARKS.md")).replace(/^# .*\n+/, "").replace(/\n(?!\n)/g, " ").trim()).replace(/\n\n/g, "</p><p>")}</p>`);
  h.push(`<details><summary>Additional permission (DirectML)</summary><pre>${esc(read(join(ROOT, "LICENSE-EXCEPTION")))}</pre></details>`);
  h.push(`<details><summary>GNU General Public License, version 3</summary><pre>${esc(read(join(ROOT, "LICENSE")))}</pre></details>`);

  h.push(`<h2 id="components">Bundled components</h2>`);
  others.forEach((o, i) => {
    h.push(`<div class="card"><h3>${esc(o.name)}</h3><p>${esc(o.what)}</p><p><strong>Licence:</strong> ${esc(o.license)}</p>`);
    if (o.notes) h.push(`<p>${o.notes}</p>`);
    if (otherTexts[i].length) {
      h.push(`<p>Texts: ${otherTexts[i].map((t) => `<a href="#${t.id}">${esc(t.title)}</a>`).join(", ")}</p>`);
    }
    h.push(`</div>`);
  });

  h.push(`<h2 id="crates">Rust crates</h2>`);
  h.push(
    `<p>ShredLab is built from the following crates from <a href="https://crates.io">crates.io</a> (Windows and macOS builds ` +
      `combined), grouped by licence. Each crate's source code is available from its linked crates.io page; this ` +
      `includes the crates under MPL-2.0, which are used unmodified.</p>`,
  );
  for (const [expr, list] of groupList) {
    h.push(`<h3>${esc(expr)} (${list.length})</h3><table><thead><tr><th>Crate</th><th>Licence texts</th></tr></thead><tbody>`);
    for (const c of list) {
      const key = `${c.name} ${c.version}`;
      const links = crateTexts.get(key).map((t) => `<a href="#${t.id}">${esc(t.title.replace(/ \(.*\)$/, ""))}</a>`);
      h.push(
        `<tr><td><a href="https://crates.io/crates/${esc(c.name)}/${esc(c.version)}">${esc(key)}</a></td><td>${links.join(", ")}</td></tr>`,
      );
    }
    h.push(`</tbody></table>`);
  }

  h.push(`<h2 id="texts">Licence texts</h2>`);
  h.push(`<p class="muted">Identical texts are shown once, with the components that use them.</p>`);
  for (const t of texts.values()) {
    h.push(
      `<details id="${t.id}"><summary>${esc(t.title)}</summary><p class="users">Used by: ${esc(t.users.join(", "))}</p><pre>${esc(t.body)}</pre></details>`,
    );
  }
  // Opening a #t-... link should expand that text.
  h.push(
    `<script>function o(){var d=location.hash&&document.getElementById(location.hash.slice(1));if(d&&d.tagName==="DETAILS")d.open=true}addEventListener("hashchange",o);o()</script>`,
  );
  h.push(`</main></body></html>\n`);
  return h.join("\n");
}

// ---------------------------------------------------------------- main

const crates = collectCrates();
const html = render(crates, otherComponents(crates));
if (process.argv.includes("--check")) {
  const current = existsSync(OUT) ? read(OUT) : "";
  if (current !== html) {
    console.error(`${rel(OUT)} is out of date: run node tools/third_party_licenses/generate.mjs`);
    process.exit(1);
  }
  console.log(`${rel(OUT)} is up to date (${crates.length} crates).`);
} else {
  writeFileSync(OUT, html);
  console.log(`wrote ${rel(OUT)}: ${crates.length} crates, ${texts.size} unique texts, ${(html.length / 1024).toFixed(0)} KiB`);
}
