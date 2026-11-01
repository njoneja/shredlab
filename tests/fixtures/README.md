# Golden test fixtures

Reference outputs from the Python implementation in `tools/reference/`, for
validating the Rust port. Two sets:

- **Committed (this folder, ~1.9 MiB):** model-free DSP and chunking vectors.
  Regenerate with `.venv\Scripts\python.exe tools/make_fixtures.py`.
- **Local (`local/`, gitignored, ~400 MB):** full-model outputs on a real
  song excerpt. Regenerate with `.venv\Scripts\python.exe tools/make_goldens.py`
  (needs ffmpeg and the model folders; see `tools/README.md`).

## Binary format

Every `.bin` file is raw **little-endian float32**, C order (last axis
fastest), no header. Shapes and descriptions are in the `index.json` next to
it (`files.<name>.shape`). Complex arrays have a trailing axis of 2:
`[..., 0]` = real, `[..., 1]` = imaginary.

## Committed files

Input for all DSP fixtures: `synth_input.bin`, the first **22050 samples
(0.5 s)** of the `synth-v1` self-test signal (docs/MODEL_MANIFEST.md).
0.5 s instead of 1 s keeps the folder under 2 MB; 22050 is not a multiple of
either hop, so the edge padding paths are exercised.

| File | Shape | What |
|---|---|---|
| `synth_input.bin` | (2, 22050) | synth-v1, channels 0 and 1. Tests the synth generator too. |
| `demucs_stft.bin` | (2, 2048, 22, 2) | `demucs_hybrid.spec(x)` = HTDemucs `_spec`: (channel, bin, frame, re/im). |
| `demucs_istft.bin` | (2, 22050) | `demucs_hybrid.ispec(demucs_stft, 22050)`. **Not** equal to the input (see below). |
| `roformer_stft.bin` | (2, 1025, 44, 2) | `torch.stft(x, 2048, 512, hann_window(2048), center=True, reflect)`. |
| `roformer_istft.bin` | (2, 22050) | `roformer_separate.istft(roformer_stft, 22050)` (≈ input, max err 2e-7). |
| `index.json` | | Shapes, plus `song_normalization` and `checks` (below). |
| `chunking.json` | | Chunk schedules, weights and fake-model overlap-add vectors. |

`index.json` extras:
- `song_normalization`: demucs song stats (mean, std of the mono mix
  `(L+R)/2`) for synth-v1 of length 22050, 343980 (htdemucs self-test) and
  588800. Computed with numpy float32 (population std, ddof=0). Rust may
  accumulate in f64; expect ~1e-7 relative difference.
- `checks`: how the numpy reference compares to torch (all ~1e-7).

### Demucs STFT rules (`spec` / `ispec`, n_fft 4096, hop 1024)

`spec(x)`, x of length L:
1. `le = ceil(L / hop)`, `pad = 3 * hop / 2 = 1536`.
2. Reflect-pad x by `pad` on the left and `pad + le*hop - L` on the right.
3. Reflect-pad again by `n_fft/2` both sides (torch.stft center=True).
4. Frames at `k*hop`, `1 + (len - n_fft) / hop` of them; multiply by
   periodic Hann `0.5 - 0.5 cos(2πn/N)` **divided by sqrt(n_fft)**
   (normalized=True); rfft.
5. Drop the last bin (Nyquist) → 2048 bins; keep frames `[2, 2 + le)`.

`ispec(z, L)`:
1. Append a zero Nyquist bin; zero-pad 2 frames on each side.
2. irfft each frame (n_fft), multiply by `sqrt(n_fft) * hann`.
3. Overlap-add at hop; divide by the overlap-added `hann²` envelope
   (floor 1e-11).
4. Output index 0 corresponds to padded position `n_fft/2`; take
   `[pad, pad + L)` after that offset.

Because frames 0–1 and the tail frames are discarded in `spec`, the
roundtrip is not exact near the edges (max error 0.23 on this input). That's
demucs' own behaviour (matches torch to 2e-7); compare against
`demucs_istft.bin`, not the input.

CaC layout for the graph's `mag` input: channels `[L.re, L.im, R.re, R.im]`;
the graph's `spec` output per stem uses the same layout.

