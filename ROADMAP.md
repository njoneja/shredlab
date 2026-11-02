# Roadmap

What's being worked on and what's being considered for ShredLab. Nothing
here is a promise and there are no dates; plans change as features get
tried out. Ideas and requests are welcome as
[issues](https://github.com/njoneja/shredlab/issues).

ShredLab is a practice and transcription tool, not a DAW: no recording,
no multitrack editing, no effects chain. Features that would turn it into
one are out of scope.

## In progress

- **Smaller song library**: less disk space per separated song.

## Toward 1.0

- Audio output device selection.
- Separation in a separate process, so a GPU driver crash can't take the
  app down with it.
- Automatic updates.
- Clearer first-run experience: GPU detection and what happens when the
  app falls back to the CPU.

## Being considered

### Click track and measures

- **Beat grid editor** for songs where the detected beats are wrong or
  the song changes tempo or time signature: sections with their own time
  signature and grouping (7/8 as 2+2+3), accents that follow the grouping,
  halve or double the tempo in a range, move, add or delete beats, tap
  along to fix a passage, fixed tempo (or a gradual speed-up or slow-down)
  where detection fails. Edit against the drum stem on its own. Export the
  tempo map as a MIDI file for a DAW or Guitar Pro, and share corrected
  grids as a file.
- **Click only**: mute the music and keep the song's own tempo as a
  metronome.

### Practice

- **Speed trainer**: loop a passage starting slow and raise the speed a
  step every few passes up to the target.
- **Saved loops and sections**, named per song, snapping to bars.
- **Markers**, and automatically suggested sections (verse, chorus, solo).
- **Call and response**: play the loop, then leave a gap of the same
  length for you to play it back.
- **Mix presets** such as "bass practice".
- **Hands-free control**: remappable keyboard shortcuts (works with
  Bluetooth page-turner pedals) and MIDI pedals.
- **Split ears**: backing track in one ear, the isolated part in the
  other.

### Transcription

- **Tuning detection**: notice that a song is a half step down or not at
  A = 440 Hz and retune it in one click.
- Key and chord detection.
- Note readout for the bass stem (and possibly single-note guitar parts).
- Spectrogram or pitch view of a stem.
- Repeat a tiny slice while held, for fast licks.
- Drum hits shown as kick, snare and hi-hat lanes.

### Separation

- **Drum kit separation** into kick, snare, toms and cymbals.
- A higher-quality downloadable model, once one with a clear license is
  available.
- Smaller, faster model files.
- Importing your own models inside the app.
- A frequency range for the stereo exception in Refine, for example to
  remove bass bleed from the center of a guitar stem while keeping the
  rest of the center.

### Library and export

- Separate a whole folder or playlist in the background.
- Open video files (lessons, live clips) and use their audio.
- Library search, tags and favorites.
- Export every stem at once to a folder.
- Practice exports for your phone: a loop repeated or ramping up in
  speed.

### Platforms

- Linux.
