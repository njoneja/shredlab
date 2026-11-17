# Using ShredLab

The basics explain themselves: open a song, wait a few seconds while it is
split into stems (vocals, guitar, bass, drums, keys, other), then switch
stems on and off and press play. This page covers the parts that aren't
obvious. The same text, shorter, is in the app under **Help & shortcuts**
(the question-mark button in the header).

## Practice a part

- **Loop a section.** Drag across the waveform to set an A–B loop. Or put
  the playhead where you want and press `[` to set A and `]` to set B.
  `L` turns the loop on and off.
- **Speed and pitch.** Speed (50–150 %) slows the song down without
  changing the key. Pitch moves the key in semitones, without changing the
  speed. Click the cents value next to it for fine tuning. Use pitch to match a
  detuned recording or a guitar tuned down a half step.
- **Count-in.** Turn on Count-in to hear clicks before playback starts. Its
  length is in beats (hover it to see the bars). The count-in always ends
  exactly where playback (or the loop) starts. To count in again before
  every loop repeat, turn that on under the cog in the Click control.
- **Volume.** The speaker next to Play is the master
  volume: everything you hear, stems and click. Click it for a slider, or
  scroll over it. Exports are not affected. The slider inside the Click
  control sets only the click and count-in level. Both are remembered for
  every song; double-click a slider to reset it (100 % and 80 %).
- **Pickups.** If the part starts on a pickup before the bar, set A on the
  pickup note and set the count-in to the beats before it. In 4/4 with a
  one-beat pickup, 7 beats (1¾ bars) brings you in on the pickup.
- **Count in to where the music starts.** Many recordings start with
  studio talk or a few seconds of silence. With the count-in on, playing
  from the top skips that quiet intro and counts you straight into the
  music. A start or loop you set yourself is never moved, and fade-ins are
  never skipped. You can turn this off in Settings.

## Play along

- **⊘ (all but this)** on a stem row plays everything except that stem:
  the band without the guitar, say, so you play the guitar. `Alt` + the
  stem's number does the same.
- **S (solo)** plays just that stem, to learn the part by ear. Solo
  doesn't change the on/off switches, so turning it off brings back what
  you had.
- **All on** switches every stem back on and clears solo.
- **Export what you hear** writes exactly what is playing: the same stems
  at the same volumes, at the current speed and pitch, plus the click and
  count-in when they are on. You can export the whole song, from the
  playhead, or just the loop. The line under the button says what will be
  exported.

## When the click is off

The click follows the beats detected in the song, so tempo changes and
meter changes work. Detection is good but not perfect. To fix it, open the
cog in the Click control. Fixes are saved with the song.

- **This is beat 1.** Put the playhead on a downbeat and press it. The bar
  lines move so that beat is beat 1.
- **Beats per bar from bar …** Set the number of beats per bar from a chosen
  bar onward, for a meter change or a song detected in the wrong meter.
- **Click subdivision.** 2 or 3 clicks per beat. 3 gives a 6/8 or swing
  feel; 2 gives an eighth-note click.

Where detection goes wrong:

- **Odd meters** (5/4, 7/4) often get the beats right but put the
  downbeats in the wrong place. Use *This is beat 1*, then set the beats
  per bar.
- **Additive meters** (7/8 played as 2+2+3) may track as an even pulse.
- **3/4 and 6/8** can't be told apart from the audio. A song in 6/8
  usually shows as 2 beats per bar; use subdivision 3 for the feel.

The meter is shown as beats per bar rather than a time signature, because
that is what the click actually follows.

## Your songs

ShredLab keeps the stems of every song you open, so opening it again is
instant. Recent songs are listed under **Open** and on the start screen.

- **Remove from recent.** Hover over a song in the list and click its ✕,
  then **Remove**. The song leaves the list, but its stems are kept: open
  the same file again and it loads instantly and comes back into the list.
  To free its space as well, tick **Also delete its stems and settings**
  first (the box shows how much it frees; not possible for the song that
  is open).
- **Free up space.** Stems take a few hundred MB per song. **Settings →
  Manage storage…** lists every song with its size, when you last opened
  it, and whether the original file is still there. **Delete stems**
  removes a song's stems and its saved settings (mixer, Refine, beat
  fixes). Opening it again means splitting it again. The song that is
  open can't be deleted.

## Tuning

Many recordings are not at A440, and many songs are in a lowered tuning
such as Eb standard. The tuning-fork button next to Pitch shows how the
song is tuned once the stems are ready: gray when it is in tune (or still
being measured, with a spinner), amber with a dot when it is off A440, and
blue when the song has been shifted to match your tuning. Click it, or
press `T`, to open the Tuning panel.

- **Reference pitch** compares the song's A, as measured (for example
  A435.4, 18 cents flat), with yours. A line under it shows when the
  tuning drifts during the song.
