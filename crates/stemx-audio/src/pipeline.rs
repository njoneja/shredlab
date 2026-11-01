//! The playback pipeline at the song's sample rate: transport (play, pause,
//! seek, loop, end), count-in, mix -> time-stretch/pitch-shift, crossfades and
//! the click track. Shared by the real-time engine (on its producer thread)
//! and the offline renderer. Not real-time itself: it may allocate when
//! settings change, but steady-state rendering does not allocate.

use crate::click::{self, ClickBank, ClickKind, CountInPlan};
use crate::limiter::Limiter;
use crate::mix::{MixMode, Mixer};
use crate::stretch::Stretcher;
use crate::{ClickConfig, Position, Track};

/// Crossfade length for seeks, loop wraps, pause and stretch on/off (~10 ms).
pub const XFADE: usize = 441;
/// Fade-in when playback starts from silence (~1.5 ms; keeps attacks crisp).
pub const START_FADE: usize = 64;
/// Shortest loop accepted (seconds).
pub const MIN_LOOP: f64 = 0.05;

pub const F_PLAYING: u32 = 1;
pub const F_END: u32 = 1 << 1;
pub const F_WRAP: u32 = 1 << 2;
const COUNT_SHIFT: u32 = 3;
const COUNT_MASK: u32 = 0x1fff;
const GEN_SHIFT: u32 = 16;

/// Per-frame metadata: the song time being heard and state flags.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Meta {
    /// Song seconds of this frame.
    pub pos: f64,
    /// [`F_PLAYING`], [`F_END`], [`F_WRAP`], count-in beats left, command generation.
    pub flags: u32,
}

impl Meta {
    pub fn playing(&self) -> bool {
        self.flags & F_PLAYING != 0
    }
    pub fn count_in_left(&self) -> u32 {
        (self.flags >> COUNT_SHIFT) & COUNT_MASK
    }
    pub fn gen(&self) -> u16 {
        (self.flags >> GEN_SHIFT) as u16
    }
    pub fn position(&self) -> Position {
        Position {
            seconds: self.pos,
            playing: self.playing(),
            count_in_beats_left: self.count_in_left(),
        }
    }
}

/// Transport plus stretcher for one continuous stretch of playback.
struct Voice {
    stretcher: Stretcher,
    stretching: bool,
    speed: f64,
    /// Next input sample fed to the stretcher (or read, in bypass).
    in_pos: i64,
    /// Song sample position of the next output frame.
    heard: f64,
    lat_in: usize,
    lat_out: usize,
    in_l: Vec<f32>,
    in_r: Vec<f32>,
}

impl Voice {
    fn new(sr: u32) -> Self {
        let stretcher = Stretcher::new(sr);
        let (lat_in, lat_out) = (stretcher.input_latency(), stretcher.output_latency());
        Self {
            stretcher,
            stretching: false,
            speed: 1.0,
            in_pos: 0,
            heard: 0.0,
            lat_in,
            lat_out,
            in_l: Vec::new(),
            in_r: Vec::new(),
        }
    }

    /// Starts output at song sample `pos`. When stretching, primes the
    /// stretcher so the first output frame corresponds exactly to `pos`.
    fn start(&mut self, mixer: &mut Mixer, pos: f64, stretching: bool, speed: f64, semis: f64) {
        self.stretching = stretching;
        self.speed = speed;
        let p = pos.round() as i64;
        if !stretching {
            self.in_pos = p;
            self.heard = p as f64;
            return;
        }
        let st = &mut self.stretcher;
        st.reset();
        st.set_transpose_semitones(semis as f32);
        self.lat_in = st.input_latency();
        self.lat_out = st.output_latency();
        // Pre-roll ending at p + lat_in puts the processing time at p.
        let pre = st.preroll_len();
        let end = p + self.lat_in as i64;
        ensure_len(&mut self.in_l, &mut self.in_r, pre);
        mixer.mix(
            end - pre as i64,
            &mut self.in_l[..pre],
            &mut self.in_r[..pre],
            false,
        );
        st.seek(&self.in_l[..pre], &self.in_r[..pre], speed);
        self.in_pos = end;
        // The first lat_out output frames precede the processing time: discard them.
        self.heard = p as f64 - self.lat_out as f64 * speed;
        let mut dl = vec![0.0; self.lat_out];
        let mut dr = vec![0.0; self.lat_out];
        self.render(mixer, &mut dl, &mut dr);
        self.heard = p as f64;
    }

    /// Renders `l.len()` output frames (overwrites).
    fn render(&mut self, mixer: &mut Mixer, l: &mut [f32], r: &mut [f32]) {
        let n = l.len();
        if !self.stretching {
            mixer.mix(self.in_pos, l, r, true);
            self.in_pos += n as i64;
            self.heard = self.in_pos as f64;
            return;
        }
        let heard_end = self.heard + n as f64 * self.speed;
        let target =
            (heard_end + self.lat_in as f64 + self.lat_out as f64 * self.speed).round() as i64;
        let n_in = (target - self.in_pos).max(0) as usize;
        ensure_len(&mut self.in_l, &mut self.in_r, n_in);
        let (il, ir) = (&mut self.in_l[..n_in], &mut self.in_r[..n_in]);
        mixer.mix(self.in_pos, il, ir, true);
        self.stretcher.process(il, ir, l, r);
        self.in_pos += n_in as i64;
        self.heard = heard_end;
    }

