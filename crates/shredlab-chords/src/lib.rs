// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Chord detection for ShredLab.
//!
//! [`detect_chords`] turns a song's pitched stems and its beat grid into
//! contiguous chord segments at concert pitch, plus a key estimate. Pure
//! DSP: mono `f32` samples in, a [`ChordAnalysis`] out; no paths, no
//! devices. Naming (spelling, transposing to a tuning) is the caller's job.

mod chroma;
mod decode;
mod detail;
mod grid;
mod key;

pub use shredlab_tuning::{StemAudio, StemKind};

/// Algorithm version of [`detect_chords`]. Stored results with an older
/// version are recomputed. Bump it whenever a change can move the result.
pub const VERSION: u32 = 8;

/// Chord types. [`Quality::ALL`] are the ones the segmentation decodes;
/// [`Quality::EXTENDED`] only name a segment in more detail
/// ([`ChordSegment::detailed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Quality {
    Major,
    Minor,
    /// Root and fifth only.
    Power,
    Dom7,
    Maj7,
    Min7,
    Sus2,
    Sus4,
    Dim,
    /// Major sixth: A6 (the same notes as F#m7; the bass decides).
    Six,
    /// Minor sixth.
    Min6,
    /// Major triad with an added ninth (no seventh).
    Add9,
    /// Dominant ninth.
    Dom9,
    /// Dominant seventh with a suspended fourth.
    SevenSus4,
}

impl Quality {
    /// Extensions used only for detailed names.
    pub const EXTENDED: [Quality; 5] = [
        Quality::Six,
        Quality::Min6,
        Quality::Add9,
        Quality::Dom9,
        Quality::SevenSus4,
    ];

    /// The qualities the segmentation decodes.
    pub const ALL: [Quality; 9] = [
        Quality::Major,
        Quality::Minor,
        Quality::Power,
        Quality::Dom7,
        Quality::Maj7,
        Quality::Min7,
        Quality::Sus2,
        Quality::Sus4,
        Quality::Dim,
    ];

    /// Suffix after the root: "", "m", "5", "7", "maj7", "m7", "sus2",
    /// "sus4", "dim", and for the extensions "6", "m6", "add9", "9", "7sus4".
    pub fn suffix(self) -> &'static str {
        match self {
            Quality::Major => "",
            Quality::Minor => "m",
            Quality::Power => "5",
            Quality::Dom7 => "7",
            Quality::Maj7 => "maj7",
            Quality::Min7 => "m7",
            Quality::Sus2 => "sus2",
            Quality::Sus4 => "sus4",
            Quality::Dim => "dim",
            Quality::Six => "6",
            Quality::Min6 => "m6",
            Quality::Add9 => "add9",
            Quality::Dom9 => "9",
            Quality::SevenSus4 => "7sus4",
        }
    }

    /// Inverse of [`Quality::suffix`].
    pub fn from_suffix(s: &str) -> Option<Quality> {
        Quality::ALL
            .into_iter()
            .chain(Quality::EXTENDED)
            .find(|q| q.suffix() == s)
    }

    /// Pitch classes above the root, root (0) included.
    pub fn intervals(self) -> &'static [u8] {
        match self {
            Quality::Major => &[0, 4, 7],
            Quality::Minor => &[0, 3, 7],
            Quality::Power => &[0, 7],
            Quality::Dom7 => &[0, 4, 7, 10],
            Quality::Maj7 => &[0, 4, 7, 11],
            Quality::Min7 => &[0, 3, 7, 10],
            Quality::Sus2 => &[0, 2, 7],
            Quality::Sus4 => &[0, 5, 7],
            Quality::Dim => &[0, 3, 6],
            Quality::Six => &[0, 4, 7, 9],
            Quality::Min6 => &[0, 3, 7, 9],
            Quality::Add9 => &[0, 2, 4, 7],
            Quality::Dom9 => &[0, 2, 4, 7, 10],
            Quality::SevenSus4 => &[0, 5, 7, 10],
        }
    }
}

/// A chord at concert pitch. Pitch classes: C = 0 … B = 11, relative to
/// the song's own reference (the tuning offset is already removed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub root: u8,
    pub quality: Quality,
    /// Bass note when it is not the root (slash chord), e.g. D/F#.
    pub bass: Option<u8>,
}

/// One stretch of the song with a single chord.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChordSegment {
    pub start_s: f64,
    pub end_s: f64,
    /// `None`: no chord (silence, drums only, unpitched).
    pub chord: Option<Chord>,
    /// The segment starts off the beat (an eighth or sixteenth early or late).
    pub push: bool,
    /// The same chord named in more detail (A6, Aadd9, D6, E9, A7sus4):
    /// same root and bass, the quality possibly one of
    /// [`Quality::EXTENDED`]. Equal to `chord` when no extension fits
    /// better; `None` exactly when `chord` is.
    pub detailed: Option<Chord>,
}

/// A chord that passes by inside a segment, too short to be one (a beat
/// or less): a quick Bm on the way back to D. The segment's chord stays.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PassingChord {
    pub start_s: f64,
    pub end_s: f64,
    pub chord: Chord,
}

