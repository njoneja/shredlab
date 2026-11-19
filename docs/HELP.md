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
- The first time the tuner listens on macOS, macOS asks whether ShredLab
  may use the microphone; choose Allow.
- If the level meter stays empty, the microphone may be blocked. On
  Windows, check Settings > Privacy & security > Microphone > Let desktop
  apps access your microphone. On macOS, check System Settings > Privacy &
  Security > Microphone and turn on ShredLab.

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

The names are for your instrument. Unless you set **My tuning** in the
Tuning panel, ShredLab assumes Standard tuning, and every name is what you
hear and what you would fret on a Standard guitar. With My tuning set to
something else (say Eb standard), the main name is the shape you fret in
that tuning, and a second, smaller name under it says how it sounds: E5
over Eb5. The line at the end of the panel's header says what it knows
about the song's tuning. If the guess thinks the song is in an alternate
tuning, it says so and offers **Set song to Eb standard** (or similar).
Once **Song is in** is set and it doesn't match your tuning, the line says
how to play the song with its usual shapes: tune down (then press **I'm
tuned down**, which sets My tuning) or press **Shift song to my tuning**.
When the song is shifted (by that button or by **Pitch**), the line says by
how much; the names follow, so they still match what you hear.
The notes panel follows the same rules.

Chord detection makes mistakes, more with dense or distorted parts and
with chords beyond major, minor, power chords, sevenths and sus chords.
The panel says so ("best guess"): treat the names as a starting point and
trust your ears.

## Notes

ShredLab also works out the single notes of the guitar, bass, vocals, keys
and other stems, one stem at a time in the background after the chords.
Press **Notes** (or `N`) for the notes panel, under the chords panel. It
shows one stem at a time as a piano roll: each note is a bar at its pitch,
colored like the chord roots (an A note has the color of an A chord), with
its name on it where it fits (above it when the bar is too short). A
keyboard down the left edge names the rows, and lights the key of the
note under the playhead. Pick the stem in the panel's header; ShredLab
remembers it for the song. Until a stem's notes are in, the panel says it
is finding them.

The notes are a rough draft, and the panel says so. Automatic note
detection from a mixed recording is far from reliable: expect missed
notes (more in fast runs, chords and busy mixes), some wrong notes or
octaves, and now and then a note from another part, such as the rhythm
guitar under a solo. Use them as a starting point and check by ear at slow
speed. When two parts share a stem, Refine's stereo mask on the part you
want usually gives better notes.

The notes panel uses the same stretch of the song as the chords panel, so
the bar lines line up: **Fit loop** or **Follow playhead**, and `−` and
`+` (or `Ctrl` and the mouse wheel) zoom both panels. With the chords
panel closed, the notes panel's header has those switches and the tuning
line itself.

- A bend rises in a curve from the note to where it is bent, labeled ½,
  full or 1½ (half, whole or one and a half steps).
- A slide is a slanted line from the note before.
- A hammer-on or pull-off is a small arc from the note before, with h or p.
- Where the stem plays several notes at once (rhythm guitar, piano
  chords), the panel shows a hatched block labeled CHORDS with the chord
  names from the chords panel; no single notes are guessed there.

The bold bars are the lead line, one note at a time. Behind them, thinner
and faint bars in the same colors, without names, are the other notes
heard in the stem: a note that keeps ringing after the next one starts
(the lead bar ends where the next note begins, the faint bar shows how
long it rang), notes of a rhythm part or a second voice, and inside
CHORDS blocks the notes of the chord. They show what the detector heard
but didn't pick for the lead line, which is also where a missed lead note
often is. Clear **Also heard** in the panel's header to hide them;
ShredLab remembers the choice. The keyboard only lights the lead note.

Click a note to play from it (faint notes too). Shift-click two notes to
loop from the first to the end of the second. Hover a note for its octave,
where it starts, its length and any bend; a faint note says "Also heard
(not in the lead line)".

Names follow the same rule as the chords: the shape you fret in My tuning,
with what you hear smaller below when the two differ.

The notes are found on the stem as you hear it after **Separation**
strength and Refine's stereo window, not its EQ. Two guitars panned left
and right are one guitar stem; set Refine's stereo window to the side of
the lead and the panel shows the lead line, and the stem picker says
"Guitar, refined: left". Changing the strength or the stereo window finds
that stem's notes again; going back to a setting you used before is
instant.

Two settings in the panel's header change how notes are found, for every
stem of the song but the bass (the bass ignores them); ShredLab remembers
them for the song.

- **Fast runs: Fewer · Normal · More.** When a quick run comes out as a few
  long notes, **More** splits it into separate notes, but it can also chop
  a held note into repeated short ones. **Fewer** keeps only clearly picked
  notes, for a part that comes out cluttered.
- **Quiet notes: Skip · Include.** **Include** lets softer notes into the
  lead line, such as quiet fills and the softer notes of a run. It adds
  few wrong notes.

The defaults are Normal and Skip. While either setting is off its default,
**Reset** next to them puts both back in one step. Changing a setting
finds the notes again; going back to settings you used before is instant.

While a loop is on, the two settings apply to the loop only, so you can
tune them for a solo without changing the rest of the song. A small label
next to them says which they apply to: **This loop** or **Whole song**. A
new loop starts with the song's settings. Changing one finds the notes of
the loop alone (a second or so for a short loop) and shows them inside the
loop, with the song's notes around it; the song's own settings and notes
stay as they were. Going back to settings you used before on the same
loop is instant. When you move, change or turn off the loop, its settings
go back to the song's. With a loop on, **Reset** puts the loop back on
the song's settings; with no loop, it puts the song back on Normal and
Skip.

Like the chords, the notes are a best guess, more often wrong in fast,
dense or heavily distorted parts. Check them by ear at a slower speed.

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
| `N` | Notes panel |
| `1`–`6` | Toggle stem on / off |
| `Shift` + `1`–`6` | Solo stem on / off |
| `Alt` + `1`–`6` | All but this stem (play along) |

Stems are numbered top to bottom: 1 vocals, 2 guitar, 3 bass, 4 drums,
5 piano / keys, 6 other.
