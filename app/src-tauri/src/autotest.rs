// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Debug-only headless check of the whole flow, driven through the same
//! backend calls the commands use:
//!
//! ```text
//! SHREDLAB_AUTOTEST=<song> [SHREDLAB_AUTOTEST_OUT=<dir>] [SHREDLAB_AUTOTEST_EXIT=1] pnpm --dir app tauri dev
//! ```
//!
//! quality High -> open -> separate (unless cached) -> beats -> play 3 s ->
//! re-open (cache, High) -> cancel a Standard separation -> switch to
//! Standard (re-separates) -> back to High (instant, from the cache) ->
//! "All but guitar" export in P1 mode (checked against original - guitar)
//! -> the same with click + count-in (unstretched and at 80 %)
//! -> a loop A-B export with count-in (played once, faded edges) -> the
//! music start: whole-song count-in export and play from 0 start there
//! ("Count in to where the music starts"; off = from 0)
//! -> Refine (guitar with the stereo exception "left" + EQ, bass EQ): all on
//! = guitar ON except left, and "All but guitar" = guitar OFF except left
//! (only the left guitar plays), exports vs the offline refine reference,
//! stretched refined export -> strength (exact and
//! Smooth: plain sum of all stems vs the original)
//! -> +6 dB boost export (no clipping) -> WAV/FLAC/MP3 exports. Each step
//! is logged with its timing; the last line is `AUTOTEST PASS` or
//! `AUTOTEST FAIL: ...`. The saved settings (app settings, and the song's
//! refine and strength) are restored at the end. Use a
//! fresh `SHREDLAB_DATA_DIR` to time both separations.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::backend::Backend;
use crate::log;
use crate::types::*;

type Event = (String, serde_json::Value);
type Tap = Arc<dyn Fn(&str, &serde_json::Value) + Send + Sync>;

static RX: OnceLock<Mutex<Option<Receiver<Event>>>> = OnceLock::new();

/// Event observer for the sink, if the autotest is enabled.
pub fn event_tap() -> Option<Tap> {
    std::env::var_os("SHREDLAB_AUTOTEST")?;
    let (tx, rx): (Sender<Event>, Receiver<Event>) = channel();
    RX.get_or_init(|| Mutex::new(Some(rx)));
    let tx = Mutex::new(tx);
    Some(Arc::new(move |name, v| {
        if name != "playback-position"
            && name != "separation-progress"
            && name != "export-progress"
            && name != "strength-progress"
            && name != "refine-field"
        {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send((name.to_string(), v.clone()));
            }
        }
    }))
}

pub fn maybe_start(backend: Arc<Backend>, app: tauri::AppHandle) {
    let Some(song) = std::env::var_os("SHREDLAB_AUTOTEST") else {
        return;
    };
    let Some(rx) = RX.get().and_then(|m| m.lock().ok()?.take()) else {
        return;
    };
    let out = std::env::var_os("SHREDLAB_AUTOTEST_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| backend.data_dir().join("autotest"));
    let exit = std::env::var_os("SHREDLAB_AUTOTEST_EXIT").is_some();
    let _ = std::thread::Builder::new()
        .name("shredlab-autotest".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            let saved = backend.app_settings();
            let result = run(&backend, &PathBuf::from(song), &out, &rx);
            let _ = backend.set_app_settings(AppSettingsUpdate {
                model_id: saved.model_choice,
                quality: saved.quality_choice,
            });
            let _ = backend.set_count_in_to_music_start(saved.count_in_to_music_start);
            let code = match result {
                Ok(()) => {
                    log!("AUTOTEST PASS");
                    0
                }
                Err(e) => {
                    log!("AUTOTEST FAIL: {e}");
                    1
                }
            };
            if exit {
                app.exit(code);
            }
        });
}

fn wait_for(
    rx: &Receiver<Event>,
    timeout: Duration,
    mut pred: impl FnMut(&str, &serde_json::Value) -> Option<Result<(), String>>,
) -> Result<(), String> {
    let t0 = Instant::now();
    loop {
        let left = timeout
            .checked_sub(t0.elapsed())
            .ok_or("timed out waiting for event")?;
        let (name, v) = rx
            .recv_timeout(left)
            .map_err(|_| "timed out waiting for event".to_string())?;
        log!("autotest: event {name} {v}");
        if let Some(r) = pred(&name, &v) {
            return r;
        }
    }
}

fn set_quality(b: &Arc<Backend>, q: Quality) -> Result<AppSettingsInfo, String> {
    let cur = b.app_settings();
    let s = b.set_app_settings(AppSettingsUpdate {
        model_id: cur.model_choice,
        quality: Some(q),
    })?;
    if s.quality != q {
        return Err(format!("quality is {:?} after setting {q:?}", s.quality));
    }
    Ok(s)
}

