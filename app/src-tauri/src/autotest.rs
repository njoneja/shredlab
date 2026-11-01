//! Debug-only headless check of the whole flow, driven through the same
//! backend calls the commands use:
//!
//! ```text
//! STEMX_AUTOTEST=<song> [STEMX_AUTOTEST_OUT=<dir>] [STEMX_AUTOTEST_EXIT=1] pnpm --dir app tauri dev
//! ```
//!
//! quality High -> open -> separate (unless cached) -> beats -> play 3 s ->
//! re-open (cache, High) -> cancel a Standard separation -> switch to
//! Standard (re-separates) -> back to High (instant, from the cache) ->
//! "All but guitar" export in P1 mode (checked against original - guitar)
//! -> +6 dB boost export (no clipping) -> WAV/FLAC/MP3 exports. Each step
//! is logged with its timing; the last line is `AUTOTEST PASS` or
//! `AUTOTEST FAIL: ...`. The saved settings are restored at the end. Use a
//! fresh `STEMX_DATA_DIR` to time both separations.

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
    std::env::var_os("STEMX_AUTOTEST")?;
    let (tx, rx): (Sender<Event>, Receiver<Event>) = channel();
    RX.get_or_init(|| Mutex::new(Some(rx)));
    let tx = Mutex::new(tx);
    Some(Arc::new(move |name, v| {
        if name != "playback-position" && name != "separation-progress" && name != "export-progress"
        {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send((name.to_string(), v.clone()));
            }
        }
    }))
}

pub fn maybe_start(backend: Arc<Backend>, app: tauri::AppHandle) {
    let Some(song) = std::env::var_os("STEMX_AUTOTEST") else {
        return;
    };
    let Some(rx) = RX.get().and_then(|m| m.lock().ok()?.take()) else {
        return;
    };
    let out = std::env::var_os("STEMX_AUTOTEST_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| backend.data_dir().join("autotest"));
    let exit = std::env::var_os("STEMX_AUTOTEST_EXIT").is_some();
    let _ = std::thread::Builder::new()
        .name("stemx-autotest".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            let saved = backend.app_settings();
            let result = run(&backend, &PathBuf::from(song), &out, &rx);
            let _ = backend.set_app_settings(AppSettingsUpdate {
                model_id: saved.model_choice,
                quality: saved.quality_choice,
            });
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
        })
        .collect()
}

/// Minimum SNR (dB) of an unstretched export against its reference mix
/// (the export limiter caps it below the 24-bit rounding limit).
const MIX_SNR_MIN: f64 = 40.0;

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
    let a = stemx_io::decode_file(path).map_err(|e| e.to_string())?;
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
                })
                .collect(),
            solo: true,
            speed_percent: 100.0,
            semitones: 0.0,
            cents: 0.0,
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
                    })
                    .collect(),
                solo: false,
                speed_percent: 80.0,
                semitones: -2.0,
                cents: 0.0,
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
