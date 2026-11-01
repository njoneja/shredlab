//! EQ presets of the Refine panel (SPEC §5.4 P5). Data only; a preset sets
//! the EQ and leaves the stereo selection alone (see [`apply_preset`]).
//!
//! Reasoning, briefly (frequencies are where the energy of each thing sits
//! in a typical rock/pop mix):
//!
//! - **Bass: hear the notes** — a bass line's pitch is carried more by its
//!   2nd–5th harmonics (≈ 200 Hz – 2 kHz) than by the fundamental
//!   (41–200 Hz), especially on small speakers and headphones. Trim the
//!   boom (−3 dB at 80 Hz), lift the growl (+5 dB at 700 Hz) and the
//!   string/finger articulation (+4 dB at 1.6 kHz). A 30 Hz cut removes
//!   sub rumble that would only eat headroom after the boosts.
//! - **Bass: tame slap/pick noise** — the click of slap, pops and pick
//!   attack lives at roughly 2–5 kHz with fret noise above. −6 dB at
//!   2.5 kHz (broad) plus a 5 kHz high cut keeps the note and loses the
//!   clack.
//! - **Guitar: focus** — cut what isn't guitar tone: rumble and bass bleed
//!   below 90 Hz, mud around 250 Hz (−3 dB), lift presence at 2.5 kHz
//!   (+3 dB), and cut fizz and cymbal bleed above 9 kHz.
//! - **Guitar: remove bass bleed** — a 120 Hz 24 dB/oct low cut. Bass
//!   fundamentals sit at 41–200 Hz; the guitar's lowest note is 82 Hz but
//!   its tone is in the harmonics, so losing the low-E fundamental costs
//!   little while most of the bleed goes.
//! - **Vocals: clarity** — 100 Hz low cut (breath pops, bleed), −2 dB at
//!   300 Hz (boxiness), +3 dB at 3 kHz (intelligibility), +2 dB broad at
//!   10 kHz (air).
//! - **Backing track: make room for guitar** — for the stems you play along
//!   with: scoop the guitar's range so your own guitar sits on top,
//!   −2 dB at 400 Hz (body) and −4 dB broad at 2 kHz (presence).
//! - **Flat** — everything off.

use super::eq::{BandParams, CutParams, EqParams};
use super::RefineParams;

/// One EQ preset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqPreset {
    /// Stable id for saving and the UI.
    pub id: &'static str,
    /// Display name.
    pub name: &'static str,
    pub eq: EqParams,
}

const OFF_BAND: BandParams = BandParams {
    enabled: false,
    freq: 1000.0,
    gain_db: 0.0,
    q: 1.0,
};

const fn cut(freq: f32) -> CutParams {
    CutParams {
        enabled: true,
        freq,
    }
}

const NO_LOW: CutParams = CutParams {
    enabled: false,
    freq: 80.0,
};

const NO_HIGH: CutParams = CutParams {
    enabled: false,
    freq: 12_000.0,
};

/// The preset list, in UI order (Flat last).
pub const EQ_PRESETS: [EqPreset; 7] = [
    EqPreset {
        id: "bass-notes",
        name: "Bass: hear the notes",
        eq: EqParams {
            low_cut: cut(30.0),
            high_cut: NO_HIGH,
            bands: [
                BandParams::new(80.0, -3.0, 0.7),
                BandParams::new(700.0, 5.0, 0.9),
                BandParams::new(1600.0, 4.0, 1.0),
            ],
        },
    },
    EqPreset {
        id: "bass-tame-attack",
        name: "Bass: tame slap/pick noise",
        eq: EqParams {
            low_cut: NO_LOW,
            high_cut: cut(5000.0),
            bands: [OFF_BAND, OFF_BAND, BandParams::new(2500.0, -6.0, 0.9)],
        },
    },
    EqPreset {
        id: "guitar-focus",
        name: "Guitar: focus",
        eq: EqParams {
            low_cut: cut(90.0),
            high_cut: cut(9000.0),
            bands: [
                BandParams::new(250.0, -3.0, 1.0),
                BandParams::new(2500.0, 3.0, 1.0),
                OFF_BAND,
            ],
        },
    },
    EqPreset {
        id: "guitar-no-bass",
        name: "Guitar: remove bass bleed (low cut)",
        eq: EqParams {
            low_cut: cut(120.0),
            high_cut: NO_HIGH,
            bands: [OFF_BAND; 3],
        },
    },
    EqPreset {
        id: "vocals-clarity",
        name: "Vocals: clarity",
        eq: EqParams {
            low_cut: cut(100.0),
            high_cut: NO_HIGH,
            bands: [
                BandParams::new(300.0, -2.0, 1.0),
                BandParams::new(3000.0, 3.0, 1.0),
                BandParams::new(10_000.0, 2.0, 0.7),
            ],
        },
    },
    EqPreset {
        id: "backing-room-for-guitar",
        name: "Backing track: make room for guitar",
        eq: EqParams {
            low_cut: NO_LOW,
            high_cut: NO_HIGH,
            bands: [
                BandParams::new(400.0, -2.0, 1.0),
                BandParams::new(2000.0, -4.0, 0.8),
                OFF_BAND,
            ],
        },
    },
    EqPreset {
        id: "flat",
        name: "Flat",
        eq: EqParams {
            low_cut: NO_LOW,
            high_cut: NO_HIGH,
            bands: [OFF_BAND; 3],
        },
    },
];

/// Looks up a preset by id.
pub fn eq_preset(id: &str) -> Option<&'static EqPreset> {
    EQ_PRESETS.iter().find(|p| p.id == id)
}

/// Applies a preset's EQ to `params`, keeping the stereo selection.
pub fn apply_preset(params: &mut RefineParams, preset: &EqPreset) {
    params.eq = preset.eq;
}
