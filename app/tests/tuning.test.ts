// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  DEFAULT_TUNING,
  centsOff,
  driftSummary,
  holdStep,
  matchString,
  tuneAdvice,
  tunerLowHz,
  tunerTarget,
  formatPitch,
  fromSaved,
  medianSmooth,
  offsetText,
  toneHz,
  toSaved,
  tuningLongName,
  matchCents,
  rateTempoPercent,
  sanitize,
  songRefHz,
  songState,
  songTuningName,
  splitPitch,
  strings,
  tuningName,
  type MyTuning,
} from "../src/lib/tuning.ts";

const near = (a: number, b: number, eps: number) =>
  assert.ok(Math.abs(a - b) <= eps, `${a} not within ${eps} of ${b}`);

const ebAt430: MyTuning = { ...DEFAULT_TUNING, shift: -1, refHz: 430 };

test("match: song 18 cents flat, standard at A440", () => {
  const m = matchCents(DEFAULT_TUNING, 0, -18);
  assert.equal(m, 18);
  assert.deepEqual(splitPitch(m), { semitones: 0, cents: 18 });
  assert.equal(formatPitch(splitPitch(m)!), "0 st +18¢");
});

test("match: song 18 cents flat, Eb standard at A430", () => {
  const m = matchCents(ebAt430, 0, -18);
  assert.equal(m, -122);
  assert.deepEqual(splitPitch(m), { semitones: -1, cents: -22 });
  assert.equal(formatPitch(splitPitch(m)!), "−1 st −22¢");
});

test("match: song in Eb standard, player in Eb standard", () => {
  assert.equal(matchCents({ ...DEFAULT_TUNING, shift: -1 }, -1, -4), 4);
});

test("splitPitch keeps cents within ±50 and rejects out-of-range shifts", () => {
  assert.deepEqual(splitPitch(150), { semitones: 1, cents: 50 });
  assert.deepEqual(splitPitch(-150), { semitones: -1, cents: -50 });
  assert.deepEqual(splitPitch(250), { semitones: 2, cents: 50 });
  assert.deepEqual(splitPitch(151), { semitones: 2, cents: -49 });
  assert.deepEqual(splitPitch(-51), { semitones: -1, cents: 49 });
  assert.deepEqual(splitPitch(0), { semitones: 0, cents: 0 });
  assert.equal(splitPitch(1300), null);
});

test("playback-rate tempo change", () => {
  near(rateTempoPercent(18), 1.0, 0.05);
  near(rateTempoPercent(-122), -6.8, 0.05);
});

test("string notes and frequencies", () => {
  const std = strings(DEFAULT_TUNING);
  assert.deepEqual(
    std.map((s) => s.label),
    ["E2", "A2", "D3", "G3", "B3", "E4"],
  );
  assert.deepEqual(
    std.map((s) => s.number),
    [6, 5, 4, 3, 2, 1],
  );
  near(std[0].hz, 82.41, 0.01);
  near(std[1].hz, 110, 1e-9);
  const eb = strings(ebAt430);
  assert.deepEqual(
    eb.map((s) => s.name),
    ["Eb", "Ab", "Db", "Gb", "Bb", "Eb"],
  );
  near(eb[3].hz, 180.8, 0.01);
  const bass5 = strings({ ...DEFAULT_TUNING, instrument: "bass5" });
  near(bass5[0].hz, 30.87, 0.01);
});

test("tuning names", () => {
  assert.equal(tuningName(DEFAULT_TUNING), "Standard");
  assert.equal(tuningName(ebAt430), "Eb standard, A430");
  assert.equal(tuningName({ ...DEFAULT_TUNING, preset: "dropD", shift: -2 }), "Drop C");
  assert.equal(tuningName({ ...DEFAULT_TUNING, preset: "dadgad", shift: -1 }), "DADGAD −1");
  assert.equal(tuningName({ ...DEFAULT_TUNING, refHz: 435.5 }), "Standard, A435.5");
  assert.equal(songTuningName(0), "Standard");
  assert.equal(songTuningName(-1), "Eb standard (−1)");
});

test("sanitize clamps and falls back", () => {
  assert.deepEqual(sanitize(null), DEFAULT_TUNING);
  const t = sanitize({ instrument: "bass4", preset: "dadgad", shift: 40, refHz: 1000 });
  assert.equal(t.preset, "standard");
  assert.equal(t.shift, 12);
  assert.equal(t.refHz, 480);
  assert.equal(sanitize({ preset: "custom", custom: [] }).preset, "standard");
});

test("song state", () => {
  const m = { offset_cents: -18, reference_hz: 435.5, confidence: 0.8, peaks: 5000, drifts: false };
  assert.equal(songState(null, true), "measuring");
  assert.equal(songState(null, false), "unmeasured");
  assert.equal(songState(m, false), "off");
  assert.equal(songState({ ...m, offset_cents: 2 }, false), "in-tune");
  assert.equal(songState({ ...m, offset_cents: 2, drifts: true }, false), "off");
  assert.equal(songState({ ...m, confidence: 0.1 }, false), "unmeasured");
  // 440 · 2^(−18/1200) = 435.44 (the mocks round it to 435.5).
  assert.equal(songRefHz(-18), 435.4);
});

test("long names and the offset line", () => {
  assert.equal(tuningLongName(DEFAULT_TUNING), "Standard at A440");
  assert.equal(tuningLongName(ebAt430), "Eb standard at A430");
  assert.equal(offsetText(ebAt430), "140 cents below Standard at A440");
  assert.equal(offsetText(DEFAULT_TUNING), "Standard at A440");
  assert.equal(offsetText({ ...DEFAULT_TUNING, refHz: 440.3 }), "1 cent above Standard at A440");
});

