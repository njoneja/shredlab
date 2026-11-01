//! Safe wrapper around the vendored Signalsmith Stretch (MIT) via a tiny C ABI
//! (`cpp/stretch_wrapper.cpp`). Planar stereo.

use std::ffi::c_void;

extern "C" {
    fn stemx_stretch_new(channels: i32, sample_rate: f32) -> *mut c_void;
    fn stemx_stretch_free(s: *mut c_void);
    fn stemx_stretch_reset(s: *mut c_void);
    fn stemx_stretch_input_latency(s: *mut c_void) -> i32;
    fn stemx_stretch_output_latency(s: *mut c_void) -> i32;
    fn stemx_stretch_block_samples(s: *mut c_void) -> i32;
    fn stemx_stretch_interval_samples(s: *mut c_void) -> i32;
    fn stemx_stretch_set_transpose_semitones(s: *mut c_void, semitones: f32);
    fn stemx_stretch_seek(s: *mut c_void, inputs: *const *const f32, n: i32, rate: f64);
    fn stemx_stretch_process(
        s: *mut c_void,
        inputs: *const *const f32,
        n_in: i32,
        outputs: *const *mut f32,
        n_out: i32,
    );
}

/// A stereo Signalsmith Stretch instance (default preset).
pub struct Stretcher {
    ptr: *mut c_void,
}

// SAFETY: the C++ object has no thread affinity; we only use it through `&mut self`.
unsafe impl Send for Stretcher {}

impl Stretcher {
    pub fn new(sample_rate: u32) -> Self {
        // SAFETY: plain constructor; returns a valid heap object.
        let ptr = unsafe { stemx_stretch_new(2, sample_rate as f32) };
        assert!(!ptr.is_null());
        Self { ptr }
    }

    pub fn reset(&mut self) {
        unsafe { stemx_stretch_reset(self.ptr) }
    }

    pub fn input_latency(&self) -> usize {
        unsafe { stemx_stretch_input_latency(self.ptr) as usize }
    }

    pub fn output_latency(&self) -> usize {
        unsafe { stemx_stretch_output_latency(self.ptr) as usize }
    }

    /// Recommended pre-roll length for [`Self::seek`]: one block plus one interval.
    pub fn preroll_len(&self) -> usize {
        unsafe {
            (stemx_stretch_block_samples(self.ptr) + stemx_stretch_interval_samples(self.ptr))
                as usize
        }
    }

    pub fn set_transpose_semitones(&mut self, semitones: f32) {
        unsafe { stemx_stretch_set_transpose_semitones(self.ptr, semitones) }
    }

    /// Feeds pre-roll input without producing output.
    pub fn seek(&mut self, l: &[f32], r: &[f32], playback_rate: f64) {
        let n = l.len().min(r.len());
        let ins = [l.as_ptr(), r.as_ptr()];
        unsafe { stemx_stretch_seek(self.ptr, ins.as_ptr(), n as i32, playback_rate) }
    }

    /// Consumes `in_l/in_r` (same length) and fills `out_l/out_r` (same length).
    pub fn process(&mut self, in_l: &[f32], in_r: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        let n_in = in_l.len().min(in_r.len());
        let n_out = out_l.len().min(out_r.len());
        let ins = [in_l.as_ptr(), in_r.as_ptr()];
        let outs = [out_l.as_mut_ptr(), out_r.as_mut_ptr()];
        unsafe {
            stemx_stretch_process(
                self.ptr,
                ins.as_ptr(),
                n_in as i32,
                outs.as_ptr(),
                n_out as i32,
            )
        }
    }
}

impl Drop for Stretcher {
    fn drop(&mut self) {
        unsafe { stemx_stretch_free(self.ptr) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stretch_preserves_pitch_and_scales_length() {
        let sr = 44_100;
        let n = sr as usize * 2;
        let x: Vec<f32> = (0..n)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / sr as f32).sin())
            .collect();
        let mut s = Stretcher::new(sr);
        assert!(s.input_latency() > 0 && s.output_latency() > 0);
        let n_out = n * 5 / 4; // speed 0.8
        let mut ol = vec![0.0; n_out];
        let mut or = vec![0.0; n_out];
        s.process(&x, &x, &mut ol, &mut or);
        // Count zero crossings in a steady middle section: frequency must stay ~440 Hz.
        let mid = &ol[n_out / 4..n_out * 3 / 4];
        let zc = mid.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        let secs = mid.len() as f64 / sr as f64;
        let f = zc as f64 / secs;
        assert!((f - 440.0).abs() < 5.0, "freq {f}");
    }
}