    /// Output frames until the heard position reaches song sample `target`.
    fn frames_until(&self, target: f64) -> usize {
        let step = if self.stretching { self.speed } else { 1.0 };
        ((target - self.heard) / step).ceil().max(0.0) as usize
    }
}

fn ensure_len(l: &mut Vec<f32>, r: &mut Vec<f32>, n: usize) {
    if l.len() < n {
        l.resize(n, 0.0);
        r.resize(n, 0.0);
    }
}

#[derive(Debug, Clone, Copy)]
struct ActiveClick {
    kind: ClickKind,
    gain: f32,
    /// Frames from the start of the current block until the click starts.
    delay: usize,
    /// Next sample index into the click sound.
    idx: usize,
}

const MAX_CLICKS: usize = 16;

enum State {
    Paused,
    CountIn {
        plan: CountInPlan,
        /// Count-in plan in frames.
        interval: f64,
        elapsed: usize,
        /// Clicks already triggered.
        next: usize,
        start_secs: f64,
    },
    Playing,
}

/// See the module docs.
pub struct Pipeline {
    mixer: Mixer,
    voice: Voice,
    sr: u32,
    state: State,
    paused_pos: f64,
    speed: f64,
    semis: f64,
    loop_region: Option<(f64, f64)>,
    click: ClickConfig,
    bank: ClickBank,
    clicks: [Option<ActiveClick>; MAX_CLICKS],
    click_enabled: bool,
    tail_l: Vec<f32>,
    tail_r: Vec<f32>,
    tail_pos: usize,
    fade_in_pos: usize,
    fade_in_len: usize,
    gen: u16,
    pending_flags: u32,
    fade_on_start: bool,
    /// Playback safety limiter on the music (before the clicks), if enabled.
    limiter: Option<Limiter>,
    /// Count-in clicks still due after the audio started (frames since the
    /// audio start, accent), when a pickup or intro plays under them.
    lead: Vec<(usize, bool)>,
    lead_next: usize,
    lead_elapsed: usize,
}

impl Pipeline {
    /// `realtime == false` (offline render) disables the click track (see
    /// [`Self::set_click_track`]) and the start fade so a unity, unstretched
    /// render is bit-exact.
    pub fn new(tracks: Vec<Track>, gains: &[(String, f32)], realtime: bool) -> Self {
        let mixer = Mixer::new(tracks, gains);
        let sr = mixer.sample_rate();
        Self {
            voice: Voice::new(sr),
            mixer,
            sr,
            state: State::Paused,
            paused_pos: 0.0,
            speed: 1.0,
            semis: 0.0,
            loop_region: None,
            click: ClickConfig::default(),
            bank: ClickBank::new(sr),
            clicks: [None; MAX_CLICKS],
            click_enabled: realtime,
            tail_l: vec![0.0; XFADE],
            tail_r: vec![0.0; XFADE],
            tail_pos: XFADE,
            fade_in_pos: 0,
            fade_in_len: 0,
            gen: 0,
            lead: Vec::with_capacity(64),
            lead_next: 0,
            lead_elapsed: 0,
            pending_flags: 0,
            fade_on_start: realtime,
            limiter: None,
        }
    }

    /// Enables the playback safety limiter ([`crate::limiter`]) on the music.
    /// Clicks and count-in are mixed after it (delayed by the limiter's
    /// latency so they stay in sync). Off by default and never used by the
    /// offline render, which has its own limiter at encode time.
    pub fn set_limiter(&mut self, on: bool) {
        self.limiter = on.then(|| Limiter::new(self.sr));
    }

    /// Turns the click track on or off independently of [`ClickConfig::enabled`]
    /// (on for real-time pipelines; the offline render turns it on to export
    /// the click). The count-in follows `count_in_beats` either way.
    pub fn set_click_track(&mut self, on: bool) {
        self.click_enabled = on;
    }

    pub fn sample_rate(&self) -> u32 {
        self.sr
    }

    pub fn duration(&self) -> f64 {
        self.mixer.len() as f64 / self.sr as f64
    }

    pub fn is_playing(&self) -> bool {
        !matches!(self.state, State::Paused)
    }

    fn stretching(&self) -> bool {
        (self.speed - 1.0).abs() > 1e-9 || self.semis.abs() > 1e-9
    }

    /// Current producer-side position (ahead of what is heard by the output buffer).
    pub fn position(&self) -> Position {
        match &self.state {
            State::Paused => Position {
                seconds: self.paused_pos,
                playing: false,
                count_in_beats_left: 0,
            },
            State::CountIn {
                plan,
                next,
                start_secs,
                ..
            } => Position {
                seconds: *start_secs,
                playing: true,
                count_in_beats_left: plan.count() - *next as u32,
            },
            State::Playing => Position {
                seconds: (self.voice.heard / self.sr as f64).max(0.0),
                playing: true,
                count_in_beats_left: 0,
            },
        }
    }

    pub fn set_gen(&mut self, gen: u16) {
        self.gen = gen;
    }

    /// Replaces the tracks, keeping the position (crossfaded when playing).
    pub fn load(&mut self, tracks: Vec<Track>) {
        self.load_with_original(tracks, None);
    }

