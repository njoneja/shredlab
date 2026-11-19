// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

import { test } from "node:test";
import assert from "node:assert/strict";
import { chordFrame } from "../src/lib/chords.ts";
import {
  bendLabel,
  bendWords,
  firstNoteFrom,
  formatHeard,
  heardViews,
  loopBetween,
  nameIn,
  noteIndexAt,
  noteViews,
  octaveName,
  panelNoteSettings,
  parseHeard,
  pickNotesStem,
  refinedSide,
  rollRange,
  sameSpan,
  withLoopNotes,
} from "../src/lib/notes.ts";
import type { StemName, StemNotes } from "../src/lib/types.ts";

// A lick at concert pitch in Eb (a song in Eb standard): Eb4, F#4 (hammer-on), Ab4 bent a whole step, a slide to F#4.
const LICK: StemNotes = {
  stem: "guitar",
  source: "htdemucs_6s_s1|st0",
  notes: [
    { start_s: 0, end_s: 0.5, midi: 63 },
    { start_s: 0.52, end_s: 1, midi: 66, attack: "legato" },
    { start_s: 1, end_s: 2, midi: 68, bend: { start_s: 1.2, peak_s: 1.4, semitones: 2 } },
    { start_s: 2.05, end_s: 2.5, midi: 66, attack: "slide" },
    { start_s: 3, end_s: 3.5, midi: 63, attack: "legato" },
  ],
  chordal: [[4, 6]],
};
const EB_KEY = { tonic: 3, minor: false };

test("names follow the chord frame: shape on top, what you hear below", () => {
  // Guitar tuned to Eb, song in Eb, no shift: E shapes, Eb heard.
  const f = chordFrame({ songSemitones: -1, confirmed: true, offsetCents: 0, shiftCents: 0, mySemitones: -1 });
  const v = noteViews(LICK, f, EB_KEY);
  assert.deepEqual(
    v.map((n) => [n.main, n.second]),
    [
      ["E", "Eb"],
      ["G", "Gb"],
      ["A", "Ab"],
      ["G", "Gb"],
      ["E", "Eb"],
    ],
  );
  assert.equal(v[0].octave, "E4");
  // Standard guitar (or My tuning unknown): what you hear, one name.
  const heard = noteViews(LICK, chordFrame({ songSemitones: -1, confirmed: false, offsetCents: 0, shiftCents: 0 }), EB_KEY);
  assert.deepEqual(heard.map((n) => n.second), [null, null, null, null, null]);
  assert.equal(heard[0].main, "Eb");
  // Shifted up a half step to standard: shape and heard agree again.
  const shifted = noteViews(LICK, chordFrame({ songSemitones: -1, confirmed: true, offsetCents: 0, shiftCents: 100 }), EB_KEY);
  assert.equal(shifted[0].main, "E");
  assert.equal(shifted[0].second, null);
  // The bend target, named in the shape frame.
  assert.equal(nameIn(68 + 2, f, EB_KEY), "B");
  assert.equal(octaveName(40, "common"), "E2");
  assert.deepEqual(noteViews(null, f, EB_KEY), []);
});

test("attacks join the note before only without a gap", () => {
  const v = noteViews(LICK, chordFrame({ songSemitones: 0, confirmed: true, offsetCents: 0, shiftCents: 0 }), null);
  assert.deepEqual(
    v.map((n) => [n.attack, n.joinsPrev]),
    [
      ["picked", false],
      ["legato", true],
      ["picked", false],
      ["slide", true],
      ["legato", false],
    ],
  );
  assert.equal(v[2].bend?.label, "full");
  assert.equal(v[2].bend?.release, null);
});

test("bend labels as tab writes them", () => {
  assert.equal(bendLabel(0.5), "¼");
  assert.equal(bendLabel(1), "½");
  assert.equal(bendLabel(0.8), "½");
  assert.equal(bendLabel(2), "full");
  assert.equal(bendLabel(2.2), "full");
  assert.equal(bendLabel(3), "1½");
  assert.equal(bendLabel(4), "2");
  assert.equal(bendLabel(-2), "full");
  assert.equal(bendWords(1), "a half step");
  assert.equal(bendWords(2), "a whole step");
  assert.equal(bendWords(3), "1½ steps");
});

test("the roll's range fits the visible notes and bend targets", () => {
  const fb = { lo: 52, hi: 76 };
  assert.deepEqual(rollRange([], fb), fb);
  // One note: at least 8 semitones, around it.
  assert.deepEqual(rollRange([{ midi: 64 }], fb), { lo: 61, hi: 69 });
  // A bend raises the top; a wide line keeps its margins.
  assert.deepEqual(rollRange([{ midi: 60 }, { midi: 72, bend: { semitones: 2 } }], fb), { lo: 59, hi: 76 });
  // A downward bend lowers the bottom.
  assert.deepEqual(rollRange([{ midi: 60, bend: { semitones: -3 } }, { midi: 70 }], fb), { lo: 56, hi: 72 });
});

