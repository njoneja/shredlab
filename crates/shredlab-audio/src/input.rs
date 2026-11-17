// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Audio input capture for the live tuner: one channel of an input device,
//! as f32 blocks delivered on a worker thread.
//!
//! The cpal callback only converts the chosen channel to f32 and pushes it
//! into a lock-free ring ([`rtrb`]); a worker thread, which also owns the
//! stream (it is `!Send` on some platforms), drains the ring and calls the
//! consumer. Independent of the playback [`Engine`](crate::Engine).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};

use crate::AudioError;

/// Ring capacity in seconds of audio at the device rate.
const RING_SECS: f32 = 1.0;
/// Most samples handed to the consumer in one call.
const MAX_BLOCK: usize = 4096;
/// Worker sleep when the ring is empty.
const IDLE_SLEEP: Duration = Duration::from_millis(2);

/// An audio input device as listed by [`list_input_devices`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDevice {
    /// Identifier to save in settings and pass to [`InputCapture::start`].
    /// cpal 0.16 has no persistent device id, so this is the device name;
    /// when several devices share a name, the second and later ones get
    /// `" #2"`, `" #3"`, ... in enumeration order.
    pub id: String,
    /// Display name reported by the system.
    pub name: String,
    /// Channels of the device's default input configuration.
    pub channels: u16,
    /// This is the system's default input device.
    pub default: bool,
}

/// Input devices of the default host. Devices whose name or default config
/// cannot be read are skipped. Never fails: no devices gives an empty list.
pub fn list_input_devices() -> Vec<InputDevice> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().and_then(|d| d.name().ok());
    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };
    let mut named = Vec::new();
    for d in devices {
        let Ok(name) = d.name() else { continue };
        let Ok(cfg) = d.default_input_config() else {
            continue;
        };
        named.push((name, cfg.channels()));
    }
    let ids = unique_ids(named.iter().map(|(n, _)| n.as_str()));
    let mut default_seen = false;
    named
        .into_iter()
        .zip(ids)
        .map(|((name, channels), id)| {
            // The first device with the default's name is the default.
            let default = !default_seen && default_name.as_deref() == Some(name.as_str());
            default_seen |= default;
            InputDevice {
                id,
                name,
                channels,
                default,
            }
        })
        .collect()
}

/// Ids for names in enumeration order: the name, with `" #k"` for the k-th
/// repeat of a name.
fn unique_ids<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen: Vec<(&str, u32)> = Vec::new();
    names
        .map(|n| {
            let k = match seen.iter_mut().find(|(s, _)| *s == n) {
                Some((_, k)) => {
                    *k += 1;
                    *k
                }
                None => {
                    seen.push((n, 1));
                    1
                }
            };
            if k == 1 {
                n.to_string()
            } else {
                format!("{n} #{k}")
            }
        })
        .collect()
}

/// Finds the device for `id` (see [`InputDevice::id`]), or the default
/// input device for `None`.
fn find_device(id: Option<&str>) -> Result<cpal::Device, AudioError> {
    let host = cpal::default_host();
    let Some(id) = id else {
        return host
            .default_input_device()
            .ok_or_else(|| AudioError::Input("no input device".into()));
    };
    let devices: Vec<(cpal::Device, String)> = host
        .input_devices()
        .map_err(|e| AudioError::Input(e.to_string()))?
        .filter_map(|d| {
            let name = d.name().ok()?;
            d.default_input_config().ok()?;
            Some((d, name))
        })
        .collect();
    let ids = unique_ids(devices.iter().map(|(_, n)| n.as_str()));
    devices
        .into_iter()
        .zip(ids)
        .find(|(_, i)| i == id)
        .map(|((d, _), _)| d)
        .ok_or_else(|| AudioError::Input(format!("device not found: {id}")))
}

/// What the capture opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputInfo {
    pub device_name: String,
    pub sample_rate: u32,
    /// Channels of the stream; the captured one is `channel`.
    pub channels: u16,
    pub channel: u16,
}

/// State shared by the stream callbacks, the worker and the handle.
#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    /// Samples dropped because the ring was full (consumer too slow).
    dropped: AtomicU64,
    /// Samples delivered to the consumer.
    delivered: AtomicU64,
    /// First stream error (device unplugged, ...). Written from cpal's error
    /// callback, never from the data callback.
    error: Mutex<Option<String>>,
}

/// A running capture of one input channel. Dropping it (or [`stop`]) stops
/// the stream and joins the worker thread.
///
/// [`stop`]: InputCapture::stop
pub struct InputCapture {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    info: InputInfo,
}

