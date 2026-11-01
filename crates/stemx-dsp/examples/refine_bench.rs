//! CPU cost of one RefineChain (one stereo stem) in real-time-sized blocks.
//!
//! cargo run --release -p stemx-dsp --example refine_bench [seconds]

use std::time::Instant;

use stemx_dsp::refine::{eq_preset, PanParams, RefineChain, RefineParams, FIELD_LEN};

const SR: u32 = 44_100;
const BLOCK: usize = 256;

fn noise(n: usize, seed: u64) -> Vec<f32> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            ((s >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0) * 0.3
        })
        .collect()
}

fn main() {
    let secs: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(120);
    let n = SR as usize * secs;
    let (l, r) = (noise(n, 1), noise(n, 2));
    let eq = eq_preset("guitar-focus").unwrap().eq;
    let pan = PanParams::left();
    let cases: [(&str, RefineParams, bool); 5] = [
        ("bypass (identity)", RefineParams::default(), false),
        (
            "bypass + stereo-field analysis",
            RefineParams::default(),
            true,
        ),
        (
            "EQ only (guitar-focus)",
            RefineParams {
                eq,
                ..Default::default()
            },
            false,
        ),
        (
            "pan mask only (keep left)",
            RefineParams {
                pan,
                ..Default::default()
            },
            false,
        ),
        ("pan + EQ + analysis", RefineParams { pan, eq }, true),
    ];
    println!("{secs} s of stereo audio per case, blocks of {BLOCK}, 44.1 kHz");
    println!(
        "{:<34} {:>12} {:>12} {:>14}",
        "case", "ms/s audio", "% of a core", "6 stems: %"
    );
    let mut field = [0.0f32; FIELD_LEN];
    for (name, p, analysis) in cases {
        let mut chain = RefineChain::new(SR);
        chain.set_params_now(&p);
        chain.set_analysis(analysis);
        let mut bl = [0.0f32; BLOCK];
        let mut br = [0.0f32; BLOCK];
        let mut sink = 0.0f32;
        let t = Instant::now();
        for (cl, cr) in l.chunks(BLOCK).zip(r.chunks(BLOCK)) {
            let m = cl.len();
            chain.process(cl, cr, &mut bl[..m], &mut br[..m]);
            sink += bl[0];
            if analysis {
                chain.take_field(&mut field);
            }
        }
        let el = t.elapsed().as_secs_f64();
        let per_sec_ms = el * 1000.0 / secs as f64;
        println!(
            "{name:<34} {per_sec_ms:>12.3} {:>12.3} {:>14.2}",
            per_sec_ms / 10.0,
            6.0 * per_sec_ms / 10.0
        );
        std::hint::black_box(sink);
    }
}