/// Starts a separation with the current settings and waits for it to
/// finish. Returns (seconds, separated_with, original_available).
fn separate(
    b: &Arc<Backend>,
    rx: &Receiver<Event>,
    beats_ready: &mut bool,
) -> Result<(f64, SeparatedWith, bool), String> {
    let t = Instant::now();
    let started = b.start_separation()?;
    log!(
        "autotest: separation started: {} ({}) {:?} on {}",
        started.model_name,
        started.model_id,
        started.quality,
        started.device_label
    );
    let mut done = None;
    wait_for(rx, Duration::from_secs(1800), |name, v| match name {
        "separation-done" => {
            done = Some(v.clone());
            Some(Ok(()))
        }
        "separation-error" => Some(Err(format!("separation failed: {v}"))),
        "beats-ready" => {
            *beats_ready = true;
            None
        }
        _ => None,
    })?;
    let secs = t.elapsed().as_secs_f64();
    let v = done.unwrap_or_default();
    let with: SeparatedWith = serde_json::from_value(v["separated_with"].clone())
        .map_err(|e| format!("bad separation-done payload: {e}"))?;
    let orig = v["original_available"] == true;
    log!(
        "autotest: separation done in {secs:.1}s: {} {:?} on {}, original_available {orig}",
        with.model_id,
        with.quality,
        with.device_label
    );
    Ok((secs, with, orig))
}

fn mix_all_but(without: &str, boost: Option<(&str, f32)>) -> Vec<ExportStem> {
    STEMS
        .iter()
        .filter(|s| **s != without)
        .map(|s| ExportStem {
            stem: s.to_string(),
            volume: match boost {
                Some((n, v)) if n == *s => v,
                _ => 1.0,
            },
            on: true,
        })
        .collect()
}

/// Minimum SNR (dB) of an unstretched export against its reference mix.
/// The export limiter caps it well below the 24-bit rounding limit, more so
/// on loud songs (Mr. Crowley's solo export: 37 dB). The wrong mix scores
/// around 16 dB, so 30 dB still tells them apart.
const MIX_SNR_MIN: f64 = 30.0;

fn export_wait(b: &Arc<Backend>, rx: &Receiver<Event>, req: ExportRequest) -> Result<f64, String> {
    let t = Instant::now();
    b.export_mix(req)?;
    wait_for(rx, Duration::from_secs(600), |name, v| match name {
        "export-done" => Some(Ok(())),
        "export-error" => Some(Err(format!("export failed: {v}"))),
        _ => None,
    })?;
    Ok(t.elapsed().as_secs_f64())
}

fn read_wav(path: &Path) -> Result<[Vec<f32>; 2], String> {
    let a = shredlab_io::decode_file(path).map_err(|e| e.to_string())?;
    let mut ch = a.channels.into_iter();
    let l = ch.next().unwrap_or_default();
    let r = ch.next().unwrap_or_else(|| l.clone());
    Ok([l, r])
}

/// SNR (dB) of `x` against `reference` after the least-squares gain
/// (export may scale the mix down to avoid clipping), and that gain.
fn snr_db(x: &[Vec<f32>; 2], reference: &[Vec<f32>; 2]) -> (f64, f64) {
    let (mut xr, mut rr) = (0.0f64, 0.0f64);
    for c in 0..2 {
        for (a, b) in x[c].iter().zip(reference[c].iter()) {
            xr += *a as f64 * *b as f64;
            rr += *b as f64 * *b as f64;
        }
    }
    let k = if rr > 0.0 { xr / rr } else { 1.0 };
    let (mut sig, mut err) = (0.0f64, 0.0f64);
    for c in 0..2 {
        for (a, b) in x[c].iter().zip(reference[c].iter()) {
            let r = k * *b as f64;
            sig += r * r;
            err += (*a as f64 - r).powi(2);
        }
    }
    (10.0 * (sig / err.max(1e-30)).log10(), k)
}

/// Rising edges (|x| > `thr`) after at least `gap` quiet samples.
fn onsets(x: &[f32], thr: f32, gap: usize) -> Vec<usize> {
    let mut out = vec![];
    let mut quiet = gap + 1;
    for (i, &v) in x.iter().enumerate() {
        if v.abs() > thr {
            if quiet > gap {
                out.push(i);
            }
            quiet = 0;
        } else {
            quiet += 1;
        }
    }
    out
}

fn window_peak(x: &[f32], from: usize, len: usize) -> f32 {
    x[from.min(x.len())..(from + len).min(x.len())]
        .iter()
        .fold(0.0f32, |m, v| m.max(v.abs()))
}

