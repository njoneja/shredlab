// Real backend: Tauri commands + events. Command and argument names follow
// API.md. Tauri converts camelCase JS argument keys to snake_case Rust
// parameters, so `{ nPoints }` here arrives as `n_points` in Rust.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { Backend } from "./types";

export const tauriBackend: Backend = {
  isMock: false,
  pickAudioFile: () => invoke("pick_audio_file"),
  pickExportPath: (defaultName, format) => invoke("pick_export_path", { defaultName, format }),
  openSong: (path) => invoke("open_song", { path }),
  closeSong: () => invoke("close_song"),
  getLibrary: () => invoke("get_library"),
  hideRecent: (songId) => invoke("hide_recent", { songId }),
  listLibrary: () => invoke("list_library"),
  songSize: (songId) => invoke("song_size", { songId }),
  deleteSong: (songId) => invoke("delete_song", { songId }),
  startSeparation: () => invoke("start_separation"),
  cancelSeparation: () => invoke("cancel_separation"),
  play: () => invoke("play"),
  pause: () => invoke("pause"),
  seek: (positionSec) => invoke("seek", { positionSec }),
  setMix: (stems) => invoke("set_mix", { stems }),
  setSpeed: (percent) => invoke("set_speed", { percent }),
  setPitch: (semitones, cents) => invoke("set_pitch", { semitones, cents }),
  setLoop: (loop) => invoke("set_loop", { loopState: loop }),
  setClick: (click) => invoke("set_click", { click }),
  setMeterOverrides: (payload) => invoke("set_meter_overrides", { payload }),
  getWaveform: (nPoints) => invoke("get_waveform", { nPoints }),
  getBeats: () => invoke("get_beats"),
  exportMix: (req) => invoke("export_mix", { req }),
  cancelExport: () => invoke("cancel_export"),
  saveSongSettings: (songId, settings) => invoke("save_song_settings", { songId, settings }),
  getAppSettings: () => invoke("get_app_settings"),
  setAppSettings: (settings) => invoke("set_app_settings", { settings }),
  setTheme: (theme) => invoke("set_theme", { theme }),
  setCountInToMusicStart: (on) => invoke("set_count_in_to_music_start", { on }),
  setStemFormat: (format) => invoke("set_stem_format", { format }),
  setClickVolume: (volume) => invoke("set_click_volume", { volume }),
  setMasterVolume: (volume, save) => invoke("set_master_volume", { volume, save }),
  getRefinePresets: () => invoke("get_refine_presets"),
  setRefine: (stem, params) => invoke("set_refine", { update: { stem, params } }),
  setRefineAnalysis: (stem) => invoke("set_refine_analysis", { stem }),
  setStrength: (strength) => invoke("set_strength", { strength }),
  async onFileDrop(cb) {
    return getCurrentWebview().onDragDropEvent((e) => {
      if (e.payload.type === "drop" && e.payload.paths.length) cb(e.payload.paths);
    });
  },
  async listen(event, cb) {
    return listen(event, (e) => cb(e.payload as never));
  },
};
