// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! The analysis grid: each beat interval split into sixteenths.

/// Where a cell starts relative to the beat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pos {
    Beat,
    /// The off-beat eighth (halfway through the beat).
    Eighth,
    /// The first or third sixteenth.
    Sixteenth,
}

/// One grid cell, a sixteenth of a beat interval, clipped to the song.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Cell {
    pub start: f64,
    pub end: f64,
    pub pos: Pos,
}

pub(crate) struct Grid {
    pub cells: Vec<Cell>,
    /// False when the song had no usable beat grid and a fixed one was
    /// made up: cell positions then say nothing about the music.
    pub has_beats: bool,
}

/// Beat length of the made-up grid when there are fewer than two beats
/// (120 BPM).
const NO_GRID_BEAT_S: f64 = 0.5;
/// Beats closer than this to the previous one are dropped (600 BPM).
const MIN_BEAT_S: f64 = 0.1;
/// A beat interval longer than this many median intervals is a gap in the
/// beat tracking (a break, a quiet intro) and is split into beats of about
/// the median length.
const GAP_FACTOR: f64 = 1.5;
/// Cells per beat.
const SUBDIVISIONS: usize = 4;
const POSITIONS: [Pos; SUBDIVISIONS] = [Pos::Beat, Pos::Sixteenth, Pos::Eighth, Pos::Sixteenth];
/// Cells shorter than this after clipping to the song are dropped.
const MIN_CELL_S: f64 = 1e-4;

/// Builds the grid covering `0..duration` from the tracked beats. The beats
/// are extended before the first and after the last with the median
/// interval; gaps are filled the same way. With fewer than two beats a
/// fixed grid of [`NO_GRID_BEAT_S`] beats is used.
pub(crate) fn build(beats: &[f64], duration: f64) -> Grid {
    let mut b: Vec<f64> = beats
        .iter()
        .copied()
        .filter(|t| t.is_finite() && *t >= 0.0 && *t < duration)
        .collect();
    b.sort_by(f64::total_cmp);
    let mut clean: Vec<f64> = Vec::with_capacity(b.len());
    for t in b {
        if clean.last().is_none_or(|&l| t - l >= MIN_BEAT_S) {
            clean.push(t);
        }
    }
    let has_beats = clean.len() >= 2;
    let interval = if has_beats {
        let mut d: Vec<f64> = clean.windows(2).map(|w| w[1] - w[0]).collect();
        d.sort_by(f64::total_cmp);
        d[d.len() / 2]
    } else {
        NO_GRID_BEAT_S
    };
    let anchor = if has_beats {
        clean.clone()
    } else {
        vec![clean.first().copied().unwrap_or(0.0)]
    };

    // Every beat, real or extended, from at or before 0 to at or after the end.
    let mut full = Vec::with_capacity(anchor.len() + 16);
    let first = anchor[0];
    let before = (first / interval).ceil() as usize;
    for i in (1..=before).rev() {
        full.push(first - i as f64 * interval);
    }
    for w in anchor.windows(2) {
        let (a, c) = (w[0], w[1]);
        let n = if c - a > GAP_FACTOR * interval {
            ((c - a) / interval).round().max(1.0) as usize
        } else {
            1
        };
        for j in 0..n {
            full.push(a + j as f64 * (c - a) / n as f64);
        }
    }
    let mut t = *anchor.last().expect("anchor is not empty");
    full.push(t);
    while t < duration {
        t += interval;
        full.push(t);
    }

    let mut cells = Vec::with_capacity(full.len() * SUBDIVISIONS);
    for w in full.windows(2) {
        let (a, c) = (w[0], w[1]);
        for (i, &pos) in POSITIONS.iter().enumerate() {
            let s = a + (c - a) * i as f64 / SUBDIVISIONS as f64;
            let e = a + (c - a) * (i + 1) as f64 / SUBDIVISIONS as f64;
            let (s, e) = (s.max(0.0), e.min(duration));
            if e - s >= MIN_CELL_S {
                cells.push(Cell {
                    start: s,
                    end: e,
                    pos,
                });
            }
        }
    }
    // Contiguous by construction; pin the ends exactly.
    if let Some(c) = cells.first_mut() {
        c.start = 0.0;
    }
    if let Some(c) = cells.last_mut() {
        c.end = duration;
    }
    for i in 1..cells.len() {
        cells[i].start = cells[i - 1].end;
    }
    Grid { cells, has_beats }
}
