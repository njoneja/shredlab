# shredlab-library

Stores every opened song as a folder with a JSON manifest (`song.json`) and
its cached stems, so separation runs once per song, model and quality, and
the per-song settings survive restarts.

## Boundaries

Owns song ids, the folder layout, the `song.json` schema and its migration,
reading and writing stem caches (WAV, FLAC, MP3) with a per-stem scale, the
cached copy of the original mix, tag reading, the recent-songs list, sizes
on disk and safe deletion.

Does not choose the cache format (the caller passes a `CacheFormat`; the app
takes it from Settings) or the library location (the caller passes the
root; the app uses `<data dir>/library`). It does not interpret the opaque
UI `settings` JSON or the `RefineParams` it stores. No Tauri dependency.

Dependencies: `shredlab-io` (decoding and resampling cache files),
`shredlab-dsp` (only for the `RefineParams` type), serde/serde_json, sha2,
symphonia (tags), flacenc and mp3lame-encoder. Used by `app/src-tauri`;
`shredlab-cli` only prints its version.

## Layout

Start with `Library` and `SongMeta` in `src/lib.rs`.

- `src/lib.rs`: manifest types, `Library` (load/save, recent list, stem and original cache, deletion), `song_id_for`, `read_tags`, the WAV cache reader and writer.
- `src/codec.rs`: FLAC and MP3 cache writers, `read` (decode, check or resample, scale, fit to length), atomic file writes.

## How it works

```text
<root>/songs/<song_id>/song.json                          manifest (schema 2)
<root>/songs/<song_id>/original.<ext>                     copy of the original mix
<root>/songs/<song_id>/stems/<model_id>_s<shifts>/<stem>.<mp3|flac|wav>
<root>/songs/<song_id>/stems/<stem>.wav                   schema 1 stem sets
```

The song id is sha256 over the file size plus its first and last 4 MB,
truncated to 32 hex characters. It doesn't depend on the path or the
modification time, so a moved or copied file keeps its cache.

`song.json` (`SongMeta`) holds the source path, title and artist (from tags,
else the file name), duration and decoded format, then:

- `stem_sets`: one `StemsInfo` per (model, shifts), most recent last:
  model id and display name, device label, sample rate, length, folder,
  `format` and one `{name, scale}` per stem. `put_stem_set` replaces the
  entry for the same model and shifts.
- `original`: sample rate, length, scale and file name of the cached original.
- `beats`: the beat grid (beats, downbeats, tempo, beats per bar).
- `refine`: `RefineParams` per stem name, only for refined stems.
- `strength`: separation strength level (0 = the model's stems) and the
  smooth flag.
- `settings`: per-song UI settings, stored and returned verbatim.
- `hidden` (removed from the recent list, data kept) and timestamps.

`save` writes `song.json.tmp` and renames it, bumping `updated_ms`.

Stems are cached per model and shifts, so switching back is instant. Each
stem is divided by `scale = max(peak, 1)` before encoding and multiplied by
it on load, so loud stems never clip. Formats:

| `CacheFormat` | Encoding | Size, 6 stems of a 7-minute song |
|---|---|---|
| `Mp3` | LAME VBR V0 with a LAME tag (app default) | about 75 MB |
| `Flac` | 16-bit | about 135 MB |
| `Wav` | 16-bit PCM (caches written before 2026-11-02) | about 440 MB |

The MP3 listening tests for playback, slowed playback and "All but this" are
in `spikes/mp3_cache_test.py`. The LAME tag records the encoder delay and
padding; symphonia's gapless mode trims both, so a decoded MP3 stem lines up
sample for sample with the original, and the decoded length is then fitted
to the length in `song.json`.

`write_stems` encodes all stems in parallel threads, each to a temp file
renamed into place, then deletes same-named stems in the other formats in
that folder. It doesn't touch `song.json`: the caller adds the set with
`put_stem_set` and saves afterwards, so a half-written cache is never
referenced. `read_stems` decodes in parallel; check `stems_present` before
relying on a set.

The original is cached so the backing track still works after the source
file moves, and it is never re-encoded lossily: "All but this" subtracts
stems from it, which exposes its coding noise (measured 22.8 dB vs 38.5 dB
backing SNR). A lossy source (`mp3`, `m4a`, `aac`, `mp4`, `ogg`) is copied
as `original.<ext>`, but only if decoding and resampling the copy gives
exactly the samples the caller passed; otherwise it is stored as 16-bit
FLAC at the song rate. Older `original.*` files are removed.

## Invariants and gotchas

- Old manifests must keep loading. Every new field needs `#[serde(default)]`
  (or `Option`) so a `song.json` without it still parses. Existing
  defaults encode history: missing `shifts` = 1, `dir` = `stems`,
  `format` = WAV, original `file` = `original.wav`. So `CacheFormat`'s
  `Default` must stay `Wav` even though new caches are MP3. `RefineParams`
  in `shredlab-dsp` is `#[serde(default)]` for the same reason.
- Schema 1 manifests have a single `stems` entry; `SongMeta::migrate` moves
  it into `stem_sets` on load, and the next save writes schema 2.
- Write files first, then the manifest, so a new set is only referenced
  once its files exist. Re-encoding a set in another format deletes the
  old files before the caller updates the manifest, which is one reason
  readers check `stems_present` first.
- Treat names from `song.json` as untrusted: `save` rejects ids with
  separators or dots, the original's file name must be a plain name,
  stem folders drop `..` segments, and `safe_song_dir` canonicalizes the
  folder and refuses symlinks before anything is deleted. Keep these checks
  for any new path read from the manifest.
- Stems must decode at the stem set's sample rate (no resampling);
  only a copied lossy original is resampled on load.
- FLAC caches store `scale` widened by 32768/32767, because symphonia
  decodes 16-bit samples as `i / 32768` while the writer maps the peak to
  32767.
- The lossy-copy check depends on `shredlab-io` decoding deterministically
  and the caller passing that same decode at that rate. If decoding ever
  changes, the check fails and the original falls back to FLAC, which is
  safe.
- Retagging a file changes its bytes, and usually its song id.
- `list` walks every song folder for sizes; call it off the UI thread.

## Testing

```sh
cargo test -p shredlab-library
```

Tests use temp directories; no models or fixtures are needed. The MP3
alignment check is `mp3_is_sample_aligned`; manifest compatibility is
covered by `old_manifests_load_as_wav` and
`schema1_stems_migrate_to_stem_sets`.

## Common changes

- Adding a field to `song.json`: add it to `SongMeta` with
  `#[serde(default)]` (plus `skip_serializing_if` if it is usually empty),
  initialize it in `SongMeta::new`, and add a test that loads JSON without
  it. Bump `SCHEMA` only for an incompatible change, with a migration in
  `SongMeta::migrate`.
- Adding a cache format: a `CacheFormat` variant and extension, a writer in
  `src/codec.rs` that returns the scale to store, a branch in `read_stems`,
  and the new variant in the stale-format cleanup in `write_stems`. The
  app's Settings choice maps to it in `app/src-tauri/src/settings.rs`.
- Copying another lossy source type as the original: add its extension to
  `LOSSY_SOURCE_EXTS` (also used to clean up old copies).
