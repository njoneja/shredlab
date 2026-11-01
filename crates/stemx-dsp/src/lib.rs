//! DSP primitives for separation, ported from the Python reference
//! implementations (`tools/reference/`, `spikes/`):
//!
//! - [`HybridStft`]: the STFT/iSTFT glue of the `demucs-hybrid` adapter
//!   (`HTDemucs._spec` / `_ispec`).
//! - [`Stft`]: `torch.stft` / `torch.istft` with `center=True` (reflect
//!   padding, periodic Hann, not normalized), used by `roformer-mask`.
//! - [`chunk`]: demucs `apply_model` chunking (triangular weights, padded
//!   short chunks) and MSST-style windowed overlap-add, plus song
//!   normalization.
//! - [`refine`]: the Refine panel's per-stem pan/width mask and EQ
//!   ([`refine::RefineChain`]), real-time safe.
//! - [`strength`]: separation strength, re-deriving stems from the original
//!   with a sharpened ratio mask (offline).
//!
//! Audio is planar: one `Vec<f32>` / slice per channel. Multi-stem outputs
//! are flat `(stems, channels, samples)` buffers, last axis fastest.

pub mod chunk;
mod hybrid;
pub mod refine;
mod stft;
pub mod strength;

pub use hybrid::HybridStft;
pub use realfft::num_complex::{Complex32, Complex64};
pub use stft::Stft;

/// Version of this crate, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Crate name, from `Cargo.toml`.
pub const NAME: &str = env!("CARGO_PKG_NAME");

/// Periodic Hann window (`scipy.signal.get_window("hann", n)`,
/// `torch.hann_window(n)`), computed in f64.
pub fn hann_periodic_f64(n: usize) -> Vec<f64> {
    (0..n)
        .map(|k| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * k as f64 / n as f64).cos())
        .collect()
}

/// [`hann_periodic_f64`] cast to f32.
pub fn hann_periodic(n: usize) -> Vec<f32> {
    hann_periodic_f64(n).into_iter().map(|v| v as f32).collect()
}

/// Symmetric Hamming window (`scipy.signal.windows.hamming(n)`), f32.
pub fn hamming_symmetric(n: usize) -> Vec<f32> {
    if n == 1 {
        return vec![1.0];
    }
    (0..n)
        .map(|k| {
            (0.54 - 0.46 * (2.0 * std::f64::consts::PI * k as f64 / (n - 1) as f64).cos()) as f32
        })
        .collect()
}

/// Index into a length-`n` signal for position `i` of its reflect-padded
/// extension (`numpy.pad(mode="reflect")`: mirror without repeating the edge
/// sample; pads longer than the signal keep reflecting).
pub fn reflect_index(i: isize, n: usize) -> usize {
    assert!(n > 0, "reflect padding of an empty signal");
    if n == 1 {
        return 0;
    }
    let period = 2 * (n as isize - 1);
    let m = i.rem_euclid(period);
    if m >= n as isize {
        (period - m) as usize
    } else {
        m as usize
    }
}

/// Reflect-pad `x` by `left` and `right` samples (`numpy.pad(mode="reflect")`).
pub fn reflect_pad(x: &[f32], left: usize, right: usize) -> Vec<f32> {
    let n = x.len();
    (0..left + n + right)
        .map(|j| x[reflect_index(j as isize - left as isize, n)])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_set() {
        assert_eq!(NAME, "stemx-dsp");
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn reflect_pad_matches_numpy() {
        // np.pad([1, 2, 3, 4], (3, 5), mode="reflect")
        let got = reflect_pad(&[1.0, 2.0, 3.0, 4.0], 3, 5);
        assert_eq!(
            got,
            vec![4.0, 3.0, 2.0, 1.0, 2.0, 3.0, 4.0, 3.0, 2.0, 1.0, 2.0, 3.0]
        );
        assert_eq!(reflect_pad(&[7.0], 2, 1), vec![7.0; 4]);
    }

    #[test]
    fn windows() {
        let w = hann_periodic(4);
        assert!(
            (w[0] - 0.0).abs() < 1e-7 && (w[1] - 0.5).abs() < 1e-7 && (w[2] - 1.0).abs() < 1e-7
        );
        let h = hamming_symmetric(5);
        assert!(
            (h[0] - 0.08).abs() < 1e-6 && (h[2] - 1.0).abs() < 1e-6 && (h[4] - 0.08).abs() < 1e-6
        );
    }
}
