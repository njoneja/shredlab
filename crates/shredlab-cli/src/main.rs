// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! `shredlab-cli`: headless CLI for ShredLab (testing and batch use).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use shredlab_io::WavFormat;
use shredlab_separation::registry::env_dirs;
use shredlab_separation::{
    run_selftest, DevicePref, Ep, LoadOptions, ModelRegistry, ModelSource, Separator,
    SessionOverrides,
};

#[derive(Parser)]
#[command(
    name = "shredlab-cli",
    version,
    about = "Stem Extractor command-line interface"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Clone)]
struct ModelDirs {
    /// Model directory (each model in its own subfolder with model.json).
    /// Repeatable. Default: $SHREDLAB_MODELS_DIR, else models/registry and
    /// models/personal of the repo / current directory.
    #[arg(long = "models-dir", value_name = "DIR")]
    models_dirs: Vec<PathBuf>,
}

#[derive(Args, Clone)]
struct RuntimeArgs {
    /// Directory for the self-test / hash cache. Default: <repo>/target/shredlab-cache
    /// or the temp dir.
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,
    /// CPU intra-op threads (default: min(physical cores, 16)).
    #[arg(long, value_name = "N")]
    cpu_threads: Option<usize>,
    /// Debug: force DirectML graph fusion ON (known to break htdemucs).
    #[arg(long, hide = true)]
    debug_dml_fusion: bool,
}

impl RuntimeArgs {
    fn overrides(&self) -> SessionOverrides {
        SessionOverrides {
            dml_graph_fusion: self.debug_dml_fusion.then_some(true),
            cpu_threads: self.cpu_threads,
        }
    }

    fn cache_dir(&self) -> PathBuf {
        self.cache_dir
            .clone()
            .unwrap_or_else(|| match find_repo_root() {
                Some(root) => root.join("target").join("shredlab-cache"),
                None => std::env::temp_dir().join("shredlab-cache"),
            })
    }
}

#[derive(Subcommand)]
enum Command {
    /// Print version information for the shredlab crates and ONNX Runtime.
    Info,
    /// List available models.
    Models {
        #[command(flatten)]
        dirs: ModelDirs,
    },
    /// Run the device self-test of a model (all models if --model is omitted).
    Selftest {
        /// Model id.
        #[arg(long)]
        model: Option<String>,
        /// auto = every available device, cpu, or gpu.
        #[arg(long, default_value = "auto")]
        device: DevicePref,
        #[command(flatten)]
        dirs: ModelDirs,
        #[command(flatten)]
        rt: RuntimeArgs,
    },
    /// Decode an audio file to a 44.1 kHz stereo float WAV (decoder check).
    Decode { input: PathBuf, output: PathBuf },
    /// Separate a song into stems (<out>/<stem>.wav).
    Separate {
        /// Input audio file (MP3, FLAC, WAV, OGG, M4A).
        input: PathBuf,
        /// Model id.
        #[arg(long)]
        model: String,
        /// Output directory.
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "auto")]
        device: DevicePref,
        /// Write 16-bit PCM instead of 32-bit float WAV.
        #[arg(long)]
        int16: bool,
        /// Skip the device self-test (GPU output is then not verified).
        #[arg(long)]
        no_selftest: bool,
        /// Number of time-shifted passes to average (demucs shifts; 1 = plain).
        #[arg(long, default_value_t = 1)]
        shifts: usize,
        #[command(flatten)]
        dirs: ModelDirs,
        #[command(flatten)]
        rt: RuntimeArgs,
    },
}

/// (crate name, crate version) for every core crate.
fn crate_versions() -> [(&'static str, &'static str); 6] {
    [
        (env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
        (shredlab_dsp::NAME, shredlab_dsp::VERSION),
        (shredlab_io::NAME, shredlab_io::VERSION),
        (shredlab_separation::NAME, shredlab_separation::VERSION),
        (shredlab_audio::NAME, shredlab_audio::VERSION),
        (shredlab_library::NAME, shredlab_library::VERSION),
    ]
}

/// Repo root: the first of the current dir and the executable's ancestors
/// that contains `models/registry` or `models/personal`.
fn find_repo_root() -> Option<PathBuf> {
    let mut starts = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        starts.extend(exe.parent().map(Path::to_path_buf));
    }
    starts.iter().flat_map(|s| s.ancestors()).find_map(|d| {
        let m = d.join("models");
        (m.join("registry").is_dir() || m.join("personal").is_dir()).then(|| d.to_path_buf())
    })
}

fn model_dirs(args: &ModelDirs) -> Vec<(PathBuf, ModelSource)> {
    if !args.models_dirs.is_empty() {
        return args
            .models_dirs
            .iter()
            .map(|d| (d.clone(), ModelSource::Dev))
            .collect();
    }
    if !env_dirs().is_empty() {
        return ModelRegistry::search_dirs(None, None);
    }
    match find_repo_root() {
        Some(root) => vec![
            (root.join("models").join("registry"), ModelSource::Bundled),
            (root.join("models").join("personal"), ModelSource::Personal),
        ],
        None => Vec::new(),
    }
}

