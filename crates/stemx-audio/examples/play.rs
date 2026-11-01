//! Plays a folder of stem WAVs through the engine, or renders an export.
//!
//! ```text
//! cargo run --release -p stemx-audio --example play -- <stems-dir> [options]
//!   --secs N          play for N seconds (default 10)
//!   --start S         seek to S seconds first
//!   --speed R         tempo ratio 0.5..1.5
//!   --pitch ST        semitones (fractional = cents)
//!   --loop A,B        A–B loop in seconds
//!   --click           click track (synthetic grid from --bpm, downbeat every 4)
//!   --bpm N           tempo of the synthetic grid (default 120)
//!   --count-in N      count-in clicks
//!   --only NAME       play only this stem
//!   --mute NAME       mute this stem (repeatable)
//!   --render OUT      render offline to OUT instead of playing (.wav/.flac/.mp3)
//! ```

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use stemx_audio::{ClickConfig, Engine, ExportFormat, RenderRequest, Track};

struct Args {
    dir: PathBuf,
    secs: f64,
    start: f64,
    speed: f64,
    pitch: f64,
    loop_region: Option<(f64, f64)>,
    click: bool,
    bpm: f64,
    count_in: u32,
    only: Option<String>,
    mute: Vec<String>,
    render: Option<PathBuf>,
}

fn parse() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut a = Args {
        dir: PathBuf::new(),
        secs: 10.0,
        start: 0.0,
        speed: 1.0,
        pitch: 0.0,
        loop_region: None,
        click: false,
        bpm: 120.0,
        count_in: 0,
        only: None,
        mute: vec![],
        render: None,
    };
    let num = |v: Option<String>, n: &str| -> Result<f64, String> {
        v.ok_or(format!("{n} needs a value"))?
            .parse()
            .map_err(|e| format!("{n}: {e}"))
    };
    while let Some(x) = it.next() {
        match x.as_str() {
            "--secs" => a.secs = num(it.next(), "--secs")?,
            "--start" => a.start = num(it.next(), "--start")?,
            "--speed" => a.speed = num(it.next(), "--speed")?,
            "--pitch" => a.pitch = num(it.next(), "--pitch")?,
            "--bpm" => a.bpm = num(it.next(), "--bpm")?,
            "--count-in" => a.count_in = num(it.next(), "--count-in")? as u32,
            "--click" => a.click = true,
            "--only" => a.only = it.next(),
            "--mute" => a.mute.extend(it.next()),
            "--render" => a.render = it.next().map(PathBuf::from),
            "--loop" => {
                let v = it.next().ok_or("--loop needs A,B")?;
                let (x, y) = v.split_once(',').ok_or("--loop needs A,B")?;
                let p = |s: &str| s.trim().parse::<f64>().map_err(|e| e.to_string());
                a.loop_region = Some((p(x)?, p(y)?));
            }
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            s => a.dir = PathBuf::from(s),
        }
    }
    if a.dir.as_os_str().is_empty() {
        return Err("usage: play <stems-dir> [options]".into());
    }
    Ok(a)
}

fn load_dir(dir: &Path) -> Result<Vec<Track>, String> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("wav")))
        .collect();
    paths.sort();
    let mut tracks = vec![];
    for p in paths {
        let audio = stemx_io::load_for_separation(&p).map_err(|e| e.to_string())?;
        let name = p.file_stem().unwrap().to_string_lossy().to_string();
        let mut ch = audio.channels.into_iter();
        let l = ch.next().unwrap_or_default();
        let r = ch.next().unwrap_or_else(|| l.clone());
        tracks.push(Track {
            name,
            channels: [Arc::new(l), Arc::new(r)],
            sample_rate: audio.sample_rate,
        });
    }
    if tracks.is_empty() {
        return Err(format!("no .wav files in {}", dir.display()));
    }
    // Equalize lengths (stems from one separation already match).
    let len = tracks.iter().map(|t| t.channels[0].len()).min().unwrap();
    for t in &mut tracks {
        for c in &mut t.channels {
            if c.len() != len {
                *c = Arc::new(c[..len].to_vec());
            }
        }
    }
    Ok(tracks)
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

