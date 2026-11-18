// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  barLength,
  chordFrame,
  chordIndexAt,
  chordName,
  chordViews,
  detailWindow,
  passingViews,
  shortNames,
  spellingFor,
  tuningLine,
} from "../src/lib/chords.ts";
import type { ChordsData } from "../src/lib/types.ts";

// E5 | D/E (pushed) | N.C., at concert pitch in Eb (a song in Eb standard).
const EB: ChordsData = {
  version: 1,
  stems: "htdemucs_6s_s1",
  key: { tonic: 3, minor: false },
  segments: [
    { start_s: 0, end_s: 2, root: 3, quality: "5" },
    { start_s: 2, end_s: 3.75, root: 1, quality: "", bass: 3, push: true },
    { start_s: 3.75, end_s: 5 },
  ],
};

const frame = (o: Partial<Parameters<typeof chordFrame>[0]>) =>
  chordFrame({ songSemitones: -1, confirmed: true, offsetCents: 0, shiftCents: 0, ...o });

test("names: spelling follows the (transposed) key", () => {
  assert.equal(spellingFor({ tonic: 3, minor: false }, 0), "flats");
  assert.equal(spellingFor({ tonic: 3, minor: false }, 1), "sharps"); // E major
  assert.equal(spellingFor({ tonic: 9, minor: true }, 0), "common"); // A minor = C major
  assert.equal(spellingFor(null, 0), "common");
  assert.equal(chordName({ start_s: 0, end_s: 1, root: 2, quality: "", bass: 6 }, 0, "sharps"), "D/F#");
  assert.equal(chordName({ start_s: 0, end_s: 1 }, 0, "sharps"), null);
  assert.deepEqual(shortNames("D/F#"), ["D/F#", "D"]);
  assert.deepEqual(shortNames("C#m7"), ["C#m7", "C#m"]);
  assert.deepEqual(shortNames("E5"), ["E5", "E"]);
});

test("unconfirmed tuning: one name, what you hear", () => {
  const v = chordViews(EB, frame({ confirmed: false }));
  assert.deepEqual(
    v.map((x) => [x.main, x.second]),
    [
      ["Eb5", null],
      ["Db/Eb", null],
      [null, null],
    ],
  );
  assert.equal(v[1].push, true);
});

test("Standard guitar, song in Eb, not shifted: one name, what you hear", () => {
  assert.deepEqual(chordViews(EB, frame({})).map((x) => [x.main, x.second])[0], ["Eb5", null]);
});

test("Eb guitar, song in Eb, not shifted: shape and what you hear", () => {
  const v = chordViews(EB, frame({ mySemitones: -1 }));
  assert.deepEqual(
    v.map((x) => [x.main, x.second]),
    [
      ["E5", "Eb5"],
      ["D/E", "Db/Eb"],
      [null, null],
    ],
  );
});

test("confirmed Eb, shifted up a half step: one name", () => {
  const v = chordViews(EB, frame({ shiftCents: 100 }));
  assert.deepEqual(v[0], { start: 0, end: 2, main: "E5", second: null, push: false });
});

test("the measured offset rounds into what you hear", () => {
  // 30 cents sharp of Eb plus a 75 cent shift: nearest is E.
  assert.equal(frame({ offsetCents: 30, shiftCents: 75 }).heard, 1);
  assert.equal(frame({ offsetCents: -30, shiftCents: 0 }).heard, 0);
});

test("chordIndexAt finds the chord at a time", () => {
  const v = chordViews(EB, frame({}));
  assert.equal(chordIndexAt(v, 0), 0);
  assert.equal(chordIndexAt(v, 2.5), 1);
  assert.equal(chordIndexAt(v, 4.9), 2);
  assert.equal(chordIndexAt(v, 6), -1);
});