/// Key estimate of the whole song.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    /// Pitch class of the tonic.
    pub tonic: u8,
    pub minor: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChordAnalysis {
    /// Contiguous, in time order, covering 0 to the song's duration.
    pub segments: Vec<ChordSegment>,
    pub key: Option<Key>,
    /// Passing chords in time order, each inside one chord segment.
    pub passing: Vec<PassingChord>,
    /// Figures: stretches of at least a beat where a segment's chord gains
    /// a note on its root (A6 for a beat in an A segment). `chord` has the
    /// segment's root, an extension quality and no bass. In time order,
    /// each inside one chord segment, never overlapping a passing chord.
    pub figures: Vec<PassingChord>,
}

/// Detects the chords of a song.
///
/// `stems`: mono stems at `sample_rate` (drums must not be passed).
/// Guitar and keys count most, other less, bass mostly for the root and
/// slash chords. Vocals are used only when every other stem is silent.
/// `beats`: beat times in seconds from the beat tracker, ascending. With
/// fewer than two beats a fixed grid of 0.5 s beats is used and no
/// segment is marked as a push.
/// `offset_cents`: the song's measured tuning offset (−50..50), so pitch
/// classes are centered on the song's own reference.
/// `duration_s`: length of the song; the segments cover 0..duration_s.
/// A duration that is not positive gives no segments.
pub fn detect_chords(
    stems: &[StemAudio],
    sample_rate: u32,
    beats: &[f64],
    offset_cents: f64,
    duration_s: f64,
) -> ChordAnalysis {
    if !(duration_s.is_finite() && duration_s > 0.0) {
        return ChordAnalysis::default();
    }
    let grid = grid::build(beats, duration_s);
    let cells = &grid.cells;
    let offset = if offset_cents.is_finite() {
        offset_cents
    } else {
        0.0
    };
    if sample_rate == 0 || cells.is_empty() {
        return no_chord(duration_s);
    }

    let rate = f64::from(sample_rate);
    let frames = chroma::Frames::new(rate, duration_s);
    let (pitched, vocals): (Vec<&StemAudio>, Vec<&StemAudio>) =
        stems.iter().partition(|s| s.kind != StemKind::Vocals);
    let bass_stem = stems.iter().find(|s| s.kind == StemKind::Bass);
    let (mut analyzed, bass) = std::thread::scope(|scope| {
        let bass = bass_stem
            .map(|b| scope.spawn(move || chroma::bass_notes(b.samples, sample_rate, offset)));
        let analyzed = analyze_all(&pitched, frames, offset);
        let bass = bass
            .map(|h| h.join().expect("bass analysis thread"))
            .unwrap_or_default();
        (analyzed, bass)
    });
    if analyzed.iter().all(chroma::StemFrames::is_silent) {
        analyzed = analyze_all(&vocals, frames, offset);
    }

    let features = chroma::cell_features(cells, frames, &analyzed, &bass);
    let reference = decode::reference_level(&features);
    let scores = decode::scores(&features, reference);
    let penalties = decode::penalties(cells, &features, grid.has_beats);
    let path = decode::viterbi(&scores, &penalties);
    let runs = decode::merged_runs(&path, &scores);
    let mut segments = decode::segments(&runs, cells, &features, grid.has_beats);
    let passes = detail::passing(&runs, &scores, cells, &features, reference, grid.has_beats);
    detail::refine(&mut segments, &runs, &passes, &scores, &features);
    let key = key::estimate(&segments, &features);
    let passing: Vec<PassingChord> = passes
        .iter()
        .filter_map(|p| {
            let (root, quality) = decode::state_chord(p.state)?;
            Some(PassingChord {
                start_s: cells[p.start].start,
                end_s: cells[p.end - 1].end,
                chord: Chord {
                    root,
                    quality,
                    bass: None,
                },
            })
        })
        .collect();
    let figures: Vec<PassingChord> = detail::figures(
        &segments,
        &runs,
        &passes,
        &scores,
        cells,
        &features,
        reference,
        grid.has_beats,
    )
    .iter()
    .map(|f| PassingChord {
        start_s: cells[f.start].start,
        end_s: cells[f.end - 1].end,
        chord: Chord {
            root: segments[f.run].chord.map_or(0, |c| c.root),
            quality: f.quality,
            bass: None,
        },
    })
    .collect();
    if let Some(range) = diag::requested() {
        diag::fill(
            range, cells, &features, reference, &scores, &penalties, &path, &segments, &passing,
            &figures,
        );
    }
    ChordAnalysis {
        segments,
        key,
        passing,
        figures,
    }
}

pub mod diag;

/// Analyzes stems in parallel, one thread each.
fn analyze_all(
    stems: &[&StemAudio],
    frames: chroma::Frames,
    offset: f64,
) -> Vec<chroma::StemFrames> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = stems
            .iter()
            .map(|s| scope.spawn(move || chroma::analyze_stem(s, frames, offset)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("stem analysis thread"))
            .collect()
    })
}

fn no_chord(duration_s: f64) -> ChordAnalysis {
    ChordAnalysis {
        segments: vec![ChordSegment {
            start_s: 0.0,
            end_s: duration_s,
            chord: None,
            push: false,
            detailed: None,
        }],
        key: None,
        passing: Vec::new(),
        figures: Vec::new(),
    }
}

#[cfg(test)]
mod tests;
