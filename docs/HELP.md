# Using ShredLab

The basics explain themselves: open a song, wait a few seconds while it is
split into stems (vocals, guitar, bass, drums, keys, other), then switch
stems on and off and press play. This page covers the parts that aren't
obvious. The same text, shorter, is in the app under **Help & shortcuts**
(the keyboard button in the header).

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
| `1`–`6` | Toggle stem on / off |
| `Shift` + `1`–`6` | Solo stem on / off |
| `Alt` + `1`–`6` | All but this stem (play along) |

Stems are numbered top to bottom: 1 vocals, 2 guitar, 3 bass, 4 drums,
5 piano / keys, 6 other.