fn median(mut v: Vec<f32>) -> f32 {
    v.sort_by(f32::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

/// Click + count-in export of the "All but guitar" mix (`plain` is its
/// unstretched export without them): the count-in (4 clicks, 4 per bar)
/// comes first over silence, then the same music with a click on each
/// beat; at 80 % speed the count-in is respaced. No clipping.
fn check_click_export(
    b: &Arc<Backend>,
    rx: &Receiver<Event>,
    out: &Path,
    plain: &[Vec<f32>; 2],
) -> Result<(), String> {
    let grid = b.get_beats()?.ok_or("no beat grid")?;
    b.set_meter_overrides(MeterOverridesPayload {
        overrides: vec![],
        downbeats: grid.downbeats.clone(),
    });
    // This check compares against the whole song from 0:00.
    b.set_count_in_to_music_start(false)?;
    b.set_click(ClickState {
        enabled: true,
        count_in_enabled: true,
        count_in_beats: 4,
        subdivision: 1,
        beats_per_bar: 4,
        volume: 0.5,
        count_in_each_repeat: false,
    });
    let req = |file: &Path, speed: f64, click: bool| ExportRequest {
        path: file.display().to_string(),
        format: ExportFormat::Wav,
        stems: mix_all_but("guitar", None),
        solo: false,
        speed_percent: speed,
        semitones: 0.0,
        cents: 0.0,
        click,
        count_in: click,
        start_sec: None,
        end_sec: None,
    };
    let res = (|| {
        for speed in [100.0, 80.0] {
            let plain = if speed == 100.0 {
                plain.clone()
            } else {
                let file = out.join("autotest-no-click-80.wav");
                let _ = std::fs::remove_file(&file);
                export_wait(b, rx, req(&file, speed, false))?;
                read_wav(&file)?
            };
            let file = out.join(format!("autotest-click-{speed}.wav"));
            let _ = std::fs::remove_file(&file);
            let secs = export_wait(b, rx, req(&file, speed, true))?;
            let got = read_wav(&file)?;
            let sr = crate::backend::RATE as f64;
            // The count-in leads into the first beat: clicks over silence
            // first, then any music before that beat under the last clicks.
            let cfg = crate::audio::ClickConfig {
                beats: grid.beats.clone(),
                count_in_beats: 4,
                beats_per_bar: 4,
                ..Default::default()
            };
            let plan = crate::audio::click::plan_count_in(&cfg, 0.0, speed / 100.0)
                .ok_or("no count-in plan")?;
            let interval = plan.interval * sr;
            let ci = got[0].len().saturating_sub(plain[0].len());
            let ci_want = (plan.audio_start() * sr).round() as usize;
            let silent: Vec<usize> = (0..4)
                .map(|j| (plan.click_time(j) * sr).round() as usize)
                .filter(|&t| t + 2000 <= ci_want)
                .collect();
            let peak = got
                .iter()
                .flat_map(|c| c.iter())
                .fold(0.0f32, |m, v| m.max(v.abs()));
            let on = onsets(&got[0][..ci.min(got[0].len())], 1e-3, 2000);
            log!(
                "autotest: click+count-in export at {speed}% {secs:.1}s: count-in {ci} samples (want {ci_want}), onsets {on:?}, peak {peak:.4}"
            );
            if ci.abs_diff(ci_want) > 1 {
                return Err(format!("count-in is {ci} samples, want {ci_want}"));
            }
            if on.len() != silent.len() || on.iter().zip(&silent).any(|(&o, &w)| o.abs_diff(w) > 2)
            {
                return Err(format!(
                    "count-in clicks at {on:?}, want {silent:?} (interval {interval:.1})"
                ));
            }
            if on.len() >= 2
                && window_peak(&got[0], on[0], 1000) < 1.3 * window_peak(&got[0], on[1], 1000)
            {
                return Err("first count-in click (a bar before the start) is not accented".into());
            }
            if peak > 1.0 {
                return Err(format!("click export clips (peak {peak})"));
            }
            if speed != 100.0 {
                continue;
            }
            // Unstretched: what the click adds is the click alone, on the beats.
            let diff: Vec<f32> = got[0][ci..]
                .iter()
                .zip(&plain[0])
                .map(|(a, b)| a - b)
                .collect();
            let mut at_beat = vec![];
            let mut between = vec![];
            let mut late = 0;
            for w in grid.beats.windows(2) {
                let f = (w[0] * sr).round() as usize;
                let mid = ((w[0] + w[1]) / 2.0 * sr).round() as usize;
                if mid + 300 >= diff.len() {
                    break;
                }
                at_beat.push(window_peak(&diff, f, 300));
                between.push(window_peak(&diff, mid, 300));
                let first = (f.saturating_sub(50)..f + 300).find(|&i| diff[i].abs() > 0.02);
                if first.is_none_or(|i| i.abs_diff(f) > 30) {
                    late += 1;
                }
            }
            let (mb, mm) = (median(at_beat.clone()), median(between));
            log!(
                "autotest: click on beats: median peak {mb:.3} at beats vs {mm:.4} between, {late} of {} off the beat",
                at_beat.len()
            );
            if mb < 0.15 || mm > 0.02 || late * 10 > at_beat.len() {
                return Err("exported click is not on the beats".into());
            }
        }
        Ok(())
    })();
    b.set_click(ClickState::default());
    res
}

/// Loop A-B export with count-in (8 beats from the first beat after 30 s,
/// played once): count-in clicks over silence, then exactly the range of
/// `plain` (the unstretched whole-song export of the same mix), with faded
/// edges. Then the music start: a whole-song count-in export and `play`
/// from 0 start at `BeatGrid.music_start` with the setting on, at 0 with
/// it off.
fn check_range_export(
    b: &Arc<Backend>,
    rx: &Receiver<Event>,
    out: &Path,
    plain: &[Vec<f32>; 2],
) -> Result<(), String> {
    let grid = b.get_beats()?.ok_or("no beat grid")?;
    let sr = crate::backend::RATE as f64;
    b.set_click(ClickState {
        enabled: false,
        count_in_enabled: true,
        count_in_beats: 4,
        subdivision: 1,
        beats_per_bar: 4,
        volume: 0.5,
        count_in_each_repeat: false,
    });
    let cfg = crate::audio::ClickConfig {
        beats: grid.beats.clone(),
        count_in_beats: 4,
        beats_per_bar: 4,
        ..Default::default()
    };
    let req = |file: &Path, start: Option<f64>, end: Option<f64>| ExportRequest {
        path: file.display().to_string(),
        format: ExportFormat::Wav,
        stems: mix_all_but("guitar", None),
        solo: false,
        speed_percent: 100.0,
        semitones: 0.0,
        cents: 0.0,
        click: false,
        count_in: true,
        start_sec: start,
        end_sec: end,
    };
    let res = (|| {
        b.set_count_in_to_music_start(true)?;
        let k = grid.beats.partition_point(|&t| t < 30.0);
        let (a, z) = match (grid.beats.get(k), grid.beats.get(k + 8)) {
            (Some(&a), Some(&z)) => (a, z),
            _ => return Err("song too short for the loop export".into()),
        };
        b.set_loop(LoopState {
            a_sec: Some(a),
            b_sec: Some(z),
            enabled: true,
        });
        let file = out.join("autotest-loop-count-in.wav");
        let _ = std::fs::remove_file(&file);
        let secs = export_wait(b, rx, req(&file, Some(a), Some(z)))?;
        let got = read_wav(&file)?;
        let plan = crate::audio::click::plan_count_in(&cfg, a, 1.0).ok_or("no count-in plan")?;
        let lead = (plan.audio_start() * sr).round() as usize;
        let (ai, zi) = ((a * sr).round() as usize, (z * sr).round() as usize);
        let want = lead + zi - ai;
        let on = onsets(&got[0][..lead.min(got[0].len())], 1e-3, 2000);
        let fade = (crate::audio::EDGE_FADE_SECS * sr).round() as usize + 2;
        let n = got[0].len().min(want);
        let body = |x: &[Vec<f32>; 2], from: usize, len: usize| {
            [
                x[0][from..from + len].to_vec(),
                x[1][from..from + len].to_vec(),
            ]
        };
        let len = n.saturating_sub(lead + 2 * fade);
        let (snr, k) = snr_db(&body(&got, lead + fade, len), &body(plain, ai + fade, len));
        let edge_in = got[0][lead].abs().max(got[1][lead].abs());
        let edge_out = got[0][n - 1].abs().max(got[1][n - 1].abs());
        log!(
            "autotest: loop {a:.2}-{z:.2}s count-in export {secs:.1}s: {} samples (want {want}), count-in interval {:.3}s, onsets {on:?}, vs plain range {snr:.1} dB (gain {k:.3}), edges {edge_in:.5} / {edge_out:.5}",
            got[0].len(),
            plan.interval
        );
        if got[0].len().abs_diff(want) > 1 {
            return Err(format!(
                "loop export is {} samples, want {want}",
                got[0].len()
            ));
        }
        if on.len() != 4 {
            return Err(format!("loop export count-in clicks {on:?}, want 4"));
        }
        if snr < MIX_SNR_MIN {
            return Err(format!(
                "loop export music differs from the song ({snr:.1} dB)"
            ));
        }
        if edge_in > 1e-3 || edge_out > 1e-3 {
            return Err("loop export edges are not faded".into());
        }
        b.set_loop(LoopState::default());

        // Whole song with count-in: starts at the music start, if any.
        let file = out.join("autotest-music-start.wav");
        let _ = std::fs::remove_file(&file);
        let secs = export_wait(b, rx, req(&file, None, None))?;
        let got = read_wav(&file)?;
        let start = grid.music_start.unwrap_or(0.0);
        let plan =
            crate::audio::click::plan_count_in(&cfg, start, 1.0).ok_or("no count-in plan")?;
        let lead = (plan.audio_start() * sr).round() as usize;
        let want = lead + plain[0].len() - (start * sr).round() as usize;
        log!(
            "autotest: music start {:?}: whole-song count-in export {secs:.1}s, {} samples (want {want}), count-in interval {:.3}s ({:.1} BPM)",
            grid.music_start,
            got[0].len(),
            plan.interval,
            60.0 / plan.interval
        );
        if got[0].len().abs_diff(want) > 1 {
            return Err(format!(
                "music-start export is {} samples, want {want}",
                got[0].len()
            ));
        }
        // Playback from 0 jumps there too (the playhead shows it).
        b.seek(0.0);
        b.play()?;
        let p = b.position().ok_or("no engine")?;
        b.pause();
        log!("autotest: play from 0 starts at {:.3}s", p.seconds);
        if (p.seconds - start).abs() > 1e-3 {
            return Err(format!(
                "play from 0 started at {}, want {start}",
                p.seconds
            ));
        }
        // Setting off: from 0 as before.
        b.set_count_in_to_music_start(false)?;
        b.seek(0.0);
        b.play()?;
        let p = b.position().ok_or("no engine")?;
        b.pause();
        if p.seconds.abs() > 1e-3 {
            return Err(format!("setting off: play from 0 started at {}", p.seconds));
        }
        Ok(())
    })();
    b.set_loop(LoopState::default());
    b.set_click(ClickState::default());
    res
}

fn wait_strength(rx: &Receiver<Event>) -> Result<f64, String> {
    let t = Instant::now();
    wait_for(rx, Duration::from_secs(120), |name, v| match name {
        "strength-done" => Some(Ok(())),
        "strength-error" if v["cancelled"] != true => Some(Err(format!("strength failed: {v}"))),
        _ => None,
    })?;
    Ok(t.elapsed().as_secs_f64())
}

fn plain_req(path: &Path, stems: Vec<ExportStem>, solo: bool, speed: f64) -> ExportRequest {
    ExportRequest {
        path: path.display().to_string(),
        format: ExportFormat::Wav,
        stems,
        solo,
        speed_percent: speed,
        semitones: 0.0,
        cents: 0.0,
        click: false,
        count_in: false,
        start_sec: None,
        end_sec: None,
    }
}

/// Plays 4 s from 60 s and logs the producer thread's CPU load (render time
/// per audio time, one core) over that interval.
fn measure_load(b: &Arc<Backend>, what: &str) {
    let Some(s0) = b.debug_engine_stats() else {
        return;
    };
    b.seek(60.0);
    if b.play().is_err() {
        return;
    }
    std::thread::sleep(Duration::from_secs(4));
    b.pause();
    if let Some(s1) = b.debug_engine_stats() {
        let ns = s1.render_nanos.saturating_sub(s0.render_nanos) as f64;
        let fr = s1.rendered_frames.saturating_sub(s0.rendered_frames) as f64;
        let audio_ns = fr * 1e9 / s1.device_sample_rate.max(1) as f64;
        log!(
            "autotest: CPU {what}: {:.2} % of one core, underruns {} -> {}",
            if audio_ns > 0.0 {
                100.0 * ns / audio_ns
            } else {
                0.0
            },
            s0.underruns,
            s1.underruns
        );
    }
}

/// Refine and strength, through the same calls as the UI. `plain` is the
/// unrefined "All but guitar" export.
fn check_refine_strength(
    b: &Arc<Backend>,
    rx: &Receiver<Event>,
    out: &Path,
    plain: &[Vec<f32>; 2],
) -> Result<(), String> {
    use shredlab_dsp::refine::{eq_preset, PanParams, RefineParams};
    for stem in STEMS {
        b.set_refine(stem, RefineParams::default())?;
    }
    if b.set_strength(StrengthSetting::default())? > 0 {
        wait_strength(rx)?;
    }
    b.set_mix(
        STEMS
            .iter()
            .map(|s| StemMixState {
                stem: s.to_string(),
                on: *s != "guitar",
                volume: 1.0,
                solo: false,
            })
            .collect(),
    );
    measure_load(b, "unrefined (all but guitar)");
    let guitar = RefineParams {
        pan: PanParams::left(),
        eq: eq_preset("guitar-focus").ok_or("no preset")?.eq,
    };
    let bass = RefineParams {
        eq: eq_preset("bass-notes").ok_or("no preset")?.eq,
        ..RefineParams::default()
    };
    b.set_refine("guitar", guitar)?;
    b.set_refine("bass", bass)?;
    b.set_refine_analysis(Some("guitar".into()));
    measure_load(
        b,
        "refined (guitar off except left + EQ, bass EQ, field analysis)",
    );
    b.set_speed(80);
    measure_load(b, "refined, 80% speed");
    b.set_speed(100);
    // The export carries each row's switch: guitar on or off, everything
    // else on. Reference rows: every stem heard, guitar per its switch.
    let rows = |guitar_on: bool| -> Vec<(&'static str, bool)> {
        STEMS
            .iter()
            .map(|s| (*s, *s != "guitar" || guitar_on))
            .collect()
    };
    let stems_with = |guitar_on: bool| -> Vec<ExportStem> {
        STEMS
            .iter()
            .map(|s| ExportStem {
                stem: s.to_string(),
                volume: 1.0,
                on: *s != "guitar" || guitar_on,
            })
            .collect()
    };
    let mut got_off = None;
    for guitar_on in [true, false] {
        let what = if guitar_on {
            "guitar ON except left"
        } else {
            "guitar OFF except left"
        };
        let file = out.join(format!(
            "autotest-refine-guitar-{}-except-left.wav",
            if guitar_on { "on" } else { "off" }
        ));
        let _ = std::fs::remove_file(&file);
        let secs = export_wait(b, rx, plain_req(&file, stems_with(guitar_on), false, 100.0))?;
        let got = read_wav(&file)?;
        let want = b
            .debug_refine_reference(&rows(guitar_on))
            .ok_or("no refine reference")?;
        // The same exception with the switch the other way must not match.
        let flipped = b
            .debug_refine_reference(&rows(!guitar_on))
            .ok_or("no refine reference")?;
        let (snr, k) = snr_db(&got, &want);
        let (snr_flip, _) = snr_db(&got, &flipped);
        let (snr_plain, _) = snr_db(&got, plain);
        log!(
            "autotest: refined export, {what} {secs:.1}s: vs refine reference {snr:.1} dB (gain {k:.3}), vs switch flipped {snr_flip:.1} dB, vs unrefined all-but-guitar {snr_plain:.1} dB"
        );
        if snr < MIX_SNR_MIN || snr_flip > snr - 3.0 || snr_plain > snr - 3.0 {
            return Err(format!(
                "refined export ({what}) does not match the reference"
            ));
        }
        if !guitar_on {
            got_off = Some(got);
        }
    }
    let got = got_off.ok_or("no export")?;
    // Stretched, refined: right length, no clipping.
    let file = out.join("autotest-refine-80.wav");
    let _ = std::fs::remove_file(&file);
    let secs = export_wait(b, rx, plain_req(&file, stems_with(false), false, 80.0))?;
    let st = read_wav(&file)?;
    let peak = st
        .iter()
        .flat_map(|c| c.iter())
        .fold(0.0f32, |m, v| m.max(v.abs()));
    let ratio = st[0].len() as f64 / got[0].len() as f64;
    log!("autotest: refined export at 80% {secs:.1}s: length ratio {ratio:.4}, peak {peak:.3}");
    if (ratio - 1.25).abs() > 0.002 || peak > 1.0 {
        return Err("stretched refined export is wrong".into());
    }
    b.set_refine_analysis(None);
    b.set_refine("guitar", RefineParams::default())?;
    b.set_refine("bass", RefineParams::default())?;

    // Strength: the plain sum of all stems (solo all) vs the original.
    let all: Vec<ExportStem> = mix_all_but("", None);
    let sum_vs_original = |name: &str| -> Result<f64, String> {
        let file = out.join(format!("autotest-strength-{name}.wav"));
        let _ = std::fs::remove_file(&file);
        export_wait(b, rx, plain_req(&file, all.clone(), true, 100.0))?;
        let got = read_wav(&file)?;
        let [orig, _] = b.debug_mix_references(&STEMS).ok_or("no reference")?;
        Ok(snr_db(&got, &orig).0)
    };
    let natural = sum_vs_original("natural")?;
    let job = b.set_strength(StrengthSetting {
        level: 2,
        smooth: false,
    })?;
    if job == 0 {
        return Err("strength did not start".into());
    }
    let secs = wait_strength(rx)?;
    let medium = sum_vs_original("medium")?;
    b.set_strength(StrengthSetting {
        level: 2,
        smooth: true,
    })?;
    let secs_s = wait_strength(rx)?;
    let smooth = sum_vs_original("medium-smooth")?;
    log!(
        "autotest: strength: sum of stems vs original: Natural {natural:.1} dB, Medium {medium:.1} dB ({secs:.2}s), Medium+Smooth {smooth:.1} dB ({secs_s:.2}s)"
    );
    if medium < MIX_SNR_MIN || medium < natural + 5.0 || smooth > medium - 5.0 {
        return Err("strength: derived stems don't sum to the original (or Smooth does)".into());
    }
    Ok(())
}

fn run(b: &Arc<Backend>, song: &Path, out: &Path, rx: &Receiver<Event>) -> Result<(), String> {
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let path = song.display().to_string();
    log!("autotest: song {path}");
    let s = set_quality(b, Quality::High)?;
    log!(
        "autotest: settings: model {:?} ({} installed), quality {:?} (default {:?}) on {}",
        s.model_id,
        s.models.len(),
        s.quality,
        s.default_quality,
        s.device_label
    );

    let t = Instant::now();
    let info = b.open_song(&path)?;
    log!(
        "autotest: open_song {:.2}s: \"{}\" by {:?}, {:.1}s, {} Hz, stems_cached {} ({:?}), beats_ready {}",
        t.elapsed().as_secs_f64(),
        info.title,
        info.artist,
        info.duration_sec,
        info.sample_rate,
        info.stems_cached,
        info.separated_with,
        info.beats_ready
    );
    let w = b.get_waveform(1200)?;
    if w.peaks.len() != 1200 {
        return Err("waveform has wrong length".into());
    }
    log!(
        "autotest: waveform max peak {:.3}",
        w.peaks.iter().cloned().fold(0.0, f32::max)
    );

    let mut beats_ready = info.beats_ready;
    let mut timings: Vec<(Quality, f64)> = Vec::new();
    let high_cached = info
        .separated_with
        .as_ref()
        .is_some_and(|w| w.quality == Quality::High);
    if !high_cached {
        b.play()?;
        let (secs, with, orig) = separate(b, rx, &mut beats_ready)?;
        log!(
            "autotest: position now {:.1}s",
            b.position().map_or(0.0, |p| p.seconds)
        );
        b.pause();
        if with.quality != Quality::High || !orig {
            return Err(format!("High separation gave {with:?}, original {orig}"));
        }
        timings.push((Quality::High, secs));
    }
    if !beats_ready {
        let t = Instant::now();
        wait_for(rx, Duration::from_secs(300), |name, _| {
            (name == "beats-ready").then_some(Ok(()))
        })?;
        log!(
            "autotest: beats-ready after {:.1}s more",
            t.elapsed().as_secs_f64()
        );
    }
    let grid = b.get_beats()?.ok_or("no beat grid")?;
    log!(
        "autotest: grid {} beats, {} downbeats, bpm {:?}",
        grid.beats.len(),
        grid.downbeats.len(),
        grid.bpm
    );
    b.set_meter_overrides(MeterOverridesPayload {
        overrides: vec![],
        downbeats: grid.downbeats.clone(),
    });
    b.set_click(ClickState {
        enabled: true,
        count_in_enabled: true,
        count_in_beats: 4,
        subdivision: 1,
        beats_per_bar: 4,
        volume: 0.5,
        count_in_each_repeat: false,
    });

    // Play a few seconds (with count-in) from 30 s: "All but guitar"
    // (P1 mode) with bass boosted +6 dB (exercises the playback limiter).
    b.set_mix(
        STEMS
            .iter()
            .map(|s| StemMixState {
                stem: s.to_string(),
                on: *s != "guitar",
                volume: if *s == "bass" { 1.995 } else { 1.0 },
                solo: false,
            })
            .collect(),
    );
    b.seek(30.0);
    b.play()?;
    for _ in 0..8 {
        std::thread::sleep(Duration::from_millis(500));
        if let Some(p) = b.position() {
            log!(
                "autotest: position {:.2}s playing {} count-in {}",
                p.seconds,
                p.playing,
                p.count_in_beats_left
            );
        }
    }
    b.pause();
    b.set_click(ClickState::default());

    // Re-open: must come from the cache, with the High set.
    let t = Instant::now();
    let again = b.open_song(&path)?;
    let reopen = t.elapsed().as_secs_f64();
    log!(
        "autotest: re-open {reopen:.2}s, stems_cached {}, separated_with {:?}, original_available {}, beats_ready {}, bpm {:?}",
        again.stems_cached,
        again.separated_with,
        again.original_available,
        again.beats_ready,
        again.bpm
    );
    if !again.stems_cached || !again.beats_ready {
        return Err("re-open did not use the cache".into());
    }
    if again.separated_with.as_ref().map(|w| w.quality) != Some(Quality::High) {
        return Err("re-open did not load the High set".into());
    }
    if !again.original_available {
        return Err("re-open has no original for the backing track".into());
    }

    // Switch to Standard. First a cancel check (Standard is not cached
    // yet, so this is a real separation), then the real run.
    set_quality(b, Quality::Standard)?;
    let s = b.start_separation()?;
    if s.quality != Quality::Standard {
        return Err(format!("expected a Standard separation, got {s:?}"));
    }
    std::thread::sleep(Duration::from_millis(800));
    b.cancel_separation();
    let mut cancel_ok = false;
    wait_for(rx, Duration::from_secs(10), |name, v| match name {
        "separation-error" if v["cancelled"] == true => {
            cancel_ok = true;
            Some(Ok(()))
        }
        // A cached Standard set (re-run with an old data dir) finishes instantly.
        "separation-done" => Some(Ok(())),
        "separation-error" => Some(Err(format!("expected a cancel, got {name} {v}"))),
        _ => None,
    })?;
    if b.separation_running() {
        return Err("separation still running after cancel".into());
    }
    log!(
        "autotest: cancel {}",
        if cancel_ok {
            "ok"
        } else {
            "skipped (Standard was cached)"
        }
    );
    // The cancelled run may still hold the model for a moment; the next
    // separation waits for it.
    let (secs, with, orig) = separate(b, rx, &mut beats_ready)?;
    if with.quality != Quality::Standard || !orig {
        return Err(format!(
            "Standard separation gave {with:?}, original {orig}"
        ));
    }
    if cancel_ok {
        timings.push((Quality::Standard, secs));
    }

    // Back to High: instant from the cache.
    set_quality(b, Quality::High)?;
    let (secs, with, _) = separate(b, rx, &mut beats_ready)?;
    log!("autotest: switch back to High took {secs:.2}s");
    if with.quality != Quality::High {
        return Err(format!("switch back gave {with:?}"));
    }
    if secs > 5.0 {
        return Err(format!("switch back to High took {secs:.1}s (not cached?)"));
    }

    // "All but guitar" export in P1 mode, unstretched: must equal
    // original - guitar (up to the export's scale and 24-bit rounding), and
    // differ from the plain sum of the other stems.
    let file = out.join("autotest-all-but-guitar.wav");
    let _ = std::fs::remove_file(&file);
    let secs = export_wait(
        b,
        rx,
        ExportRequest {
            path: file.display().to_string(),
            format: ExportFormat::Wav,
            stems: mix_all_but("guitar", None),
            solo: false,
            speed_percent: 100.0,
            semitones: 0.0,
            cents: 0.0,
            click: false,
            count_in: false,
            start_sec: None,
            end_sec: None,
        },
    )?;
    let got = read_wav(&file)?;
    let others: Vec<&str> = STEMS.iter().copied().filter(|s| *s != "guitar").collect();
    let [p1, sum] = b
        .debug_mix_references(&others)
        .ok_or("no reference (stems or original missing)")?;
    let (snr_p1, k) = snr_db(&got, &p1);
    let (snr_sum, _) = snr_db(&got, &sum);
    log!(
        "autotest: all-but-guitar export {secs:.1}s: vs original-guitar {snr_p1:.1} dB (gain {k:.3}), vs sum of stems {snr_sum:.1} dB"
    );
    // The export's peak limiter touches the few samples above full scale
    // (Houses Of The Holy peaks at ~1.07), which caps the SNR near 47 dB;
    // the plain sum is far worse (~17 dB).
    if snr_p1 < MIX_SNR_MIN || snr_sum > snr_p1 - 10.0 {
        return Err("all-but-guitar export is not original - guitar".into());
    }
    check_click_export(b, rx, out, &got)?;
    check_range_export(b, rx, out, &got)?;
    {
        // The song's own refine/strength go back afterwards, pass or fail.
        let saved = (again.refine.clone(), again.strength);
        let r = check_refine_strength(b, rx, out, &got);
        let restore = (|| -> Result<(), String> {
            for stem in STEMS {
                b.set_refine(stem, saved.0.get(stem).copied().unwrap_or_default())?;
            }
            if b.set_strength(saved.1)? > 0 {
                wait_strength(rx)?;
            }
            Ok(())
        })();
        r?;
        restore?;
    }

    // Solo export of vocals, bass and drums: what is heard is the plain sum
    // of the soloed stems, even though 3 of 6 on would be P1 mode without
    // solo.
    let soloed = ["vocals", "bass", "drums"];
    let file = out.join("autotest-solo.wav");
    let _ = std::fs::remove_file(&file);
    let secs = export_wait(
        b,
        rx,
        ExportRequest {
            path: file.display().to_string(),
            format: ExportFormat::Wav,
            stems: soloed
                .iter()
                .map(|s| ExportStem {
                    stem: s.to_string(),
                    volume: 1.0,
                    on: true,
                })
                .collect(),
            solo: true,
            speed_percent: 100.0,
            semitones: 0.0,
            cents: 0.0,
            click: false,
            count_in: false,
            start_sec: None,
            end_sec: None,
        },
    )?;
    let got = read_wav(&file)?;
    let [p1, sum] = b
        .debug_mix_references(&soloed)
        .ok_or("no reference (stems or original missing)")?;
    let (snr_sum, k) = snr_db(&got, &sum);
    let (snr_p1, _) = snr_db(&got, &p1);
    log!(
        "autotest: solo export {secs:.1}s: vs sum of soloed stems {snr_sum:.1} dB (gain {k:.3}), vs P1 backing track {snr_p1:.1} dB"
    );
    if snr_sum < MIX_SNR_MIN || snr_p1 > snr_sum - 10.0 {
        return Err("solo export is not the plain sum of the soloed stems".into());
    }

    // +6 dB boost on bass (P1 mode): must not clip.
    let file = out.join("autotest-bass-boost.wav");
    let _ = std::fs::remove_file(&file);
    let secs = export_wait(
        b,
        rx,
        ExportRequest {
            path: file.display().to_string(),
            format: ExportFormat::Wav,
            stems: mix_all_but("", Some(("bass", 1.995))),
            solo: false,
            speed_percent: 100.0,
            semitones: 0.0,
            cents: 0.0,
            click: false,
            count_in: false,
            start_sec: None,
            end_sec: None,
        },
    )?;
    let got = read_wav(&file)?;
    let peak = got
        .iter()
        .flat_map(|c| c.iter())
        .fold(0.0f32, |m, v| m.max(v.abs()));
    log!("autotest: +6 dB bass export {secs:.1}s, peak {peak:.4}");
    if peak > 1.0 {
        return Err(format!("boosted export clips (peak {peak})"));
    }

    // Stretched exports in all formats.
    let formats: &[ExportFormat] = if crate::audio::IS_STUB {
        &[ExportFormat::Wav]
    } else {
        &[ExportFormat::Wav, ExportFormat::Flac, ExportFormat::Mp3]
    };
    for &format in formats {
        let file = out.join(format!("autotest-mix.{}", format.ext()));
        let _ = std::fs::remove_file(&file);
        let secs = export_wait(
            b,
            rx,
            ExportRequest {
                path: file.display().to_string(),
                format,
                stems: ["guitar", "bass", "drums"]
                    .iter()
                    .map(|s| ExportStem {
                        stem: s.to_string(),
                        volume: 0.8,
                        on: true,
                    })
                    .collect(),
                solo: false,
                speed_percent: 80.0,
                semitones: -2.0,
                cents: 0.0,
                click: false,
                count_in: false,
                start_sec: None,
                end_sec: None,
            },
        )?;
        let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
        log!(
            "autotest: export {:?} {secs:.1}s -> {} ({} KB)",
            format,
            file.display(),
            size / 1024
        );
        if size == 0 {
            return Err("export produced no file".into());
        }
    }
    let lib = b.library();
    log!("autotest: library has {} entries", lib.len());
    for (q, secs) in &timings {
        log!("autotest: TIMING separation {q:?}: {secs:.1}s");
    }
    Ok(())
}