    /// Like [`load`](Self::load), with the original mix for
    /// [`MixMode::Original`] (see [`crate::mix`]).
    pub fn load_with_original(&mut self, tracks: Vec<Track>, original: Option<Track>) {
        let sr = tracks
            .first()
            .or(original.as_ref())
            .map_or(self.sr, |t| t.sample_rate);
        if sr != self.sr {
            // Different rate: rebuild everything rate-dependent and stop.
            let pos = self.position().seconds;
            self.mixer.load_with_original(tracks, original, false);
            if self.limiter.is_some() {
                self.limiter = Some(Limiter::new(sr));
            }
            self.sr = sr;
            self.voice = Voice::new(sr);
            self.bank = ClickBank::new(sr);
            self.clicks = [None; MAX_CLICKS];
            self.tail_pos = XFADE;
            self.state = State::Paused;
            self.paused_pos = pos.min(self.duration());
            return;
        }
        let playing = self.is_playing();
        self.mixer.load_with_original(tracks, original, playing);
        let dur = self.duration();
        if self.paused_pos > dur {
            self.paused_pos = dur;
        }
    }

    pub fn set_gains(&mut self, gains: Vec<(String, f32)>) {
        self.mixer.set_gains(gains);
    }

    /// Gains and mix mode together (one control message, no glitch between).
    pub fn set_mix(&mut self, gains: Vec<(String, f32)>, mode: MixMode) {
        self.mixer.set_gains(gains);
        self.mixer.set_mode(mode);
    }

    pub fn set_click(&mut self, cfg: ClickConfig) {
        self.click = cfg;
    }

    pub fn set_loop(&mut self, region: Option<(f64, f64)>) {
        self.loop_region = region.and_then(|(a, b)| {
            let a = a.max(0.0);
            (a.is_finite() && b.is_finite() && b - a >= MIN_LOOP).then_some((a, b))
        });
    }

    pub fn set_speed(&mut self, ratio: f64) {
        let ratio = if ratio.is_finite() {
            ratio.clamp(0.5, 1.5)
        } else {
            1.0
        };
        self.change_params(ratio, self.semis);
    }

    pub fn set_pitch(&mut self, semitones: f64) {
        let s = if semitones.is_finite() {
            semitones.clamp(-12.5, 12.5)
        } else {
            0.0
        };
        self.change_params(self.speed, s);
    }

    fn change_params(&mut self, speed: f64, semis: f64) {
        let was = self.stretching();
        let old_semis = self.semis;
        self.speed = speed;
        self.semis = semis;
        let now = self.stretching();
        match self.state {
            State::Paused => {}
            State::CountIn { start_secs, .. } => {
                let p = start_secs * self.sr as f64;
                self.voice.start(&mut self.mixer, p, now, speed, semis);
            }
            State::Playing => {
                if was != now {
                    let pos = self.voice.heard;
                    self.transition(pos);
                } else if now {
                    self.voice.speed = speed;
                    if semis != old_semis {
                        self.voice.stretcher.set_transpose_semitones(semis as f32);
                    }
                }
            }
        }
    }

    /// Starts playback (see the crate docs for the rules). Returns the new position.
    pub fn play(&mut self) -> Position {
        if self.is_playing() || self.mixer.is_empty() {
            return self.position();
        }
        let start = resolve_play_start(self.paused_pos, self.duration(), self.loop_region);
        self.paused_pos = start;
        let p = start * self.sr as f64;
        let stretching = self.stretching();
        self.voice
            .start(&mut self.mixer, p, stretching, self.speed, self.semis);
        self.lead.clear();
        match click::plan_count_in(&self.click, start, self.speed) {
            Some(plan) if (plan.audio_start() * self.sr as f64).round() < 1.0 => {
                self.set_lead(&plan, 0, 0.0);
                self.begin_audio();
            }
            Some(plan) => {
                let interval = plan.interval * self.sr as f64;
                self.state = State::CountIn {
                    plan,
                    interval,
                    elapsed: 0,
                    next: 0,
                    start_secs: start,
                };
            }
            None => self.begin_audio(),
        }
        self.position()
    }

    /// Keeps count-in clicks `from..` to fire after the audio starts at wall
    /// time `audio_at` (frames).
    fn set_lead(&mut self, plan: &CountInPlan, from: usize, audio_at: f64) {
        self.lead.clear();
        self.lead_next = 0;
        self.lead_elapsed = 0;
        let sr = self.sr as f64;
        for j in from..plan.count() as usize {
            let t = (plan.click_time(j) * sr - audio_at).round().max(0.0) as usize;
            self.lead.push((t, plan.accents[j]));
        }
    }

    fn begin_audio(&mut self) {
        self.state = State::Playing;
        self.fade_in_pos = 0;
        self.fade_in_len = if self.fade_on_start { START_FADE } else { 0 };
    }

    /// Pauses (fading out); aborts a count-in. Returns the new position.
    pub fn pause(&mut self) -> Position {
        match self.state {
            State::Paused => {}
            State::CountIn { start_secs, .. } => {
                self.paused_pos = start_secs;
                self.state = State::Paused;
            }
            State::Playing => {
                self.lead.clear();
                self.paused_pos = (self.voice.heard / self.sr as f64).clamp(0.0, self.duration());
                self.begin_tail();
                self.state = State::Paused;
            }
        }
        self.position()
    }

    /// Seeks (clamped to [0, duration]); crossfades when playing.
    pub fn seek(&mut self, seconds: f64) -> Position {
        let s = if seconds.is_finite() {
            seconds.clamp(0.0, self.duration())
        } else {
            0.0
        };
        match &mut self.state {
            State::Paused => self.paused_pos = s,
            State::CountIn { start_secs, .. } => {
                *start_secs = s;
                self.paused_pos = s;
                let st = self.stretching();
                self.voice.start(
                    &mut self.mixer,
                    s * self.sr as f64,
                    st,
                    self.speed,
                    self.semis,
                );
            }
            State::Playing => {
                self.lead.clear();
                self.transition(s * self.sr as f64);
            }
        }
        self.position()
    }