### RoFormer STFT rules (n_fft 2048, hop 512)

`torch.stft(center=True, pad_mode="reflect", window=hann_window(2048)
(periodic), normalized=False, onesided)`: reflect-pad n_fft/2 both sides,
`1 + L // hop` frames, rfft → 1025 bins. iSTFT: irfft, multiply by window,
overlap-add, divide by the summed `window²` (floor 1e-8), drop `n_fft/2` at
the start, keep L samples.

Graph input `stft`: (1, 2, 1025, F, 2), i.e. (batch, channel, bin, frame,
re/im). Graph output `mask`: (1, N, 2050, F, 2) where axis 2 is
`bin * 2 + channel` (stereo fastest). Stem spectrum = complex multiply of the
channel's STFT with its mask.

### `chunking.json`

- `demucs.triangular_weights[S]`: apply_model weights for segment S =
  `concat(1..=S/2, (S - S/2)..=1) / max`, float32 (S/2 integer division).
- `demucs.cases[]`: `segment` S, `length` L, `stride = int(0.75 * S)`;
  `chunks[]` = every `offset` in `0, stride, 2*stride, … < L`, its length
  `n = min(S, L - offset)` and `window_start = offset - (S - n) / 2`
  (integer division; can be negative). The model window is
  `x[window_start .. window_start + S]` with zeros outside `[0, L)`; the
  output is cropped back to `[offset - window_start, … + n)`, weighted by
  `weights[:n]`, summed, and divided by the summed weights.
  `input`/`output` use a fake model `y[j] = window[j] * (1 + j / S)` (j =
  position inside the model window), no song normalization.
- `roformer.hamming[C]`: symmetric Hamming window
  `0.54 - 0.46 cos(2πn/(C-1))`, float32.
- `roformer.cases[]`: `chunk` C, `num_overlap` K, `step = C / K` (integer
  division), `starts` = `0, step, …` while `start + C < L`, then a final
  `max(L - C, 0)` unless equal to the previous start. Chunks shorter than C
  (only when L < C) are zero-padded at the end; output = `Σ y·w / max(Σ w,
  1e-10)` with w = hamming[:n]. Same fake model as above.

## Local files (`local/`, gitignored)

Source: `song.mp3`, decoded
with `ffmpeg -f f32le -ac 2 -ar 44100`. All inference on the ONNX CPU EP.
`local/index.json` lists shapes and also stores
`demucs_song_normalization`, `demucs_chunks` and `roformer_chunk_starts`.

| File | Shape | What |
|---|---|---|
| `demucs_input.bin` | (2, 441000) | Song 60–70 s. |
| `demucs_stems.bin` | (6, 2, 441000) | Full `demucs_hybrid.separate` (2 chunks, normalization). Stems: drums, bass, other, vocals, guitar, piano. |
| `demucs_w0_mix.bin` | (2, 343980) | Window 0 graph input `mix` (normalized). |
| `demucs_w0_mag.bin` | (4, 2048, 336) | Window 0 graph input `mag`. |
| `demucs_w0_wave.bin` | (6, 2, 343980) | Window 0 graph output `wave`. |
| `demucs_w0_spec.bin` | (6, 4, 2048, 336) | Window 0 graph output `spec`. |
| `demucs_w0_out.bin` | (6, 2, 343980) | `wave + ispec(uncac(spec))`, normalized domain, before weighting. |
| `roformer_input.bin` | (2, 1323000) | Song 60–90 s (30 s → 4 full chunks incl. the end-aligned tail). |
| `roformer_stems.bin` | (6, 2, 1323000) | Full `roformer_separate.separate` with BS-RoFormer-SW (personal model). Stems: bass, drums, other, vocals, guitar, piano. |
| `roformer_c0_stft.bin` | (2, 1025, 1151, 2) | Chunk 0 graph input. |
| `roformer_c0_mask.bin` | (6, 2050, 1151, 2) | Chunk 0 graph output. |
| `roformer_c0_out.bin` | (6, 2, 588800) | Chunk 0 stems before windowing/overlap-add. |

Suggested pass criterion for the Rust goldens: SNR ≥ 60 dB per stem on CPU.
Python vs Python across ORT thread counts already differs at
~1e-7 absolute.