fn scan(args: &ModelDirs) -> Result<ModelRegistry> {
    let dirs = model_dirs(args);
    if dirs.is_empty() {
        bail!("no model directories (use --models-dir or set SHREDLAB_MODELS_DIR)");
    }
    let reg = ModelRegistry::scan(&dirs);
    for p in &reg.problems {
        eprintln!("warning: {}: {}", p.path.display(), p.message);
    }
    Ok(reg)
}

fn cmd_models(dirs: &ModelDirs) -> Result<()> {
    let reg = scan(dirs)?;
    if reg.models.is_empty() {
        println!("no models found in:");
        for (d, _) in model_dirs(dirs) {
            println!("  {}", d.display());
        }
        return Ok(());
    }
    for e in &reg.models {
        let m = &e.manifest;
        println!(
            "{:<18} {}{}",
            m.id,
            m.name,
            if e.is_personal() { "  [personal]" } else { "" }
        );
        println!(
            "    adapter {}, stems: {}",
            m.adapter.as_str(),
            m.stems.join(", ")
        );
        println!(
            "    license {}{}{}",
            m.license.spdx,
            if m.license.source.is_empty() {
                String::new()
            } else {
                format!(" ({})", m.license.source)
            },
            if e.is_personal() {
                " - personal use, licensing is your responsibility"
            } else {
                ""
            }
        );
        println!("    {} [{}]", e.dir.display(), e.source.as_str());
    }
    Ok(())
}

fn cmd_selftest(
    model: Option<&str>,
    device: DevicePref,
    dirs: &ModelDirs,
    rt: &RuntimeArgs,
) -> Result<bool> {
    let reg = scan(dirs)?;
    let entries: Vec<_> = match model {
        Some(id) => vec![reg.require(id)?.clone()],
        None => reg.models.clone(),
    };
    if entries.is_empty() {
        bail!("no models found");
    }
    let eps: Vec<Ep> = match device {
        DevicePref::Cpu => vec![Ep::Cpu],
        DevicePref::Gpu => Ep::gpu_candidates(),
        DevicePref::Auto => Ep::gpu_candidates().into_iter().chain([Ep::Cpu]).collect(),
    };
    if eps.is_empty() {
        bail!("no GPU execution provider in this build");
    }
    let ov = rt.overrides();
    let cache = rt.cache_dir();
    println!(
        "ONNX Runtime {}",
        shredlab_separation::runtime::ort_version()
    );
    let mut all_ok = true;
    for e in &entries {
        let sha = shredlab_separation::selftest::sha256_file(&e.onnx_path())?;
        let sha_ok = sha.eq_ignore_ascii_case(&e.manifest.sha256);
        println!(
            "{} ({}): sha256 {}",
            e.id(),
            e.dir.display(),
            if sha_ok { "ok" } else { "MISMATCH" }
        );
        all_ok &= sha_ok;
        for &ep in &eps {
            let label = format!(
                "{}{}",
                ep.label(),
                if ep == Ep::DirectMl && ov.dml_graph_fusion == Some(true) {
                    " (graph fusion FORCED ON)"
                } else {
                    ""
                }
            );
            match run_selftest(e, ep, &ov, Some(&cache)) {
                Ok(r) => {
                    println!(
                        "  {label}: {} ({:.2}s)",
                        if r.passed { "PASS" } else { "FAIL" },
                        r.seconds
                    );
                    for s in &r.stems {
                        println!(
                            "    {:<8} probe SNR {:7.1} dB  rms err {:.2e} (ratio {:.4})  {}",
                            s.stem,
                            s.snr_db,
                            s.rms_err,
                            s.rms_ratio,
                            if s.pass { "ok" } else { "FAIL" }
                        );
                    }
                    all_ok &= r.passed;
                }
                Err(err) => {
                    println!("  {label}: ERROR {err}");
                    all_ok = false;
                }
            }
        }
    }
    Ok(all_ok)
}

