//! Tauri commands: thin wrappers over [`Backend`]. Names and parameters
//! follow `app/src/lib/API.md` exactly (camelCase JS keys map to these
//! snake_case parameter names).
//!
//! All commands are `async` so they never run on the main (UI) thread;
//! anything slow goes through `spawn_blocking`.

use std::sync::Arc;

use tauri::{State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

use crate::backend::Backend;
use crate::types::*;

type B<'a> = State<'a, Arc<Backend>>;
type R<T> = Result<T, String>;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> R<T> + Send + 'static) -> R<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("Internal error: {e}"))?
}

const AUDIO_EXTS: [&str; 7] = ["mp3", "wav", "flac", "m4a", "aac", "ogg", "opus"];

#[tauri::command]
pub async fn pick_audio_file(window: WebviewWindow) -> R<Option<String>> {
    blocking(move || {
        let picked = window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title("Open a song")
            .add_filter("Audio", &AUDIO_EXTS)
            .add_filter("All files", &["*"])
            .blocking_pick_file();
        match picked {
            None => Ok(None),
            Some(p) => p
                .into_path()
                .map(|p| Some(p.display().to_string()))
                .map_err(|e| e.to_string()),
        }
    })
    .await
}

#[tauri::command]
pub async fn pick_export_path(
    window: WebviewWindow,
    default_name: String,
    format: ExportFormat,
) -> R<Option<String>> {
    blocking(move || {
        let ext = format.ext();
        let picked = window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title("Export mix")
            .set_file_name(format!("{default_name}.{ext}"))
            .add_filter(ext.to_uppercase(), &[ext])
            .blocking_save_file();
        match picked {
            None => Ok(None),
            Some(p) => {
                let mut p = p.into_path().map_err(|e| e.to_string())?;
                let has_ext = p
                    .extension()
                    .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case(ext));
                if !has_ext {
                    let mut s = p.into_os_string();
                    s.push(format!(".{ext}"));
                    p = s.into();
                }
                Ok(Some(p.display().to_string()))
            }
        }
    })
    .await
}

#[tauri::command]
pub async fn open_song(b: B<'_>, path: String) -> R<SongInfo> {
    let b = b.inner().clone();
    blocking(move || b.open_song(&path)).await
}

#[tauri::command]
pub async fn close_song(b: B<'_>) -> R<()> {
    b.close_song();
    Ok(())
}

#[tauri::command]
pub async fn get_library(b: B<'_>) -> R<Vec<LibraryEntry>> {
    let b = b.inner().clone();
    blocking(move || Ok(b.library())).await
}

#[tauri::command]
pub async fn hide_recent(b: B<'_>, song_id: String) -> R<()> {
    let b = b.inner().clone();
    blocking(move || b.hide_recent(&song_id)).await
}

#[tauri::command]
pub async fn list_library(b: B<'_>) -> R<LibraryListing> {
    let b = b.inner().clone();
    blocking(move || Ok(b.list_library())).await
}

#[tauri::command]
pub async fn delete_song(b: B<'_>, song_id: String) -> R<u64> {
    let b = b.inner().clone();
    blocking(move || b.delete_song(&song_id)).await
}

#[tauri::command]
pub async fn start_separation(b: B<'_>) -> R<SeparatedWith> {
    let b = b.inner().clone();
    blocking(move || b.start_separation()).await
}

#[tauri::command]
pub async fn cancel_separation(b: B<'_>) -> R<()> {
    b.cancel_separation();
    Ok(())
}

#[tauri::command]
pub async fn play(b: B<'_>) -> R<()> {
    b.play()
}

#[tauri::command]
pub async fn pause(b: B<'_>) -> R<()> {
    b.pause();
    Ok(())
}

#[tauri::command]
pub async fn seek(b: B<'_>, position_sec: f64) -> R<()> {
    b.seek(position_sec);
    Ok(())
}

#[tauri::command]
pub async fn set_mix(b: B<'_>, stems: Vec<StemMixState>) -> R<()> {
    b.set_mix(stems);
    Ok(())
}

#[tauri::command]
pub async fn set_speed(b: B<'_>, percent: u32) -> R<()> {
    b.set_speed(percent);
    Ok(())
}

#[tauri::command]
pub async fn set_pitch(b: B<'_>, semitones: i32, cents: i32) -> R<()> {
    b.set_pitch(semitones, cents);
    Ok(())
}

#[tauri::command]
pub async fn set_loop(b: B<'_>, loop_state: LoopState) -> R<()> {
    b.set_loop(loop_state);
    Ok(())
}

#[tauri::command]
pub async fn set_click(b: B<'_>, click: ClickState) -> R<()> {
    b.set_click(click);
    Ok(())
}

#[tauri::command]
pub async fn set_meter_overrides(b: B<'_>, payload: MeterOverridesPayload) -> R<()> {
    b.set_meter_overrides(payload);
    Ok(())
}

#[tauri::command]
pub async fn get_waveform(b: B<'_>, n_points: u32) -> R<Waveform> {
    let b = b.inner().clone();
    blocking(move || b.get_waveform(n_points)).await
}

#[tauri::command]
pub async fn get_beats(b: B<'_>) -> R<Option<BeatGrid>> {
    b.get_beats()
}

#[tauri::command]
pub async fn export_mix(b: B<'_>, req: ExportRequest) -> R<()> {
    b.export_mix(req)
}

#[tauri::command]
pub async fn cancel_export(b: B<'_>) -> R<()> {
    b.cancel_export();
    Ok(())
}

#[tauri::command]
pub async fn save_song_settings(b: B<'_>, song_id: String, settings: serde_json::Value) -> R<()> {
    let b = b.inner().clone();
    blocking(move || b.save_song_settings(&song_id, settings)).await
}

#[tauri::command]
pub async fn get_app_settings(b: B<'_>) -> R<AppSettingsInfo> {
    let b = b.inner().clone();
    blocking(move || Ok(b.app_settings())).await
}

#[tauri::command]
pub async fn set_app_settings(b: B<'_>, settings: AppSettingsUpdate) -> R<AppSettingsInfo> {
    let b = b.inner().clone();
    blocking(move || b.set_app_settings(settings)).await
}

#[tauri::command]
pub async fn set_count_in_to_music_start(b: B<'_>, on: bool) -> R<AppSettingsInfo> {
    let b = b.inner().clone();
    blocking(move || b.set_count_in_to_music_start(on)).await
}

#[tauri::command]
pub async fn set_theme(b: B<'_>, theme: String) -> R<AppSettingsInfo> {
    let b = b.inner().clone();
    blocking(move || b.set_theme(&theme)).await
}

#[tauri::command]
pub async fn get_refine_presets(b: B<'_>) -> R<Vec<RefinePresetInfo>> {
    Ok(b.refine_presets())
}

#[tauri::command]
pub async fn set_refine(b: B<'_>, update: RefineUpdate) -> R<()> {
    b.set_refine(&update.stem, update.params)
}

#[tauri::command]
pub async fn set_refine_analysis(b: B<'_>, stem: Option<String>) -> R<()> {
    b.set_refine_analysis(stem);
    Ok(())
}

#[tauri::command]
pub async fn set_strength(b: B<'_>, strength: StrengthSetting) -> R<StrengthStarted> {
    let b = b.inner().clone();
    blocking(move || b.set_strength(strength).map(|job| StrengthStarted { job })).await
}
