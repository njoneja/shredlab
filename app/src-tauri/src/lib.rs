//! Tauri shell for ShredLab. Commands (`commands.rs`) are thin
//! wrappers over `backend.rs`, which holds the state and background jobs
//! and is Tauri-free.
//!
//! Environment variables (all optional):
//! - `SHREDLAB_DATA_DIR`: data directory instead of the app data dir.
//! - `SHREDLAB_MODELS_DIR`: extra model directories (dev).
//! - `SHREDLAB_BEAT_MODEL`: path of `beat_this_final0.onnx`.
//! - `SHREDLAB_DEVICE`: `auto` (default), `cpu` or `gpu` for separation.
//! - `SHREDLAB_BEATS_DEVICE`: `cpu` (default), `auto` or `gpu` for beats.
//! - `SHREDLAB_MODEL`: separation model id; overrides the model chosen in
//!   Settings (`<data dir>/settings.json`).
//! - `SHREDLAB_NO_PRELOAD`: don't load the model at startup.
//! - debug builds: `SHREDLAB_AUTOTEST=<song>` runs the headless flow in
//!   `autotest.rs`.

pub mod audio;
#[cfg(debug_assertions)]
mod autotest;
pub mod backend;
mod commands;
pub mod log;
pub mod models;
pub mod music_start;
pub mod settings;
pub mod types;
#[cfg(windows)]
mod win_icon;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use shredlab_separation::DevicePref;
use tauri::{Emitter, Manager};

use backend::{Backend, Config, Sink};

fn device_env(name: &str, default: DevicePref) -> DevicePref {
    match std::env::var(name) {
        Ok(v) => v.parse().unwrap_or_else(|e| {
            log!("{name}: {e}; using {default:?}");
            default
        }),
        Err(_) => default,
    }
}

/// Bundle identifier before the rename; the app data folder was named after it.
const OLD_IDENTIFIER: &str = "dev.stemextractor.app";

/// Moves the data folder of the old identifier to `data_dir` (library, stem
/// cache, settings, personal models), if `data_dir` doesn't exist yet and the
/// old sibling folder does. Returns a message for the log when it tried.
fn migrate_old_data_dir(data_dir: &Path) -> Option<String> {
    if data_dir.exists() {
        return None;
    }
    let old = data_dir.parent()?.join(OLD_IDENTIFIER);
    if !old.is_dir() {
        return None;
    }
    Some(match std::fs::rename(&old, data_dir) {
        Ok(()) => format!(
            "moved data folder {} -> {}",
            old.display(),
            data_dir.display()
        ),
        Err(e) => format!(
            "could not move data folder {} -> {}: {e}; starting with an empty one",
            old.display(),
            data_dir.display()
        ),
    })
}