test("other notes heard widen the roll only near the lead notes", () => {
  const fb = { lo: 52, hi: 76 };
  const lead = [{ midi: 64 }, { midi: 67 }];
  // E3 and E5 are within an octave of the lead (E4 to G4) and count; D3
  // and G#5 are further out and don't.
  assert.deepEqual(rollRange(lead, fb, 8, [{ midi: 55 }, { midi: 76 }, { midi: 50 }, { midi: 80 }]), { lo: 54, hi: 78 });
  // Without lead notes in view, other notes near the fallback range set it.
  assert.deepEqual(rollRange([], fb, 8, [{ midi: 59 }, { midi: 64 }]), { lo: 58, hi: 66 });
  assert.deepEqual(rollRange([], fb, 8, [{ midi: 20 }]), fb);
});

test("other notes heard read from and write to their text form", () => {
  const text = "0.5 1.25 64 0.52,1 3 59 0.3";
  const spans = parseHeard(text);
  assert.deepEqual(spans, [
    { start_s: 0.5, end_s: 1.25, midi: 64, conf: 0.52 },
    { start_s: 1, end_s: 3, midi: 59, conf: 0.3 },
  ]);
  assert.equal(formatHeard(spans), text);
  assert.equal(formatHeard([{ start_s: 1.23456, end_s: 2, midi: 60, conf: 0.999 }]), "1.235 2 60 1");
  // Malformed notes are skipped.
  assert.deepEqual(parseHeard("1 2 64 0.5,x 3 60 0.2,4 5 61,6 5 60 0.1").map((h) => h.midi), [64]);
  assert.deepEqual(parseHeard(undefined), []);
  // Named in the chord frame, like the lead notes.
  const f = chordFrame({ songSemitones: -1, confirmed: true, offsetCents: 0, shiftCents: 0 });
  const v = heardViews({ ...LICK, others: "0.5 1.25 63 0.5" }, f, null);
  assert.equal(v.length, 1);
  assert.equal(v[0].main, noteViews({ ...LICK, notes: [LICK.notes[0]] }, f, null)[0].main);
  assert.equal(heardViews(LICK, f, null).length, 0);
});

test("note at a time and the first note in a window", () => {
  const v = noteViews(LICK, chordFrame({ songSemitones: 0, confirmed: false, offsetCents: 0, shiftCents: 0 }), null);
  assert.equal(noteIndexAt(v, 0.1), 0);
  assert.equal(noteIndexAt(v, 0.51), -1);
  assert.equal(noteIndexAt(v, 1.5), 2);
  assert.equal(noteIndexAt(v, 2), -1);
  assert.equal(noteIndexAt(v, 10), -1);
  assert.equal(firstNoteFrom(v, 0), 0);
  assert.equal(firstNoteFrom(v, 0.7), 1);
  assert.equal(firstNoteFrom(v, 2.6), 4);
  assert.equal(firstNoteFrom(v, 9), 5);
});

test("Shift-click loop covers both notes in either order", () => {
  assert.deepEqual(loopBetween({ start: 3, end: 3.5 }, { start: 1, end: 2 }), { a: 1, b: 3.5 });
});

test("refined label and the default stem", () => {
  const pan = { enabled: true, center: -1, width: 1.34, softness: 0.2 };
  assert.equal(refinedSide(pan), "left");
  assert.equal(refinedSide({ ...pan, center: 0.4 }), "custom");
  assert.equal(refinedSide({ ...pan, enabled: false }), null);
  assert.equal(refinedSide(undefined), null);
  const all: StemName[] = ["guitar", "bass", "vocals", "piano", "other"];
  const none = () => false;
  // Remembered wins when available.
  assert.equal(pickNotesStem("vocals", all, none, none), "vocals");
  // Guitar with notes (or still pending), else bass.
  assert.equal(pickNotesStem(null, all, (s) => s === "guitar", none), "guitar");
  assert.equal(pickNotesStem(null, all, none, (s) => s === "guitar"), "guitar");
  assert.equal(pickNotesStem(null, all, (s) => s === "vocals", none), "bass");
  // A 4-stem set without guitar and bass picks the first with notes.
  assert.equal(pickNotesStem("guitar", ["vocals", "other"], (s) => s === "other", none), "other");
  assert.equal(pickNotesStem(null, [], none, none), null);
});

