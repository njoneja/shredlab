// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Live tuner plumbing: picks the input device, runs a `LiveDetector` on
//! the captured channel and throttles its readings for the UI. The capture
//! itself is `shredlab-audio`'s `InputCapture`; the backend owns it.

use std::time::{Duration, Instant};

use shredlab_tuning::{LiveDetector, LiveReading, LEVEL_FLOOR_DB};

use crate::audio::{InputCapture, InputDevice};
use crate::types::TunerReading;

/// At most one `tuner-reading` per this interval (about 30 Hz).
pub const EMIT_INTERVAL: Duration = Duration::from_millis(33);

const NO_INPUT: &str = "No audio input device found. Connect an audio interface or a microphone.";

/// The device to open for `requested` (`None` = the system default):
/// `(id, fell_back)`. A requested id that is not in `devices` falls back
/// to the default device (or the first one), with `fell_back` set.
pub fn resolve_device(
    requested: Option<&str>,
    devices: &[InputDevice],
) -> Result<(String, bool), String> {
    if let Some(id) = requested {
        if devices.iter().any(|d| d.id == id) {
            return Ok((id.to_string(), false));
        }
    }
    let d = devices
        .iter()
        .find(|d| d.default)
        .or_else(|| devices.first())
        .ok_or_else(|| NO_INPUT.to_string())?;
    Ok((d.id.clone(), requested.is_some()))
}

/// Collects detector readings and lets one through per [`EMIT_INTERVAL`]:
/// the latest pitch and clarity, with the highest level of the interval
/// (so the meter shows a pluck's attack, not whatever came last).
#[derive(Debug)]
pub struct ReadingThrottle {
    last_emit: Option<Instant>,
    latest: Option<LiveReading>,
    max_level: f32,
}

impl Default for ReadingThrottle {
    fn default() -> Self {
        Self {
            last_emit: None,
            latest: None,
            max_level: LEVEL_FLOOR_DB,
        }
    }
}

impl ReadingThrottle {
    pub fn add(&mut self, r: &LiveReading) {
        self.latest = Some(*r);
        self.max_level = self.max_level.max(r.level_db);
    }

    /// The reading to send at `now`, if one is due.
    pub fn take(&mut self, now: Instant) -> Option<TunerReading> {
        let r = self.latest?;
        if self
            .last_emit
            .is_some_and(|t| now.duration_since(t) < EMIT_INTERVAL)
        {
            return None;
        }
        self.last_emit = Some(now);
        self.latest = None;
        let level_db = std::mem::replace(&mut self.max_level, LEVEL_FLOOR_DB);
        Some(TunerReading {
            hz: r.hz,
            clarity: r.clarity,
            level_db,
        })
    }
}

/// Starts capturing `channel` of device `id` into a detector for notes
/// down to `low_hz`; `on_reading` gets the throttled readings on the
/// capture's worker thread. The detector is made when the stream's rate is
/// known and made again if it changes.
pub fn start(
    id: &str,
    channel: u16,
    low_hz: f32,
    on_reading: impl Fn(TunerReading) + Send + 'static,
) -> Result<InputCapture, String> {
    let mut detector: Option<(u32, LiveDetector)> = None;
    let mut out = Vec::new();
    let mut throttle = ReadingThrottle::default();
    InputCapture::start(Some(id), channel, move |block, rate| {
        if detector.as_ref().is_some_and(|(r, _)| *r != rate) {
            detector = None;
        }
        let (_, d) = detector.get_or_insert_with(|| (rate, LiveDetector::new(rate, low_hz)));
        out.clear();
        d.push(block, &mut out);
        for r in &out {
            throttle.add(r);
        }
        if let Some(r) = throttle.take(Instant::now()) {
            on_reading(r);
        }
    })
    .map_err(|e| format!("Cannot open the input \"{id}\": {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str, default: bool) -> InputDevice {
        InputDevice {
            id: id.into(),
            name: id.into(),
            channels: 2,
            default,
        }
    }

    #[test]
    fn device_resolution() {
        let devs = [dev("Mic", false), dev("Interface", true)];
        assert_eq!(
            resolve_device(Some("Mic"), &devs),
            Ok(("Mic".into(), false))
        );
        assert_eq!(resolve_device(None, &devs), Ok(("Interface".into(), false)));
        // A saved device that is gone: the default, flagged.
        assert_eq!(
            resolve_device(Some("Gone"), &devs),
            Ok(("Interface".into(), true))
        );
        // No default: the first device.
        let devs = [dev("Mic", false)];
        assert_eq!(resolve_device(None, &devs), Ok(("Mic".into(), false)));
        assert!(resolve_device(Some("Mic"), &[]).is_err());
    }

    fn reading(hz: Option<f32>, level_db: f32) -> LiveReading {
        LiveReading {
            hz,
            clarity: 0.9,
            level_db,
        }
    }

    #[test]
    fn throttle_sends_latest_pitch_and_peak_level() {
        let mut t = ReadingThrottle::default();
        let t0 = Instant::now();
        assert_eq!(t.take(t0), None);
        t.add(&reading(Some(110.0), -20.0));
        let r = t.take(t0).unwrap();
        assert_eq!((r.hz, r.level_db), (Some(110.0), -20.0));
        // Within the interval: held back, the peak level is kept.
        t.add(&reading(Some(111.0), -10.0));
        t.add(&reading(None, -30.0));
        assert_eq!(t.take(t0 + Duration::from_millis(10)), None);
        let r = t.take(t0 + EMIT_INTERVAL).unwrap();
        assert_eq!((r.hz, r.level_db), (None, -10.0));
        // Nothing new: nothing sent; the level starts over.
        assert_eq!(t.take(t0 + 3 * EMIT_INTERVAL), None);
        t.add(&reading(None, -50.0));
        assert_eq!(t.take(t0 + 3 * EMIT_INTERVAL).unwrap().level_db, -50.0);
    }
}