/// Shrinks the window (created hidden at its configured default size) to
/// fit the monitor's work area, centres it and shows it. A 1080p laptop at
/// 150 % scaling is only 720 logical pixels tall, less the taskbar.
fn fit_and_show_window(app: &tauri::App) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if let (Ok(Some(monitor)), Ok(size)) = (window.current_monitor(), window.inner_size()) {
        let scale = monitor.scale_factor();
        let area = monitor.work_area().size.to_logical::<f64>(scale);
        let size = size.to_logical::<f64>(window.scale_factor().unwrap_or(scale));
        // Leave room for the title bar and borders.
        let fit = tauri::LogicalSize::new(
            size.width.min(area.width * 0.95),
            size.height.min(area.height * 0.92),
        );
        if fit != size {
            let _ = window.set_size(fit);
        }
        let _ = window.center();
    }
    let _ = window.show();
}

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    fit_and_show_window(app);
    let mut migrated = None;
    let data_dir = match std::env::var_os("SHREDLAB_DATA_DIR") {
        Some(d) => PathBuf::from(d),
        None => match app.path().app_data_dir() {
            Ok(d) => {
                migrated = migrate_old_data_dir(&d);
                d
            }
            Err(_) => std::env::temp_dir().join("shredlab"),
        },
    };
    log::init(&data_dir);
    log!(
        "ShredLab {} starting ({} build, audio engine: {})",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        if audio::IS_STUB {
            "STUB (silent)"
        } else {
            "shredlab-audio"
        }
    );
    if let Some(m) = migrated {
        log!("{m}");
    }
    #[cfg(windows)]
    win_icon::apply(app);
    let resource_dir = app.path().resource_dir().ok();
    if let Some(r) = &resource_dir {
        log!("resource dir: {}", r.display());
    }
    let cfg = Config {
        model_dirs: models::model_dirs(resource_dir.as_deref(), &data_dir),
        beat_model: models::find_beat_model(resource_dir.as_deref(), &data_dir),
        device: device_env("SHREDLAB_DEVICE", DevicePref::Auto),
        beat_device: device_env("SHREDLAB_BEATS_DEVICE", DevicePref::Cpu),
        data_dir,
    };
    let engine = match audio::Engine::new() {
        Ok(e) => Some(e),
        Err(e) => {
            log!("audio engine unavailable: {e}; playback disabled");
            None
        }
    };

    let handle = app.handle().clone();
    #[cfg(debug_assertions)]
    let tap = autotest::event_tap();
    let sink: Sink = Arc::new(move |name, payload| {
        #[cfg(debug_assertions)]
        if let Some(tap) = &tap {
            tap(name, &payload);
        }
        if let Err(e) = handle.emit(name, payload) {
            log!("emit {name} failed: {e}");
        }
    });
    let backend = Backend::new(cfg, engine, sink)?;
    backend.start_background();
    app.manage(backend.clone());
    #[cfg(debug_assertions)]
    autotest::maybe_start(backend, app.handle().clone());
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(setup)
        .invoke_handler(tauri::generate_handler![
            commands::pick_audio_file,
            commands::pick_export_path,
            commands::open_song,
            commands::close_song,
            commands::get_library,
            commands::hide_recent,
            commands::list_library,
            commands::song_size,
            commands::delete_song,
            commands::start_separation,
            commands::cancel_separation,
            commands::play,
            commands::pause,
            commands::seek,
            commands::set_mix,
            commands::set_speed,
            commands::set_pitch,
            commands::set_loop,
            commands::set_click,
            commands::set_meter_overrides,
            commands::get_waveform,
            commands::get_beats,
            commands::export_mix,
            commands::cancel_export,
            commands::save_song_settings,
            commands::get_app_settings,
            commands::set_app_settings,
            commands::set_theme,
            commands::set_count_in_to_music_start,
            commands::set_stem_format,
            commands::set_click_volume,
            commands::set_master_volume,
            commands::get_refine_presets,
            commands::set_refine,
            commands::set_refine_analysis,
            commands::set_strength,
        ])
        .run(tauri::generate_context!());
    if let Err(e) = result {
        log!("fatal: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("shredlab-migrate-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn old_data_dir_moves_only_when_new_is_missing() {
        // New folder missing: the old one is moved, contents and all.
        let root = temp("move");
        let old = root.join(OLD_IDENTIFIER);
        std::fs::create_dir_all(old.join("library")).unwrap();
        std::fs::write(old.join("settings.json"), "{}").unwrap();
        let new = root.join("io.github.njoneja.shredlab");
        let msg = migrate_old_data_dir(&new).unwrap();
        assert!(msg.starts_with("moved"), "{msg}");
        assert!(!old.exists());
        assert!(new.join("library").is_dir());
        assert_eq!(
            std::fs::read_to_string(new.join("settings.json")).unwrap(),
            "{}"
        );
        // Nothing left to move.
        assert_eq!(migrate_old_data_dir(&new), None);

        // New folder exists: the old one is left alone.
        let root = temp("keep");
        let old = root.join(OLD_IDENTIFIER);
        std::fs::create_dir_all(&old).unwrap();
        let new = root.join("io.github.njoneja.shredlab");
        std::fs::create_dir_all(&new).unwrap();
        assert_eq!(migrate_old_data_dir(&new), None);
        assert!(old.is_dir());

        // Neither exists: nothing to do.
        let root = temp("none");
        assert_eq!(
            migrate_old_data_dir(&root.join("io.github.njoneja.shredlab")),
            None
        );
    }
}