impl InputCapture {
    /// Opens `device_id` (or the default input device for `None`) at its
    /// default input config and captures channel `channel` (0-based; not a
    /// downmix). `on_samples(block, sample_rate)` runs on a worker thread
    /// with blocks of up to 4096 mono f32 samples, in order; it may take a
    /// few milliseconds per call without losing audio (the ring holds 1 s).
    /// Returns once the stream is running, or the error that prevented it.
    pub fn start(
        device_id: Option<&str>,
        channel: u16,
        on_samples: impl FnMut(&[f32], u32) + Send + 'static,
    ) -> Result<InputCapture, AudioError> {
        let shared = Arc::new(Shared::default());
        let id = device_id.map(str::to_string);
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker_shared = shared.clone();
        let worker = std::thread::Builder::new()
            .name("shredlab-input".into())
            .spawn(move || {
                worker_thread(id, channel, worker_shared, ready_tx, on_samples);
            })
            .map_err(|e| AudioError::Input(format!("cannot start thread: {e}")))?;
        match ready_rx.recv() {
            Ok(Ok(info)) => Ok(InputCapture {
                shared,
                worker: Some(worker),
                info,
            }),
            Ok(Err(e)) => {
                let _ = worker.join();
                Err(e)
            }
            Err(_) => {
                let _ = worker.join();
                Err(AudioError::Input("input thread exited".into()))
            }
        }
    }

    /// Device, rate and channel the capture runs with.
    pub fn info(&self) -> &InputInfo {
        &self.info
    }

    /// The stream error, if one occurred (e.g. the device was unplugged).
    /// The capture keeps its thread until stopped but delivers nothing more.
    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().ok().and_then(|e| e.clone())
    }

    /// Samples dropped because the consumer fell more than the ring behind.
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// Samples delivered to the consumer so far.
    pub fn delivered(&self) -> u64 {
        self.shared.delivered.load(Ordering::Relaxed)
    }

    /// Stops the stream and joins the worker (same as dropping).
    pub fn stop(self) {}
}

impl Drop for InputCapture {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Owns the stream: opens it, reports through `ready`, then drains the ring
/// into `on_samples` until stopped. Dropping the stream at the end closes
/// the device.
fn worker_thread(
    id: Option<String>,
    channel: u16,
    shared: Arc<Shared>,
    ready: mpsc::Sender<Result<InputInfo, AudioError>>,
    mut on_samples: impl FnMut(&[f32], u32),
) {
    let (stream, mut consumer, info) = match open_input(id.as_deref(), channel, &shared) {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let rate = info.sample_rate;
    let _ = ready.send(Ok(info));
    let mut buf = vec![0.0f32; MAX_BLOCK];
    while !shared.stop.load(Ordering::Relaxed) {
        let n = drain(&mut consumer, &mut buf, |b| on_samples(b, rate));
        shared.delivered.fetch_add(n as u64, Ordering::Relaxed);
        if n == 0 {
            std::thread::sleep(IDLE_SLEEP);
        }
    }
    drop(stream);
}

/// Pops everything currently in the ring, handing it to `f` in blocks of at
/// most `buf.len()`. Returns the number of samples delivered.
fn drain(consumer: &mut rtrb::Consumer<f32>, buf: &mut [f32], mut f: impl FnMut(&[f32])) -> usize {
    let mut total = 0;
    loop {
        let n = consumer.slots().min(buf.len());
        if n == 0 {
            return total;
        }
        let (got, _) = consumer.pop_partial_slice(&mut buf[..n]);
        let got = got.len();
        f(&buf[..got]);
        total += got;
    }
}

/// Pushes channel `channel` of interleaved `data` (`channels` per frame),
/// converted to f32, into `producer`. Returns the samples dropped because
/// the ring was full. Real-time safe: no locks, no allocation.
fn push_channel<T>(
    data: &[T],
    channels: usize,
    channel: usize,
    producer: &mut rtrb::Producer<f32>,
) -> usize
where
    T: Sample,
    f32: FromSample<T>,
{
    let frames = data.len() / channels.max(1);
    let n = producer.slots().min(frames);
    if n > 0 {
        if let Ok(chunk) = producer.write_chunk_uninit(n) {
            chunk.fill_from_iter(
                data.iter()
                    .skip(channel)
                    .step_by(channels.max(1))
                    .map(|&s| f32::from_sample(s)),
            );
        }
    }
    frames - n
}

type Opened = (cpal::Stream, rtrb::Consumer<f32>, InputInfo);

fn open_input(id: Option<&str>, channel: u16, shared: &Arc<Shared>) -> Result<Opened, AudioError> {
    let device = find_device(id)?;
    let device_name = device.name().unwrap_or_default();
    let supported = device
        .default_input_config()
        .map_err(|e| AudioError::Input(format!("{device_name}: {e}")))?;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    if channel >= config.channels {
        return Err(AudioError::Invalid(format!(
            "{device_name} has {} input channels; channel {} requested",
            config.channels,
            channel + 1
        )));
    }
    let cap = ((config.sample_rate.0 as f32 * RING_SECS) as usize).max(MAX_BLOCK);
    let (producer, consumer) = rtrb::RingBuffer::<f32>::new(cap);
    use cpal::SampleFormat as F;
    let stream = match format {
        F::F32 => build::<f32>(&device, &config, channel, producer, shared),
        F::I16 => build::<i16>(&device, &config, channel, producer, shared),
        F::U16 => build::<u16>(&device, &config, channel, producer, shared),
        F::I32 => build::<i32>(&device, &config, channel, producer, shared),
        F::F64 => build::<f64>(&device, &config, channel, producer, shared),
        F::I8 => build::<i8>(&device, &config, channel, producer, shared),
        F::U8 => build::<u8>(&device, &config, channel, producer, shared),
        other => {
            return Err(AudioError::Input(format!(
                "{device_name}: unsupported sample format {other}"
            )));
        }
    }
    .map_err(|e| AudioError::Input(format!("{device_name}: {e}")))?;
    stream
        .play()
        .map_err(|e| AudioError::Input(format!("{device_name}: {e}")))?;
    let info = InputInfo {
        device_name,
        sample_rate: config.sample_rate.0,
        channels: config.channels,
        channel,
    };
    Ok((stream, consumer, info))
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channel: u16,
    mut producer: rtrb::Producer<f32>,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let ch = config.channels as usize;
    let channel = channel as usize;
    let data_shared = shared.clone();
    let err_shared = shared.clone();
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                // Real-time: no locks, no allocation.
                let dropped = push_channel(data, ch, channel, &mut producer);
                if dropped > 0 {
                    data_shared
                        .dropped
                        .fetch_add(dropped as u64, Ordering::Relaxed);
                }
            },
            move |e| {
                if let Ok(mut slot) = err_shared.error.lock() {
                    slot.get_or_insert_with(|| e.to_string());
                }
            },
            None,
        )
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_channel_takes_one_channel_and_converts() {
        let (mut p, mut c) = rtrb::RingBuffer::<f32>::new(16);
        // Three channels, two frames, i16.
        let data: [i16; 6] = [0, 16384, -1, i16::MIN, -16384, 7];
        assert_eq!(push_channel(&data, 3, 1, &mut p), 0);
        assert_eq!(c.pop(), Ok(0.5));
        assert_eq!(c.pop(), Ok(-0.5));
        assert!(c.pop().is_err());

        let data: [u8; 2] = [128, 255];
        push_channel(&data, 1, 0, &mut p);
        assert_eq!(c.pop(), Ok(0.0));
        assert!((c.pop().unwrap() - 127.0 / 128.0).abs() < 1e-6);

        let data: [f64; 4] = [0.25, -0.75, 0.125, 1.0];
        push_channel(&data, 2, 0, &mut p);
        assert_eq!(c.pop(), Ok(0.25));
        assert_eq!(c.pop(), Ok(0.125));

        let data: [i32; 2] = [i32::MIN, 1 << 30];
        push_channel(&data, 2, 1, &mut p);
        assert_eq!(c.pop(), Ok(0.5));
    }

