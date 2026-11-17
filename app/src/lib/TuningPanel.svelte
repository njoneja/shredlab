<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Navneet Joneja -->

<script lang="ts">
  // The Tuning panel: the song's measured tuning and the match to the
  // player's tuning (left), reference tones for the strings (right). The
  // song keeps playing while it is open. Opened by the transport's Tuning
  // button or T; Esc closes it and stops any tone.
  import { onMount, tick } from "svelte";
  import Icon from "./Icon.svelte";
  import { app } from "./store.svelte";
  import type { MatchMethod, ToneSound } from "./types";
  import {
    CUSTOM_NOTE_MAX,
    CUSTOM_NOTE_MIN,
    IN_TUNE_CENTS,
    INSTRUMENTS,
    RATE_MAX_CENTS,
    REF_MAX,
    REF_MIN,
    SHIFT_MAX,
    baseStrings,
    driftSummary,
    formatCents,
    formatHz,
    formatPitch,
    instrument,
    medianSmooth,
    noteLabel,
    offsetText,
    rateTempoPercent,
    refCents,
    songRefHz,
    songTuningName,
    splitPitch,
    strings,
    tuningLongName,
    type InstrumentId,
    type PresetId,
  } from "./tuning";

  let closeBtn: HTMLButtonElement;

  const t = $derived(app.tuning);
  const tstate = $derived(app.tuningState);
  const m = $derived(app.matchNow);
  const my = $derived(app.myTuning);
  const strs = $derived(strings(my));
  const inst = $derived(instrument(my.instrument));
  const canPitch = $derived(app.canMatch("pitch", m));
  const canRate = $derived(app.canMatch("rate", m));
  const canShift = $derived(app.canMatch(app.matchMethod, m));
  /** The song's tuning written the way "Tune to the song" would set it. */
  const songAsTuning = $derived(t ? { ...my, refHz: songRefHz(t.offset_cents), shift: app.songSemitones } : null);
  const drift = $derived(t?.drifts ? driftSummary(t.segments) : null);
  const semitoneOptions = Array.from({ length: 2 * SHIFT_MAX + 1 }, (_, i) => SHIFT_MAX - i);
  const customNotes = Array.from({ length: CUSTOM_NOTE_MAX - CUSTOM_NOTE_MIN + 1 }, (_, i) => CUSTOM_NOTE_MIN + i);

  const minus = (n: number) => (n < 0 ? "−" : n > 0 ? "+" : "");
  const tempo = (cents: number) => {
    const p = rateTempoPercent(cents);
    return `${minus(Math.round(p * 10))}${Math.abs(p).toFixed(1)}%`;
  };
  const shiftText = (n: number) => `${minus(n)}${Math.abs(n)} st`;

  const shiftReason = $derived.by(() => {
    if (m == null || canShift) return "";
    return app.matchMethod === "pitch"
      ? `The shift (${formatCents(m)}) is beyond Pitch's range of ±12 semitones`
      : `Playback rate works up to ±${RATE_MAX_CENTS}¢ (about ±12% tempo); this shift is ${formatCents(m)}`;
  });

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      app.closeTuning();
      return;
    }
    if (e.ctrlKey || e.metaKey || e.altKey || isTyping(e.target)) return;
    const d = /^(?:Digit|Numpad)([1-9])$/.exec(e.code);
    if (d) {
      const n = Number(d[1]);
      if (n <= strs.length) {
        e.preventDefault();
        app.playTone(n);
      }
    } else if (e.code === "Space") {
      e.preventDefault();
      if (app.toneString != null) app.stopTone();
      else app.togglePlay();
    }
  }

  // A focused button would also "click" on Space keyup; swallow it.
  function onKeyup(e: KeyboardEvent) {
    if (e.code === "Space" && !isTyping(e.target)) e.preventDefault();
  }

  function isTyping(el: EventTarget | null) {
    if (!(el instanceof HTMLElement)) return false;
    if (el.isContentEditable || el instanceof HTMLTextAreaElement || el instanceof HTMLSelectElement) return true;
    return el instanceof HTMLInputElement && !["range", "checkbox", "radio", "button"].includes(el.type);
  }

  // ---- My tuning ----
  function setInstrument(id: InstrumentId) {
    const next = instrument(id);
    if (my.preset === "custom") app.setMyTuning({ ...my, instrument: id, custom: [...next.presets[0].strings] });
    else app.setMyTuning({ ...my, instrument: id, preset: next.presets.some((p) => p.id === my.preset) ? my.preset : "standard", custom: null });
  }

  function setPreset(id: PresetId) {
    if (id === my.preset) return;
    app.setMyTuning(id === "custom" ? { ...my, preset: id, custom: [...baseStrings(my)] } : { ...my, preset: id, custom: null });
  }

  function setCustomString(i: number, midi: number) {
    const custom = [...baseStrings(my)];
    custom[i] = midi;
    app.setMyTuning({ ...my, preset: "custom", custom });
  }

  // The A = field edits text; it commits on Enter or leaving the field.
  let refText = $state("");
  let refFocused = false;
  $effect(() => {
    const hz = my.refHz;
    if (!refFocused) refText = hz.toFixed(1);
  });

  function commitRef() {
    const v = Number(refText.replace(",", "."));
    if (Number.isFinite(v) && v > 0) app.setMyTuning({ ...my, refHz: Math.min(REF_MAX, Math.max(REF_MIN, Math.round(v * 10) / 10)) });
    refText = app.myTuning.refHz.toFixed(1);
  }

  function refKeys(e: KeyboardEvent) {
    if (e.key === "Enter") {
      commitRef();
    } else if (e.key === "ArrowUp" || e.key === "ArrowDown") {
      e.preventDefault();
      const step = (e.shiftKey ? 1 : 0.1) * (e.key === "ArrowUp" ? 1 : -1);
      app.setMyTuning({ ...my, refHz: Math.min(REF_MAX, Math.max(REF_MIN, Math.round((my.refHz + step) * 10) / 10)) });
      refText = app.myTuning.refHz.toFixed(1);
    }
  }

  // ---- drift curve ----
  const curve = $derived.by(() => {
    if (!t?.drifts || t.segments.length < 2) return null;
    const v = medianSmooth(t.segments.map((s) => s.offset_cents));
    const end = Math.max(app.song?.duration_sec ?? 0, t.segments[t.segments.length - 1].start_s + 10);
    const lo = Math.min(...v, t.offset_cents) - 2;
    const hi = Math.max(...v, t.offset_cents) + 2;
    const y = (c: number) => 2 + (30 * (hi - c)) / (hi - lo);
    const pts = t.segments.map((s, i) => `${((220 * (s.start_s + 5)) / end).toFixed(1)} ${y(v[i]).toFixed(1)}`);
    return { d: `M${pts.join(" L")}`, avg: y(t.offset_cents) };
  });

  // Radiogroup arrows for the segmented controls (move the choice, like native radios).
  async function segKeys(e: KeyboardEvent, values: string[], current: string, pick: (v: string) => void) {
    const dir = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
    if (!dir) return;
    e.preventDefault();
    const group = e.currentTarget as HTMLElement;
    const enabled = values.filter((v) => !group.querySelector<HTMLButtonElement>(`[data-v="${v}"]`)?.disabled);
    const next = enabled[(enabled.indexOf(current) + dir + enabled.length) % enabled.length];
    pick(next);
    await tick();
    group.querySelector<HTMLElement>(`[data-v="${next}"]`)?.focus();
  }

  onMount(async () => {
    await tick();
    closeBtn?.focus();
  });
