//! Audio I/O: decoding, encoding (WAV/FLAC/MP3) and resampling to 44.1 kHz.
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
        assert_eq!(NAME, "stemx-io");
        assert!(!VERSION.is_empty());
    }
}