    /// Captures a fading-out tail of the current voice, to be mixed under what follows.
    fn begin_tail(&mut self) {
        let mut l = [0.0f32; XFADE];
        let mut r = [0.0f32; XFADE];
        self.voice.render(&mut self.mixer, &mut l, &mut r);
        for i in 0..XFADE {
            let g = 1.0 - i as f32 / XFADE as f32;
            let old = self.tail_pos + i;
            let (ol, or) = if old < XFADE {
                (self.tail_l[old], self.tail_r[old])
            } else {
                (0.0, 0.0)
            };
            l[i] = l[i] * g + ol;
            r[i] = r[i] * g + or;
        }
        self.tail_l.copy_from_slice(&l);
        self.tail_r.copy_from_slice(&r);
        self.tail_pos = 0;
    }

    /// Crossfade to song sample `pos` while playing (seek, loop wrap, stretch on/off).
    fn transition(&mut self, pos: f64) {
        self.begin_tail();
        let st = self.stretching();
        self.voice
            .start(&mut self.mixer, pos, st, self.speed, self.semis);
        self.fade_in_pos = 0;
        self.fade_in_len = XFADE;
    }

    fn mix_tail(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = (XFADE - self.tail_pos).min(l.len());
        for i in 0..n {
            l[i] += self.tail_l[self.tail_pos + i];
            r[i] += self.tail_r[self.tail_pos + i];
        }
        self.tail_pos += n;
    }

    fn trigger(&mut self, kind: ClickKind, delay: usize, gain: f32) {
        if gain <= 0.0 {
            return;
        }
        let lat = if self.limiter.is_some() {
            Limiter::latency()
        } else {
            0
        };
        let c = ActiveClick {
            kind,
            gain,
            delay: delay + lat,
            idx: 0,
        };
        // Free slot, else replace the oldest (largest idx).
        let slot = self
            .clicks
            .iter()
            .position(Option::is_none)
            .unwrap_or_else(|| {
                (0..MAX_CLICKS)
                    .max_by_key(|&i| self.clicks[i].map_or(0, |c| c.idx))
                    .unwrap_or(0)
            });
        self.clicks[slot] = Some(c);
    }

    /// Adds the active clicks (mono) to `l` and, if given, `r`.
    fn mix_clicks(&mut self, l: &mut [f32], mut r: Option<&mut [f32]>) {
        let n = l.len();
        for slot in self.clicks.iter_mut() {
            let Some(c) = slot else { continue };
            let snd = self.bank.get(c.kind);
            let start = c.delay.min(n);
            let avail = snd.len() - c.idx;
            let m = (n - start).min(avail);
            for i in 0..m {
                let s = snd[c.idx + i] * c.gain;
                l[start + i] += s;
                if let Some(r) = r.as_deref_mut() {
                    r[start + i] += s;
                }
            }
            c.idx += m;
            c.delay = c.delay.saturating_sub(n);
            if c.idx >= snd.len() {
                *slot = None;
            }
        }
    }

    fn flags(&mut self, playing: bool, count_left: u32) -> u32 {
        let f = (playing as u32 * F_PLAYING)
            | (count_left.min(COUNT_MASK) << COUNT_SHIFT)
            | ((self.gen as u32) << GEN_SHIFT);
        f | std::mem::take(&mut self.pending_flags)
    }

    /// Renders `l.len()` frames (overwrites `l`, `r`, `meta`): the music
    /// (through the safety limiter, if on) plus the clicks.
    pub fn render(&mut self, l: &mut [f32], r: &mut [f32], meta: &mut [Meta]) {
        self.render_music(l, r, meta);
        if let Some(lim) = &mut self.limiter {
            lim.process(l, r);
        }
        self.mix_clicks(l, Some(r));
    }

    /// Like [`Self::render`], but the clicks (click track and count-in, mono)
    /// go to `click` (overwritten) instead of being added to the music, and
    /// there must be no safety limiter (clicks would be delayed by its
    /// latency). For the offline render, which limits the music on its own
    /// before adding the clicks.
    pub fn render_split(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        click: &mut [f32],
        meta: &mut [Meta],
    ) {
        debug_assert!(click.len() == l.len() && self.limiter.is_none());
        self.render_music(l, r, meta);
        click.fill(0.0);
        self.mix_clicks(click, None);
    }