test("saved tuning round trip and defaults", () => {
  const t: MyTuning = { instrument: "bass5", preset: "custom", custom: [23, 28, 33, 38, 43], shift: -2, refHz: 432.1 };
  const s = toSaved(t);
  assert.equal(s.ref_hz, 432.1);
  assert.deepEqual(fromSaved(JSON.parse(JSON.stringify(s))), t);
  assert.deepEqual(fromSaved(null), DEFAULT_TUNING);
  assert.deepEqual(fromSaved({ shift: 3 }), { ...DEFAULT_TUNING, shift: 3 });
});

test("reference tone frequencies", () => {
  near(toneHz(DEFAULT_TUNING, 6, false)!, 82.41, 0.01);
  near(toneHz(DEFAULT_TUNING, 6, true)!, 164.81, 0.01);
  near(toneHz(DEFAULT_TUNING, 1, false)!, 329.63, 0.01);
  assert.equal(toneHz(DEFAULT_TUNING, 7, false), null);
});

test("drift summary", () => {
  assert.deepEqual(medianSmooth([1, 9, 2, 3], 3), [5, 2, 3, 2.5]);
  assert.deepEqual(medianSmooth([1, 9, 2, 3, 40, 4]), [2, 2.5, 3, 4, 3.5, 4]);
  const seg = (vals: number[]) => vals.map((v, i) => ({ start_s: i * 10, offset_cents: v, weight: 1 }));
  assert.deepEqual(driftSummary(seg([-22, -20, -17, -14, -11, -9])), { kind: "from", a: -20, b: -11 });
  assert.deepEqual(driftSummary(seg([-20, -18, -10, -5, -4, -6, -12, -19, -20])), { kind: "between", a: -19, b: -6 });
  assert.equal(driftSummary([]), null);
});

test("live tuner: the string a reading belongs to", () => {
  const std = strings(DEFAULT_TUNING); // E2 A2 D3 G3 B3 E4
  const at = (hz: number) => matchString(hz, std)?.number ?? null;
  assert.equal(at(82.41), 6);
  assert.equal(at(110 * Math.pow(2, -20 / 1200)), 5); // A string, 20 cents flat
  // Between E2 and A2 (500 cents apart) the nearer one wins.
  assert.equal(at(82.41 * Math.pow(2, 240 / 1200)), 6);
  assert.equal(at(82.41 * Math.pow(2, 260 / 1200)), 5);
  // Below the lowest string: up to half the gap to its neighbor (250 cents).
  assert.equal(at(82.41 * Math.pow(2, -240 / 1200)), 6);
  assert.equal(at(82.41 * Math.pow(2, -260 / 1200)), null);
  // Above the highest: half the B–E gap (250 cents).
  assert.equal(at(329.63 * Math.pow(2, 240 / 1200)), 1);
  assert.equal(at(329.63 * Math.pow(2, 260 / 1200)), null);
  assert.equal(at(0), null);
  assert.equal(matchString(100, []), null);
});

test("live tuner: target, cents and advice", () => {
  // Eb standard at A430: 180.07 Hz is the G♭ string, a few cents flat.
  const eb: MyTuning = { ...DEFAULT_TUNING, shift: -1, refHz: 430 };
  const t = tunerTarget(180.07, eb, false)!;
  assert.equal(t.string, 3);
  assert.equal(`${t.name}${t.octave}`, "Gb3");
  near(t.hz, 180.8, 0.05);
  near(t.cents, -7, 0.1);
  // Chromatic ignores the strings: the nearest note in the same reference.
  const c = tunerTarget(185, eb, true)!;
  assert.equal(c.string, null);
  assert.equal(c.name, "Gb");
  near(c.hz, 180.8, 0.05);
  // No string near: the nearest note too.
  const far = tunerTarget(1000, DEFAULT_TUNING, false)!;
  assert.equal(far.string, null);
  assert.equal(`${far.name}${far.octave}`, "B5");
  assert.equal(tunerTarget(0, DEFAULT_TUNING, false), null);
  near(centsOff(220, 110), 1200, 1e-9);

  assert.deepEqual(tuneAdvice(-7.4), { state: "flat", text: "Flat: tune up", full: "7¢ flat: tune up" });
  assert.deepEqual(tuneAdvice(12), { state: "sharp", text: "Sharp: tune down", full: "12¢ sharp: tune down" });
  assert.equal(tuneAdvice(3.4).state, "in-tune");
  assert.equal(tuneAdvice(-3.6).state, "flat");

  assert.equal(tunerLowHz(DEFAULT_TUNING, true), 30);
  near(tunerLowHz(DEFAULT_TUNING, false), 82.41, 0.01);
  near(tunerLowHz({ ...DEFAULT_TUNING, instrument: "bass5" }, false), 30.87, 0.01);
});

test("live tuner: a string counts as in tune after holding for a second", () => {
  let h = holdStep(null, 3, 1, 0);
  assert.equal(h.settled, null);
  h = holdStep(h.hold, 3, -2, 900);
  assert.equal(h.settled, null);
  h = holdStep(h.hold, 3, 2.4, 1000);
  assert.deepEqual(h.settled, { string: 3, inTune: true });
  // Out of tune starts a new hold; so does another string.
  h = holdStep(h.hold, 3, 6, 1100);
  assert.equal(h.settled, null);
  assert.deepEqual(holdStep(h.hold, 3, 6, 2100).settled, { string: 3, inTune: false });
  h = holdStep(h.hold, 2, 0, 1200);
  assert.equal(h.hold?.since, 1200);
  // No reading or no string: no hold.
  assert.deepEqual(holdStep(h.hold, null, 0, 1300), { hold: null, settled: null });
  assert.deepEqual(holdStep(h.hold, 2, null, 1300), { hold: null, settled: null });
});
