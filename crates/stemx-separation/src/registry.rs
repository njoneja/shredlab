//! Model registry: scans model folders (`<dir>/<id>/model.json`).
//!
//! Directories are passed in by the caller, in priority order: the bundled
//! resource dir, the personal models dir, then the dev directories from
//! `STEMX_MODELS_DIR` (see [`ModelRegistry::search_dirs`]). If two folders
//! declare the same id, the first one wins.

use std::path::{Path, PathBuf};

use crate::manifest::{Manifest, SelfTestSpec};
use crate::{Error, Result};

/// Environment variable with extra model directories (dev use), separated
/// like `PATH` (`;` on Windows, `:` elsewhere).
pub const MODELS_DIR_ENV: &str = "STEMX_MODELS_DIR";

/// Where a model was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSource {
    Bundled,
    Personal,
    Dev,
}

impl ModelSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::Personal => "personal",
            Self::Dev => "dev",
        }
    }
}

/// A model folder with a valid manifest.
#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub source: ModelSource,
}

impl ModelEntry {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    pub fn onnx_path(&self) -> PathBuf {
        self.dir.join(&self.manifest.file)
    }

    pub fn selftest_path(&self) -> PathBuf {
        self.dir.join(&self.manifest.self_test)
    }

    /// Personal = flagged in the manifest or found in the personal folder.
    pub fn is_personal(&self) -> bool {
        self.manifest.personal || self.source == ModelSource::Personal
    }

    pub fn load_selftest(&self) -> Result<SelfTestSpec> {
        let st = SelfTestSpec::load(&self.selftest_path())?;
        st.validate_for(&self.manifest)?;
        Ok(st)
    }
}

/// A folder that looked like a model but could not be loaded.
#[derive(Debug, Clone)]
pub struct ScanProblem {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Default, Clone)]
pub struct ModelRegistry {
    pub models: Vec<ModelEntry>,
    pub problems: Vec<ScanProblem>,
}

impl ModelRegistry {
    /// The standard search list: `bundled`, `personal`, then each entry of
    /// `STEMX_MODELS_DIR`.
    pub fn search_dirs(
        bundled: Option<&Path>,
        personal: Option<&Path>,
    ) -> Vec<(PathBuf, ModelSource)> {
        let mut dirs = Vec::new();
        if let Some(b) = bundled {
            dirs.push((b.to_path_buf(), ModelSource::Bundled));
        }
        if let Some(p) = personal {
            dirs.push((p.to_path_buf(), ModelSource::Personal));
        }
        dirs.extend(env_dirs().into_iter().map(|d| (d, ModelSource::Dev)));
        dirs
    }

    /// Scans the given directories. Missing directories are skipped silently;
    /// invalid model folders are recorded in `problems`.
    pub fn scan(dirs: &[(PathBuf, ModelSource)]) -> Self {
        let mut reg = Self::default();
        for (dir, source) in dirs {
            let Ok(rd) = std::fs::read_dir(dir) else {
                continue;
            };
            let mut subdirs: Vec<PathBuf> = rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir() && !is_hidden(p))
                .collect();
            subdirs.sort();
            for sub in subdirs {
                let mf = sub.join("model.json");
                if !mf.is_file() {
                    continue;
                }
                match load_entry(&sub, *source) {
                    Ok(entry) => {
                        if let Some(prev) = reg.get(entry.id()) {
                            reg.problems.push(ScanProblem {
                                path: sub.clone(),
                                message: format!(
                                    "duplicate model id {:?} (already loaded from {})",
                                    entry.id(),
                                    prev.dir.display()
                                ),
                            });
                        } else {
                            reg.models.push(entry);
                        }
                    }
                    Err(e) => reg.problems.push(ScanProblem {
                        path: sub,
                        message: e.to_string(),
                    }),
                }
            }
        }
        reg
    }

    pub fn get(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.id() == id)
    }

    pub fn require(&self, id: &str) -> Result<&ModelEntry> {
        self.get(id).ok_or_else(|| {
            let known: Vec<&str> = self.models.iter().map(|m| m.id()).collect();
            Error::Manifest(format!(
                "unknown model {id:?} (available: {})",
                known.join(", ")
            ))
        })
    }
}

fn is_hidden(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}

fn load_entry(dir: &Path, source: ModelSource) -> Result<ModelEntry> {
    let manifest = Manifest::load(&dir.join("model.json"))?;
    let entry = ModelEntry {
        manifest,
        dir: dir.to_path_buf(),
        source,
    };
    if !entry.onnx_path().is_file() {
        return Err(Error::Manifest(format!(
            "missing model file {}",
            entry.onnx_path().display()
        )));
    }
    Ok(entry)
}

/// Directories from `STEMX_MODELS_DIR`.
pub fn env_dirs() -> Vec<PathBuf> {
    std::env::var_os(MODELS_DIR_ENV)
        .map(|v| {
            std::env::split_paths(&v)
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_model(root: &Path, id: &str, personal: bool) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let json = crate::manifest::tests::EXAMPLE
            .replace("\"htdemucs_6s\"", &format!("\"{id}\""))
            .replace("\"personal\": false", &format!("\"personal\": {personal}"));
        std::fs::write(dir.join("model.json"), json).unwrap();
        std::fs::write(dir.join("htdemucs_6s_own.onnx"), b"x").unwrap();
    }

    #[test]
    fn scans_in_order_and_dedups() {
        let root = std::env::temp_dir().join(format!("stemx-reg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (a, b) = (root.join("bundled"), root.join("personal"));
        write_model(&a, "m1", false);
        write_model(&b, "m1", true);
        write_model(&b, "m2", true);
        std::fs::create_dir_all(b.join("broken")).unwrap();
        std::fs::write(b.join("broken").join("model.json"), "{").unwrap();
        std::fs::create_dir_all(b.join(".partial")).unwrap();
        std::fs::write(b.join(".partial").join("model.json"), "{").unwrap();

        let reg = ModelRegistry::scan(&[
            (a.clone(), ModelSource::Bundled),
            (b.clone(), ModelSource::Personal),
            (root.join("missing"), ModelSource::Dev),
        ]);
        assert_eq!(reg.models.len(), 2);
        assert_eq!(reg.get("m1").unwrap().source, ModelSource::Bundled);
        assert!(!reg.get("m1").unwrap().is_personal());
        assert!(reg.get("m2").unwrap().is_personal());
        assert_eq!(reg.problems.len(), 2, "{:?}", reg.problems); // duplicate + broken
        assert!(reg.require("nope").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
