//! End-to-end golden tests against the Python reference outputs in
//! `tests/fixtures/local/` (gitignored; `tools/make_goldens.py`) using the
//! model folders in `models/registry` and `models/personal`.
//!
//! Slow and machine-specific, so ignored by default:
//!
//! ```text
//! cargo test --release -p stemx-separation --test golden -- --ignored --nocapture --test-threads 1
//! ```
//!
//! Each test checks the pipeline stage by stage (graph inputs, graph
//! outputs, DSP after the graph) on CPU, then full separation on CPU and on
//! the GPU (if one passes the self-test), and requires >= 60 dB SNR per
//! loud stem: rms >= -40 dB relative to the input mix. Quieter stems (e.g.
//! guitar at -51 dB in the demucs excerpt) are reported but not asserted:
//! their SNR mostly measures ONNX Runtime build differences (pyke's static
//! ORT 1.31 vs Microsoft's builds differ at ~80 dB on the raw graph output).

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use stemx_separation::adapters::{DemucsHybrid, RoformerMask};
use stemx_separation::runtime::build_session;
use stemx_separation::{
    DevicePref, Ep, LoadOptions, ModelRegistry, ModelSource, Separator, SessionOverrides,
};

const MIN_SNR_DB: f64 = 60.0;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn local() -> Option<PathBuf> {
    let d = root().join("tests/fixtures/local");
    if d.join("index.json").is_file() {
        Some(d)
    } else {
        eprintln!(
            "skipping: {} not found (run tools/make_goldens.py)",
            d.display()
        );
        None
    }
}

fn registry() -> ModelRegistry {
    ModelRegistry::scan(&[
        (root().join("models/registry"), ModelSource::Bundled),
        (root().join("models/personal"), ModelSource::Personal),
    ])
}

