//! Tauri shell for Stem Extractor. Commands here are thin wrappers over the
//! `stemx-*` core crates.

use serde::Serialize;

/// Response of the `hello` command: proves the webview -> Rust IPC path.
#[derive(Debug, Serialize)]
pub struct Hello {
    pub message: String,
    pub app_version: &'static str,
    pub crates: Vec<CrateInfo>,
}

#[derive(Debug, Serialize)]
pub struct CrateInfo {
    pub name: &'static str,
    pub version: &'static str,
}

fn hello_payload() -> Hello {
    let crates = [
        (stemx_dsp::NAME, stemx_dsp::VERSION),
        (stemx_io::NAME, stemx_io::VERSION),
        (stemx_separation::NAME, stemx_separation::VERSION),
        (stemx_audio::NAME, stemx_audio::VERSION),
        (stemx_library::NAME, stemx_library::VERSION),
    ]
    .into_iter()
    .map(|(name, version)| CrateInfo { name, version })
    .collect();

    Hello {
        message: "Hello from Rust".to_string(),
        app_version: env!("CARGO_PKG_VERSION"),
        crates,
    }
}

#[tauri::command]
fn hello() -> Hello {
    hello_payload()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![hello])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_lists_core_crates() {
        let h = hello_payload();
        assert_eq!(h.message, "Hello from Rust");
        assert_eq!(h.crates.len(), 5);
    }
}
