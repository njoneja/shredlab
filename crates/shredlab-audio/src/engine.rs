// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Real-time playback: a producer thread renders the [`Pipeline`] into a
//! lock-free ring buffer; the cpal callback only copies frames out.

use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use rubato::{Resampler, SincFixedIn};

use crate::mix::MixMode;
use crate::pipeline::{Meta, Pipeline, F_END, F_WRAP};
use crate::rate::{clamp_cents, rate, sinc_params, RATIO_HEADROOM};
use crate::tone::{Tone, ToneSound};
use crate::{AudioError, ClickConfig, Position, Track};
use shredlab_dsp::refine::{RefineParams, FIELD_LEN};

/// Frames rendered per pipeline call (song rate).
const BLOCK: usize = 256;
/// Ring capacity in device frames.
const RING: usize = 16_384;
/// Minimum fill target (seconds) of the ring buffer.
const MIN_AHEAD_SECS: f64 = 0.025;
/// How long control calls wait for the producer thread to confirm.
const REPLY_TIMEOUT: Duration = Duration::from_millis(500);
/// How often the producer publishes the stereo field (about 30 Hz).
const FIELD_EVERY: Duration = Duration::from_millis(33);
/// Paused this long with no reference tone sounding, the output stream is
/// closed and the producer waits for commands instead of polling every
/// millisecond (idle CPU wakeups); `play` or a tone opens a fresh stream.
/// Short pauses keep the stream running, so Bluetooth
/// outputs stay awake.
const SUSPEND_AFTER: Duration = Duration::from_secs(5);
/// Longest wait for a command while suspended.
const SUSPENDED_WAIT: Duration = Duration::from_millis(250);
/// Time constant of the master volume smoothing (no zipper noise).
const MASTER_SMOOTH_SECS: f32 = 0.01;
/// Fade length (song frames) when the rate correction adds or removes the
/// resampler at the song rate; the rest of the block stays silent so the
/// resampler's delay line holds nothing audible when it is dropped.
const SWITCH_FADE: usize = BLOCK * 3 / 4;

/// Latest stereo field of the analyzed track (single slot; the producer
/// overwrites it, readers take it).
#[derive(Default)]
struct FieldSlot {
    fresh: bool,
    bins: Vec<f32>,
}

/// Master volume (linear 0..1, as f32 bits); 1.0 by default.
struct MasterGain(AtomicU32);

impl Default for MasterGain {
    fn default() -> Self {
        Self(AtomicU32::new(1.0f32.to_bits()))
    }
}

impl MasterGain {
    fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
    fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
}

/// One output frame plus the metadata the callback publishes.
#[derive(Debug, Clone, Copy, Default)]
struct Frame {
    l: f32,
    r: f32,
    meta: Meta,
}

/// State shared between the API, the producer thread and the audio callback.
#[derive(Default)]
struct Shared {
    pos_bits: AtomicU64,
    flags: AtomicU32,
    underruns: AtomicU64,
    callbacks: AtomicU64,
    end_count: AtomicU64,
    wrap_count: AtomicU64,
    max_cb_frames: AtomicUsize,
    stream_error: AtomicBool,
    device_changed: AtomicBool,
    quit: AtomicBool,
    duration_bits: AtomicU64,
    device_rate: AtomicU32,
    device_channels: AtomicU32,
    buffered: AtomicUsize,
    render_nanos: AtomicU64,
    rendered_frames: AtomicU64,
    field: Mutex<FieldSlot>,
    /// Master volume, applied to playback only.
    master: MasterGain,
    /// Playback-rate correction in cents (0 = off).
    rate_cents: AtomicI32,
}

/// Diagnostics (interface addition).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EngineStats {
    /// Callbacks that found the ring buffer short of frames (after it first filled).
    pub underruns: u64,
    pub callbacks: u64,
    pub device_sample_rate: u32,
    pub device_channels: u32,
    /// Largest buffer the device asked for in one callback (frames).
    pub max_callback_frames: usize,
    /// Frames currently queued ahead of the device.
    pub buffered_frames: usize,
    /// Producer render time / audio time produced (CPU load of one core, 0..1).
    pub producer_load: f64,
    /// Raw counters behind `producer_load` (for measuring an interval).
    pub render_nanos: u64,
    pub rendered_frames: u64,
}

type Callback = Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>;

enum Cmd {
    Load(Vec<Track>, Option<Track>),
    Mix(Vec<(String, f32)>, MixMode),
    Play(u16, Sender<Position>),
    Pause(u16, Sender<Position>),
    Seek(u16, f64, Sender<Position>),
    Gains(Vec<(String, f32)>),
    Speed(f64),
    Pitch(f64),
    Loop(Option<(f64, f64)>),
    Click(ClickConfig),
    Refine(String, RefineParams),
    RefineAll(Vec<(String, RefineParams)>),
    Analysis(Option<String>),
    Tone(f32, ToneSound),
    StopTone,
    Quit,
}

/// Plays tracks on the default output device. All methods are cheap and
/// non-blocking except `play`/`pause`/`seek`, which wait (a few ms) for the
/// producer thread so that [`Engine::position`] is immediately consistent.
pub struct Engine {
    tx: Mutex<Sender<Cmd>>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    gen: AtomicU16,
    pending: Mutex<Option<(u16, Position)>>,
    on_end: Callback,
    on_wrap: Callback,
}