- **Tuning** holds **My tuning**: your instrument, preset and shift in
  semitones, saved with each song. Under it, **Song is in** says how the
  song itself is tuned. Only the cents are measured: a song in Eb
  standard sounds in tune to the measurement, so set it here before
  shifting.
- **Guess** suggests the song's tuning from the lowest notes the bass
  plays, as **Likely** or **Possibly** (for example "Likely Eb standard").
  **Use** sets **Song is in**; nothing changes until you choose. A drop
  tuning and the lowered standard reach the same low note (drop D and D
  standard both go down to D), so both are offered. A dot on the button
  means a likely guess differs from what **Song is in** says.
- **Shift song to my tuning** moves the song to your tuning, so you don't
  have to retune. The arrow next to it picks how: **Pitch shift** (the
  default) keeps the tempo; **Playback rate** changes pitch and tempo
  together, like a tape running faster or slower, and sounds cleanest for
  small corrections. The last choice is kept for new songs. **Undo shift**
  puts it back.
- **Tune to the song** sets your reference and shift to the song's
  instead, for tuning your instrument to the recording as it is.
- **Reference tones** play each string of your tuning. Number keys play a
  string while that tab is shown (1 is the highest string), and `Space`
  stops the tone.
- **Live tuner** listens to your instrument through an audio interface or
  a microphone. Pick the input device, and for an interface with several
  inputs, the one your instrument is plugged into; both are remembered.
  Play a string: after the pluck settles (a fraction of a second), the
  tuner shows the note, how many cents it is off and which way to turn
  the peg ("7¢ flat: tune up"), and the string turns
  "in tune" once it stays within 3 cents for a second. **Chromatic**
  ignores your tuning and shows the nearest note. The song is paused while
  the tuner listens, and pressing Play stops the tuner so the speakers
  don't feed the microphone; **Listen again** starts it again. The tuner
  stops when you switch tabs or close the panel.
- If the level meter stays empty on Windows, the microphone may be blocked:
  check Settings > Privacy & security > Microphone > Let desktop apps
  access your microphone.

## Chords

Once the stems are ready, ShredLab works out the chords from the guitar,
bass, keys and other stems, on the song's beats. They show along the bottom
of the waveform, colored by root so verse and chorus look alike wherever
they come back, and the chord at the playhead is next to the time. Press
**Chords** (or `K`) for the chords panel: a few bars with the names written
out, in the same colors. With a loop on it shows the loop (**Fit loop**); otherwise it follows
the playhead (**Follow playhead**). `−` and `+`, or `Ctrl` and the mouse
wheel, show more or fewer bars. Click a chord to jump to it; hover for where
it starts.

- A chord that starts off the beat, like a riff that pushes the change an
  eighth note early, starts where it is played, before the beat line, with
  a small amber corner.
- A slash chord such as D/E means the bass holds that note under the chord.
- **N.C.** means no chord (silence, drums only).
- **Simple** names major, minor, power, seventh and sus chords. **Detailed**
  also names 6, add9, 9 and 7sus4 chords where the added note keeps
  sounding (A6 rather than A), and shows short passing chords, such as a
  quick Bm on the way back to D, as small tags above the chord they pass
  through. It also shows figures, where a chord moves to a richer version
  of itself and back (A to A6 in a boogie or rock rhythm part, or A to
  Asus4 and back), as tags with a solid outline. The chords' boundaries are the same either way.

The names depend on how the song is tuned. Until you set **Song is in** in
the Tuning panel, every name is what you hear; if the guess thinks the song
is tuned down, the panel offers **Use Eb standard shapes** (or similar).
Once it is set, the main name is the shape you would play in that tuning,
and when what you hear is different (a song in Eb, not shifted to your
Standard guitar), a second, dimmer name under it says what you hear: E5
over Eb5. The line at the end of the panel's header says which way round it
is, and turns amber when the shapes won't match your instrument, with
**Shift song to my tuning** and **I'm tuned down** (which sets My tuning).

Chord detection makes mistakes, more with dense or distorted parts and
with chords beyond major, minor, power chords, sevenths and sus chords.
The panel says so ("best guess"): treat the names as a starting point and
trust your ears.

## Keyboard shortcuts

| Keys | What |
|---|---|
| `Space` | Play / pause |
| `L` | Loop on / off |
| `[` | Set A at the playhead |
| `]` | Set B at the playhead |
| `←` `→` | Seek 5 s |
| `↑` `↓` | Speed ±5 % |
| `Shift` + `↑` `↓` | Pitch ±1 semitone |
| `C` | Click on / off |
| `T` | Tuning panel |
| `K` | Chords panel |
| `1`–`6` | Toggle stem on / off |
| `Shift` + `1`–`6` | Solo stem on / off |
| `Alt` + `1`–`6` | All but this stem (play along) |

Stems are numbered top to bottom: 1 vocals, 2 guitar, 3 bass, 4 drums,
5 piano / keys, 6 other.