test("tuning line: guess, set, shifted, mismatched", () => {
  const guess = { semitones: -1, label: "Eb standard" };
  const line = (o: Partial<Parameters<typeof tuningLine>[0]>) =>
    tuningLine({ frame: frame({}), songSemitones: -1, mySemitones: 0, shiftCents: 0, guess, mismatch: 0, sample: null, ...o });

  // Nothing set: a likely alternate tuning, one row of names.
  let l = line({ frame: frame({ confirmed: false }), songSemitones: 0 });
  assert.equal(l.text, "Song may be in an alternate tuning (probably Eb standard). Names will adjust if you set your tuning.");
  assert.deepEqual(l.useGuess, guess);
  assert.equal(line({ frame: frame({ confirmed: false }), songSemitones: 0, guess: null }).text, "");

  // Song set to Eb, Standard guitar, no shift: names are what you hear,
  // with a tip on how to play the song's usual shapes.
  l = line({ mismatch: 1 });
  assert.equal(l.warn, false);
  assert.equal(l.legend, null);
  assert.equal(l.text, "Song may be in Eb. To play its usual shapes, tune down a half step or shift the song.");
  assert.equal(l.fixes?.tunedLabel, "I'm tuned down");

  // Guitar tuned to Eb: shapes over what you hear.
  const eb = frame({ mySemitones: -1 });
  const sample = chordViews(EB, eb)[0];
  l = line({ frame: eb, mySemitones: -1, sample });
  assert.deepEqual(l.legend, { main: "E5", heard: "Eb5" });
  assert.equal(l.fixes, null);
  assert.equal(l.text, "Chords shifted to match your tuning shift (down a half step). Smaller names show how they sound.");

  // Shifted to the player's Standard.
  assert.equal(line({ frame: frame({ shiftCents: 100 }), shiftCents: 100 }).text, "Song was shifted up a half step. Chords match what you hear.");

  // Standard song pitched up a whole step for a player in Standard.
  l = line({ frame: frame({ songSemitones: 0, shiftCents: 200 }), songSemitones: 0, shiftCents: 200, guess: null, mismatch: -2 });
  assert.equal(l.text, "Song was shifted up a whole step. Chords match what you hear.");
  assert.equal(l.fixes?.tunedLabel, "I'm tuned up");
});

test("detail window: fits the loop, else follows the playhead", () => {
  const downbeats = Array.from({ length: 50 }, (_, i) => 1 + 2 * i);
  assert.equal(barLength(downbeats), 2);
  assert.equal(barLength([]), 2);
  // A few short bars (a misdetected intro) don't change it.
  assert.equal(barLength([0, 0.5, 1.5, 2, 4, 6, 8, 10, 12]), 2);
  const loop = { a_sec: 11, b_sec: 19, enabled: true };
  let w = detailWindow({ position: 12, duration: 100, loop, fit: true, bars: 8, downbeats });
  assert.equal(w.fitting, true);
  assert.ok(w.start < 11 && w.start > 10.5 && w.end > 19 && w.end < 19.5);
  w = detailWindow({ position: 40, duration: 100, loop, fit: false, bars: 8, downbeats });
  assert.deepEqual(w, { start: 36, end: 52, fitting: false });
  // Loop off: follows even when fit is on. Clamped at the start and end.
  w = detailWindow({ position: 1, duration: 100, loop: { ...loop, enabled: false }, fit: true, bars: 8, downbeats });
  assert.deepEqual(w, { start: 0, end: 16, fitting: false });
  w = detailWindow({ position: 99, duration: 100, loop: { ...loop, enabled: false }, fit: true, bars: 8, downbeats });
  assert.deepEqual(w, { start: 84, end: 100, fitting: false });
});

test("detailed names and passing chords", () => {
  const data: ChordsData = {
    version: 2,
    stems: "htdemucs_6s_s1",
    key: { tonic: 9, minor: false },
    segments: [
      { start_s: 0, end_s: 4, root: 9, quality: "", detail: "6" },
      { start_s: 4, end_s: 8, root: 2, quality: "" },
    ],
    passing: [{ start_s: 6, end_s: 6.25, root: 11, quality: "m" }],
  };
  const f = chordFrame({ songSemitones: 0, confirmed: true, offsetCents: 0, shiftCents: 0 });
  assert.deepEqual(
    chordViews(data, f).map((v) => v.main),
    ["A", "D"],
  );
  assert.deepEqual(
    chordViews(data, f, true).map((v) => v.main),
    ["A6", "D"],
  );
  assert.deepEqual(passingViews(data, f), [{ start: 6, end: 6.25, main: "Bm", second: null, figure: false }]);
  // Figures come in the same list, in time order, flagged.
  const withFig = { ...data, figures: [{ start_s: 1, end_s: 1.5, root: 9, quality: "6" }] };
  assert.deepEqual(
    passingViews(withFig, f).map((p) => [p.main, p.figure]),
    [
      ["A6", true],
      ["Bm", false],
    ],
  );
  // In Eb, not shifted: the passing chord gets both names too.
  const eb = chordFrame({ songSemitones: -1, confirmed: true, offsetCents: 0, shiftCents: 0, mySemitones: -1 });
  // A song in A shapes, tuned down: Ab at concert pitch.
  const shifted = { ...data, key: { tonic: 8, minor: false }, passing: [{ start_s: 6, end_s: 6.25, root: 10, quality: "m" }] };
  assert.deepEqual(passingViews(shifted, eb)[0], { start: 6, end: 6.25, main: "Bm", second: "Bbm", figure: false });
  assert.deepEqual(passingViews({ ...data, passing: undefined }, f), []);
});
