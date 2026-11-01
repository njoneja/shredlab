//! Debug-only headless check of the whole flow, driven through the same
//! backend calls the commands use:
//!
//! ```text
//! STEMX_AUTOTEST=<song> [STEMX_AUTOTEST_OUT=<dir>] [STEMX_AUTOTEST_EXIT=1] pnpm --dir app tauri dev
//! ```
//!
//! open -> separate (unless cached) -> beats -> play 3 s -> re-open (cache)
//! -> export WAV/FLAC/MP3. Each step is logged with its timing; the last
//! line is `AUTOTEST PASS` or `AUTOTEST FAIL: ...`.

use std::path::PathBuf;
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
            let code = match run(&backend, &PathBuf::from(song), &out, &rx) {
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

fn run(
    b: &Arc<Backend>,
    song: &std::path::Path,
    out: &std::path::Path,
    rx: &Receiver<Event>,
) -> Result<(), String> {
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let path = song.display().to_string();
    log!("autotest: song {path}");

    let t = Instant::now();
    let info = b.open_song(&path)?;
    log!(
        "autotest: open_song {:.2}s: \"{}\" by {:?}, {:.1}s, {} Hz, stems_cached {}, beats_ready {}",
        t.elapsed().as_secs_f64(),
        info.title,
        info.artist,
        info.duration_sec,
        info.sample_rate,
        info.stems_cached,
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
    if !info.stems_cached {
        let t = Instant::now();
        let started = b.start_separation()?;
        log!(
            "autotest: separation started: {} on {}",
            started.model_name,
            started.device_label
        );
        b.play();
        wait_for(rx, Duration::from_secs(1800), |name, v| match name {
            "separation-done" => Some(Ok(())),
            "separation-error" => Some(Err(format!("separation failed: {v}"))),
            "beats-ready" => {
                beats_ready = true;
                None
            }
            _ => None,
        })?;
        log!(
            "autotest: separation done in {:.1}s (position now {:.1}s)",
            t.elapsed().as_secs_f64(),
            b.position().map_or(0.0, |p| p.seconds)
        );
        b.pause();
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

    // Play a few seconds (with count-in) from 30 s, guitar + bass only.
    b.set_mix(
        STEMS
            .iter()
            .map(|s| StemMixState {
                stem: s.to_string(),
                on: matches!(*s, "guitar" | "bass"),
                volume: 1.0,
                solo: false,
            })
            .collect(),
    );
    b.seek(30.0);
    b.play();
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

    // Re-open: must come from the cache.
    let t = Instant::now();
    let again = b.open_song(&path)?;
    let reopen = t.elapsed().as_secs_f64();
    log!(
        "autotest: re-open {reopen:.2}s, stems_cached {}, separated_with {:?}, beats_ready {}, bpm {:?}",
        again.stems_cached,
        again.separated_with,
        again.beats_ready,
        again.bpm
    );
    if !again.stems_cached || !again.beats_ready {
        return Err("re-open did not use the cache".into());
    }

    // Exports.
    let formats: &[ExportFormat] = if crate::audio::IS_STUB {
        &[ExportFormat::Wav]
    } else {
        &[ExportFormat::Wav, ExportFormat::Flac, ExportFormat::Mp3]
    };
    for &format in formats {
        let file = out.join(format!("autotest-mix.{}", format.ext()));
        let _ = std::fs::remove_file(&file);
        let t = Instant::now();
        b.export_mix(ExportRequest {
            path: file.display().to_string(),
            format,
            stems: ["guitar", "bass", "drums"]
                .iter()
                .map(|s| ExportStem {
                    stem: s.to_string(),
                    volume: 0.8,
                })
                .collect(),
            speed_percent: 80.0,
            semitones: -2.0,
            cents: 0.0,
        })?;
        wait_for(rx, Duration::from_secs(600), |name, v| match name {
            "export-done" => Some(Ok(())),
            "export-error" => Some(Err(format!("export failed: {v}"))),
            _ => None,
        })?;
        let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
        log!(
            "autotest: export {:?} {:.1}s -> {} ({} KB)",
            format,
            t.elapsed().as_secs_f64(),
            file.display(),
            size / 1024
        );
        if size == 0 {
            return Err("export produced no file".into());
        }
    }
    let lib = b.library();
    log!("autotest: library has {} entries", lib.len());

    // Cancel: re-separate, cancel after a second, expect cancelled error
    // and the cached stems still in use.
    b.start_separation()?;
    std::thread::sleep(Duration::from_secs(1));
    b.cancel_separation();
    wait_for(rx, Duration::from_secs(10), |name, v| match name {
        "separation-error" if v["cancelled"] == true => Some(Ok(())),
        "separation-error" | "separation-done" => {
            Some(Err(format!("expected a cancel, got {name} {v}")))
        }
        _ => None,
    })?;
    std::thread::sleep(Duration::from_secs(3));
    if b.separation_running() {
        return Err("separation still running after cancel".into());
    }
    if b.start_separation().is_ok() {
        // The cancelled thread has released the job slot; cancel again.
        b.cancel_separation();
    }
    log!("autotest: cancel ok");
    Ok(())
}
