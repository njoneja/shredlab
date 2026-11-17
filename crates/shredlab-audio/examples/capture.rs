// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Lists input devices, then captures 2 s from one channel and prints the
//! RMS level every 250 ms.
//!
//! ```sh
//! cargo run -p shredlab-audio --example capture -- [device-id] [channel]
//! ```
//!
//! Without arguments it uses the default input device, channel 1.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use shredlab_audio::{list_input_devices, InputCapture};

fn main() {
    let devices = list_input_devices();
    if devices.is_empty() {
        println!("no input devices");
        return;
    }
    for d in &devices {
        let mark = if d.default { " (default)" } else { "" };
        println!("{:?}: {} channels{mark}", d.id, d.channels);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let id = args.first().map(String::as_str);
    let channel = args
        .get(1)
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(1)
        .max(1)
        - 1;

    // (sum of squares, count) since the last print.
    let acc = Arc::new(Mutex::new((0.0f64, 0usize)));
    let sink = acc.clone();
    let cap = match InputCapture::start(id, channel, move |block, _rate| {
        let mut a = sink.lock().unwrap();
        a.0 += block.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>();
        a.1 += block.len();
    }) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cannot start capture: {e}");
            std::process::exit(1);
        }
    };
    let info = cap.info().clone();
    println!(
        "capturing {} channel {} of {} at {} Hz",
        info.device_name,
        info.channel + 1,
        info.channels,
        info.sample_rate
    );
    for i in 1..=8 {
        std::thread::sleep(Duration::from_millis(250));
        let (sum, n) = std::mem::take(&mut *acc.lock().unwrap());
        let rms = if n > 0 { (sum / n as f64).sqrt() } else { 0.0 };
        let db = 20.0 * rms.max(1e-10).log10();
        println!("{:>5} ms: {n:>6} samples, RMS {db:7.1} dBFS", i * 250);
        if let Some(e) = cap.error() {
            println!("stream error: {e}");
        }
    }
    println!(
        "delivered {} samples, dropped {}",
        cap.delivered(),
        cap.dropped()
    );
    cap.stop();
}
