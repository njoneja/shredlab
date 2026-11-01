//! Tauri shell for Stem Extractor. Commands (`commands.rs`) are thin
//! wrappers over `backend.rs`, which holds the state and background jobs
//! and is Tauri-free.
//!
//! Environment variables (all optional):
//! - `STEMX_DATA_DIR`: data directory instead of the app data dir.
//! - `STEMX_MODELS_DIR`: extra model directories (dev).
//! - `STEMX_BEAT_MODEL`: path of `beat_this_final0.onnx`.
//! - `STEMX_DEVICE`: `auto` (default), `cpu` or `gpu` for separation.
//! - `STEMX_BEATS_DEVICE`: `cpu` (default), `auto` or `gpu` for beats.
//! - `STEMX_MODEL`: separation model id; overrides the model chosen in
//!   Settings (`<data dir>/settings.json`).
//! - `STEMX_NO_PRELOAD`: don't load the model at startup.
//! - debug builds: `STEMX_AUTOTEST=<song>` runs the headless flow in
//!   `autotest.rs`.

pub mod audio;
#[cfg(debug_assertions)]
mod autotest;
pub mod backend;
mod commands;
pub mod log;
pub mod models;
pub mod settings;
pub mod types;

use std::path::PathBuf;
use std::sync::Arc;

use stemx_separation::DevicePref;
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

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = match std::env::var_os("STEMX_DATA_DIR") {
        Some(d) => PathBuf::from(d),
        None => app
            .path()
            .app_data_dir()
            .unwrap_or_else(|_| std::env::temp_dir().join("stem-extractor")),
    };
    log::init(&data_dir);
    log!(
        "Stem Extractor {} starting ({} build, audio engine: {})",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        if audio::IS_STUB {
            "STUB (silent)"
        } else {
            "stemx-audio"
        }
    );
    let resource_dir = app.path().resource_dir().ok();
    if let Some(r) = &resource_dir {
        log!("resource dir: {}", r.display());
    }
    let cfg = Config {
        model_dirs: models::model_dirs(resource_dir.as_deref(), &data_dir),
        beat_model: models::find_beat_model(resource_dir.as_deref(), &data_dir),
        device: device_env("STEMX_DEVICE", DevicePref::Auto),
        beat_device: device_env("STEMX_BEATS_DEVICE", DevicePref::Cpu),
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
        ])
        .run(tauri::generate_context!());
    if let Err(e) = result {
        log!("fatal: {e}");
        std::process::exit(1);
    }
}
