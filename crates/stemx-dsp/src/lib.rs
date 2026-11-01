//! DSP primitives: STFT/iSTFT, chunking and overlap-add (ported from the Python spikes).
//!
//! M0 skeleton: no functionality yet.

/// Version of this crate, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Crate name, from `Cargo.toml`.
pub const NAME: &str = env!("CARGO_PKG_NAME");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_set() {
        assert_eq!(NAME, "stemx-dsp");
        assert!(!VERSION.is_empty());
    }
}