test("the panel's settings follow the loop while one is on", () => {
  const song = { fast: "more" as const, quiet: false };
  const loop = { start: 10, end: 20 };
  // No loop: the song's.
  assert.deepEqual(panelNoteSettings(song, null, null), { fast: "more", quiet: false, scope: "song", own: false });
  assert.deepEqual(panelNoteSettings(song, null, { ...loop, fast: "fewer", quiet: true }), {
    fast: "more",
    quiet: false,
    scope: "song",
    own: false,
  });
  // A loop starts on the song's settings.
  assert.deepEqual(panelNoteSettings(song, loop, null), { fast: "more", quiet: false, scope: "loop", own: false });
  // Its own, when set on this loop (to the millisecond).
  const own = { start: 10.0002, end: 19.9996, fast: "normal" as const, quiet: true };
  assert.deepEqual(panelNoteSettings(song, loop, own), { fast: "normal", quiet: true, scope: "loop", own: true });
  // Set on another loop: the song's again.
  assert.deepEqual(panelNoteSettings(song, { start: 10, end: 21 }, own).own, false);
  // Equal to the song's: nothing of its own to find.
  assert.equal(panelNoteSettings(song, loop, { ...loop, ...song }).own, false);
  assert.ok(sameSpan(loop, own));
  assert.ok(!sameSpan(loop, null));
});

test("loop notes inside the loop, the song's outside it", () => {
  const song: StemNotes = {
    stem: "guitar",
    source: "m_s1|st0",
    notes: [
      { start_s: 1, end_s: 2, midi: 60 },
      // Crosses the loop's start; its bend starts before the loop, so it stays.
      { start_s: 4.5, end_s: 5.5, midi: 62, bend: { start_s: 4.6, peak_s: 4.8, semitones: 2 } },
      { start_s: 6, end_s: 7, midi: 64 },
      // Crosses the loop's end; its bend starts after it, so it stays.
      { start_s: 9.5, end_s: 11, midi: 65, bend: { start_s: 10.2, peak_s: 10.4, release_s: 10.8, semitones: 1 } },
      // Spans the whole loop (overlapping the others, only to test the cut): both ends stay.
      { start_s: 3, end_s: 12, midi: 40 },
    ].sort((a, b) => a.start_s - b.start_s),
    chordal: [
      [0, 0.5],
      [7, 8],
      [9, 13],
    ],
    others: "1 2.5 55 0.4,5.2 5.8 57 0.3,8 12 59 0.2",
  };
  const loop: StemNotes = {
    stem: "guitar",
    source: "m_s1|st0|fmore|loop5000-10000",
    notes: [
      { start_s: 5, end_s: 5.4, midi: 61 },
      { start_s: 5.4, end_s: 6, midi: 62, attack: "legato" },
      { start_s: 8, end_s: 10, midi: 67 },
    ],
    chordal: [[6.5, 7]],
    others: "5 5.5 50 0.5",
  };
  const range = { start: 5, end: 10 };
  const m = withLoopNotes(song, loop, range)!;
  const spans = m.notes.map((n) => [n.start_s, n.end_s, n.midi]);
  assert.deepEqual(spans, [
    [1, 2, 60],
    [3, 5, 40],
    [4.5, 5, 62],
    [5, 5.4, 61],
    [5.4, 6, 62],
    [8, 10, 67],
    [10, 12, 40],
    [10, 11, 65],
  ]);
  // Cut notes keep a bend that starts in the part kept.
  assert.deepEqual(m.notes[2].bend, { start_s: 4.6, peak_s: 4.8, release_s: null, semitones: 2 });
  assert.deepEqual(m.notes[7].bend, { start_s: 10.2, peak_s: 10.4, release_s: 10.8, semitones: 1 });
  assert.equal(m.notes[4].attack, "legato");
  assert.deepEqual(m.chordal, [
    [0, 0.5],
    [6.5, 7],
    [10, 13],
  ]);
  assert.deepEqual(
    parseHeard(m.others).map((h) => [h.start_s, h.end_s, h.midi]),
    [
      [1, 2.5, 55],
      [5, 5.5, 50],
      [10, 12, 59],
    ],
  );
  assert.equal(m.source, song.source);
  // While the loop's notes are being found, the loop stays empty.
  const busy = withLoopNotes(song, null, range)!;
  assert.ok(busy.notes.every((n) => n.end_s <= 5 || n.start_s >= 10));
  assert.ok(busy.chordal!.every(([a, b]) => b <= 5 || a >= 10));
  // A bend whose rise the cut interrupts ends its rise at the cut; one starting in the loop goes.
  const cut = withLoopNotes(
    {
      ...song,
      notes: [
        { start_s: 3, end_s: 6, midi: 50, bend: { start_s: 4, peak_s: 5.5, release_s: 5.8, semitones: 2 } },
        { start_s: 9, end_s: 11, midi: 52, bend: { start_s: 9.5, peak_s: 9.7, semitones: 1 } },
      ],
    },
    null,
    range,
  )!;
  assert.deepEqual(cut.notes[0].bend, { start_s: 4, peak_s: 5, release_s: null, semitones: 2 });
  assert.equal(cut.notes[1].bend, undefined);
  // Without a loop: the song's notes as they are.
  assert.equal(withLoopNotes(song, loop, null), song);
  assert.equal(withLoopNotes(undefined, null, range), undefined);
  // Notes views read the result like any stem's notes.
  assert.equal(noteViews(m, chordFrame({ songSemitones: 0, confirmed: false, offsetCents: 0, shiftCents: 0, mySemitones: 0 }), null).length, 8);
});
