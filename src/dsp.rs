//! Sound generators: drum voices, subtractive, wavetable, FM, plucked string, organ and sampler voices.

use crate::project::{AudioData, Env, Instrument, KitStyle, SynthParams, Wave, drum_index};
use std::f32::consts::{PI, TAU};
use std::sync::Arc;

/// Small xorshift noise generator (no external dependency needed).
#[derive(Clone)]
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        Rng(seed.wrapping_mul(2_654_435_761).max(1))
    }

    /// Uniform noise in -1..=1.
    pub fn next_f32(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

fn mtof(pitch: f32) -> f32 {
    440.0 * 2f32.powf((pitch - 69.0) / 12.0)
}

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Attack,
    Decay,
    Sustain,
    Release,
}

/// ADSR envelope with a hold time (note length) before release.
#[derive(Clone)]
pub struct Envelope {
    stage: Stage,
    level: f32,
    rel_rate: f32,
    hold: u32,
    done: bool,
}

impl Envelope {
    pub fn new(hold_samples: u32) -> Self {
        Self { stage: Stage::Attack, level: 0.0, rel_rate: 0.0, hold: hold_samples.max(1), done: false }
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Advances one sample and returns the envelope level.
    pub fn tick(&mut self, e: Env, sr: f32) -> f32 {
        match self.stage {
            Stage::Attack => {
                self.level += 1.0 / (e.attack.max(0.001) * sr);
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.level -= (1.0 - e.sustain) / (e.decay.max(0.001) * sr);
                if self.level <= e.sustain {
                    self.level = e.sustain;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => {}
            Stage::Release => {
                self.level -= self.rel_rate;
                if self.level <= 0.0 {
                    self.level = 0.0;
                    self.done = true;
                }
            }
        }
        if self.stage != Stage::Release {
            if self.hold > 0 {
                self.hold -= 1;
            } else {
                self.stage = Stage::Release;
                self.rel_rate = self.level / (e.release.max(0.005) * sr);
            }
        }
        self.level
    }
}

fn synth_env(p: &SynthParams) -> Env {
    Env { attack: p.attack, decay: p.decay, sustain: p.sustain, release: p.release }
}

/// Precomputed state-variable filter coefficients.
pub struct FilterCoef {
    f: f32,
    q: f32,
}

impl FilterCoef {
    pub fn new(cutoff: f32, resonance: f32, sr: f32) -> Self {
        // Chamberlin SVF is only stable for modest cutoffs relative to the sample rate.
        let fc = cutoff.clamp(20.0, sr * 0.16);
        Self {
            f: 2.0 * (PI * fc / sr).sin(),
            q: (2.0 - 1.9 * resonance.clamp(0.0, 1.0)).max(0.1),
        }
    }

    fn apply(&self, x: f32, low: &mut f32, band: &mut f32) -> f32 {
        *low += self.f * *band;
        let high = x - *low - self.q * *band;
        *band += self.f * high;
        *low
    }
}

/// One-shot drum sound. `kind` indexes `project::DRUM_KIT`; kinds 8 and 9 are metronome clicks.
pub struct DrumVoice {
    kind: u8,
    t: f32,
    amp: f32,
    phase: f32,
    lp: f32,
    rate: f32,
    pitch: f32,
    noise: f32,
    done: bool,
}

impl DrumVoice {
    pub fn new(kind: u8, amp: f32, tone: (f32, f32, f32)) -> Self {
        let (rate, pitch, noise) = tone;
        Self { kind, t: 0.0, amp, phase: 0.0, lp: 0.0, rate, pitch, noise, done: false }
    }

    pub fn next(&mut self, sr: f32, rng: &mut Rng) -> f32 {
        let t = self.t;
        self.t += 1.0 / sr;
        let r = self.rate;
        let p = self.pitch;
        let n = self.noise;
        let noise = rng.next_f32();
        self.lp += 0.25 * (noise - self.lp);
        let hp = (noise - self.lp) * n; // high-passed noise, scaled by the kit's noise gain

        let sample = match self.kind {
            0 => {
                // Kick: falling sine plus a click
                let f = (45.0 + 120.0 * (-t * 28.0).exp()) * p;
                self.phase = (self.phase + f / sr).fract();
                (TAU * self.phase).sin() * (-t * 6.5 * r).exp() + hp * 0.12 * (-t * 250.0).exp()
            }
            1 => (TAU * 185.0 * p * t).sin() * (-t * 22.0 * r).exp() * 0.5 + hp * (-t * 16.0 * r).exp() * 0.7,
            2 => {
                // Clap: a few noise bursts followed by a tail
                let mut env = 0.0;
                for k in 0..3 {
                    let d = t - k as f32 * 0.012;
                    if d >= 0.0 {
                        env += (-d * 280.0).exp();
                    }
                }
                if t > 0.036 {
                    env += 0.6 * (-(t - 0.036) * 18.0 * r).exp();
                }
                hp * env * 0.8
            }
            3 => hp * (-t * 55.0 * r).exp() * 0.5,
            4 => hp * (-t * 9.0 * r).exp() * 0.45,
            5 => {
                let f = (95.0 + 110.0 * (-t * 9.0).exp()) * p;
                self.phase = (self.phase + f / sr).fract();
                (TAU * self.phase).sin() * (-t * 5.5 * r).exp() * 0.8
            }
            6 => (TAU * 1700.0 * p * t).sin() * (-t * 350.0 * r).exp() * 0.6 + hp * (-t * 400.0).exp() * 0.2,
            7 => ((TAU * 562.0 * p * t).sin() + 0.7 * (TAU * 845.0 * p * t).sin()) * (-t * 13.0 * r).exp() * 0.35,
            8 => (TAU * 1000.0 * t).sin() * (-t * 120.0).exp() * 0.5,
            _ => (TAU * 1500.0 * t).sin() * (-t * 120.0).exp() * 0.6,
        };

        let limit = if self.kind >= 8 { 0.15 } else { 1.6 };
        if t > limit {
            self.done = true;
        }
        sample * self.amp
    }
}

/// Classic subtractive voice: two detuned oscillators, ADSR, low-pass filter.
pub struct SubVoice {
    env: Envelope,
    freq: f32,
    amp: f32,
    phase: f32,
    phase2: f32,
    low: f32,
    band: f32,
}

impl SubVoice {
    pub fn new(pitch: u8, vel: f32, hold: u32) -> Self {
        Self {
            env: Envelope::new(hold),
            freq: mtof(f32::from(pitch)),
            amp: vel * 0.3,
            phase: 0.0,
            phase2: 0.0,
            low: 0.0,
            band: 0.0,
        }
    }

    pub fn next(&mut self, p: &SynthParams, sr: f32, fc: &FilterCoef) -> f32 {
        let level = self.env.tick(synth_env(p), sr);
        let inc = self.freq / sr;
        self.phase = (self.phase + inc).fract();
        self.phase2 = (self.phase2 + inc * (1.0 + p.detune)).fract();
        let osc = 0.5 * (wave_sample(p.wave, self.phase) + wave_sample(p.wave, self.phase2));
        fc.apply(osc, &mut self.low, &mut self.band) * level * self.amp
    }
}

fn wave_sample(wave: Wave, phase: f32) -> f32 {
    match wave {
        Wave::Saw => 2.0 * phase - 1.0,
        Wave::Square => {
            if phase < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        Wave::Triangle => 4.0 * (phase - 0.5).abs() - 1.0,
        Wave::Sine => (TAU * phase).sin(),
    }
}

/// Custom wavetable voice: reads a user-drawn single cycle with linear interpolation.
pub struct WtVoice {
    env: Envelope,
    freq: f32,
    amp: f32,
    phase: f32,
    phase2: f32,
    low: f32,
    band: f32,
}

impl WtVoice {
    pub fn new(pitch: u8, vel: f32, hold: u32) -> Self {
        Self {
            env: Envelope::new(hold),
            freq: mtof(f32::from(pitch)),
            amp: vel * 0.3,
            phase: 0.0,
            phase2: 0.0,
            low: 0.0,
            band: 0.0,
        }
    }

    pub fn next(&mut self, table: &[f32], env: Env, sr: f32, fc: &FilterCoef) -> f32 {
        let level = self.env.tick(env, sr);
        let inc = self.freq / sr;
        self.phase = (self.phase + inc).fract();
        self.phase2 = (self.phase2 + inc * 1.005).fract();
        let osc = 0.5 * (read_table(table, self.phase) + read_table(table, self.phase2));
        fc.apply(osc, &mut self.low, &mut self.band) * level * self.amp
    }
}

fn read_table(table: &[f32], phase: f32) -> f32 {
    let n = table.len();
    if n == 0 {
        return 0.0;
    }
    let x = phase * n as f32;
    let i0 = x as usize % n;
    let frac = x - x.floor();
    table[i0] * (1.0 - frac) + table[(i0 + 1) % n] * frac
}

/// Two-operator FM voice: a sine modulator bends the phase of a sine carrier.
pub struct FmVoice {
    env: Envelope,
    freq: f32,
    amp: f32,
    carrier: f32,
    modulator: f32,
    age: f32,
}

impl FmVoice {
    pub fn new(pitch: u8, vel: f32, hold: u32) -> Self {
        Self {
            env: Envelope::new(hold),
            freq: mtof(f32::from(pitch)),
            amp: vel * 0.35,
            carrier: 0.0,
            modulator: 0.0,
            age: 0.0,
        }
    }

    pub fn next(&mut self, ratio: f32, index: f32, mod_decay: f32, env: Env, sr: f32) -> f32 {
        let level = self.env.tick(env, sr);
        let t = self.age;
        self.age += 1.0 / sr;
        let mod_index = index * (-t * mod_decay).exp();
        self.modulator = (self.modulator + self.freq * ratio / sr).fract();
        self.carrier = (self.carrier + self.freq / sr).fract();
        (TAU * self.carrier + mod_index * (TAU * self.modulator).sin()).sin() * level * self.amp
    }
}

/// Karplus-Strong string: a noise burst circulating in a tuned delay line.
pub struct PluckVoice {
    buf: Vec<f32>,
    idx: usize,
    decay: f32,
    amp: f32,
    age: usize,
    max_age: usize,
}

impl PluckVoice {
    pub fn new(pitch: u8, vel: f32, decay: f32, brightness: f32, sr: f32, rng: &mut Rng) -> Self {
        let len = (sr / mtof(f32::from(pitch))).max(2.0) as usize;
        let mut buf = vec![0.0f32; len];
        let mut y = 0.0f32;
        let smooth = 0.25 + 0.75 * brightness.clamp(0.0, 1.0);
        for v in buf.iter_mut() {
            y += (rng.next_f32() - y) * smooth;
            *v = y;
        }
        Self {
            buf,
            idx: 0,
            decay: decay.clamp(0.9, 0.9999),
            amp: vel * 0.6,
            age: 0,
            max_age: (sr * 4.0) as usize,
        }
    }

    pub fn next(&mut self) -> f32 {
        let len = self.buf.len();
        let a = self.buf[self.idx];
        let b = self.buf[(self.idx + 1) % len];
        self.buf[self.idx] = self.decay * 0.5 * (a + b);
        self.idx = (self.idx + 1) % len;
        self.age += 1;
        a * self.amp
    }

    pub fn is_done(&self) -> bool {
        self.age >= self.max_age
    }
}

const ORGAN_RATIOS: [f32; 9] = [0.5, 1.5, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0];

/// Additive organ: nine sine partials weighted by the drawbars.
pub struct OrganVoice {
    env: Envelope,
    freq: f32,
    amp: f32,
    phases: [f32; 9],
}

impl OrganVoice {
    pub fn new(pitch: u8, vel: f32, hold: u32) -> Self {
        Self { env: Envelope::new(hold), freq: mtof(f32::from(pitch)), amp: vel * 0.25, phases: [0.0; 9] }
    }

    pub fn next(&mut self, drawbars: &[f32; 9], sr: f32) -> f32 {
        let level = self.env.tick(Env { attack: 0.004, decay: 0.0, sustain: 1.0, release: 0.05 }, sr);
        let total: f32 = drawbars.iter().sum::<f32>().max(1e-3);
        let mut out = 0.0;
        for i in 0..9 {
            self.phases[i] = (self.phases[i] + self.freq * ORGAN_RATIOS[i] / sr).fract();
            out += drawbars[i] * (TAU * self.phases[i]).sin();
        }
        out / total * level * self.amp * 2.0
    }
}

/// Plays a stereo sample pitched relative to `root`. Used for samplers and custom drum pads.
pub struct SampleVoice {
    data: Arc<AudioData>,
    pos: f64,
    step: f64,
    amp: f32,
    env: Envelope,
    ended: bool,
}

impl SampleVoice {
    pub fn new(data: Arc<AudioData>, root: u8, pitch: u8, vel: f32, hold: u32, sr: f32) -> Self {
        let ratio = f64::from(data.sample_rate) / f64::from(sr);
        let semis = f64::from(pitch) - f64::from(root);
        Self {
            data,
            pos: 0.0,
            step: ratio * 2f64.powf(semis / 12.0),
            amp: vel * 0.8,
            env: Envelope::new(hold),
            ended: false,
        }
    }

    pub fn next(&mut self, sr: f32) -> [f32; 2] {
        let frames = self.data.samples.len() / 2;
        let i0 = self.pos as usize;
        if i0 + 1 >= frames {
            self.ended = true;
            return [0.0, 0.0];
        }
        let fr = (self.pos - i0 as f64) as f32;
        let s = &self.data.samples;
        let l = s[i0 * 2] + (s[i0 * 2 + 2] - s[i0 * 2]) * fr;
        let r = s[i0 * 2 + 1] + (s[i0 * 2 + 3] - s[i0 * 2 + 1]) * fr;
        let level = self.env.tick(Env { attack: 0.002, decay: 0.0, sustain: 1.0, release: 0.03 }, sr);
        self.pos += self.step;
        [l * level * self.amp, r * level * self.amp]
    }

    pub fn is_done(&self) -> bool {
        self.ended || self.env.is_done()
    }
}

/// Any voice a track can hold.
pub enum NoteVoice {
    Sub(SubVoice),
    Wt(WtVoice),
    Fm(FmVoice),
    Pluck(PluckVoice),
    Organ(OrganVoice),
    Sample(SampleVoice),
    Drum(DrumVoice),
}

impl NoteVoice {
    /// Builds the voice a note should trigger on the given instrument. `None` = nothing to play.
    pub fn new(inst: &Instrument, pitch: u8, vel: f32, hold: u32, sr: f32, rng: &mut Rng) -> Option<Self> {
        Some(match inst {
            Instrument::Subtractive(_) => NoteVoice::Sub(SubVoice::new(pitch, vel, hold)),
            Instrument::Wavetable { .. } => NoteVoice::Wt(WtVoice::new(pitch, vel, hold)),
            Instrument::Fm { .. } => NoteVoice::Fm(FmVoice::new(pitch, vel, hold)),
            Instrument::Pluck { decay, brightness } => {
                NoteVoice::Pluck(PluckVoice::new(pitch, vel, *decay, *brightness, sr, rng))
            }
            Instrument::Organ { .. } => NoteVoice::Organ(OrganVoice::new(pitch, vel, hold)),
            Instrument::Sampler { root, data: Some(d), .. } => {
                NoteVoice::Sample(SampleVoice::new(Arc::clone(d), *root, pitch, vel, hold, sr))
            }
            Instrument::Sampler { data: None, .. } => return None,
            Instrument::DrumKit { style, slots } => {
                let k = drum_index(pitch)?;
                match slots.get(k).and_then(|s| s.data.clone()) {
                    Some(d) => NoteVoice::Sample(SampleVoice::new(d, pitch, pitch, vel, u32::MAX / 2, sr)),
                    None => NoteVoice::Drum(DrumVoice::new(k as u8, vel, style.tone())),
                }
            }
        })
    }

    /// Metronome click or built-in drum sound with no instrument context.
    pub fn click(accent: bool) -> Self {
        NoteVoice::Drum(DrumVoice::new(if accent { 9 } else { 8 }, 1.0, KitStyle::Classic808.tone()))
    }

    /// False when the voice no longer belongs to the track's current instrument.
    pub fn matches(&self, inst: &Instrument) -> bool {
        matches!(
            (self, inst),
            (NoteVoice::Sub(_), Instrument::Subtractive(_))
                | (NoteVoice::Wt(_), Instrument::Wavetable { .. })
                | (NoteVoice::Fm(_), Instrument::Fm { .. })
                | (NoteVoice::Pluck(_), Instrument::Pluck { .. })
                | (NoteVoice::Organ(_), Instrument::Organ { .. })
                | (NoteVoice::Sample(_), _)
                | (NoteVoice::Drum(_), _)
        )
    }

    pub fn is_done(&self) -> bool {
        match self {
            NoteVoice::Sub(v) => v.env.is_done(),
            NoteVoice::Wt(v) => v.env.is_done(),
            NoteVoice::Fm(v) => v.env.is_done(),
            NoteVoice::Pluck(v) => v.is_done(),
            NoteVoice::Organ(v) => v.env.is_done(),
            NoteVoice::Sample(v) => v.is_done(),
            NoteVoice::Drum(v) => v.done,
        }
    }

    /// One stereo frame.
    pub fn next(&mut self, inst: &Instrument, sr: f32, fc: &FilterCoef, rng: &mut Rng) -> [f32; 2] {
        match (self, inst) {
            (NoteVoice::Sub(v), Instrument::Subtractive(p)) => {
                let s = v.next(p, sr, fc);
                [s, s]
            }
            (NoteVoice::Wt(v), Instrument::Wavetable { table, env, .. }) => {
                let s = v.next(table, *env, sr, fc);
                [s, s]
            }
            (NoteVoice::Fm(v), Instrument::Fm { ratio, index, mod_decay, env }) => {
                let s = v.next(*ratio, *index, *mod_decay, *env, sr);
                [s, s]
            }
            (NoteVoice::Pluck(v), _) => {
                let s = v.next();
                [s, s]
            }
            (NoteVoice::Organ(v), Instrument::Organ { drawbars }) => {
                let s = v.next(drawbars, sr);
                [s, s]
            }
            (NoteVoice::Sample(v), _) => v.next(sr),
            (NoteVoice::Drum(v), _) => {
                let s = v.next(sr, rng);
                [s, s]
            }
            _ => [0.0, 0.0],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::wavetable_saw;

    #[test]
    fn kick_is_finite_and_decays() {
        let mut rng = Rng::new(7);
        let mut v = DrumVoice::new(0, 1.0, (1.0, 1.0, 1.0));
        let mut peak = 0.0f32;
        for _ in 0..88200 {
            let s = v.next(44100.0, &mut rng);
            assert!(s.is_finite());
            peak = peak.max(s.abs());
        }
        assert!(peak > 0.3);
        assert!(v.done);
    }

    #[test]
    fn every_engine_produces_finite_audio() {
        let sr = 44100.0;
        let mut rng = Rng::new(3);
        let fc = FilterCoef::new(2000.0, 0.2, sr);
        let instruments = [
            Instrument::default(),
            Instrument::preset("Hollow Table").unwrap(),
            Instrument::preset("Bell").unwrap(),
            Instrument::preset("String").unwrap(),
            Instrument::preset("Jazz Organ").unwrap(),
        ];
        for inst in &instruments {
            let mut v = NoteVoice::new(inst, 60, 1.0, 8000, sr, &mut rng).unwrap();
            let mut peak = 0.0f32;
            for _ in 0..22050 {
                let s = v.next(inst, sr, &fc, &mut rng);
                assert!(s[0].is_finite() && s[1].is_finite());
                peak = peak.max(s[0].abs());
            }
            assert!(peak > 0.01, "{} is silent", inst.label());
        }
    }

    #[test]
    fn wavetable_reads_with_interpolation() {
        let t = wavetable_saw();
        assert!(read_table(&t, 0.25).is_finite());
        assert!((read_table(&[0.0, 1.0], 0.25) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn sampler_plays_then_ends() {
        let data = Arc::new(AudioData::from_stereo(44100, vec![0.5; 2000]));
        let mut v = SampleVoice::new(data, 60, 60, 1.0, 100, 44100.0);
        let mut n = 0;
        while !v.is_done() && n < 10_000 {
            v.next(44100.0);
            n += 1;
        }
        assert!(v.is_done());
    }
}