impl Engine {
    /// Opens the default output device. Handles any device sample rate by
    /// resampling in the engine.
    pub fn new() -> Result<Engine, AudioError> {
        let shared = Arc::new(Shared::default());
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let on_end: Callback = Arc::new(Mutex::new(None));
        let on_wrap: Callback = Arc::new(Mutex::new(None));
        let thread = {
            let shared = shared.clone();
            let (on_end, on_wrap) = (on_end.clone(), on_wrap.clone());
            std::thread::Builder::new()
                .name("shredlab-audio".into())
                .spawn(move || producer_thread(rx, shared, ready_tx, on_end, on_wrap))
                .map_err(|e| AudioError::Device(format!("cannot start audio thread: {e}")))?
        };
        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => return Err(AudioError::Device("audio thread died".into())),
        }
        Ok(Engine {
            tx: Mutex::new(tx),
            shared,
            thread: Some(thread),
            gen: AtomicU16::new(0),
            pending: Mutex::new(None),
            on_end,
            on_wrap,
        })
    }

    fn send(&self, cmd: Cmd) {
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(cmd);
        }
    }

    /// Sends a transport command and records the resulting position until the
    /// output catches up with it.
    fn transport(&self, make: impl FnOnce(u16, Sender<Position>) -> Cmd) {
        let gen = self.gen.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        let (rtx, rrx) = mpsc::channel();
        self.send(make(gen, rtx));
        if let Ok(pos) = rrx.recv_timeout(REPLY_TIMEOUT) {
            if let Ok(mut p) = self.pending.lock() {
                *p = Some((gen, pos));
            }
        }
    }

    /// Replaces the tracks and keeps the position (crossfaded while playing),
    /// e.g. to switch from the original mix to stems seamlessly.
    pub fn load(&self, tracks: Vec<Track>) {
        self.load_with_original(tracks, None);
    }

    /// Like [`load`](Self::load), plus the original mix (sample-aligned with
    /// the stems) for the backing-track mode [`MixMode::Original`].
    pub fn load_with_original(&self, tracks: Vec<Track>, original: Option<Track>) {
        let len = tracks
            .iter()
            .chain(original.as_ref())
            .map(Track::len)
            .min()
            .unwrap_or(0);
        let sr = tracks
            .first()
            .or(original.as_ref())
            .map_or(44_100, |t| t.sample_rate.max(1));
        let dur = len as f64 / sr as f64;
        self.shared
            .duration_bits
            .store(dur.to_bits(), Ordering::Relaxed);
        self.send(Cmd::Load(tracks, original));
    }

    /// Starts playback: at or past the end restarts from 0; with a loop, a
    /// position outside [a, b) jumps to a; count-in first if configured.
    pub fn play(&self) {
        self.transport(Cmd::Play);
    }

    /// Pauses (aborts a running count-in).
    pub fn pause(&self) {
        self.transport(Cmd::Pause);
    }

    /// Seeks to `seconds` (clamped to [0, duration]); works while playing or paused.
    pub fn seek(&self, seconds: f64) {
        self.transport(|g, tx| Cmd::Seek(g, seconds, tx));
    }

    /// Linear gain per track name; 0 = silent; names not listed are silent.
    pub fn set_gains(&self, gains: Vec<(String, f32)>) {
        self.send(Cmd::Gains(gains));
    }

    /// Gains and [`MixMode`] at once (applied atomically; a mode change
    /// crossfades over [`crate::mix::MODE_XFADE`] samples).
    pub fn set_mix(&self, gains: Vec<(String, f32)>, mode: MixMode) {
        self.send(Cmd::Mix(gains, mode));
    }

    /// Tempo ratio 0.5..=1.5, pitch preserved.
    pub fn set_speed(&self, ratio: f64) {
        self.send(Cmd::Speed(ratio));
    }

    /// Pitch shift in semitones (semitones + cents/100), -12.5..=12.5, independent of speed.
    pub fn set_pitch(&self, semitones: f64) {
        self.send(Cmd::Pitch(semitones));
    }

    /// A–B loop in song seconds (`None` = off). Loops shorter than 50 ms are ignored.
    pub fn set_loop(&self, region: Option<(f64, f64)>) {
        self.send(Cmd::Loop(region));
    }

    pub fn set_click(&self, cfg: ClickConfig) {
        self.send(Cmd::Click(cfg));
    }

    /// Master volume, linear 0..1 (clamped; non-finite is ignored): scales
    /// everything played, music and click, smoothed. Playback only;
    /// offline renders and exports are not affected.
    pub fn set_master_volume(&self, volume: f32) {
        if volume.is_finite() {
            self.shared.master.set(volume.clamp(0.0, 1.0));
        }
    }

    /// Playback-rate correction in cents, clamped to
    /// ±[`MAX_RATE_CENTS`](crate::MAX_RATE_CENTS); 0 = off. The song plays
    /// `2^(cents/1200)` times faster, pitch and tempo together, on top of
    /// the speed (the stretcher is not involved). Positions, loop and end
    /// events stay in song time. Playback only; exports take
    /// [`RenderRequest::rate_cents`](crate::RenderRequest::rate_cents).
    pub fn set_rate_correction(&self, cents: i32) {
        self.shared
            .rate_cents
            .store(clamp_cents(cents), Ordering::Relaxed);
    }

    /// Plays a reference tone at `hz` (clamped to
    /// [`TONE_MIN_HZ`](crate::TONE_MIN_HZ)..=[`TONE_MAX_HZ`](crate::TONE_MAX_HZ);
    /// non-finite is ignored), at -12 dBFS under the master volume, whether
    /// or not the song plays. It ignores speed, pitch and the rate
    /// correction and is never exported. A new frequency or sound replaces
    /// the current tone after a 10 ms release.
    pub fn play_tone(&self, hz: f32, sound: ToneSound) {
        self.send(Cmd::Tone(hz, sound));
    }

    /// Stops the reference tone (30 ms release).
    pub fn stop_tone(&self) {
        self.send(Cmd::StopTone);
    }

    /// Refine parameters of one track (live and smoothed; also used for
    /// track sets loaded later). See [`crate::mix`].
    pub fn set_refine(&self, name: &str, p: RefineParams) {
        self.send(Cmd::Refine(name.to_string(), p));
    }

    /// Replaces all refine parameters (tracks not listed are unrefined).
    pub fn set_refine_all(&self, list: Vec<(String, RefineParams)>) {
        self.send(Cmd::RefineAll(list));
    }

    /// Stereo-field analysis on one track only (the open Refine panel), or
    /// off. Results come from [`Engine::take_field`].
    pub fn set_analysis(&self, name: Option<&str>) {
        self.send(Cmd::Analysis(name.map(str::to_string)));
    }

    /// Copies the newest stereo field of the analyzed track into `out`
    /// ([`FIELD_LEN`] floats, band-major: 3 bands × 41 pan bins) if one
    /// arrived since the last call. About 30 per second while playing.
    pub fn take_field(&self, out: &mut [f32; FIELD_LEN]) -> bool {
        let Ok(mut f) = self.shared.field.lock() else {
            return false;
        };
        if !f.fresh || f.bins.len() != FIELD_LEN {
            return false;
        }
        out.copy_from_slice(&f.bins);
        f.fresh = false;
        true
    }

    /// Position as heard at the output (cheap; fine to call at 30 Hz). Right
    /// after `play`/`pause`/`seek` it reports the commanded state.
    pub fn position(&self) -> Position {
        let meta = Meta {
            pos: f64::from_bits(self.shared.pos_bits.load(Ordering::Acquire)),
            flags: self.shared.flags.load(Ordering::Acquire),
        };
        if let Ok(mut p) = self.pending.lock() {
            if let Some((g, pos)) = *p {
                if meta.gen() == g {
                    *p = None;
                } else {
                    return pos;
                }
            }
        }
        meta.position()
    }

    /// Duration in seconds of the loaded tracks (0 if none).
    pub fn duration(&self) -> f64 {
        f64::from_bits(self.shared.duration_bits.load(Ordering::Relaxed))
    }

    /// Called (on an engine events thread) when playback reaches the end of the
    /// song and stops, as heard. May call back into the engine.
    pub fn set_on_end(&self, f: Box<dyn Fn() + Send + Sync>) {
        if let Ok(mut c) = self.on_end.lock() {
            *c = Some(f);
        }
    }

    /// Interface addition: called (on the engine events thread) when the A–B loop
    /// wraps from b to a, as heard. Useful for the "playback-position after
    /// loop wrap" event.
    pub fn set_on_loop_wrap(&self, f: Box<dyn Fn() + Send + Sync>) {
        if let Ok(mut c) = self.on_wrap.lock() {
            *c = Some(f);
        }
    }

    /// Interface addition: diagnostics (underruns, device format, CPU load).
    pub fn stats(&self) -> EngineStats {
        let s = &self.shared;
        let frames = s.rendered_frames.load(Ordering::Relaxed);
        let rate = s.device_rate.load(Ordering::Relaxed).max(1);
        let audio_ns = frames as f64 * 1e9 / rate as f64;
        EngineStats {
            underruns: s.underruns.load(Ordering::Relaxed),
            callbacks: s.callbacks.load(Ordering::Relaxed),
            device_sample_rate: rate,
            device_channels: s.device_channels.load(Ordering::Relaxed),
            max_callback_frames: s.max_cb_frames.load(Ordering::Relaxed),
            buffered_frames: s.buffered.load(Ordering::Relaxed),
            producer_load: if audio_ns > 0.0 {
                s.render_nanos.load(Ordering::Relaxed) as f64 / audio_ns
            } else {
                0.0
            },
            render_nanos: s.render_nanos.load(Ordering::Relaxed),
            rendered_frames: frames,
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::Relaxed);
        self.send(Cmd::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The open device stream plus the producer side of its ring buffer.
struct Output {
    /// Held only to keep the device open; dropping it closes the stream.
    _stream: cpal::Stream,
    feeder: Feeder,
    device_name: String,
}

/// Producer side of the ring: renders the pipeline, resamples to the device
/// rate (and by the rate correction) if needed, adds the reference tone and
/// pushes frames.
struct Feeder {
    producer: rtrb::Producer<Frame>,
    rate: u32,
    /// Master gain currently applied (follows `Shared::master`).
    master: f32,
    resampler: Option<Rs>,
    /// The resampler was just added or removed behind a fade-out: fade the
    /// next block in.
    fade_in_next: bool,
}

/// The device-rate resampler, built with headroom for the rate correction.
struct Rs {
    /// Song rate it was built for.
    from: u32,
    to: u32,
    rs: SincFixedIn<f32>,
    out: Vec<Vec<f32>>,
    /// Rate correction its ratio is set to.
    cents: i32,
}

impl Rs {
    fn new(from: u32, to: u32, cents: i32) -> Option<Rs> {
        let base = to as f64 / from as f64;
        let rs = SincFixedIn::<f32>::new(base, RATIO_HEADROOM, sinc_params(), BLOCK, 2).ok()?;
        let out = rs.output_buffer_allocate(true);
        let mut r = Rs {
            from,
            to,
            rs,
            out,
            cents: 0,
        };
        r.set_cents(cents, false);
        Some(r)
    }

    /// Output frames per song frame: device/song rate over the playback rate.
    fn set_cents(&mut self, cents: i32, ramp: bool) {
        if cents != self.cents {
            let ratio = self.to as f64 / self.from as f64 / rate(cents);
            if self.rs.set_resample_ratio(ratio, ramp).is_ok() {
                self.cents = cents;
            }
        }
    }
}

/// Gain shape applied to one song-rate block around a resampler switch.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Fade {
    None,
    /// To silence over [`SWITCH_FADE`] frames; the resampler is switched
    /// after this block.
    Out,
    In,
}

fn open_output(shared: &Arc<Shared>) -> Result<Output, AudioError> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| AudioError::Device("no output device".into()))?;
    let device_name = device.name().unwrap_or_default();
    let mut supported = device
        .default_output_config()
        .map_err(|e| AudioError::Device(format!("{device_name}: {e}")))?;
    // Testing aid: request a specific device rate (exercises the resampler).
    if let Some(rate) = std::env::var("SHREDLAB_AUDIO_DEVICE_RATE")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    {
        if let Ok(mut configs) = device.supported_output_configs() {
            if let Some(c) = configs.find(|c| {
                c.channels() == supported.channels()
                    && c.min_sample_rate().0 <= rate
                    && c.max_sample_rate().0 >= rate
            }) {
                supported = c.with_sample_rate(cpal::SampleRate(rate));
            }
        }
    }
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let (producer, consumer) = rtrb::RingBuffer::<Frame>::new(RING);
    shared.stream_error.store(false, Ordering::Relaxed);
    shared
        .device_rate
        .store(config.sample_rate.0, Ordering::Relaxed);
    shared
        .device_channels
        .store(config.channels as u32, Ordering::Relaxed);
    use cpal::SampleFormat as F;
    let stream = match format {
        F::F32 => build::<f32>(&device, &config, consumer, shared.clone()),
        F::I16 => build::<i16>(&device, &config, consumer, shared.clone()),
        F::U16 => build::<u16>(&device, &config, consumer, shared.clone()),
        F::I32 => build::<i32>(&device, &config, consumer, shared.clone()),
        F::F64 => build::<f64>(&device, &config, consumer, shared.clone()),
        F::I8 => build::<i8>(&device, &config, consumer, shared.clone()),
        F::U8 => build::<u8>(&device, &config, consumer, shared.clone()),
        other => {
            return Err(AudioError::Device(format!(
                "unsupported sample format {other}"
            )));
        }
    }
    .map_err(|e| AudioError::Device(format!("{device_name}: {e}")))?;
    Ok(Output {
        _stream: stream,
        feeder: Feeder {
            producer,
            rate: config.sample_rate.0,
            master: shared.master.get(),
            resampler: None,
            fade_in_next: false,
        },
        device_name,
    })
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut consumer: rtrb::Consumer<Frame>,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let ch = config.channels as usize;
    let err_shared = shared.clone();
    let mut primed = false;
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                // Real-time: no locks, no allocation.
                let frames = data.len() / ch.max(1);
                shared.callbacks.fetch_add(1, Ordering::Relaxed);
                shared.max_cb_frames.fetch_max(frames, Ordering::Relaxed);
                let avail = consumer.slots().min(frames);
                let mut last = None;
                if let Ok(chunk) = consumer.read_chunk(avail) {
                    let (a, b) = chunk.as_slices();
                    for (i, f) in a.iter().chain(b.iter()).enumerate() {
                        let out = &mut data[i * ch..(i + 1) * ch];
                        if ch == 1 {
                            out[0] = T::from_sample(0.5 * (f.l + f.r));
                        } else {
                            out[0] = T::from_sample(f.l);
                            out[1] = T::from_sample(f.r);
                            for o in &mut out[2..] {
                                *o = T::EQUILIBRIUM;
                            }
                        }
                        if f.meta.flags & F_END != 0 {
                            shared.end_count.fetch_add(1, Ordering::Relaxed);
                        }
                        if f.meta.flags & F_WRAP != 0 {
                            shared.wrap_count.fetch_add(1, Ordering::Relaxed);
                        }
                        last = Some(f.meta);
                    }
                    chunk.commit_all();
                }
                if avail > 0 {
                    primed = true;
                }
                if avail < frames {
                    for s in &mut data[avail * ch..] {
                        *s = T::EQUILIBRIUM;
                    }
                    if primed {
                        shared.underruns.fetch_add(1, Ordering::Relaxed);
                    }
                }
                if let Some(m) = last {
                    shared.pos_bits.store(m.pos.to_bits(), Ordering::Release);
                    shared.flags.store(m.flags, Ordering::Release);
                }
            },
            move |_err| {
                err_shared.stream_error.store(true, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

impl Feeder {
    /// Scales a rendered block by the master volume, gliding to a new value.
    fn apply_master(&mut self, shared: &Shared, sr: u32, bufs: &mut Bufs) {
        let target = shared.master.get();
        if self.master == target && target == 1.0 {
            return;
        }
        let k = 1.0 - (-1.0 / (MASTER_SMOOTH_SECS * sr as f32)).exp();
        for (l, r) in bufs.l.iter_mut().zip(bufs.r.iter_mut()) {
            self.master += (target - self.master) * k;
            if (self.master - target).abs() < 1e-5 {
                self.master = target;
            }
            *l *= self.master;
            *r *= self.master;
        }
    }

    /// Whether the next block needs a resampler at all.
    fn wants_resampler(&self, src: u32, cents: i32) -> bool {
        src != self.rate || cents != 0
    }

    /// Brings the resampler in line with the song rate and the rate
    /// correction before a block. A ratio change on a running resampler
    /// glides over the block. Adding or removing it at the song rate (the
    /// correction turned on or off) jumps by its delay, so that happens
    /// behind a fade: this block fades out, the switch follows it
    /// ([`Self::finish_switch`]) and the next block fades in.
    fn prepare(&mut self, src: u32, cents: i32) -> Fade {
        if std::mem::take(&mut self.fade_in_next) {
            return Fade::In;
        }
        let want = self.wants_resampler(src, cents);
        match &mut self.resampler {
            Some(r) if r.from == src && want => {
                r.set_cents(cents, true);
                Fade::None
            }
            // At the song rate (`want` is false only there): off, or on.
            Some(r) if r.from == src => Fade::Out,
            None if want && src == self.rate => Fade::Out,
            None if !want => Fade::None,
            // First block, or a track set at another rate: no audio to keep.
            _ => {
                self.resampler = if want {
                    Rs::new(src, self.rate, cents)
                } else {
                    None
                };
                Fade::None
            }
        }
    }

    /// After a [`Fade::Out`] block: adds or removes the resampler.
    fn finish_switch(&mut self, src: u32, cents: i32) {
        self.resampler = if self.wants_resampler(src, cents) {
            Rs::new(src, self.rate, cents)
        } else {
            None
        };
        self.fade_in_next = true;
    }

    /// Renders until the ring holds the target amount of audio.
    fn fill(&mut self, pipe: &mut Pipeline, shared: &Shared, bufs: &mut Bufs, tone: &mut Tone) {
        let max_cb = shared.max_cb_frames.load(Ordering::Relaxed);
        let target = ((MIN_AHEAD_SECS * self.rate as f64) as usize)
            .max(2 * max_cb + BLOCK)
            .min(RING - 4 * BLOCK);
        let src_rate = pipe.sample_rate();
        tone.set_sample_rate(self.rate as f32);
        loop {
            let buffered = RING - self.producer.slots();
            shared.buffered.store(buffered, Ordering::Relaxed);
            if buffered >= target || self.producer.slots() < 3 * BLOCK {
                break;
            }
            let t0 = Instant::now();
            let cents = shared.rate_cents.load(Ordering::Relaxed);
            let fade = self.prepare(src_rate, cents);
            pipe.render(&mut bufs.l, &mut bufs.r, &mut bufs.meta);
            let m0 = self.master;
            self.apply_master(shared, src_rate, bufs);
            let m1 = self.master;
            match fade {
                Fade::None => {}
                Fade::Out => {
                    for (i, (l, r)) in bufs.l.iter_mut().zip(bufs.r.iter_mut()).enumerate() {
                        let g = SWITCH_FADE.saturating_sub(i + 1) as f32 / SWITCH_FADE as f32;
                        *l *= g;
                        *r *= g;
                    }
                }
                Fade::In => {
                    for (i, (l, r)) in bufs.l.iter_mut().zip(bufs.r.iter_mut()).enumerate() {
                        let g = (i as f32 / SWITCH_FADE as f32).min(1.0);
                        *l *= g;
                        *r *= g;
                    }
                }
            }
            let resampled = self.resampler.is_some();
            let (n, out_l, out_r): (usize, &[f32], &[f32]) = match &mut self.resampler {
                None => (BLOCK, &bufs.l, &bufs.r),
                Some(rs) => {
                    let input = [&bufs.l[..], &bufs.r[..]];
                    let Ok((_, n_out)) = rs.rs.process_into_buffer(&input, &mut rs.out, None)
                    else {
                        break;
                    };
                    (n_out, &rs.out[0], &rs.out[1])
                }
            };
            // The reference tone runs at the device rate, after the
            // resampler, so the rate correction does not shift it.
            let with_tone = tone.is_active();
            if with_tone {
                if bufs.tone.len() < n {
                    bufs.tone.resize(n, 0.0);
                }
                tone.render(&mut bufs.tone[..n]);
            }
            let Ok(mut chunk) = self.producer.write_chunk_uninit(n) else {
                break;
            };
            // Resampled: events (wrap/end) go on the output frame
            // proportional to their input frame, for any ratio.
            let mut ev = [(usize::MAX, 0u32); 4];
            let mut ne = 0;
            if resampled {
                for (i, m) in bufs.meta.iter().enumerate() {
                    let e = m.flags & (F_END | F_WRAP);
                    if e != 0 && ne < ev.len() {
                        ev[ne] = ((i * n / BLOCK).min(n.saturating_sub(1)), e);
                        ne += 1;
                    }
                }
            }
            let (a, b) = chunk.as_mut_slices();
            for (j, slot) in a.iter_mut().chain(b.iter_mut()).enumerate() {
                let meta = if resampled {
                    let src = (j * BLOCK / n.max(1)).min(BLOCK - 1);
                    let mut meta = bufs.meta[src];
                    meta.flags &= !(F_END | F_WRAP);
                    for &(k, e) in &ev[..ne] {
                        if k == j {
                            meta.flags |= e;
                        }
                    }
                    meta
                } else {
                    bufs.meta[j]
                };
                let (mut l, mut r) = (out_l[j], out_r[j]);
                if with_tone {
                    // Under the master volume, gliding with it.
                    let g = m0 + (m1 - m0) * (j + 1) as f32 / n as f32;
                    let t = bufs.tone[j] * g;
                    l += t;
                    r += t;
                }
                slot.write(Frame { l, r, meta });
            }
            // SAFETY: all n slots were initialized above.
            unsafe { chunk.commit_all() };
            if fade == Fade::Out {
                self.finish_switch(src_rate, cents);
            }
            shared
                .render_nanos
                .fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
            shared
                .rendered_frames
                .fetch_add(n as u64, Ordering::Relaxed);
        }
    }
}

struct Bufs {
    l: Vec<f32>,
    r: Vec<f32>,
    meta: Vec<Meta>,
    /// Reference tone at the device rate (grows to the largest block once).
    tone: Vec<f32>,
}

impl Bufs {
    fn new() -> Self {
        Bufs {
            l: vec![0.0; BLOCK],
            r: vec![0.0; BLOCK],
            meta: vec![Meta::default(); BLOCK],
            tone: vec![0.0; 2 * BLOCK],
        }
    }
}

/// Counts how long the producer has been idle: paused with no tone sounding.
#[derive(Default)]
struct IdleTimer(Option<Instant>);

impl IdleTimer {
    /// True once idle for [`SUSPEND_AFTER`].
    fn suspend(&mut self, busy: bool, now: Instant) -> bool {
        if busy {
            self.0 = None;
            false
        } else {
            now.duration_since(*self.0.get_or_insert(now)) >= SUSPEND_AFTER
        }
    }
}

/// Playing (or counting in), or a reference tone is sounding.
fn busy(pipe: &Pipeline, tone: &Tone) -> bool {
    pipe.is_playing() || tone.is_active()
}

fn producer_thread(
    rx: Receiver<Cmd>,
    shared: Arc<Shared>,
    ready: Sender<Result<(), AudioError>>,
    on_end: Callback,
    on_wrap: Callback,
) {
    let mut out = match open_output(&shared) {
        Ok(o) => Some(o),
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    let watcher = spawn_device_watcher(shared.clone(), out.as_ref().map(|o| o.device_name.clone()));
    let mut pipe = Pipeline::new(Vec::new(), &[], true);
    pipe.set_limiter(true);
    let mut bufs = Bufs::new();
    let mut tone = Tone::new(shared.device_rate.load(Ordering::Relaxed) as f32);
    let (ev_tx, ev_rx) = mpsc::channel::<Callback>();
    let events = std::thread::Builder::new()
        .name("shredlab-audio-events".into())
        .spawn(move || {
            for cb in ev_rx {
                if let Ok(cb) = cb.lock() {
                    if let Some(f) = cb.as_ref() {
                        f();
                    }
                }
            }
        })
        .ok();
    let (mut ends, mut wraps) = (0u64, 0u64);
    let mut last_reopen = Instant::now();
    let mut field = [0.0f32; FIELD_LEN];
    let mut last_field = Instant::now();
    let mut idle = IdleTimer::default();
    // The stream was closed for idleness (not lost): reopen at once on wake.
    let mut closed_idle = false;
    let mut next: Option<Cmd> = None;
    'main: loop {
        while let Some(cmd) = next.take().or_else(|| rx.try_recv().ok()) {
            match cmd {
                Cmd::Quit => break 'main,
                Cmd::Load(t, o) => pipe.load_with_original(t, o),
                Cmd::Mix(g, m) => pipe.set_mix(g, m),
                Cmd::Play(g, tx) => {
                    pipe.set_gen(g);
                    let _ = tx.send(pipe.play());
                }
                Cmd::Pause(g, tx) => {
                    pipe.set_gen(g);
                    let _ = tx.send(pipe.pause());
                }
                Cmd::Seek(g, s, tx) => {
                    pipe.set_gen(g);
                    let _ = tx.send(pipe.seek(s));
                }
                Cmd::Gains(g) => pipe.set_gains(g),
                Cmd::Speed(s) => pipe.set_speed(s),
                Cmd::Pitch(p) => pipe.set_pitch(p),
                Cmd::Loop(l) => pipe.set_loop(l),
                Cmd::Click(c) => pipe.set_click(c),
                Cmd::Refine(n, p) => pipe.set_refine(&n, p),
                Cmd::RefineAll(l) => pipe.set_refine_all(l),
                Cmd::Analysis(n) => pipe.set_analysis(n.as_deref()),
                Cmd::Tone(hz, sound) => tone.start(hz, sound),
                Cmd::StopTone => tone.stop(),
            }
        }
        // User callbacks run on their own thread, so they may call back into the Engine.
        let e = shared.end_count.load(Ordering::Relaxed);
        if e != ends {
            ends = e;
            let _ = ev_tx.send(on_end.clone());
        }
        let w = shared.wrap_count.load(Ordering::Relaxed);
        if w != wraps {
            wraps = w;
            let _ = ev_tx.send(on_wrap.clone());
        }
        // A sounding tone keeps the stream open; one started while
        // suspended wakes the loop (the command ends the wait) and reopens it.
        let suspend = idle.suspend(busy(&pipe, &tone), Instant::now());
        if suspend {
            // Idle: close the stream rather than pause it. Resuming a paused
            // stream hung PulseAudio (ALSA's pulse plugin under WSLg) for
            // every client; a fresh stream works on every host.
            if out.take().is_some() {
                closed_idle = true;
            }
        } else {
            // Waking from idle, device lost, or default device changed:
            // (re)open. Reopening after a failure is limited to every 500 ms.
            let lost = shared.stream_error.load(Ordering::Relaxed) || out.is_none();
            let changed = shared.device_changed.swap(false, Ordering::Relaxed);
            let waking = std::mem::take(&mut closed_idle);
            if waking || ((lost || changed) && last_reopen.elapsed() > Duration::from_millis(500)) {
                last_reopen = Instant::now();
                drop(out.take()); // drop the old stream first
                out = open_output(&shared).ok();
            }
            if let Some(o) = &mut out {
                o.feeder.fill(&mut pipe, &shared, &mut bufs, &mut tone);
            }
        }
        if suspend {
            match rx.recv_timeout(SUSPENDED_WAIT) {
                Ok(cmd) => next = Some(cmd),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break 'main,
            }
            continue;
        }
        if last_field.elapsed() >= FIELD_EVERY {
            last_field = Instant::now();
            if pipe.take_field(&mut field) > 0 {
                // Never blocks the producer: skip this frame if a reader holds it.
                if let Ok(mut f) = shared.field.try_lock() {
                    f.bins.clear();
                    f.bins.extend_from_slice(&field);
                    f.fresh = true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    shared.quit.store(true, Ordering::Relaxed);
    drop(out);
    // The events thread ends when the channel closes; not joined (a callback may own the Engine).
    drop(ev_tx);
    drop(events);
    if let Some(w) = watcher {
        let _ = w.join();
    }
}

/// Polls the default output device name every 2 s and flags changes (e.g.
/// headphones plugged in), so the producer can reopen the stream.
fn spawn_device_watcher(shared: Arc<Shared>, initial: Option<String>) -> Option<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("shredlab-audio-devices".into())
        .spawn(move || {
            let mut current = initial;
            let mut slept = Duration::ZERO;
            while !shared.quit.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));
                slept += Duration::from_millis(100);
                if slept < Duration::from_secs(2) {
                    continue;
                }
                slept = Duration::ZERO;
                let name = cpal::default_host()
                    .default_output_device()
                    .and_then(|d| d.name().ok());
                if name.is_some() && name != current {
                    current = name;
                    shared.device_changed.store(true, Ordering::Relaxed);
                }
            }
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rate::test_util::{cents, periodic_hz, sine_hz};
    use crate::Channel;

    fn feeder(producer: rtrb::Producer<Frame>, rate: u32) -> Feeder {
        Feeder {
            producer,
            rate,
            master: 1.0,
            resampler: None,
            fade_in_next: false,
        }
    }

    #[test]
    fn master_volume_glides_to_target() {
        let shared = Shared::default();
        let (producer, _consumer) = rtrb::RingBuffer::<Frame>::new(RING);
        let mut f = feeder(producer, 44_100);
        let bufs = |v: f32| Bufs {
            l: vec![v; BLOCK],
            r: vec![v; BLOCK],
            meta: vec![Meta::default(); BLOCK],
            tone: Vec::new(),
        };
        // Unity: untouched.
        let mut b = bufs(0.5);
        f.apply_master(&shared, 44_100, &mut b);
        assert!(b.l.iter().all(|&x| x == 0.5));
        // To 0.25: no jump at the start, exactly there after ~12 time constants.
        shared.master.set(0.25);
        let mut b = bufs(1.0);
        f.apply_master(&shared, 44_100, &mut b);
        assert!(b.l[0] > 0.99, "{}", b.l[0]);
        assert!(b.l.windows(2).all(|w| w[1] <= w[0]));
        for _ in 0..30 {
            let mut b = bufs(1.0);
            f.apply_master(&shared, 44_100, &mut b);
        }
        let mut b = bufs(1.0);
        f.apply_master(&shared, 44_100, &mut b);
        assert!(b.l.iter().chain(&b.r).all(|&x| x == 0.25));
    }

    fn track(secs: f64, f: impl Fn(usize) -> f32) -> Track {
        let n = (secs * 44_100.0) as usize;
        let v: Vec<f32> = (0..n).map(f).collect();
        Track {
            name: "mix".into(),
            channels: [Channel::from(v.clone()), Channel::from(v)],
            sample_rate: 44_100,
        }
    }

    fn sine_track(secs: f64) -> Track {
        track(secs, |i| {
            0.5 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 44_100.0).sin()
        })
    }

    /// The producer side without a device: fill, then drain like a callback.
    struct Rig {
        feeder: Feeder,
        consumer: rtrb::Consumer<Frame>,
        shared: Shared,
        bufs: Bufs,
        tone: Tone,
    }

    impl Rig {
        fn new(rate: u32) -> Rig {
            let (producer, consumer) = rtrb::RingBuffer::<Frame>::new(RING);
            let shared = Shared::default();
            shared.max_cb_frames.store(512, Ordering::Relaxed);
            Rig {
                feeder: feeder(producer, rate),
                consumer,
                shared,
                bufs: Bufs::new(),
                tone: Tone::new(rate as f32),
            }
        }

        /// At least `frames` more output frames.
        fn run(&mut self, pipe: &mut Pipeline, frames: usize) -> Vec<Frame> {
            let mut out = Vec::new();
            while out.len() < frames {
                self.feeder
                    .fill(pipe, &self.shared, &mut self.bufs, &mut self.tone);
                let n = self.consumer.slots().min(512);
                let chunk = self.consumer.read_chunk(n).unwrap();
                let (a, b) = chunk.as_slices();
                out.extend(a.iter().chain(b.iter()).copied());
                chunk.commit_all();
            }
            out
        }

        fn set_cents(&self, cents: i32) {
            self.shared
                .rate_cents
                .store(clamp_cents(cents), Ordering::Relaxed);
        }
    }

    fn drive(pipe: &mut Pipeline, rate: u32, frames: usize) -> Vec<Frame> {
        Rig::new(rate).run(pipe, frames)
    }

    fn left(frames: &[Frame]) -> Vec<f32> {
        frames.iter().map(|f| f.l).collect()
    }

    fn max_step(x: &[f32]) -> f32 {
        x.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn feeder_resamples_to_device_rate_keeping_pitch_position_and_events() {
        let mut pipe = Pipeline::new(vec![sine_track(3.0)], &[("mix".into(), 1.0)], true);
        pipe.set_loop(Some((1.0, 1.5)));
        pipe.seek(1.0);
        pipe.play();
        let out = drive(&mut pipe, 48_000, 48_000 * 2 + 200);
        // 2 s at 48 kHz = 4 loop passes of 0.5 s: wraps after 0.5, 1.0, 1.5, 2.0 s.
        let wraps: Vec<usize> = (0..out.len().min(96_100))
            .filter(|&i| out[i].meta.flags & F_WRAP != 0)
            .collect();
        assert_eq!(wraps.len(), 4, "{wraps:?}");
        for (k, &w) in wraps.iter().enumerate() {
            assert!(w.abs_diff((k + 1) * 24_000) < 100, "{wraps:?}");
        }
        // Position advances 1 s of song per 48000 frames.
        let p0 = out[1000].meta.pos;
        let p1 = out[1000 + 12_000].meta.pos;
        assert!(((p1 - p0) - 0.25).abs() < 0.01, "{p0} {p1}");
        // Pitch preserved: 1 kHz at the device rate.
        let seg: Vec<f32> = out[2000..22_000].iter().map(|f| f.l).collect();
        let zc = seg.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        let f = zc as f64 * 48_000.0 / seg.len() as f64;
        assert!((f - 1000.0).abs() < 10.0, "freq {f}");
    }

    #[test]
    fn feeder_bypasses_resampler_at_song_rate_bit_exact() {
        let t = sine_track(1.0);
        let src = t.channels[0].to_vec();
        let mut pipe = Pipeline::new(vec![t], &[("mix".into(), 1.0)], false);
        pipe.play();
        let mut rig = Rig::new(44_100);
        rig.set_cents(0);
        let out = rig.run(&mut pipe, 30_000);
        assert!((0..30_000).all(|i| out[i].l == src[i] && out[i].r == src[i]));
        assert!(rig.feeder.resampler.is_none());
    }

    #[test]
    fn rate_correction_keeps_positions_wraps_and_clicks_in_song_time() {
        for (rate, c) in [(44_100u32, 150), (48_000, -200), (44_100, -37)] {
            let r = crate::rate::rate(c);
            // Silent music; clicks on every half second.
            let mut pipe = Pipeline::new(vec![track(4.0, |_| 0.0)], &[("mix".into(), 1.0)], true);
            pipe.set_click(ClickConfig {
                enabled: true,
                beats: (1..8).map(|k| k as f64 * 0.5).collect(),
                ..ClickConfig::default()
            });
            pipe.set_loop(Some((0.25, 2.25)));
            pipe.play();
            let mut rig = Rig::new(rate);
            rig.set_cents(c);
            let secs = 4.5;
            let out = rig.run(&mut pipe, (secs * rate as f64) as usize);
            // Song time advances `r` seconds per output second.
            let (i0, i1) = (rate as usize / 10, rate as usize / 10 + rate as usize);
            let dp = out[i1].meta.pos - out[i0].meta.pos;
            assert!((dp - r).abs() < 0.003, "{rate} {c}: {dp} vs {r}");
            // The loop (2 s of song) wraps every 2 / r output seconds.
            let wraps: Vec<usize> = (0..out.len())
                .filter(|&i| out[i].meta.flags & F_WRAP != 0)
                .collect();
            assert_eq!(wraps.len(), 2, "{rate} {c}: {wraps:?}");
            for (k, &w) in wraps.iter().enumerate() {
                let want = (k + 1) as f64 * 2.0 / r * rate as f64;
                assert!((w as f64 - want).abs() < 0.003 * rate as f64, "{wraps:?}");
            }
            // Each click is heard where the reported position is its beat.
            let mut onsets = Vec::new();
            let quiet = rate as usize / 20;
            let mut last_loud = None::<usize>;
            for (i, f) in out.iter().enumerate() {
                if f.l.abs() > 1e-3 {
                    if last_loud.is_none_or(|j| i - j > quiet) {
                        onsets.push(i);
                    }
                    last_loud = Some(i);
                }
            }
            assert!(onsets.len() >= 8, "{rate} {c}: {onsets:?}");
            for &i in &onsets {
                let pos = out[i].meta.pos;
                let beat = (pos * 2.0).round() / 2.0;
                assert!((pos - beat).abs() < 0.004, "{rate} {c}: click at {pos}");
            }
            // And the clicks follow the rate: about 0.5 / r s apart.
            let gap = (onsets[2] - onsets[1]) as f64 / rate as f64;
            assert!((gap - 0.5 / r).abs() < 0.003, "{rate} {c}: {gap}");
        }
    }

    #[test]
    fn rate_correction_shifts_pitch_without_discontinuities() {
        // 48 kHz: the ratio glides on the running resampler. 44.1 kHz: the
        // resampler is added and removed behind a fade.
        for rate in [48_000u32, 44_100] {
            let mut pipe = Pipeline::new(vec![sine_track(6.0)], &[("mix".into(), 1.0)], true);
            pipe.play();
            let mut rig = Rig::new(rate);
            let mut all = rig.run(&mut pipe, rate as usize / 2);
            for c in [200, -200, 0, 100] {
                rig.set_cents(c);
                let seg = rig.run(&mut pipe, rate as usize / 2);
                // Settled: the sine moved by `c` cents.
                let x = left(&seg[seg.len() / 2..]);
                let f = sine_hz(&x, rate as f64);
                assert!(
                    cents(f, 1000.0 * crate::rate::rate(c)).abs() < 0.5,
                    "{rate} {c}: {f}"
                );
                assert_eq!(rig.feeder.resampler.is_some(), rate != 44_100 || c != 0);
                all.extend(seg);
            }
            // No step larger than the fastest sine's own (0.5 amplitude at
            // 1122 Hz), with a margin.
            let limit = 0.5 * 2.0 * std::f32::consts::PI * 1122.5 / rate as f32 * 1.1;
            let step = max_step(&left(&all));
            assert!(step < limit, "{rate}: step {step} > {limit}");
        }
    }

    #[test]
    fn tone_ignores_speed_pitch_and_rate_correction() {
        for (rate, c) in [(48_000u32, 150), (44_100, -120), (44_100, 0)] {
            // The music is muted (gain 0), the transport plays stretched.
            let mut pipe = Pipeline::new(vec![sine_track(3.0)], &[("mix".into(), 0.0)], true);
            pipe.set_speed(0.8);
            pipe.set_pitch(3.0);
            pipe.play();
            let mut rig = Rig::new(rate);
            rig.set_cents(c);
            for sound in [ToneSound::Drone, ToneSound::Pluck] {
                rig.tone.start(110.0, sound);
                let out = rig.run(&mut pipe, rate as usize);
                let x = left(&out[rate as usize / 5..]);
                let f = periodic_hz(&x, rate as f64, 110.0);
                assert!(cents(f, 110.0).abs() < 0.5, "{rate} {c} {sound:?}: {f}");
                assert!(out.iter().all(|f| f.l == f.r));
            }
        }
    }

    #[test]
    fn tone_plays_with_nothing_loaded_under_the_master_volume() {
        let mut pipe = Pipeline::new(Vec::new(), &[], true);
        let mut rig = Rig::new(48_000);
        rig.tone.start(220.0, ToneSound::Drone);
        let full = left(&rig.run(&mut pipe, 24_000));
        let peak = |x: &[f32]| x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let p1 = peak(&full[12_000..]);
        assert!(p1 > 0.1 && p1 <= crate::tone::LEVEL * 1.12, "{p1}");
        rig.shared.master.set(0.5);
        let half = left(&rig.run(&mut pipe, 48_000));
        let p2 = peak(&half[half.len() - 12_000..]);
        assert!((p2 / p1 - 0.5).abs() < 0.03, "{p1} {p2}");
        // Gliding, no step.
        assert!(max_step(&half) < max_step(&full[12_000..]) * 1.01);
        // Stopped: silent output again.
        rig.tone.stop();
        let tail = left(&rig.run(&mut pipe, 24_000));
        assert!(tail[tail.len() - 12_000..].iter().all(|&x| x == 0.0));
        assert!(!rig.tone.is_active());
    }

    #[test]
    fn idle_suspension_waits_for_the_tone() {
        let pipe = Pipeline::new(vec![sine_track(1.0)], &[("mix".into(), 1.0)], true);
        let mut tone = Tone::new(48_000.0);
        let mut idle = IdleTimer::default();
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        tone.start(82.41, ToneSound::Pluck);
        assert!(!pipe.is_playing());
        for s in 0..20 {
            assert!(!idle.suspend(busy(&pipe, &tone), at(s)));
        }
        // Released: idle from then on, suspended after SUSPEND_AFTER.
        tone.stop();
        let mut buf = vec![0.0; 4800];
        tone.render(&mut buf);
        assert!(!tone.is_active());
        assert!(!idle.suspend(busy(&pipe, &tone), at(20)));
        assert!(!idle.suspend(busy(&pipe, &tone), at(24)));
        assert!(idle.suspend(busy(&pipe, &tone), at(25)));
        // A tone started while suspended wakes it at once.
        tone.start(82.41, ToneSound::Drone);
        assert!(!idle.suspend(busy(&pipe, &tone), at(26)));
    }
}