    #[test]
    fn push_channel_counts_drops_when_full() {
        let (mut p, mut c) = rtrb::RingBuffer::<f32>::new(3);
        let data: Vec<f32> = (0..10).map(|i| i as f32).collect();
        // Five stereo frames, room for three.
        assert_eq!(push_channel(&data, 2, 0, &mut p), 2);
        let got: Vec<f32> = std::iter::from_fn(|| c.pop().ok()).collect();
        assert_eq!(got, [0.0, 2.0, 4.0]);
        // A partial trailing frame is ignored.
        assert_eq!(push_channel(&data[..3], 2, 1, &mut p), 0);
        assert_eq!(c.pop(), Ok(1.0));
        assert!(c.pop().is_err());
    }

    #[test]
    fn drain_delivers_everything_in_order_in_bounded_blocks() {
        let (mut p, mut c) = rtrb::RingBuffer::<f32>::new(64);
        // Wrap the ring so the read spans its end.
        for _ in 0..40 {
            p.push(0.0).unwrap();
        }
        let mut buf = [0.0; 7];
        assert_eq!(drain(&mut c, &mut buf, |_| {}), 40);
        for i in 0..50 {
            p.push(i as f32).unwrap();
        }
        let mut got = Vec::new();
        let mut sizes = Vec::new();
        let n = drain(&mut c, &mut buf, |b| {
            sizes.push(b.len());
            got.extend_from_slice(b);
        });
        assert_eq!(n, 50);
        assert_eq!(got, (0..50).map(|i| i as f32).collect::<Vec<_>>());
        assert!(sizes.iter().all(|&s| (1..=7).contains(&s)));
        assert_eq!(drain(&mut c, &mut buf, |_| panic!("empty")), 0);
    }

    #[test]
    fn unique_ids_number_repeated_names() {
        let ids = unique_ids(["Mic", "USB", "Mic", "Mic"].into_iter());
        assert_eq!(ids, ["Mic", "USB", "Mic #2", "Mic #3"]);
    }
}
