//! "Count in to where the music starts": finds where the music really
//! starts in a song that opens with a quiet intro (studio talk, a count-off,
//! silence), so a count-in from the song start can lead straight into the
//! music. Pure function of the original mix and the beat grid.
//!
//! Rule: loudness = RMS in dB of the mono mix over each beat interval
//! `[beat i, beat i+1)`, plus the stretch before the first beat as one more
//! region. Typical level = median over the beat intervals. The music starts
//! in the first region that is within [`THRESHOLD_DB`] of that median and is
//! followed by another such region (so one loud word doesn't count). Inside
//! that region (or up to one beat before it) the start is refined to the
//! first 10 ms window above the threshold (the onset), minus [`PRE_ROLL`],
//! so a pickup into the beat is kept (the count-in still lands on the beat).
//!
//! Only an abrupt start counts: the half second after the onset must be at
//! least [`JUMP_DB`] louder than the second before it. A fade-in or a slow
//! build-up is part of the song and is never skipped. No music start
//! (`None`) either when the song is loud from the beginning or the skip
//! would be under [`MIN_SKIP`].

/// How far below the song's median beat loudness still counts as music.
pub const THRESHOLD_DB: f64 = 18.0;
/// How much louder the music must be than what comes just before it.
pub const JUMP_DB: f64 = 20.0;
/// Shorter skips are ignored (a bit of silence before the first note).
pub const MIN_SKIP: f64 = 1.0;
/// Start this much before the detected onset.
pub const PRE_ROLL: f64 = 0.02;
const WINDOW: f64 = 0.01;
const BEFORE: f64 = 1.0;
const AFTER: f64 = 0.5;

fn rms_db(l: &[f32], r: &[f32]) -> f64 {
    let n = l.len().min(r.len());
    if n == 0 {
        return -200.0;
    }
    let sum: f64 = l[..n]
        .iter()
        .zip(&r[..n])
        .map(|(&a, &b)| {
            let m = 0.5 * (a as f64 + b as f64);
            m * m
        })
        .sum();
    10.0 * (sum / n as f64 + 1e-20).log10()
}

/// Where the music starts (song seconds), or `None` if there is no quiet
/// intro to skip. `mix` is the original mix (stereo, planar) at `sr`;
/// `beats` ascending seconds.
pub fn detect(mix: &[Vec<f32>; 2], sr: u32, beats: &[f64]) -> Option<f64> {
    let c = candidate(mix, sr, beats, THRESHOLD_DB)?;
    (c.jump_db >= JUMP_DB && c.start >= MIN_SKIP).then_some(c.start)
}

/// The loudness rule's answer before the jump and minimum-skip checks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    /// Start (onset minus the pre-roll), song seconds.
    pub start: f64,
    /// First window above the threshold, song seconds.
    pub onset: f64,
    /// Loudness after the onset minus loudness before it (dB).
    pub jump_db: f64,
}