</script>

<svelte:window onkeydown={onKey} onkeyup={onKeyup} />

<div class="backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && app.closeTuning()}>
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="tuning-title">
    <div class="head">
      <div class="title">
        <h2 id="tuning-title">Tuning</h2>
        {#if app.song}<span class="muted song">{app.song.title}</span>{/if}
      </div>
      <button class="btn ghost small icon-only" bind:this={closeBtn} onclick={() => app.closeTuning()} aria-label="Close"><Icon name="close" size={14} /></button>
    </div>

    <div class="body">
      <div class="left">
        <section class="card" class:matched={app.matched} aria-labelledby="tuning-song">
          <h3 class="h" id="tuning-song">Song</h3>
          {#if tstate === "measuring"}
            <p class="big">Measuring tuning…</p>
            <div class="bar" class:indeterminate={!app.separating}>
              <div class="fill" style:width={app.separating ? `${Math.round(app.sepFraction * 100)}%` : null}></div>
            </div>
            <p class="muted">Runs after separation, on the stems. The reference tones work meanwhile.</p>
          {:else if tstate === "unmeasured" || !t}
            {#if t}
              <p class="big">Couldn't measure the tuning of this song.</p>
              <p class="muted">The stems have too little steady pitch to tell. Your tuning and the reference tones still work.</p>
            {:else}
              <p class="big">Not measured yet.</p>
              <p class="muted">The tuning is measured on the stems after the song is separated.</p>
            {/if}
          {:else}
            <div class="ref">
              <span class="mono hz">A = {t.reference_hz.toFixed(1)} Hz</span>
              <span class="mono cents" class:warn={tstate === "off"} class:muted={tstate !== "off"}>{formatCents(t.offset_cents)}</span>
            </div>
            <label class="row song-in">
              <span class="muted">Song is in</span>
              <select class="field small" value={app.songSemitones} onchange={(e) => app.setSongSemitones(Number(e.currentTarget.value))}>
                {#each semitoneOptions as n (n)}<option value={n}>{songTuningName(n)}</option>{/each}
              </select>
            </label>
            {#if curve && drift}
              <svg class="curve" width="100%" height="34" viewBox="0 0 220 34" preserveAspectRatio="none" role="img" aria-label="Tuning drifts {drift.kind === 'from' ? 'from' : 'between'} {drift.a} {drift.kind === 'from' ? 'to' : 'and'} {drift.b} cents over the song">
                <line x1="0" y1={curve.avg} x2="220" y2={curve.avg} class="avg" vector-effect="non-scaling-stroke" />
                <path d={curve.d} class="line" vector-effect="non-scaling-stroke" />
              </svg>
            {/if}
            <p class="muted">
              {#if drift}
                Drifts {drift.kind === "from" ? "from" : "between"} {formatCents(drift.a)} {drift.kind === "from" ? "to" : "and"} {formatCents(drift.b)} over the song. Shift uses the average.
              {:else if tstate === "in-tune"}
                Within {IN_TUNE_CENTS} cents of A440, steady through the song.{m != null && Math.abs(m) <= IN_TUNE_CENTS ? " Nothing to correct." : ""}
              {:else}
                {Math.abs(Math.round(t.offset_cents))} cents {t.offset_cents < 0 ? "flat" : "sharp"} of A440, steady through the song.
              {/if}
            </p>

            {#if app.matched && app.match}
              <p class="muted">
                {#if app.match.method === "pitch"}
                  Shifted <span class="mono strong">{formatPitch(splitPitch(app.match.cents) ?? { semitones: 0, cents: 0 })}</span> to match {tuningLongName(my)}. Saved with this song.
                {:else}
                  Playback rate <span class="mono strong">{formatCents(app.match.cents)}</span> (tempo {tempo(app.match.cents)}) to match {tuningLongName(my)}. Saved with this song.
                {/if}
              </p>
              <div class="row"><button class="btn" onclick={() => app.undoShift()}>Undo shift</button></div>
            {:else}
              <div class="row actions">
                <button
                  class="btn"
                  class:primary={m != null && Math.abs(m) > IN_TUNE_CENTS}
                  disabled={!canShift}
                  title={shiftReason || undefined}
                  onclick={() => app.shiftToMyTuning()}
                >
                  Shift song to my tuning
                </button>
                <button class="btn" onclick={() => app.tuneToSong()}>Tune to the song</button>
              </div>
              <p class="muted">
                {#if m != null && canShift && app.matchMethod === "pitch"}
                  Shift sets Pitch to <span class="mono strong">{formatPitch(splitPitch(m)!)}</span>, so the song matches {tuningLongName(my)}.
                {:else if m != null && canShift}
                  Shift plays the song at <span class="mono strong">{formatCents(m)}</span> (tempo {tempo(m)}), so it matches {tuningLongName(my)}. Pitch stays as set.
                {:else}
                  {shiftReason}.
                {/if}
                {#if songAsTuning}Tune to the song sets your tuning to {tuningLongName(songAsTuning)} instead and leaves the audio alone.{/if}
              </p>
            {/if}

            <div class="row">
              <span class="muted">Shift by</span>
              <!-- svelte-ignore a11y_interactive_supports_focus -->
              <div
                class="seg grow"
                role="radiogroup"
                aria-label="Shift by"
                tabindex="-1"
                onkeydown={(e) => segKeys(e, ["pitch", "rate"], app.matchMethod, (v) => app.setMatchMethod(v as MatchMethod))}
              >
                <button role="radio" data-v="pitch" aria-checked={app.matchMethod === "pitch"} tabindex={app.matchMethod === "pitch" ? 0 : -1} disabled={!canPitch} onclick={() => app.setMatchMethod("pitch")}>
                  Pitch shift
                </button>
                <button
                  role="radio"
                  data-v="rate"
                  aria-checked={app.matchMethod === "rate"}
                  tabindex={app.matchMethod === "rate" ? 0 : -1}
                  disabled={!canRate}
                  title={canRate ? "Changes pitch and tempo together, like correcting a tape speed. No time-stretching." : `Playback rate works up to ±${RATE_MAX_CENTS}¢ (about ±12% tempo)`}
                  onclick={() => app.setMatchMethod("rate")}
                >
                  Playback rate{m != null && canRate ? ` (tempo ${tempo(m)})` : ""}
                </button>
              </div>
            </div>
          {/if}
        </section>

        <section class="card" aria-labelledby="tuning-mine">
          <h3 class="h" id="tuning-mine">My tuning</h3>
          <select class="field" aria-label="Instrument" value={my.instrument} onchange={(e) => setInstrument(e.currentTarget.value as InstrumentId)}>
            {#each INSTRUMENTS as i (i.id)}<option value={i.id}>{i.label}</option>{/each}
          </select>
          <!-- svelte-ignore a11y_interactive_supports_focus -->
          <div
            class="seg"
            role="radiogroup"
            aria-label="Tuning"
            tabindex="-1"
            onkeydown={(e) => segKeys(e, [...inst.presets.map((p) => p.id), "custom"], my.preset, (v) => setPreset(v as PresetId))}
          >
            {#each [...inst.presets.map((p) => ({ id: p.id, label: p.label })), { id: "custom" as PresetId, label: "Custom" }] as p (p.id)}
              <button role="radio" data-v={p.id} aria-checked={my.preset === p.id} tabindex={my.preset === p.id ? 0 : -1} onclick={() => setPreset(p.id)}>{p.label}</button>
            {/each}
          </div>
          {#if my.preset === "custom"}
            <div class="custom" role="group" aria-label="Custom strings, lowest first">
              {#each baseStrings(my) as midi, i (i)}
                <select class="field small mono" aria-label="String {baseStrings(my).length - i}" value={midi} onchange={(e) => setCustomString(i, Number(e.currentTarget.value))}>
                  {#each customNotes as n (n)}<option value={n}>{noteLabel(n)}</option>{/each}
                </select>
              {/each}
            </div>
          {/if}
          <div class="row between">
            <div class="row">
              <span class="muted">Shift</span>
              <button class="btn small icon-only" aria-label="Down a semitone" disabled={my.shift <= -SHIFT_MAX} onclick={() => app.setMyTuning({ ...my, shift: my.shift - 1 })}><Icon name="minus" size={14} /></button>
              <span class="mono shift" class:changed={my.shift !== 0}>{shiftText(my.shift)}</span>
              <button class="btn small icon-only" aria-label="Up a semitone" disabled={my.shift >= SHIFT_MAX} onclick={() => app.setMyTuning({ ...my, shift: my.shift + 1 })}><Icon name="plus" size={14} /></button>
            </div>
            <div class="row">
              <label class="muted" for="tuning-ref">A =</label>
              <input
                id="tuning-ref"
                class="field mono refin"
                class:changed={my.refHz !== 440}
                type="text"
                inputmode="decimal"
                bind:value={refText}
                title="A4 reference, {REF_MIN} to {REF_MAX} Hz. Arrow keys step 0.1 Hz (Shift: 1 Hz)."
                onfocus={() => (refFocused = true)}
                onblur={() => {
                  refFocused = false;
                  commitRef();
                }}
                onkeydown={refKeys}
              />
              <span class="muted">Hz</span>
              <span class="mono muted rc">{formatCents(refCents(my.refHz))}</span>
            </div>
          </div>
          <p class="muted">{tuningLongName(my)}{offsetText(my) === "Standard at A440" ? "" : ` (${offsetText(my)})`}.</p>
        </section>
      </div>

      <div class="right">
        <div class="tabs" role="tablist" aria-label="Tuning tools">
          <button class="tab on" role="tab" id="tab-tones" aria-selected="true" aria-controls="panel-tones">Reference tones</button>
        </div>
        <div class="tones" role="tabpanel" id="panel-tones" aria-labelledby="tab-tones">
          <div class="strings">
            {#each strs as s (s.number)}
              {@const playing = app.toneString === s.number}
              {@const shownMidi = s.midi + (app.toneOctaveUp ? 12 : 0)}
              <div class="str" class:playing>
                <span class="mono muted num">{s.number}</span>
                <span class="note">{s.name}</span>
                <span class="mono detail">{noteLabel(shownMidi, my.shift < 0)} · {(s.hz * (app.toneOctaveUp ? 2 : 1)).toFixed(2)} Hz</span>
                {#if playing}
                  <svg class="level" width="34" height="16" viewBox="0 0 34 16" aria-hidden="true">
                    <rect x="0" y="6" width="4" height="10" rx="1" /><rect x="7" y="2" width="4" height="14" rx="1" /><rect x="14" y="0" width="4" height="16" rx="1" /><rect x="21" y="4" width="4" height="12" rx="1" /><rect x="28" y="8" width="4" height="8" rx="1" />
                  </svg>
                {/if}
                <button
                  class="btn small"
                  class:primary={playing}
                  aria-label="{playing ? 'Stop' : 'Play'} string {s.number} ({noteLabel(shownMidi, my.shift < 0)})"
                  onclick={() => app.toggleTone(s.number)}
                >
                  <Icon name={playing ? "stop" : "play"} size={12} />
                  {playing ? "Stop" : "Play"}
                </button>
              </div>
            {/each}
          </div>

          <div class="opts">
            <div class="row">
              <span class="muted">Sound</span>
              <!-- svelte-ignore a11y_interactive_supports_focus -->
              <div class="seg" role="radiogroup" aria-label="Sound" tabindex="-1" onkeydown={(e) => segKeys(e, ["pluck", "drone"], app.toneSound, (v) => app.setToneSound(v as ToneSound))}>
                {#each [{ v: "pluck" as ToneSound, label: "Pluck" }, { v: "drone" as ToneSound, label: "Drone" }] as o (o.v)}
                  <button role="radio" data-v={o.v} aria-checked={app.toneSound === o.v} tabindex={app.toneSound === o.v ? 0 : -1} onclick={() => app.setToneSound(o.v)}>{o.label}</button>
                {/each}
              </div>
            </div>
            <label class="chk"><input type="checkbox" checked={app.toneOctaveUp} onchange={(e) => app.setToneOctaveUp(e.currentTarget.checked)} /> Octave up</label>
            <label class="chk" title="After 6 seconds, move to the next higher string; stop after the highest"
              ><input type="checkbox" checked={app.toneStep} onchange={(e) => app.setToneStep(e.currentTarget.checked)} /> Step to next string</label
            >
            <label class="chk" title="Starting a tone pauses the song; it doesn't resume by itself"
              ><input type="checkbox" bind:checked={app.tonePause} /> Pause song while tones play</label
            >
          </div>
          <p class="muted hint">Number keys 1–{strs.length} play a string. Space stops.</p>
        </div>
      </div>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 60;
    background: var(--backdrop);
    display: grid;
    /* Anchored near the top, so the controls stay put when the Song card grows or shrinks. */
    place-items: start center;
    grid-template-columns: minmax(0, 1fr);
    padding: max(20px, 7vh) 20px 20px;
  }
  .dialog {
    width: 960px;
    max-width: 100%;
    max-height: calc(100vh - max(20px, 7vh) - 20px);
    display: flex;
    flex-direction: column;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    box-shadow: var(--shadow);
  }
  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 14px 18px;
    border-bottom: 1px solid var(--border);
  }
  .title {
    display: flex;
    align-items: baseline;
    gap: 10px;
    min-width: 0;
  }
  h2 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
  }
  .song {
    font-size: 12.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .body {
    display: grid;
    grid-template-columns: 400px minmax(0, 1fr);
    gap: 16px;
    padding: 16px 18px;
    overflow-y: auto;
    min-height: 0;
  }
  @media (max-width: 820px) {
    .body {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .left,
  .right {
    display: flex;
    flex-direction: column;
    gap: 14px;
    min-width: 0;
  }
  .right {
    gap: 12px;
  }
  .card {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 14px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .card.matched {
    border-color: var(--accent);
  }
  .h {
    margin: 0;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
  }
  p {
    margin: 0;
  }
  .card p.muted,
  .hint {
    font-size: 12.5px;
  }
  .big {
    font-size: 15px;
  }
  .strong {
    color: var(--fg);
  }
  .warn {
    color: var(--warn);
  }
  .changed,
  .field.changed {
    color: var(--accent);
  }
  .ref {
    display: flex;
    align-items: baseline;
    gap: 12px;
  }
  .hz {
    font-size: 24px;
    line-height: 1.2;
  }
  .cents {
    font-size: 14px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .row.between {
    justify-content: space-between;
    flex-wrap: wrap;
    row-gap: 8px;
  }
  .actions {
    margin-top: 4px;
    flex-wrap: wrap;
  }
  .song-in {
    font-size: 12.5px;
  }
  .field {
    height: 28px;
    border: 1px solid var(--control-border);
    border-radius: var(--radius-sm);
    background: var(--surface);
    color: var(--fg);
    padding: 0 8px;
    font: inherit;
    font-size: 13px;
  }
  .field.small {
    height: 24px;
    font-size: 12.5px;
    padding: 0 4px;
  }
  .field.mono {
    font-family: var(--mono);
  }
  .refin {
    width: 64px;
  }
  .shift {
    font-size: 13px;
    min-width: 40px;
    text-align: center;
  }
  .rc {
    min-width: 34px;
    font-size: 12.5px;
  }
  .custom {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  /* Segmented control: the chosen option tinted like a pressed toggle. */
  .seg {
    display: flex;
    border: 1px solid var(--control-border);
    border-radius: var(--radius-sm);
    overflow: hidden;
  }
  .seg.grow {
    flex: 1;
  }
  .seg button {
    flex: 1;
    height: 28px;
    padding: 0 10px;
    border: 0;
    background: var(--surface);
    font-size: 12.5px;
    font-weight: 500;
    white-space: nowrap;
  }
  .seg button + button {
    border-left: 1px solid var(--control-border);
  }
  .seg button:hover:not(:disabled):not([aria-checked="true"]) {
    background: var(--surface-3);
  }
  .seg button[aria-checked="true"] {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .seg button:focus-visible {
    outline-offset: -2px;
  }
  .bar {
    height: 4px;
    border-radius: 2px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
  }
  .bar.indeterminate .fill {
    width: 40%;
    animation: slide 1.4s ease-in-out infinite;
  }
  @keyframes slide {
    from {
      transform: translateX(-100%);
    }
    to {
      transform: translateX(250%);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .bar.indeterminate .fill {
      animation: none;
    }
  }
  .curve .avg {
    stroke: var(--control-border);
    stroke-dasharray: 3 3;
    stroke-width: 1;
  }
  .curve .line {
    fill: none;
    stroke: var(--warn);
    stroke-width: 2;
    stroke-linejoin: round;
  }
  .tabs {
    display: flex;
    gap: 18px;
    border-bottom: 1px solid var(--border);
  }
  .tab {
    height: 34px;
    border: 0;
    border-bottom: 2px solid transparent;
    background: transparent;
    color: var(--muted);
    font-size: 13px;
    font-weight: 500;
    padding: 0 4px;
  }
  .tab.on {
    color: var(--fg);
    border-bottom-color: var(--accent);
  }
  .tones {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .strings {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .str {
    display: flex;
    align-items: center;
    gap: 14px;
    height: 46px;
    padding: 0 12px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface);
  }
  .str.playing {
    background: var(--accent-soft);
    border-color: var(--accent);
  }
  .num {
    width: 14px;
    font-size: 12.5px;
  }
  .note {
    width: 40px;
    font-size: 20px;
    font-weight: 600;
  }
  .str.playing .note {
    color: var(--accent);
  }
  .detail {
    flex: 1;
    min-width: 0;
    font-size: 13px;
    color: var(--muted);
  }
  .str.playing .detail {
    color: var(--fg);
  }
  .level {
    fill: var(--accent);
    flex: none;
  }
  .str .btn {
    flex: none;
    min-width: 68px;
    justify-content: center;
  }
  .opts {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px 18px;
    padding-top: 4px;
  }
  .opts .seg button {
    flex: none;
  }
  .chk {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12.5px;
  }
  .chk input {
    accent-color: var(--accent);
    margin: 0;
  }
</style>