    /// Music, transport and click scheduling (clicks are triggered, not mixed).
    fn render_music(&mut self, l: &mut [f32], r: &mut [f32], meta: &mut [Meta]) {
        let n = l.len();
        debug_assert!(r.len() == n && meta.len() == n);
        let sr = self.sr as f64;
        let mut off = 0;
        while off < n {
            match std::mem::replace(&mut self.state, State::Paused) {
                State::Paused => {
                    l[off..].fill(0.0);
                    r[off..].fill(0.0);
                    let pos = self.paused_pos;
                    let f = self.flags(false, 0);
                    meta[off] = Meta { pos, flags: f };
                    let f = f & !(F_END | F_WRAP);
                    for m in &mut meta[off + 1..] {
                        *m = Meta { pos, flags: f };
                    }
                    let (tl, tr) = (&mut l[off..], &mut r[off..]);
                    self.mix_tail(tl, tr);
                    off = n;
                }
                State::CountIn {
                    plan,
                    interval,
                    mut elapsed,
                    mut next,
                    start_secs,
                } => {
                    let count = plan.count();
                    let audio_at = (plan.audio_start() * sr).round() as usize;
                    let mut m = (n - off).min(audio_at.saturating_sub(elapsed));
                    let e0 = elapsed;
                    let mut fired = [(0usize, false); 8];
                    let mut nf = 0;
                    // Clicks due in [e0, e0 + m), at most 8 per sub-block.
                    while (next as u32) < count {
                        let t = (plan.click_time(next) * sr).round() as usize;
                        if t >= e0 + m {
                            break;
                        }
                        if nf == fired.len() {
                            m = t - e0;
                            break;
                        }
                        fired[nf] = (t - e0, plan.accents[next]);
                        nf += 1;
                        next += 1;
                    }
                    elapsed += m;
                    let done = elapsed >= audio_at;
                    l[off..off + m].fill(0.0);
                    r[off..off + m].fill(0.0);
                    let fired_before = next - nf;
                    for i in 0..m {
                        let k = fired_before + fired[..nf].iter().filter(|f| f.0 <= i).count();
                        let fl = self.flags(true, count - k as u32);
                        meta[off + i] = Meta {
                            pos: start_secs,
                            flags: fl,
                        };
                    }
                    let vol = self.click.volume;
                    for &(d, accent) in &fired[..nf] {
                        let kind = if accent {
                            ClickKind::Accent
                        } else {
                            ClickKind::Beat
                        };
                        self.trigger(kind, off + d, vol);
                    }
                    let (tl, tr) = (&mut l[off..off + m], &mut r[off..off + m]);
                    self.mix_tail(tl, tr);
                    off += m;
                    if done {
                        self.set_lead(&plan, next, audio_at as f64);
                        self.begin_audio();
                    } else {
                        self.state = State::CountIn {
                            plan,
                            interval,
                            elapsed,
                            next,
                            start_secs,
                        };
                    }
                }
                State::Playing => {
                    self.state = State::Playing;
                    let len = self.mixer.len() as f64;
                    let mut m = n - off;
                    let mut wrap_to = None;
                    if let Some((a, b)) = self.loop_region {
                        let bs = (b * sr).min(len);
                        if self.voice.heard < bs {
                            let k = self.voice.frames_until(bs);
                            if k <= m {
                                m = k;
                                wrap_to = Some(a * sr);
                            }
                        }
                    }
                    let k_end = self.voice.frames_until(len);
                    let end = wrap_to.is_none() && k_end <= m;
                    if end {
                        m = k_end;
                    }
                    let h0 = self.voice.heard;
                    let step = if self.voice.stretching {
                        self.voice.speed
                    } else {
                        1.0
                    };
                    {
                        let (vl, vr) = (&mut l[off..off + m], &mut r[off..off + m]);
                        self.voice.render(&mut self.mixer, vl, vr);
                        if self.fade_in_pos < self.fade_in_len {
                            let len = self.fade_in_len;
                            for i in 0..m.min(len - self.fade_in_pos) {
                                let g = (self.fade_in_pos + i) as f32 / len as f32;
                                vl[i] *= g;
                                vr[i] *= g;
                            }
                            self.fade_in_pos += m;
                        }
                    }
                    // Count-in clicks still due over the pickup/intro.
                    let (le, lead_from) = (self.lead_elapsed, self.lead_next);
                    while let Some(&(t, accent)) = self.lead.get(self.lead_next) {
                        if t >= le + m {
                            break;
                        }
                        let kind = if accent {
                            ClickKind::Accent
                        } else {
                            ClickKind::Beat
                        };
                        let vol = self.click.volume;
                        self.trigger(kind, off + t.saturating_sub(le), vol);
                        self.lead_next += 1;
                    }
                    self.lead_elapsed += m;
                    let mut k = lead_from;
                    for i in 0..m {
                        while k < self.lead_next && self.lead[k].0 <= le + i {
                            k += 1;
                        }
                        let fl = self.flags(true, (self.lead.len() - k) as u32);
                        meta[off + i] = Meta {
                            pos: ((h0 + i as f64 * step) / sr).max(0.0),
                            flags: fl,
                        };
                    }
                    if self.click_enabled && self.click.enabled && m > 0 {
                        let (t0, t1) = (h0 / sr, (h0 + m as f64 * step) / sr);
                        let mut due = [(0usize, ClickKind::Beat); 32];
                        let mut nd = 0;
                        click::clicks_in_range(&self.click, t0, t1, |t, kind| {
                            if nd < due.len() {
                                let d = (((t - t0) * sr / step).round() as usize).min(m - 1);
                                due[nd] = (d, kind);
                                nd += 1;
                            }
                        });
                        let vol = self.click.volume;
                        for &(d, kind) in &due[..nd] {
                            let g = match kind {
                                ClickKind::Sub => vol * 0.8,
                                _ => vol,
                            };
                            self.trigger(kind, off + d, g);
                        }
                    }
                    let (tl, tr) = (&mut l[off..off + m], &mut r[off..off + m]);
                    self.mix_tail(tl, tr);
                    off += m;
                    if let Some(a) = wrap_to {
                        self.transition(a);
                        self.pending_flags |= F_WRAP;
                    } else if end {
                        self.paused_pos = self.duration();
                        self.begin_tail();
                        self.state = State::Paused;
                        self.pending_flags |= F_END;
                    }
                }
            }
        }
    }
}

