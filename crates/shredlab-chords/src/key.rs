// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Whole-song key estimate from the chords and the chroma.

use crate::chroma::CellFeatures;
use crate::{ChordSegment, Key};

/// Krumhansl–Kessler key profiles, tonic first.
const MAJOR: [f64; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR: [f64; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];
/// Extra weight of a chord's root over its other tones: songs dwell on and
/// resolve to the tonic chord.
const ROOT_EXTRA: f64 = 1.0;
/// Share of the pitch histogram that comes from the chroma (melody, thirds
/// over power chords) rather than from the chords.
const CHROMA_SHARE: f64 = 0.4;

/// The key whose profile correlates best with the song's pitch classes.
/// `None` when no segment has a chord.
pub(crate) fn estimate(segments: &[ChordSegment], features: &[CellFeatures]) -> Option<Key> {
    let mut chords = [0.0f64; 12];
    for s in segments {
        let Some(c) = s.chord else { continue };
        let d = s.end_s - s.start_s;
        for &i in c.quality.intervals() {
            chords[(usize::from(c.root) + usize::from(i)) % 12] += d;
        }
        chords[usize::from(c.root)] += ROOT_EXTRA * d;
    }
    let chord_total: f64 = chords.iter().sum();
    if chord_total <= 0.0 {
        return None;
    }
    let mut chroma = [0.0f64; 12];
    for f in features {
        let sum: f32 = f.chroma.iter().sum();
        if sum > 0.0 {
            for (a, &c) in chroma.iter_mut().zip(&f.chroma) {
                *a += f64::from(c / sum);
            }
        }
    }
    let chroma_total: f64 = chroma.iter().sum();
    let mut h = [0.0f64; 12];
    for i in 0..12 {
        h[i] = (1.0 - CHROMA_SHARE) * chords[i] / chord_total;
        if chroma_total > 0.0 {
            h[i] += CHROMA_SHARE * chroma[i] / chroma_total;
        }
    }
    let mut best = (
        f64::NEG_INFINITY,
        Key {
            tonic: 0,
            minor: false,
        },
    );
    for tonic in 0..12u8 {
        for (minor, profile) in [(false, &MAJOR), (true, &MINOR)] {
            let rotated: [f64; 12] =
                std::array::from_fn(|i| profile[(i + 12 - usize::from(tonic)) % 12]);
            let r = correlation(&h, &rotated);
            if r > best.0 {
                best = (r, Key { tonic, minor });
            }
        }
    }
    Some(best.1)
}

fn correlation(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    let ma = a.iter().sum::<f64>() / 12.0;
    let mb = b.iter().sum::<f64>() / 12.0;
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        let (x, y) = (a[i] - ma, b[i] - mb);
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    if aa <= 0.0 || bb <= 0.0 {
        0.0
    } else {
        ab / (aa * bb).sqrt()
    }
}