fn read_bin(dir: &Path, name: &str) -> Vec<f32> {
    let bytes = std::fs::read(dir.join(format!("{name}.bin"))).unwrap();
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

fn snr_db(reference: &[f32], got: &[f32]) -> f64 {
    assert_eq!(reference.len(), got.len());
    let (mut s, mut e) = (0.0f64, 0.0f64);
    for (&r, &g) in reference.iter().zip(got) {
        s += (r as f64).powi(2);
        e += (g as f64 - r as f64).powi(2);
    }
    10.0 * ((s + 1e-30) / (e + 1e-30)).log10()
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

/// Per-stem SNR of flat (N, C, L) buffers; asserts loud stems (rms >= -40 dB
/// relative to `mix_rms`) >= MIN_SNR_DB.
fn compare_stems(label: &str, names: &[String], reference: &[f32], got: &[f32], mix_rms: f64) {
    let per = reference.len() / names.len();
    let rmss: Vec<f64> = (0..names.len())
        .map(|i| rms(&reference[i * per..(i + 1) * per]))
        .collect();
    let mut line = format!("{label}:");
    let mut failures = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let snr = snr_db(
            &reference[i * per..(i + 1) * per],
            &got[i * per..(i + 1) * per],
        );
        let rel_db = 20.0 * (rmss[i] / mix_rms).log10();
        let loud = rel_db >= -40.0;
        line.push_str(&format!(
            " {name} {snr:.1}{}",
            if loud {
                String::new()
            } else {
                format!("(quiet {rel_db:.0} dB)")
            }
        ));
        if loud && snr < MIN_SNR_DB {
            failures.push(format!("{name} {snr:.1} dB"));
        }
    }
    eprintln!("{line}");
    assert!(
        failures.is_empty(),
        "{label}: below {MIN_SNR_DB} dB: {failures:?}"
    );
}

fn full_separation(
    entry: &stemx_separation::ModelEntry,
    input: &[f32],
    reference: &[f32],
    device: DevicePref,
) {
    full_separation_shifts(entry, input, reference, device, 1);
}

fn full_separation_shifts(
    entry: &stemx_separation::ModelEntry,
    input: &[f32],
    reference: &[f32],
    device: DevicePref,
    shifts: usize,
) {
    let cache = std::env::temp_dir().join("stemx-golden-cache");
    let opts = LoadOptions {
        device,
        cache_dir: Some(cache),
        ..Default::default()
    };
    let mut sep = Separator::load(entry, opts, &mut |m| eprintln!("  {m}")).unwrap();
    if device == DevicePref::Gpu && !sep.ep().is_gpu() {
        eprintln!("no working GPU; skipping GPU run");
        return;
    }
    let len = input.len() / 2;
    let t0 = Instant::now();
    let stems = sep
        .separate_shifts(
            &[&input[..len], &input[len..]],
            shifts,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
    let label = format!(
        "full separation shifts={shifts} on {} ({:.2}s)",
        sep.ep().label(),
        t0.elapsed().as_secs_f64()
    );
    compare_stems(&label, &stems.names, reference, &stems.data, rms(input));
}

#[test]
#[ignore = "slow; needs tests/fixtures/local and model folders"]
fn demucs_golden() {
    let Some(dir) = local() else { return };
    let reg = registry();
    let Some(entry) = reg.get("htdemucs_6s") else {
        eprintln!("skipping: htdemucs_6s model not found");
        return;
    };
    let m = &entry.manifest;
    let session = build_session(
        &entry.onnx_path(),
        Ep::Cpu,
        &m.ep_options("cpu"),
        &SessionOverrides::default(),
        None,
    )
    .unwrap();
    let mut a = DemucsHybrid::new(m, session).unwrap();

    let mix = read_bin(&dir, "demucs_w0_mix");
    let mag_ref = read_bin(&dir, "demucs_w0_mag");
    let mag = a.spec_input(&mix);
    eprintln!("stage mag (STFT) SNR {:.1} dB", snr_db(&mag_ref, &mag));
    assert!(snr_db(&mag_ref, &mag) > 100.0);

    let (wave, spec) = a.run_graph(&mix, &mag_ref).unwrap();
    let wave_ref = read_bin(&dir, "demucs_w0_wave");
    let spec_ref = read_bin(&dir, "demucs_w0_spec");
    eprintln!(
        "stage graph (CPU) SNR wave {:.1} dB, spec {:.1} dB",
        snr_db(&wave_ref, &wave),
        snr_db(&spec_ref, &spec)
    );
    let out = a.combine(wave_ref, &spec_ref);
    let out_ref = read_bin(&dir, "demucs_w0_out");
    compare_stems("stage wave+ispec", &m.stems, &out_ref, &out, rms(&mix));
    drop(a);

    let input = read_bin(&dir, "demucs_input");
    let stems_ref = read_bin(&dir, "demucs_stems");
    full_separation(entry, &input, &stems_ref, DevicePref::Cpu);
    full_separation(entry, &input, &stems_ref, DevicePref::Gpu);
}

#[test]
#[ignore = "slow; needs tests/fixtures/local and the personal bs-roformer-sw model"]
fn roformer_golden() {
    let Some(dir) = local() else { return };
    if !dir.join("roformer_stems.bin").is_file() {
        eprintln!("skipping: roformer goldens not found");
        return;
    }
    let reg = registry();
    let Some(entry) = reg.get("bs-roformer-sw") else {
        eprintln!("skipping: bs-roformer-sw model not found");
        return;
    };
    let m = &entry.manifest;
    let session = build_session(
        &entry.onnx_path(),
        Ep::Cpu,
        &m.ep_options("cpu"),
        &SessionOverrides::default(),
        None,
    )
    .unwrap();
    let mut a = RoformerMask::new(m, session).unwrap();
    let chunk = m.segment_samples;

    let input = read_bin(&dir, "roformer_input");
    let len = input.len() / 2;
    let mut c0 = input[..chunk].to_vec();
    c0.extend_from_slice(&input[len..len + chunk]);
    let specs = a.stft_channels(&c0);
    let inp = RoformerMask::graph_input(&specs);
    let stft_ref = read_bin(&dir, "roformer_c0_stft");
    eprintln!("stage stft SNR {:.1} dB", snr_db(&stft_ref, &inp));
    assert!(snr_db(&stft_ref, &inp) > 100.0);

    let mask = a.run_graph(&stft_ref).unwrap();
    let mask_ref = read_bin(&dir, "roformer_c0_mask");
    eprintln!(
        "stage graph (CPU) mask SNR {:.1} dB",
        snr_db(&mask_ref, &mask)
    );
    let mask_own = a.run_graph(&inp).unwrap();
    eprintln!(
        "stage graph on Rust STFT input: mask SNR {:.1} dB",
        snr_db(&mask_ref, &mask_own)
    );
    let out_own = a.apply_mask(&specs, &mask_own);

    // DSP after the graph, on the reference mask and the reference STFT
    let ref_specs: Vec<Vec<stemx_dsp::Complex64>> = stft_ref
        .chunks(stft_ref.len() / 2)
        .map(|c| {
            c.as_chunks::<2>()
                .0
                .iter()
                .map(|p| stemx_dsp::Complex64::new(p[0] as f64, p[1] as f64))
                .collect()
        })
        .collect();
    let out = a.apply_mask(&ref_specs, &mask_ref);
    let out_ref = read_bin(&dir, "roformer_c0_out");
    compare_stems("stage mask+istft", &m.stems, &out_ref, &out, rms(&c0));
    compare_stems("chunk 0 end to end", &m.stems, &out_ref, &out_own, rms(&c0));
    drop(a);

    let stems_ref = read_bin(&dir, "roformer_stems");
    full_separation(entry, &input, &stems_ref, DevicePref::Cpu);
    full_separation(entry, &input, &stems_ref, DevicePref::Gpu);
}

#[test]
#[ignore = "slow; needs tests/fixtures/local and model folders"]
fn demucs_shifts_golden() {
    let Some(dir) = local() else { return };
    if !dir.join("demucs_shifts2_stems.bin").is_file() {
        eprintln!("skipping: demucs_shifts2_stems not found (tools/make_goldens.py --shifts-only)");
        return;
    }
    let reg = registry();
    let Some(entry) = reg.get("htdemucs_6s") else {
        eprintln!("skipping: htdemucs_6s model not found");
        return;
    };
    assert_eq!(
        stemx_separation::shift_offsets(2, entry.manifest.sample_rate),
        vec![0, 11025]
    );
    let input = read_bin(&dir, "demucs_input");
    let stems_ref = read_bin(&dir, "demucs_shifts2_stems");
    full_separation_shifts(entry, &input, &stems_ref, DevicePref::Cpu, 2);
    full_separation_shifts(entry, &input, &stems_ref, DevicePref::Gpu, 2);
    // sanity: shifts change the output (vs the single-pass golden)
    let single = read_bin(&dir, "demucs_stems");
    let per = single.len() / 6;
    let d = snr_db(&single[..per], &stems_ref[..per]);
    eprintln!(
        "drums: shifts=2 vs shifts=1 reference SNR {d:.1} dB (expected well below {MIN_SNR_DB})"
    );
    assert!(d < MIN_SNR_DB);
}