/// Where `play` starts: at or past the end restarts from 0; with a loop, a
/// position outside `[a, b)` jumps to `a`.
pub fn resolve_play_start(pos: f64, duration: f64, loop_region: Option<(f64, f64)>) -> f64 {
    let mut p = pos.clamp(0.0, duration);
    if p >= duration - 1e-6 {
        p = 0.0;
    }
    if let Some((a, b)) = loop_region {
        if p < a || p >= b {
            p = a.min(duration);
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const SR: usize = 44_100;

    /// A track whose sample value encodes its own index (ramp), for position checks.
    fn ramp_track(len: usize) -> Track {
        let v: Vec<f32> = (0..len).map(|i| i as f32 / len as f32).collect();
        Track {
            name: "mix".into(),
            channels: [Arc::new(v.clone()), Arc::new(v)],
            sample_rate: SR as u32,
        }
    }

    fn unity() -> Vec<(String, f32)> {
        vec![("mix".into(), 1.0)]
    }

    fn run(p: &mut Pipeline, frames: usize, block: usize) -> (Vec<f32>, Vec<f32>, Vec<Meta>) {
        let (mut l, mut r, mut m) = (vec![], vec![], vec![]);
        let mut bl = vec![0.0; block];
        let mut br = vec![0.0; block];
        let mut bm = vec![Meta::default(); block];
        let mut done = 0;
        while done < frames {
            let k = block.min(frames - done);
            p.render(&mut bl[..k], &mut br[..k], &mut bm[..k]);
            l.extend_from_slice(&bl[..k]);
            r.extend_from_slice(&br[..k]);
            m.extend_from_slice(&bm[..k]);
            done += k;
        }
        (l, r, m)
    }

    #[test]
    fn play_rules() {
        assert_eq!(resolve_play_start(10.0, 10.0, None), 0.0);
        assert_eq!(resolve_play_start(3.0, 10.0, None), 3.0);
        assert_eq!(resolve_play_start(3.0, 10.0, Some((4.0, 6.0))), 4.0);
        assert_eq!(resolve_play_start(6.0, 10.0, Some((4.0, 6.0))), 4.0);
        assert_eq!(resolve_play_start(5.0, 10.0, Some((4.0, 6.0))), 5.0);
        assert_eq!(resolve_play_start(10.0, 10.0, Some((4.0, 6.0))), 4.0);
    }

    #[test]
    fn bypass_offline_is_bit_exact_and_ends() {
        let len = SR + 123;
        let t = ramp_track(len);
        let src = t.channels[0].clone();
        let mut p = Pipeline::new(vec![t], &unity(), false);
        p.play();
        let (l, _, m) = run(&mut p, len + 500, 1000);
        assert_eq!(&l[..len], &src[..]);
        assert!(l[len..].iter().all(|&x| x == 0.0));
        assert!(m[len - 1].playing());
        assert!(!m[len].playing());
        assert_eq!(m.iter().filter(|x| x.flags & F_END != 0).count(), 1);
        assert!(m[len].flags & F_END != 0);
        assert!((m[len].pos - len as f64 / SR as f64).abs() < 1e-9);
        // Play at the end restarts from 0.
        assert_eq!(p.play().seconds, 0.0);
    }

    #[test]
    fn loop_wraps_from_b_to_a_with_crossfade() {
        let len = 4 * SR;
        let mut p = Pipeline::new(vec![ramp_track(len)], &unity(), true);
        p.set_loop(Some((1.0, 1.5)));
        // Outside the loop: play jumps to a.
        p.seek(3.0);
        assert_eq!(p.play().seconds, 1.0);
        let (l, _, m) = run(&mut p, SR + 10, 512); // 1 s = 2 loop passes
        let wraps: Vec<usize> = (0..m.len()).filter(|&i| m[i].flags & F_WRAP != 0).collect();
        assert_eq!(wraps, vec![SR / 2, SR]);
        // Position wraps from just under b to a.
        assert!((m[SR / 2 - 1].pos - (1.5 - 1.0 / SR as f64)).abs() < 1e-6);
        assert!((m[SR / 2].pos - 1.0).abs() < 1e-9);
        // Audio after the crossfade is the ramp value at a.
        let at = |s: f64| s as f32 * SR as f32 / len as f32;
        let i = SR / 2 + XFADE + 10;
        assert!((l[i] - at(1.0 + (XFADE + 10) as f64 / SR as f64)).abs() < 1e-5);
        // During the crossfade: a blend, no jump bigger than the two endpoints.
        assert!(l[SR / 2 + XFADE / 2] < at(1.5) && l[SR / 2 + XFADE / 2] > at(1.0));
        assert!(m.iter().all(|x| x.playing()));
    }

    #[test]
    fn count_in_then_audio_exactly_one_interval_after_last_click() {
        let len = 4 * SR;
        // No gains: the music is silent and only clicks remain.
        let mut p = Pipeline::new(vec![ramp_track(len)], &[], true);
        let beats: Vec<f64> = (0..8).map(|i| i as f64 * 0.5).collect();
        p.set_click(ClickConfig {
            enabled: false,
            beats,
            downbeats: vec![0.0, 2.0],
            subdivision: 1,
            count_in_beats: 3,
            beats_per_bar: 2,
            volume: 1.0,
        });
        p.set_speed(0.8);
        p.seek(1.0);
        let pos = p.play();
        assert!(pos.playing && pos.count_in_beats_left == 3 && pos.seconds == 1.0);
        let interval = (0.5 / 0.8 * SR as f64).round() as usize; // 27563
        let (l, _, m) = run(&mut p, interval * 4, 700);
        // Onsets: rising edges after silence.
        let mut onsets = vec![];
        let mut quiet = 1000;
        for (i, &x) in l.iter().enumerate() {
            if x.abs() > 1e-4 {
                if quiet > 500 {
                    onsets.push(i);
                }
                quiet = 0;
            } else {
                quiet += 1;
            }
        }
        assert_eq!(onsets.len(), 3, "{onsets:?}");
        for (j, &o) in onsets.iter().enumerate() {
            assert!(o.abs_diff(j * interval) <= 2, "{onsets:?}");
        }
        // Click 2 (of 3, 2 per bar) is accented: louder.
        let peak = |a: usize| l[a..a + 1000].iter().fold(0.0f32, |s, x| s.max(x.abs()));
        assert!(peak(onsets[1]) > peak(onsets[0]) * 1.3);
        // Count-down in the metadata, playing throughout.
        assert_eq!(m[0].count_in_left(), 2);
        assert_eq!(m[interval].count_in_left(), 1);
        assert_eq!(m[2 * interval].count_in_left(), 0);
        assert!(m.iter().all(|x| x.playing()));
        // Audio starts exactly one interval after the last click, at the start position.
        let a = (3.0 * 0.5 / 0.8 * SR as f64).round() as usize;
        assert!((m[a - 1].pos - 1.0).abs() < 1e-9 && m[a - 1].count_in_left() == 0);
        assert!((m[a].pos - 1.0).abs() < 1e-9);
        assert!(m[a + 1000].pos > 1.0);
    }

    fn count_in_onsets(p: &mut Pipeline, frames: usize) -> (Vec<usize>, Vec<Meta>) {
        let (l, _, m) = run(p, frames, 700);
        let mut onsets = vec![];
        let mut quiet = 1000;
        for (i, &x) in l.iter().enumerate() {
            if x.abs() > 1e-4 {
                if quiet > 500 {
                    onsets.push(i);
                }
                quiet = 0;
            } else {
                quiet += 1;
            }
        }
        (onsets, m)
    }

    fn lead_in_pipeline(first_beat: f64) -> Pipeline {
        let mut p = Pipeline::new(vec![ramp_track(10 * SR)], &[], true);
        let beats: Vec<f64> = (0..8).map(|i| first_beat + i as f64 * 0.5).collect();
        p.set_click(ClickConfig {
            enabled: false,
            beats,
            downbeats: vec![first_beat],
            subdivision: 1,
            count_in_beats: 4,
            beats_per_bar: 4,
            volume: 1.0,
        });
        p
    }

    #[test]
    fn count_in_lands_on_the_first_beat_after_a_pickup() {
        // First beat 0.8 s into the song: the audio starts under the count-in
        // so that beat lands one interval after the last click.
        let mut p = lead_in_pipeline(0.8);
        p.play();
        let iv = SR / 2;
        let (on, m) = count_in_onsets(&mut p, 3 * SR);
        assert_eq!(on.len(), 4, "{on:?}");
        for (j, &o) in on.iter().enumerate() {
            assert!(o.abs_diff(j * iv) <= 2, "{on:?}");
        }
        let a = (1.2 * SR as f64).round() as usize;
        assert!(m[a - 1].pos == 0.0 && m[a + 100].pos > 0.0);
        // Song time 0.8 (the beat) is reached one interval after click 4.
        let beat = m.iter().position(|x| x.pos >= 0.8).unwrap();
        assert!(beat.abs_diff(on[3] + iv) <= 2, "{beat}");
        // The countdown keeps going over the pickup.
        assert_eq!(m[a + 100].count_in_left(), 1);
        assert_eq!(m[on[3] + 10].count_in_left(), 0);
    }

    #[test]
    fn count_in_comes_in_over_a_long_intro() {
        let mut p = lead_in_pipeline(5.0);
        let pos = p.play();
        assert!(pos.playing && pos.count_in_beats_left == 0);
        let (on, m) = count_in_onsets(&mut p, 6 * SR);
        let want: Vec<usize> = (0..4).map(|j| 3 * SR + j * SR / 2).collect();
        assert_eq!(on.len(), 4, "{on:?}");
        for (o, w) in on.iter().zip(&want) {
            assert!(o.abs_diff(*w) <= 2, "{on:?}");
        }
        assert!(m[1000].pos > 0.0, "audio starts at once");
        assert_eq!(m[1000].count_in_left(), 4);
        // Pausing drops the remaining clicks.
        let mut p = lead_in_pipeline(5.0);
        p.play();
        run(&mut p, SR, 700);
        p.pause();
        p.seek(0.0);
        p.set_click(ClickConfig {
            count_in_beats: 0,
            ..p.click.clone()
        });
        p.play();
        let (on, _) = count_in_onsets(&mut p, 6 * SR);
        assert!(on.is_empty(), "{on:?}");
    }

    #[test]
    fn click_track_follows_song_time_at_any_speed() {
        let len = 6 * SR;
        let mut p = Pipeline::new(vec![ramp_track(len)], &[], true);
        p.set_click(ClickConfig {
            enabled: true,
            beats: vec![1.0, 2.0, 3.0],
            downbeats: vec![1.0],
            subdivision: 2,
            count_in_beats: 0,
            beats_per_bar: 4,
            volume: 0.5,
        });
        p.set_speed(0.5);
        p.seek(0.5);
        p.play();
        let (l, _, m) = run(&mut p, 4 * SR + 100, 512);
        let mut onsets = vec![];
        let mut quiet = 10_000;
        for (i, &x) in l.iter().enumerate() {
            if x.abs() > 1e-4 {
                if quiet > 2000 {
                    onsets.push(i);
                }
                quiet = 0;
            } else {
                quiet += 1;
            }
        }
        // Song times 1.0, 1.5 (sub), 2.0, 2.5 (sub) at half speed: wall 1, 2, 3, 4 s.
        assert_eq!(onsets.len(), 4, "{onsets:?}");
        for (j, &o) in onsets.iter().enumerate() {
            assert!(o.abs_diff((j + 1) * SR) <= 2, "{onsets:?}");
            assert!((m[o].pos - (1.0 + j as f64 * 0.5)).abs() < 1e-3);
        }
    }

    #[test]
    fn stretched_output_is_aligned_with_reported_position() {
        // An impulse at 1.0 s; start at 0.5 s with speed 0.8 and pitch +3.
        let len = 3 * SR;
        let mut v = vec![0.0f32; len];
        for (k, x) in v[SR..SR + 64].iter_mut().enumerate() {
            *x = if k % 2 == 0 { 0.9 } else { -0.9 };
        }
        let t = Track {
            name: "mix".into(),
            channels: [Arc::new(v.clone()), Arc::new(v)],
            sample_rate: SR as u32,
        };
        let mut p = Pipeline::new(vec![t], &unity(), false);
        p.set_speed(0.8);
        p.set_pitch(3.0);
        p.seek(0.5);
        p.play();
        let (l, _, m) = run(&mut p, SR, 512);
        let (imax, _) =
            l.iter().enumerate().fold(
                (0, 0.0f32),
                |b, (i, &x)| if x.abs() > b.1 { (i, x.abs()) } else { b },
            );
        let expect = (0.5 / 0.8 * SR as f64) as usize;
        let err_ms = imax.abs_diff(expect) as f64 * 1000.0 / SR as f64;
        assert!(
            err_ms < 5.0,
            "peak at {imax}, expected {expect} ({err_ms} ms)"
        );
        assert!((m[imax].pos - 1.0).abs() < 0.02, "{}", m[imax].pos);
        // Before the impulse: quiet (pre-roll is not garbage).
        assert!(l[..expect - 4000].iter().all(|x| x.abs() < 0.05));
    }

    #[test]
    fn pause_fades_and_reports_heard_position() {
        let len = 2 * SR;
        let mut p = Pipeline::new(vec![ramp_track(len)], &unity(), true);
        p.play();
        let _ = run(&mut p, 1000, 500);
        let pos = p.pause();
        assert!(!pos.playing);
        assert!((pos.seconds - 1000.0 / SR as f64).abs() < 1e-9);
        let (l, _, m) = run(&mut p, 1000, 500);
        assert!(l[0] > 0.0 && l[XFADE..].iter().all(|&x| x == 0.0));
        assert!(m.iter().all(|x| !x.playing()));
        // Resume continues where it paused.
        assert!((p.play().seconds - pos.seconds).abs() < 1e-12);
    }

    #[test]
    fn load_switches_tracks_seamlessly_keeping_position() {
        let len = 3 * SR;
        let orig = ramp_track(len);
        let mut p = Pipeline::new(vec![orig.clone()], &unity(), true);
        p.set_gains(vec![
            ("mix".into(), 1.0),
            ("a".into(), 1.0),
            ("b".into(), 1.0),
        ]);
        p.play();
        let _ = run(&mut p, SR, 512);
        // Two "stems" that sum to the original.
        let half: Vec<f32> = orig.channels[0].iter().map(|x| x * 0.5).collect();
        let stem = |n: &str| Track {
            name: n.into(),
            channels: [Arc::new(half.clone()), Arc::new(half.clone())],
            sample_rate: SR as u32,
        };
        p.load(vec![stem("a"), stem("b")]);
        let (l, _, m) = run(&mut p, SR, 512);
        assert!((m[0].pos - 1.0).abs() < 1e-9);
        assert!(m.iter().all(|x| x.playing()));
        for (i, &x) in l.iter().enumerate().step_by(97) {
            let want = orig.channels[0][SR + i];
            assert!((x - want).abs() < 1e-5, "at {i}: {x} vs {want}");
        }
    }

    #[test]
    fn speed_switch_between_bypass_and_stretch_keeps_position() {
        let len = 4 * SR;
        let mut p = Pipeline::new(vec![ramp_track(len)], &unity(), true);
        p.play();
        let _ = run(&mut p, SR, 512);
        p.set_speed(1.25);
        let (_, _, m) = run(&mut p, SR, 512);
        assert!((m[0].pos - 1.0).abs() < 1e-6);
        assert!((m[SR - 1].pos - (1.0 + 1.25 * (SR - 1) as f64 / SR as f64)).abs() < 1e-6);
        p.set_speed(1.0);
        let (_, _, m) = run(&mut p, 100, 50);
        assert!((m[0].pos - 2.25).abs() < 1e-3);
    }
}
