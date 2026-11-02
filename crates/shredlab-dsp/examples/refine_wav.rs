// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Applies a RefineChain to a stereo WAV so you can listen to it.
//!
//! cargo run --release -p shredlab-dsp --example refine_wav -- <input.wav | --song "<title>" --stem <name>>
//!     [--pan left|center|right|<center>,<width>[,<softness>]] [--invert]
//!     [--eq <preset id>] [--out <file.wav>]
//!
//! Without --out, writes out/refine/<name>_<settings>.wav (gitignored).
//! Library songs are read only. Examples:
//!   --song "Here I Go Again" --stem guitar --pan left
//!   --song "Here I Go Again" --stem guitar --pan left --invert

use std::path::PathBuf;

use shredlab_dsp::refine::{
    eq_preset, refine_offline, PanParams, RefineChain, RefineParams, EQ_PRESETS, FIELD_BANDS,
    FIELD_BINS, FIELD_LEN,
};

#[path = "common/mod.rs"]
mod common;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| {
        args.iter()
            .position(|a| a == k)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let (input, scale, name) = if let Some(title) = get("--song") {
        let stem = get("--stem").unwrap_or_else(|| "guitar".into());
        let dir = common::find_song(&title).unwrap_or_else(|| panic!("song {title:?} not found"));
        let song = common::load_song(&dir, None);
        let (_, path, scale) = song
            .stems
            .iter()
            .find(|s| s.0 == stem)
            .unwrap_or_else(|| panic!("no stem {stem}"))
            .clone();
        (
            path,
            scale,
            format!("{}_{stem}", song.title.replace(' ', "_")),
        )
    } else {
        let p = PathBuf::from(args.first().expect("input wav"));
        let n = p.file_stem().unwrap().to_string_lossy().into_owned();
        (p, 1.0, n)
    };
    let mut params = RefineParams::default();
    let mut tag = String::new();
    if let Some(pan) = get("--pan") {
        params.pan = match pan.as_str() {
            "left" => PanParams::left(),
            "center" => PanParams::center(),
            "right" => PanParams::right(),
            s => {
                let v: Vec<f32> = s.split(',').map(|x| x.parse().expect("number")).collect();
                PanParams {
                    enabled: true,
                    center: v[0],
                    width: v[1],
                    softness: v.get(2).copied().unwrap_or(0.2),
                    invert: false,
                }
            }
        };
        tag += &format!("_pan-{}", pan.replace(',', "_"));
    }
    // `--invert` (alias `--remove`): everything except the window, like an
    // "on" row with an exception.
    if args.iter().any(|a| a == "--invert" || a == "--remove") {
        params.pan.invert = true;
        tag += "_invert";
    }
    if let Some(id) = get("--eq") {
        let pr = eq_preset(&id).unwrap_or_else(|| {
            let ids: Vec<_> = EQ_PRESETS.iter().map(|p| p.id).collect();
            panic!("unknown preset {id}; one of {ids:?}")
        });
        params.eq = pr.eq;
        tag += &format!("_eq-{id}");
    }
    let (sr, ch) = common::read_wav(&input, scale);
    let t = std::time::Instant::now();
    let out = refine_offline(&params, sr, &ch[0], &ch[1]);
    let el = t.elapsed().as_secs_f64();
    // Where the input's energy sits (the live display's data, summed).
    let mut chain = RefineChain::new(sr);
    chain.set_analysis(true);
    let mut field = [0.0f32; FIELD_LEN];
    let mut total = [0.0f64; FIELD_BINS];
    let (mut bl, mut br) = (vec![0.0f32; 4096], vec![0.0f32; 4096]);
    for (cl, cr) in ch[0].chunks(4096).zip(ch[1].chunks(4096)) {
        let m = cl.len();
        chain.process(cl, cr, &mut bl[..m], &mut br[..m]);
        let hops = chain.take_field(&mut field) as f64;
        for (b, t) in total.iter_mut().enumerate() {
            *t += (0..FIELD_BANDS)
                .map(|k| field[k * FIELD_BINS + b] as f64)
                .sum::<f64>()
                * hops;
        }
    }
    let all: f64 = total.iter().sum::<f64>().max(1e-30);
    let third = |r: std::ops::Range<usize>| 100.0 * total[r].iter().sum::<f64>() / all;
    println!(
        "input stereo field: left {:.0} %, center {:.0} %, right {:.0} % (pan thirds)",
        third(0..14),
        third(14..27),
        third(27..FIELD_BINS)
    );
    let path = get("--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("out/refine/{name}{tag}.wav")));
    common::write_wav(&path, sr, &out);
    println!(
        "{} -> {} ({:.1} s of audio in {:.2} s)\nparams: {}",
        input.display(),
        path.display(),
        ch[0].len() as f64 / sr as f64,
        el,
        serde_json::to_string(&params).unwrap()
    );
}