fn cmd_decode(input: &Path, output: &Path) -> Result<()> {
    let t0 = Instant::now();
    let audio = shredlab_io::load_for_separation(input)
        .with_context(|| format!("decoding {}", input.display()))?;
    shredlab_io::write_wav(
        output,
        audio.sample_rate,
        &audio.as_slices(),
        WavFormat::F32,
    )?;
    println!(
        "{} -> {} ({:.2}s audio, {} samples, {:.2}s)",
        input.display(),
        output.display(),
        audio.duration_secs(),
        audio.len(),
        t0.elapsed().as_secs_f64()
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_separate(
    input: &Path,
    model: &str,
    out: &Path,
    device: DevicePref,
    int16: bool,
    no_selftest: bool,
    shifts: usize,
    dirs: &ModelDirs,
    rt: &RuntimeArgs,
) -> Result<()> {
    let reg = scan(dirs)?;
    let entry = reg.require(model)?;
    let m = &entry.manifest;

    let t0 = Instant::now();
    let decoded =
        shredlab_io::decode_file(input).with_context(|| format!("decoding {}", input.display()))?;
    let audio = shredlab_io::resample(&decoded, m.sample_rate)?;
    let t_decode = t0.elapsed().as_secs_f64();
    eprintln!(
        "input: {} ({:.1}s, {} Hz{}) decoded in {t_decode:.2}s",
        input.display(),
        audio.duration_secs(),
        decoded.sample_rate,
        if decoded.sample_rate != m.sample_rate {
            format!(" -> {}", m.sample_rate)
        } else {
            String::new()
        }
    );

    let t1 = Instant::now();
    let opts = LoadOptions {
        device,
        cache_dir: Some(rt.cache_dir()),
        overrides: rt.overrides(),
        verify_sha256: true,
        self_test: !no_selftest,
    };
    let mut sep = Separator::load(entry, opts, &mut |msg| eprintln!("  {msg}"))?;
    let t_load = t1.elapsed().as_secs_f64();

    let t2 = Instant::now();
    let cancel = AtomicBool::new(false);
    let mut last = -1i32;
    let stems = sep.separate_shifts(
        &audio.as_slices(),
        shifts,
        &mut |f| {
            let pct = (f * 100.0) as i32;
            if pct != last {
                last = pct;
                eprint!("\r  separating: {pct:3}%");
                let _ = std::io::stderr().flush();
            }
        },
        &cancel,
    )?;
    eprintln!();
    let t_sep = t2.elapsed().as_secs_f64();

    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    let fmt = if int16 {
        WavFormat::I16
    } else {
        WavFormat::F32
    };
    for (i, name) in stems.names.iter().enumerate() {
        let path = out.join(format!("{name}.wav"));
        shredlab_io::write_wav(&path, stems.sample_rate, &stems.stem(i), fmt)?;
    }

    for r in sep.rejected() {
        println!("device rejected: {}: {}", r.ep.label(), r.reason);
    }
    println!("model:      {} ({})", m.id, m.name);
    println!("device:     {}", sep.ep().label());
    println!("self-test:  {}", sep.selftest_status());
    println!("shifts:     {}", shifts.max(1));
    println!(
        "timing:     decode {t_decode:.2}s, load {t_load:.2}s, separate {t_sep:.2}s ({:.1}x realtime)",
        audio.duration_secs() / t_sep
    );
    println!(
        "output:     {} ({} stems: {})",
        out.display(),
        stems.names.len(),
        stems.names.join(", ")
    );
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match &cli.command {
        Command::Info => {
            println!("Stem Extractor");
            for (name, version) in crate_versions() {
                println!("  {name:<18} {version}");
            }
            println!(
                "  {:<18} {}",
                "onnxruntime",
                shredlab_separation::runtime::ort_version()
            );
            let gpus: Vec<&str> = Ep::gpu_candidates().iter().map(|e| e.label()).collect();
            println!(
                "  {:<18} {}",
                "gpu providers",
                if gpus.is_empty() {
                    "none".into()
                } else {
                    gpus.join(", ")
                }
            );
            Ok(true)
        }
        Command::Models { dirs } => cmd_models(dirs).map(|_| true),
        Command::Decode { input, output } => cmd_decode(input, output).map(|_| true),
        Command::Selftest {
            model,
            device,
            dirs,
            rt,
        } => cmd_selftest(model.as_deref(), *device, dirs, rt),
        Command::Separate {
            input,
            model,
            out,
            device,
            int16,
            no_selftest,
            shifts,
            dirs,
            rt,
        } => cmd_separate(
            input,
            model,
            out,
            *device,
            *int16,
            *no_selftest,
            *shifts,
            dirs,
            rt,
        )
        .map(|_| true),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn lists_all_crates() {
        let versions = crate_versions();
        assert!(versions.iter().all(|(_, v)| !v.is_empty()));
        assert!(versions.iter().any(|(n, _)| *n == "shredlab-separation"));
    }

    #[test]
    fn parses_separate() {
        let cli = Cli::try_parse_from([
            "shredlab-cli",
            "separate",
            "song.mp3",
            "--model",
            "htdemucs_6s",
            "--out",
            "o",
            "--device",
            "cpu",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Separate {
                device: DevicePref::Cpu,
                shifts: 1,
                ..
            }
        ));
        let cli = Cli::try_parse_from([
            "shredlab-cli",
            "separate",
            "song.mp3",
            "--model",
            "htdemucs_6s",
            "--out",
            "o",
            "--shifts",
            "2",
        ])
        .unwrap();
        assert!(matches!(cli.command, Command::Separate { shifts: 2, .. }));
        let cli = Cli::try_parse_from(["shredlab-cli", "selftest", "--debug-dml-fusion"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Selftest {
                rt: RuntimeArgs {
                    debug_dml_fusion: true,
                    ..
                },
                ..
            }
        ));
    }
}