/// The first loud region after a quiet one, refined to its onset (see the
/// module docs). `None` if the song is loud from its first region.
pub fn candidate(
    mix: &[Vec<f32>; 2],
    sr: u32,
    beats: &[f64],
    threshold_db: f64,
) -> Option<Candidate> {
    let len = mix[0].len().min(mix[1].len());
    if beats.len() < 3 || len == 0 || sr == 0 {
        return None;
    }
    let sr_f = sr as f64;
    let idx = |t: f64| ((t.max(0.0) * sr_f).round() as usize).min(len);
    let db = |a: usize, b: usize| rms_db(&mix[0][a..b.max(a)], &mix[1][a..b.max(a)]);
    // Region k: [bounds[k], bounds[k+1]); region 0 is before the first beat.
    let mut bounds = Vec::with_capacity(beats.len() + 1);
    bounds.push(0.0);
    bounds.extend(beats.iter().copied().filter(|b| b.is_finite()));
    let levels: Vec<f64> = bounds
        .windows(2)
        .map(|w| db(idx(w[0]), idx(w[1])))
        .collect();
    // Median over the beat intervals (not the region before the first beat).
    let mut sorted: Vec<f64> = levels[1..].to_vec();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    let th = median - threshold_db;
    // An empty region before a beat at 0 counts as loud (nothing to skip).
    let loud = |k: usize| levels[k] >= th || idx(bounds[k + 1]) <= idx(bounds[k]);
    let k = (0..levels.len() - 1).find(|&k| loud(k) && loud(k + 1))?;
    if k == 0 {
        return None;
    }
    // Refine: the first 10 ms window above the threshold, from up to one
    // beat before region k (a pickup into its beat) to its end.
    let from = bounds[k - 1].max(bounds[k] - (bounds[k + 1] - bounds[k]));
    let (a, b) = (idx(from), idx(bounds[k + 1]));
    let w = ((WINDOW * sr_f).round() as usize).max(1);
    let mut p = a;
    while p + w <= b && db(p, p + w) < th {
        p += w;
    }
    let onset = if p + w <= b { p } else { idx(bounds[k]) };
    let before = db(onset.saturating_sub(idx(BEFORE)), onset);
    let after = db(onset, (onset + idx(AFTER)).min(len));
    Some(Candidate {
        start: (onset as f64 / sr_f - PRE_ROLL).max(from),
        onset: onset as f64 / sr_f,
        jump_db: after - before,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 44_100;

    /// `secs` of audio: a noise-like tone at `quiet_db` until `music_at`,
    /// then at -12 dB (with a short dip every beat so it isn't flat).
    fn song(secs: f64, music_at: f64, quiet_db: f64) -> [Vec<f32>; 2] {
        let n = (secs * SR as f64) as usize;
        let mut seed = 1u32;
        let v: Vec<f32> = (0..n)
            .map(|i| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (seed >> 9) as f32 / (1u32 << 23) as f32 * 2.0 - 1.0;
                let t = i as f64 / SR as f64;
                let db = if t < music_at { quiet_db } else { -12.0 };
                noise * 10f32.powf(db as f32 / 20.0) * 1.7
            })
            .collect();
        [v.clone(), v]
    }

    fn grid(first: f64, step: f64, until: f64) -> Vec<f64> {
        let mut b = vec![];
        let mut t = first;
        while t < until {
            b.push(t);
            t += step;
        }
        b
    }

    #[test]
    fn skips_quiet_talk_before_the_music() {
        // Talk at -55 dB for 8.4 s with beats on it, then the riff.
        let mix = song(60.0, 8.4, -55.0);
        let beats = grid(0.5, 0.75, 60.0);
        let ms = detect(&mix, SR, &beats).unwrap();
        assert!((ms - (8.4 - PRE_ROLL)).abs() < 0.011, "{ms}");
        // No beats on the talk: same answer from the region before the first beat.
        let beats = grid(8.5, 0.75, 60.0);
        let ms = detect(&mix, SR, &beats).unwrap();
        assert!((ms - (8.4 - PRE_ROLL)).abs() < 0.011, "{ms}");
    }

    #[test]
    fn keeps_a_quiet_intro_that_is_part_of_the_song() {
        // A soft intro 12 dB below the rest is music, not talk.
        let mix = song(60.0, 10.0, -24.0);
        assert_eq!(detect(&mix, SR, &grid(0.5, 0.75, 60.0)), None);
        // Loud from the start.
        let mix = song(60.0, 0.0, -12.0);
        assert_eq!(detect(&mix, SR, &grid(0.0, 0.75, 60.0)), None);
        // Under a second of silence before the first note: not worth a jump.
        let mix = song(60.0, 0.6, -90.0);
        assert_eq!(detect(&mix, SR, &grid(0.6, 0.75, 60.0)), None);
    }

    #[test]
    fn a_fade_in_is_never_skipped() {
        // An organ fading in from -80 dB to -12 dB over 8 s.
        let n = 60 * SR as usize;
        let mut seed = 7u32;
        let v: Vec<f32> = (0..n)
            .map(|i| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (seed >> 9) as f32 / (1u32 << 23) as f32 * 2.0 - 1.0;
                let t = i as f64 / SR as f64;
                let db = -80.0 + 68.0 * (t / 8.0).min(1.0);
                noise * 10f32.powf(db as f32 / 20.0) * 1.7
            })
            .collect();
        let mix = [v.clone(), v];
        let beats = grid(0.5, 0.75, 60.0);
        let c = candidate(&mix, SR, &beats, THRESHOLD_DB).unwrap();
        assert!(c.start > 1.0 && c.jump_db < JUMP_DB, "{c:?}");
        assert_eq!(detect(&mix, SR, &beats), None);
    }

    #[test]
    fn one_loud_word_does_not_count_as_the_music() {
        let mut mix = song(60.0, 8.0, -55.0);
        // A single loud beat interval at 3.0..3.75 s.
        let (a, b) = (3 * SR as usize, (3.75 * SR as f64) as usize);
        for ch in mix.iter_mut() {
            for x in &mut ch[a..b] {
                *x *= 10f32.powf(40.0 / 20.0);
            }
        }
        let ms = detect(&mix, SR, &grid(0.0, 0.75, 60.0)).unwrap();
        assert!(ms > 7.5, "{ms}");
    }

    /// Real songs: `STEMX_MUSIC_START_SONGS="a.mp3;b.mp3" cargo test -p
    /// stem-extractor --lib music_start::tests::real_songs -- --ignored --nocapture`
    /// (beats from `models/onnx/beat_this_final0.onnx` on the CPU).
    #[test]
    #[ignore]
    fn real_songs() {
        use stemx_separation::{BeatModel, DevicePref};
        let songs = std::env::var("STEMX_MUSIC_START_SONGS").expect("STEMX_MUSIC_START_SONGS");
        let model = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../models/onnx/beat_this_final0.onnx");
        let mut m = BeatModel::load(&model, DevicePref::Cpu, Some(8), &mut |_| {}).unwrap();
        for path in songs.split(';').filter(|p| !p.trim().is_empty()) {
            let audio = stemx_io::decode_file(std::path::Path::new(path)).unwrap();
            let audio = stemx_io::resample(&audio, SR).unwrap();
            let l = audio.channels[0].clone();
            let r = audio.channels.get(1).cloned().unwrap_or_else(|| l.clone());
            #[derive(Debug)]
            struct E(#[allow(dead_code)] String);
            impl From<stemx_beats::Error> for E {
                fn from(e: stemx_beats::Error) -> Self {
                    Self(e.to_string())
                }
            }
            let grid = stemx_beats::track(&[&l, &r], SR, |inp: &[f32], shape| {
                m.run(inp, shape).map_err(|e| E(e.to_string()))
            })
            .unwrap();
            let mix = [l, r];
            let c = candidate(&mix, SR, &grid.beats, THRESHOLD_DB);
            println!(
                "{path}: first beat {:.2}s, music start {:?}, candidate {c:?}, 12 dB {:?}, 24 dB {:?}",
                grid.beats.first().copied().unwrap_or(f64::NAN),
                detect(&mix, SR, &grid.beats),
                candidate(&mix, SR, &grid.beats, 12.0).map(|c| (c.start, c.jump_db)),
                candidate(&mix, SR, &grid.beats, 24.0).map(|c| (c.start, c.jump_db)),
            );
        }
    }

    #[test]
    fn no_grid_no_music_start() {
        let mix = song(10.0, 5.0, -55.0);
        assert_eq!(detect(&mix, SR, &[]), None);
        assert_eq!(detect(&mix, SR, &[1.0, 2.0]), None);
    }
}