fn main() {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let t0 = Instant::now();
    let tracks = load_dir(&args.dir).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    let names: Vec<&str> = tracks.iter().map(|t| t.name.as_str()).collect();
    println!("loaded {names:?} in {:.1} s", t0.elapsed().as_secs_f64());
    let gains: Vec<(String, f32)> = tracks
        .iter()
        .map(|t| {
            let on = match &args.only {
                Some(o) => *o == t.name,
                None => !args.mute.contains(&t.name),
            };
            (t.name.clone(), if on { 1.0 } else { 0.0 })
        })
        .collect();

    if let Some(out) = &args.render {
        let sr = tracks[0].sample_rate;
        let in_len = tracks[0].channels[0].len();
        let req = RenderRequest {
            tracks: tracks.clone(),
            original: None,
            mode: stemx_audio::MixMode::Stems,
            gains: gains.clone(),
            speed: args.speed,
            semitones: args.pitch,
        };
        let t = Instant::now();
        let mut last = -1;
        let mix = stemx_audio::render(
            &req,
            &mut |f| {
                let pct = (f * 10.0) as i32;
                if pct != last {
                    last = pct;
                    print!("{}% ", pct * 10);
                }
            },
            &AtomicBool::new(false),
        )
        .expect("render");
        let render_secs = t.elapsed().as_secs_f64();
        // Reference: the unstretched mix, to compare level.
        let plain = stemx_audio::render(
            &RenderRequest {
                speed: 1.0,
                semitones: 0.0,
                ..req.clone()
            },
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .expect("render");
        let in_dur = in_len as f64 / sr as f64;
        let out_dur = mix[0].len() as f64 / sr as f64;
        println!(
            "\nrendered {:.1} s -> {:.1} s (expected {:.1} s) in {:.1} s ({:.0}x real time)",
            in_dur,
            out_dur,
            in_dur / args.speed,
            render_secs,
            out_dur / render_secs
        );
        println!(
            "RMS L: stretched {:.4} vs plain {:.4}; peak {:.3}",
            rms(&mix[0]),
            rms(&plain[0]),
            stemx_audio::peak(&mix)
        );
        let fmt = match out.extension().and_then(|e| e.to_str()).unwrap_or("") {
            "flac" => ExportFormat::Flac,
            "mp3" => ExportFormat::Mp3 { kbps: 256 },
            _ => ExportFormat::Wav16,
        };
        let t = Instant::now();
        stemx_audio::encode(out, &mix, sr, fmt).expect("encode");
        println!(
            "encoded {} ({fmt:?}) in {:.1} s",
            out.display(),
            t.elapsed().as_secs_f64()
        );
        return;
    }

    let engine = Engine::new().expect("open audio device");
    let st = engine.stats();
    println!(
        "device: {} Hz, {} channels",
        st.device_sample_rate, st.device_channels
    );
    let duration = tracks[0].channels[0].len() as f64 / tracks[0].sample_rate as f64;
    engine.load(tracks);
    engine.set_gains(gains);
    engine.set_speed(args.speed);
    engine.set_pitch(args.pitch);
    engine.set_loop(args.loop_region);
    let beat = 60.0 / args.bpm;
    let beats: Vec<f64> = (0..)
        .map(|i| i as f64 * beat)
        .take_while(|&t| t < duration)
        .collect();
    let downbeats = beats.iter().step_by(4).copied().collect();
    engine.set_click(ClickConfig {
        enabled: args.click,
        beats,
        downbeats,
        subdivision: 1,
        count_in_beats: args.count_in,
        beats_per_bar: 4,
        volume: 0.7,
    });
    engine.set_on_end(Box::new(|| println!("[end of song]")));
    engine.set_on_loop_wrap(Box::new(|| println!("[loop wrap]")));
    if args.start > 0.0 {
        engine.seek(args.start);
    }
    engine.play();
    println!("after play: {:?}", engine.position());
    let start = Instant::now();
    while start.elapsed().as_secs_f64() < args.secs {
        std::thread::sleep(Duration::from_millis(500));
        let p = engine.position();
        let s = engine.stats();
        println!(
            "t={:5.1}s pos={:7.3} playing={} count_in={} | underruns={} cb_max={} buffered={} load={:.1}%",
            start.elapsed().as_secs_f64(),
            p.seconds,
            p.playing,
            p.count_in_beats_left,
            s.underruns,
            s.max_callback_frames,
            s.buffered_frames,
            s.producer_load * 100.0
        );
    }
    engine.pause();
    println!("after pause: {:?}", engine.position());
    let s = engine.stats();
    println!(
        "done: underruns={} callbacks={} producer load={:.1}% of one core",
        s.underruns,
        s.callbacks,
        s.producer_load * 100.0
    );
}
