# Third-party components in shredlab-audio

| Component | Use | License | How it's included |
|---|---|---|---|
| [Signalsmith Stretch](https://github.com/Signalsmith-Audio/signalsmith-stretch) 1.3.1 | Time stretch and pitch shift | MIT | Vendored header in `third_party/signalsmith-stretch/` (with `LICENSE.txt`), compiled through our C wrapper `cpp/stretch_wrapper.cpp` |
| [Signalsmith Linear](https://github.com/Signalsmith-Audio/linear) | FFT/STFT used by Stretch | MIT | Vendored headers in `third_party/signalsmith-linear/` (with `LICENSE.txt`) |
| [cpal](https://crates.io/crates/cpal) | Audio output | Apache-2.0 | crates.io |
| [rtrb](https://crates.io/crates/rtrb) | Lock-free ring buffer | MIT OR Apache-2.0 | crates.io |
| [rubato](https://crates.io/crates/rubato) | Resampling to the device rate | MIT | crates.io |
| [hound](https://crates.io/crates/hound) | WAV export | Apache-2.0 | crates.io |
| [flacenc](https://crates.io/crates/flacenc) | FLAC export (pure Rust) | Apache-2.0 | crates.io |
| [mp3lame-encoder](https://crates.io/crates/mp3lame-encoder) / [mp3lame-sys](https://crates.io/crates/mp3lame-sys) | MP3 export | **LGPL-3.0** (LAME is LGPL-2.0+) | crates.io; LAME is compiled from source and **statically linked** |

## LGPL note (MP3 / LAME)

LAME is the only copyleft component. It is LGPL, which is acceptable for an
open-source app, with these obligations when distributing binaries:

- Ship the LAME license text and state that LAME is used (e.g. in the app's
  About/licenses screen).
- Because LAME is statically linked, recipients must be able to relink the
  app against a modified LAME. Publishing the complete app source (as an
  open-source project) satisfies this. A closed-source build would instead
  need LAME as a separately replaceable dynamic library.

If that ever becomes a problem, MP3 export can be dropped (WAV/FLAC remain) by
removing the `mp3lame-encoder` dependency and `ExportFormat::Mp3`.

No GPL code (e.g. Rubber Band) is used.
