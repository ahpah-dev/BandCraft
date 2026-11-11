//! Song data model, instruments, presets and the built-in loop library.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Frames per waveform peak bin (used for drawing audio clips).
pub const PEAK_BIN: usize = 256;
/// Samples in a wavetable.
pub const TABLE_LEN: usize = 256;
/// Number of drawbars on the organ.
pub const DRAWBARS: usize = 9;

/// Drum kit rows: (MIDI pitch used in notes, display name). Index = drum voice kind.
pub const DRUM_KIT: [(u8, &str); 8] = [
    (36, "Kick"),
    (38, "Snare"),
    (39, "Clap"),
    (42, "Closed Hat"),
    (46, "Open Hat"),
    (45, "Tom"),
    (37, "Rim"),
    (56, "Cowbell"),
];

/// Lowest and highest MIDI pitch shown in the note grid.
pub const SYNTH_LOW: u8 = 36;
pub const SYNTH_HIGH: u8 = 72;

pub const CAT_DRUMS: &str = "Drums";
pub const CAT_BASS: &str = "Bass";
pub const CAT_MELODY: &str = "Melody & Chords";
pub const CATEGORIES: [&str; 3] = [CAT_DRUMS, CAT_BASS, CAT_MELODY];

/// Premade instruments: (preset name, musical category). All are synthesized in code,
/// so none of them needs a sample file.
pub const INSTRUMENT_PRESETS: &[(&str, &str)] = &[
    ("Keys", "Keys"),
    ("Soft Keys", "Keys"),
    ("E-Piano", "Keys"),
    ("Wurly", "Keys"),
    ("Bass", "Bass"),
    ("Sub Bass", "Bass"),
    ("Acid Bass", "Bass"),
    ("FM Bass", "Bass"),
    ("Pluck Bass", "Bass"),
    ("Lead", "Leads"),
    ("Square Lead", "Leads"),
    ("Supersaw", "Leads"),
    ("Chiptune", "Leads"),
    ("Flute", "Leads"),
    ("Pad", "Pads"),
    ("Warm Strings", "Pads"),
    ("Soft Pad", "Pads"),
    ("Brass", "Pads"),
    ("Choir", "Pads"),
    ("Pluck", "Plucked"),
    ("String", "Plucked"),
    ("Harp", "Plucked"),
    ("Guitar", "Plucked"),
    ("Kalimba", "Plucked"),
    ("Pizzicato", "Plucked"),
    ("Full Drawbar", "Organs"),
    ("Jazz Organ", "Organs"),
    ("Church Organ", "Organs"),
    ("Rock Organ", "Organs"),
    ("Bell", "Bells & Mallets"),
    ("Glass", "Bells & Mallets"),
    ("Vibraphone", "Bells & Mallets"),
    ("Marimba", "Bells & Mallets"),
    ("Tubular Bell", "Bells & Mallets"),
    ("Saw Table", "Wavetable"),
    ("Hollow Table", "Wavetable"),
    ("Glass Table", "Wavetable"),
    ("Vocal Ahh", "Wavetable"),
    ("Digital Square", "Wavetable"),
];

/// Categories in menu order.
pub const INSTRUMENT_CATEGORIES: [&str; 8] =
    ["Keys", "Bass", "Leads", "Pads", "Plucked", "Organs", "Bells & Mallets", "Wavetable"];

/// Number of track colours in the palette.
pub const TRACK_COLORS: usize = 8;

pub fn drum_index(pitch: u8) -> Option<usize> {
    DRUM_KIT.iter().position(|(p, _)| *p == pitch)
}

pub fn note_name(pitch: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", NAMES[(pitch % 12) as usize], i32::from(pitch) / 12 - 1)
}

/// Parses names like "C4", "F#2", "Bb3" (C4 = MIDI 60).
pub fn note_number(name: &str) -> u8 {
    let bytes = name.as_bytes();
    let mut semis: i32 = match bytes.first() {
        Some(b'C') => 0,
        Some(b'D') => 2,
        Some(b'E') => 4,
        Some(b'F') => 5,
        Some(b'G') => 7,
        Some(b'A') => 9,
        Some(b'B') => 11,
        _ => 0,
    };
    let mut i = 1;
    if bytes.get(i) == Some(&b'#') {
        semis += 1;
        i += 1;
    } else if bytes.get(i) == Some(&b'b') {
        semis -= 1;
        i += 1;
    }
    let octave: i32 = name[i..].parse().unwrap_or(4);
    ((octave + 1) * 12 + semis).clamp(0, 127) as u8
}

/// Samples per beat at a given tempo and sample rate.
pub fn samples_per_beat(bpm: f32, sample_rate: f64) -> f64 {
    60.0 * sample_rate / f64::from(bpm.max(1.0))
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Wave {
    Saw,
    Square,
    Triangle,
    Sine,
}

impl Wave {
    pub const ALL: [Wave; 4] = [Wave::Saw, Wave::Square, Wave::Triangle, Wave::Sine];

    pub fn label(self) -> &'static str {
        match self {
            Wave::Saw => "Saw",
            Wave::Square => "Square",
            Wave::Triangle => "Triangle",
            Wave::Sine => "Sine",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SynthParams {
    pub wave: Wave,
    /// Seconds.
    pub attack: f32,
    /// Seconds.
    pub decay: f32,
    /// 0..=1.
    pub sustain: f32,
    /// Seconds.
    pub release: f32,
    /// Low-pass filter cutoff in Hz.
    pub cutoff: f32,
    /// 0..=1.
    pub resonance: f32,
    /// Detune of the second oscillator (0.006 = 0.6 %).
    #[serde(default = "default_detune")]
    pub detune: f32,
}

fn default_detune() -> f32 {
    0.006
}

impl SynthParams {
    #[allow(clippy::too_many_arguments)]
    fn make(wave: Wave, attack: f32, decay: f32, sustain: f32, release: f32, cutoff: f32, resonance: f32, detune: f32) -> Self {
        Self { wave, attack, decay, sustain, release, cutoff, resonance, detune }
    }

    /// Subtractive presets; unknown names fall back to "Pluck".
    pub fn preset(name: &str) -> Self {
        use Wave::*;
        match name {
            "Pad" => Self::make(Saw, 0.6, 0.8, 0.7, 1.2, 1400.0, 0.1, 0.008),
            "Bass" => Self::make(Square, 0.005, 0.25, 0.6, 0.08, 600.0, 0.35, 0.004),
            "Lead" => Self::make(Saw, 0.01, 0.2, 0.8, 0.25, 4000.0, 0.15, 0.006),
            "Keys" => Self::make(Triangle, 0.01, 0.6, 0.3, 0.4, 3000.0, 0.0, 0.004),
            "Soft Keys" => Self::make(Sine, 0.01, 0.9, 0.15, 0.7, 5000.0, 0.0, 0.002),
            "Sub Bass" => Self::make(Sine, 0.005, 0.3, 0.9, 0.12, 300.0, 0.0, 0.0),
            "Acid Bass" => Self::make(Saw, 0.001, 0.22, 0.15, 0.08, 1100.0, 0.8, 0.002),
            "Square Lead" => Self::make(Square, 0.01, 0.15, 0.8, 0.2, 5000.0, 0.1, 0.007),
            "Supersaw" => Self::make(Saw, 0.02, 0.3, 0.9, 0.4, 6000.0, 0.05, 0.02),
            "Chiptune" => Self::make(Square, 0.001, 0.05, 0.9, 0.04, 7000.0, 0.0, 0.0),
            "Flute" => Self::make(Triangle, 0.08, 0.2, 0.8, 0.2, 3500.0, 0.0, 0.003),
            "Warm Strings" => Self::make(Saw, 0.35, 0.5, 0.8, 0.8, 2000.0, 0.05, 0.012),
            "Soft Pad" => Self::make(Triangle, 0.9, 1.0, 0.8, 1.5, 1800.0, 0.0, 0.01),
            "Brass" => Self::make(Saw, 0.06, 0.3, 0.7, 0.15, 2600.0, 0.2, 0.005),
            _ => Self::make(Saw, 0.001, 0.3, 0.0, 0.12, 2200.0, 0.2, 0.006),
        }
    }
}

impl Default for SynthParams {
    fn default() -> Self {
        Self::preset("Pluck")
    }
}

/// Amplitude envelope shared by the non-subtractive engines.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Env {
    pub attack: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
}

/// Drum kit voicings: each changes the decay length, pitch and noise amount of the built-in sounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KitStyle {
    Classic808,
    Punchy909,
    Acoustic,
    LoFi,
    Trap,
    Electro,
    Rock,
    Jazz,
}

impl KitStyle {
    pub const ALL: [KitStyle; 8] = [
        KitStyle::Classic808,
        KitStyle::Punchy909,
        KitStyle::Acoustic,
        KitStyle::LoFi,
        KitStyle::Trap,
        KitStyle::Electro,
        KitStyle::Rock,
        KitStyle::Jazz,
    ];

    pub fn label(self) -> &'static str {
        match self {
            KitStyle::Classic808 => "808 (long, deep)",
            KitStyle::Punchy909 => "909 (tight, punchy)",
            KitStyle::Acoustic => "Acoustic (natural)",
            KitStyle::LoFi => "Lo-Fi (dusty)",
            KitStyle::Trap => "Trap (boomy, snappy)",
            KitStyle::Electro => "Electro (bright, short)",
            KitStyle::Rock => "Rock (full, natural)",
            KitStyle::Jazz => "Jazz (soft, airy)",
        }
    }

    /// (decay rate multiplier, pitch multiplier, noise gain).
    pub fn tone(self) -> (f32, f32, f32) {
        match self {
            KitStyle::Classic808 => (1.0, 1.0, 1.0),
            KitStyle::Punchy909 => (1.6, 1.15, 1.25),
            KitStyle::Acoustic => (0.7, 0.9, 0.8),
            KitStyle::LoFi => (0.9, 0.85, 0.5),
            KitStyle::Trap => (0.75, 0.8, 1.1),
            KitStyle::Electro => (1.9, 1.3, 1.3),
            KitStyle::Rock => (0.85, 0.95, 1.0),
            KitStyle::Jazz => (0.6, 0.85, 0.6),
        }
    }
}

/// A WAV file assigned to one drum pad. Audio is loaded on demand and never serialized.
#[derive(Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SampleSlot {
    pub path: String,
    #[serde(skip)]
    pub data: Option<Arc<AudioData>>,
}

/// The sound engine of a note track.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub enum Instrument {
    /// Classic two-oscillator subtractive synth.
    Subtractive(SynthParams),
    /// Custom single-cycle waveform drawn by the user, with a filter.
    Wavetable { table: Vec<f32>, env: Env, cutoff: f32, resonance: f32 },
    /// Two-operator FM (bells, electric pianos).
    Fm { ratio: f32, index: f32, mod_decay: f32, env: Env },
    /// Karplus-Strong plucked string.
    Pluck { decay: f32, brightness: f32 },
    /// Additive organ with nine drawbars (16' .. 1').
    Organ { drawbars: [f32; DRAWBARS] },
    /// Any WAV file, pitched across the keyboard from `root`.
    Sampler {
        path: String,
        root: u8,
        #[serde(skip)]
        data: Option<Arc<AudioData>>,
    },
    /// Eight drum pads, each either built in or replaced with a custom WAV.
    DrumKit { style: KitStyle, slots: Vec<SampleSlot> },
}

impl Instrument {
    pub fn default_drum_kit() -> Self {
        Instrument::DrumKit { style: KitStyle::Classic808, slots: vec![SampleSlot::default(); DRUM_KIT.len()] }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Instrument::Subtractive(_) => "Subtractive",
            Instrument::Wavetable { .. } => "Wavetable",
            Instrument::Fm { .. } => "FM",
            Instrument::Pluck { .. } => "Pluck",
            Instrument::Organ { .. } => "Organ",
            Instrument::Sampler { .. } => "Sampler",
            Instrument::DrumKit { .. } => "Drum kit",
        }
    }

    /// Filter (cutoff Hz, resonance) if the engine has one.
    pub fn filter(&self) -> Option<(f32, f32)> {
        match self {
            Instrument::Subtractive(p) => Some((p.cutoff, p.resonance)),
            Instrument::Wavetable { cutoff, resonance, .. } => Some((*cutoff, *resonance)),
            _ => None,
        }
    }

    /// Looks up a built-in preset by name.
    pub fn preset(name: &str) -> Option<Self> {
        let env = |a, d, s, r| Env { attack: a, decay: d, sustain: s, release: r };
        let fm = |ratio, index, mod_decay, e| Instrument::Fm { ratio, index, mod_decay, env: e };
        let pluck = |decay, brightness| Instrument::Pluck { decay, brightness };
        let organ = |drawbars| Instrument::Organ { drawbars };
        let table = |table, e, cutoff, resonance| Instrument::Wavetable { table, env: e, cutoff, resonance };
        Some(match name {
            "Pluck" | "Pad" | "Bass" | "Lead" | "Keys" | "Soft Keys" | "Sub Bass" | "Acid Bass" | "Square Lead"
            | "Supersaw" | "Chiptune" | "Flute" | "Warm Strings" | "Soft Pad" | "Brass" => {
                Instrument::Subtractive(SynthParams::preset(name))
            }
            "E-Piano" => fm(1.0, 1.6, 3.0, env(0.003, 1.2, 0.2, 0.6)),
            "Wurly" => fm(1.0, 2.4, 4.0, env(0.002, 0.9, 0.1, 0.4)),
            "FM Bass" => fm(1.0, 3.5, 6.0, env(0.001, 0.25, 0.5, 0.1)),
            "Bell" => fm(3.5, 5.0, 1.2, env(0.001, 2.5, 0.0, 1.5)),
            "Glass" => fm(2.0, 2.0, 2.0, env(0.002, 1.8, 0.1, 1.0)),
            "Vibraphone" => fm(4.0, 1.2, 2.0, env(0.002, 2.0, 0.0, 1.2)),
            "Marimba" => fm(3.99, 2.5, 8.0, env(0.001, 0.6, 0.0, 0.3)),
            "Tubular Bell" => fm(3.5, 6.0, 0.8, env(0.001, 3.5, 0.0, 2.5)),
            "String" => pluck(0.996, 0.6),
            "Harp" => pluck(0.99, 0.9),
            "Guitar" => pluck(0.995, 0.7),
            "Kalimba" => pluck(0.985, 0.95),
            "Pizzicato" => pluck(0.975, 0.5),
            "Pluck Bass" => pluck(0.993, 0.3),
            "Full Drawbar" => organ([0.6, 0.8, 1.0, 0.8, 0.6, 0.5, 0.4, 0.3, 0.3]),
            "Jazz Organ" => organ([0.9, 0.5, 1.0, 0.2, 0.0, 0.0, 0.0, 0.0, 0.0]),
            "Church Organ" => organ([1.0, 0.2, 0.8, 0.7, 0.4, 0.5, 0.2, 0.3, 0.1]),
            "Rock Organ" => organ([0.8, 0.8, 0.8, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            "Saw Table" => table(wavetable_saw(), env(0.01, 0.4, 0.7, 0.4), 2600.0, 0.2),
            "Hollow Table" => table(wavetable_hollow(), env(0.02, 0.6, 0.6, 0.8), 1800.0, 0.1),
            "Glass Table" => table(
                harmonic_table(&[1.0, 0.7, 0.0, 0.5, 0.0, 0.0, 0.0, 0.3]),
                env(0.01, 1.0, 0.4, 1.0),
                5000.0,
                0.0,
            ),
            "Vocal Ahh" => table(
                harmonic_table(&[1.0, 0.8, 0.9, 1.0, 0.8, 0.4, 0.2, 0.1, 0.1, 0.05]),
                env(0.15, 0.5, 0.8, 0.5),
                3500.0,
                0.1,
            ),
            "Digital Square" => table(
                harmonic_table(&[1.0, 0.0, 0.33, 0.0, 0.2, 0.0, 0.14, 0.0, 0.11]),
                env(0.005, 0.2, 0.8, 0.1),
                6000.0,
                0.0,
            ),
            "Choir" => table(
                harmonic_table(&[1.0, 0.6, 0.8, 0.9, 0.5, 0.3, 0.2, 0.1]),
                env(0.5, 0.6, 0.85, 1.0),
                2800.0,
                0.05,
            ),
            _ => return None,
        })
    }
}

impl Default for Instrument {
    fn default() -> Self {
        Instrument::Subtractive(SynthParams::default())
    }
}

/// Builds one cycle from harmonic weights (index 0 = fundamental), normalized to -1..=1.
pub fn harmonic_table(weights: &[f32]) -> Vec<f32> {
    let mut t = vec![0.0f32; TABLE_LEN];
    for (n, w) in weights.iter().enumerate() {
        if *w == 0.0 {
            continue;
        }
        for (i, v) in t.iter_mut().enumerate() {
            let ph = i as f32 / TABLE_LEN as f32;
            *v += (std::f32::consts::TAU * (n + 1) as f32 * ph).sin() * w;
        }
    }
    normalize(t)
}

pub fn wavetable_saw() -> Vec<f32> {
    let w: Vec<f32> = (1..=24).map(|n| 1.0 / n as f32).collect();
    harmonic_table(&w)
}

pub fn wavetable_hollow() -> Vec<f32> {
    harmonic_table(&[1.0, 0.15, 0.5, 0.0, 0.25])
}

/// A plain sine table (starting point for hand-drawn waveforms).
pub fn wavetable_sine() -> Vec<f32> {
    (0..TABLE_LEN)
        .map(|i| (std::f32::consts::TAU * i as f32 / TABLE_LEN as f32).sin())
        .collect()
}

fn normalize(mut t: Vec<f32>) -> Vec<f32> {
    let peak = t.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-6);
    for v in &mut t {
        *v /= peak;
    }
    t
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Drums,
    Synth,
    Audio,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub pitch: u8,
    /// Beats from the start of the clip.
    pub start: f32,
    /// Beats.
    pub length: f32,
    pub velocity: u8,
}

/// Decoded stereo audio, kept in memory (never serialized).
pub struct AudioData {
    pub sample_rate: u32,
    /// Interleaved L/R samples in -1..=1.
    pub samples: Vec<f32>,
    /// Peak amplitude per `PEAK_BIN` frames, for waveform drawing.
    pub peaks: Vec<f32>,
}

impl AudioData {
    pub fn from_stereo(sample_rate: u32, samples: Vec<f32>) -> Self {
        let peaks = samples
            .chunks(PEAK_BIN * 2)
            .map(|c| c.iter().fold(0.0f32, |m, v| m.max(v.abs())))
            .collect();
        Self { sample_rate, samples, peaks }
    }

    pub fn duration_secs(&self) -> f32 {
        (self.samples.len() / 2) as f32 / self.sample_rate.max(1) as f32
    }
}

impl PartialEq for AudioData {
    fn eq(&self, other: &Self) -> bool {
        self.sample_rate == other.sample_rate && self.samples.len() == other.samples.len()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub enum ClipContent {
    Notes(Vec<Note>),
    Audio {
        path: String,
        #[serde(skip)]
        data: Option<Arc<AudioData>>,
    },
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: u32,
    pub name: String,
    /// Start position on the timeline, in beats.
    pub start: f32,
    /// Pattern length in beats (note clips). Audio clips use their real duration.
    pub length: f32,
    pub content: ClipContent,
}

impl Clip {
    /// Length on the timeline in beats at the given tempo.
    pub fn length_beats(&self, bpm: f32) -> f32 {
        match &self.content {
            ClipContent::Audio { data: Some(d), .. } => d.duration_secs() * bpm / 60.0,
            _ => self.length,
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: u32,
    pub name: String,
    pub kind: TrackKind,
    /// 0..=1.5
    pub volume: f32,
    /// -1 (left) ..= 1 (right)
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
    /// 0..=1 saturation amount.
    pub drive: f32,
    /// Delay time in beats (0 = off).
    pub delay_beats: f32,
    pub delay_feedback: f32,
    pub delay_mix: f32,
    pub instrument: Instrument,
    pub clips: Vec<Clip>,
    /// Palette index (see `theme::track_color`).
    #[serde(default)]
    pub color: u8,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub bpm: f32,
    /// Length of the song in bars (4/4).
    pub bars: u32,
    pub master_volume: f32,
    pub tracks: Vec<Track>,
    pub next_id: u32,
}

impl Project {
    pub fn empty() -> Self {
        Self {
            name: "Untitled".into(),
            bpm: 120.0,
            bars: 8,
            master_volume: 0.9,
            tracks: Vec::new(),
            next_id: 1,
        }
    }

    /// A small starter song so there is something to hear immediately.
    pub fn starter() -> Self {
        let mut p = Self::empty();
        let drums = p.add_track("Drums", TrackKind::Drums, "");
        p.fill_with_loop(drums, &LIBRARY[0]);
        let bass = p.add_track("Bass", TrackKind::Synth, "Bass");
        p.fill_with_loop(bass, &LIBRARY[4]);
        let chords = p.add_track("Chords", TrackKind::Synth, "Keys");
        p.fill_with_loop(chords, &LIBRARY[7]);
        p
    }

    pub fn song_beats(&self) -> f32 {
        self.bars as f32 * 4.0
    }

    pub fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Adds a track. For `Synth` the preset name picks the instrument (falls back to subtractive).
    pub fn add_track(&mut self, name: &str, kind: TrackKind, preset: &str) -> u32 {
        let instrument = match kind {
            TrackKind::Drums => Instrument::default_drum_kit(),
            TrackKind::Synth => Instrument::preset(preset).unwrap_or_default(),
            TrackKind::Audio => Instrument::default(),
        };
        self.add_track_with(name, kind, instrument)
    }

    pub fn add_track_with(&mut self, name: &str, kind: TrackKind, instrument: Instrument) -> u32 {
        let id = self.alloc_id();
        self.tracks.push(Track {
            id,
            name: name.to_string(),
            kind,
            volume: 0.8,
            pan: 0.0,
            mute: false,
            solo: false,
            drive: 0.0,
            delay_beats: 0.0,
            delay_feedback: 0.35,
            delay_mix: 0.3,
            instrument,
            clips: Vec::new(),
            color: (self.tracks.len() % TRACK_COLORS) as u8,
        });
        id
    }

    pub fn track(&self, id: u32) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: u32) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    pub fn remove_track(&mut self, id: u32) {
        self.tracks.retain(|t| t.id != id);
    }

    pub fn remove_clip(&mut self, track_id: u32, clip_id: u32) {
        if let Some(t) = self.track_mut(track_id) {
            t.clips.retain(|c| c.id != clip_id);
        }
    }

    /// Places a copy of a library loop on a track. Returns the new clip id.
    pub fn place_loop(&mut self, track_id: u32, lp: &LoopDef, start: f32) -> u32 {
        let id = self.alloc_id();
        if let Some(track) = self.track_mut(track_id) {
            track.clips.push(Clip {
                id,
                name: lp.name.to_string(),
                start,
                length: lp.beats(),
                content: ClipContent::Notes(lp.notes()),
            });
        }
        id
    }

    /// Repeats a loop back-to-back across the whole song on a track.
    pub fn fill_with_loop(&mut self, track_id: u32, lp: &LoopDef) {
        let len = lp.beats();
        let end = self.song_beats();
        let mut start = 0.0;
        while start < end {
            self.place_loop(track_id, lp, start);
            start += len;
        }
    }
}

/// Note content of a built-in loop.
pub enum LoopContent {
    /// Drum rows: (drum pitch, 16 steps per bar; 'x' = hit, 'X' = accent, '.' = rest).
    Drums(&'static [(u8, &'static str)]),
    /// Melodic notes: (note name, start in sixteenths, length in sixteenths).
    Notes(&'static [(&'static str, u32, u32)]),
}

pub struct LoopDef {
    pub name: &'static str,
    pub category: &'static str,
    pub bars: u32,
    /// Instrument preset suggested when this loop creates a new track.
    pub preset: &'static str,
    pub content: LoopContent,
}

impl LoopDef {
    pub fn beats(&self) -> f32 {
        self.bars as f32 * 4.0
    }

    pub fn kind(&self) -> TrackKind {
        match self.content {
            LoopContent::Drums(_) => TrackKind::Drums,
            LoopContent::Notes(_) => TrackKind::Synth,
        }
    }

    pub fn notes(&self) -> Vec<Note> {
        match self.content {
            LoopContent::Drums(rows) => {
                let mut out = Vec::new();
                for &(pitch, steps) in rows.iter() {
                    for (i, c) in steps.chars().enumerate() {
                        let velocity = match c {
                            'x' => 96,
                            'X' => 127,
                            _ => continue,
                        };
                        out.push(Note { pitch, start: i as f32 * 0.25, length: 0.25, velocity });
                    }
                }
                out
            }
            LoopContent::Notes(list) => list
                .iter()
                .map(|&(name, start, len)| Note {
                    pitch: note_number(name),
                    start: start as f32 * 0.25,
                    length: len as f32 * 0.25,
                    velocity: 100,
                })
                .collect(),
        }
    }
}

pub const LIBRARY: &[LoopDef] = &[
    LoopDef {
        name: "House Four-on-Floor",
        category: CAT_DRUMS,
        bars: 1,
        preset: "",
        content: LoopContent::Drums(&[
            (36, "X...X...X...X..."),
            (39, "....X.......X..."),
            (42, "..x...x...x...x."),
        ]),
    },
    LoopDef {
        name: "Boom Bap",
        category: CAT_DRUMS,
        bars: 1,
        preset: "",
        content: LoopContent::Drums(&[
            (36, "X.....x...x....."),
            (38, "....X.......X..."),
            (42, "x.x.x.x.x.x.x.x."),
        ]),
    },
    LoopDef {
        name: "Trap Rolls",
        category: CAT_DRUMS,
        bars: 1,
        preset: "",
        content: LoopContent::Drums(&[
            (36, "X.........X.X..."),
            (38, "....X.......X..."),
            (42, "x.xxx.xx.xxx.xxx"),
        ]),
    },
    LoopDef {
        name: "Rock Beat",
        category: CAT_DRUMS,
        bars: 1,
        preset: "",
        content: LoopContent::Drums(&[
            (36, "X.......X......."),
            (38, "....X.......X..."),
            (42, "x.x.x.x.x.x.x.x."),
            (46, "..............X."),
        ]),
    },
    LoopDef {
        name: "Root Pulse",
        category: CAT_BASS,
        bars: 1,
        preset: "Bass",
        content: LoopContent::Notes(&[
            ("A1", 0, 2),
            ("A1", 2, 2),
            ("A1", 4, 2),
            ("C2", 6, 2),
            ("A1", 8, 2),
            ("A1", 10, 2),
            ("G1", 12, 2),
            ("E2", 14, 2),
        ]),
    },
    LoopDef {
        name: "Funky Walk",
        category: CAT_BASS,
        bars: 1,
        preset: "Bass",
        content: LoopContent::Notes(&[
            ("C2", 0, 2),
            ("C2", 3, 1),
            ("D#2", 4, 2),
            ("F2", 6, 2),
            ("G2", 8, 2),
            ("G#2", 11, 1),
            ("G2", 12, 2),
            ("F2", 14, 2),
        ]),
    },
    LoopDef {
        name: "Pluck Melody",
        category: CAT_MELODY,
        bars: 2,
        preset: "Pluck",
        content: LoopContent::Notes(&[
            ("E4", 0, 2),
            ("G4", 2, 2),
            ("A4", 4, 4),
            ("G4", 8, 2),
            ("E4", 10, 2),
            ("D4", 12, 4),
            ("C4", 16, 2),
            ("D4", 18, 2),
            ("E4", 20, 4),
            ("G4", 24, 2),
            ("A4", 26, 2),
            ("C5", 28, 4),
        ]),
    },
    LoopDef {
        name: "Chord Stabs",
        category: CAT_MELODY,
        bars: 4,
        preset: "Keys",
        content: LoopContent::Notes(&[
            ("C4", 0, 4), ("E4", 0, 4), ("G4", 0, 4), ("C4", 10, 2), ("E4", 10, 2), ("G4", 10, 2),
            ("G3", 16, 4), ("B3", 16, 4), ("D4", 16, 4), ("G3", 26, 2), ("B3", 26, 2), ("D4", 26, 2),
            ("A3", 32, 4), ("C4", 32, 4), ("E4", 32, 4), ("A3", 42, 2), ("C4", 42, 2), ("E4", 42, 2),
            ("F3", 48, 4), ("A3", 48, 4), ("C4", 48, 4), ("F3", 58, 2), ("A3", 58, 2), ("C4", 58, 2),
        ]),
    },
    LoopDef {
        name: "Warm Pad",
        category: CAT_MELODY,
        bars: 4,
        preset: "Pad",
        content: LoopContent::Notes(&[
            ("C3", 0, 16), ("E3", 0, 16), ("G3", 0, 16),
            ("G2", 16, 16), ("B2", 16, 16), ("D3", 16, 16),
            ("A2", 32, 16), ("C3", 32, 16), ("E3", 32, 16),
            ("F2", 48, 16), ("A2", 48, 16), ("C3", 48, 16),
        ]),
    },
    LoopDef {
        name: "Glass Bells",
        category: CAT_MELODY,
        bars: 2,
        preset: "Bell",
        content: LoopContent::Notes(&[
            ("C5", 0, 4), ("G4", 4, 4), ("E5", 8, 4), ("G4", 12, 4),
            ("A4", 16, 4), ("E5", 20, 4), ("C5", 24, 4), ("G4", 28, 4),
        ]),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_names_round_trip() {
        assert_eq!(note_number("C4"), 60);
        assert_eq!(note_number("A4"), 69);
        assert_eq!(note_number("C#2"), 37);
        assert_eq!(note_number("D#2"), 39);
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(69), "A4");
    }

    #[test]
    fn beats_to_samples() {
        assert!((samples_per_beat(120.0, 44100.0) - 22050.0).abs() < 1e-6);
    }

    #[test]
    fn library_loops_fit_their_bars() {
        for lp in LIBRARY {
            match lp.content {
                LoopContent::Drums(rows) => {
                    for (_, steps) in rows.iter() {
                        assert_eq!(steps.len(), 16 * lp.bars as usize, "row length in {}", lp.name);
                    }
                }
                LoopContent::Notes(list) => {
                    for &(name, start, len) in list.iter() {
                        assert!(start + len <= 16 * lp.bars, "note {name} overruns {}", lp.name);
                    }
                }
            }
        }
    }

    #[test]
    fn every_library_preset_exists() {
        for lp in LIBRARY {
            if !lp.preset.is_empty() {
                assert!(Instrument::preset(lp.preset).is_some(), "missing preset {}", lp.preset);
            }
        }
        for (name, _) in INSTRUMENT_PRESETS {
            assert!(Instrument::preset(name).is_some(), "missing preset {name}");
        }
    }

    #[test]
    fn starter_project_round_trips_through_json() {
        let p = Project::starter();
        let json = serde_json::to_string(&p).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert!(p == back);
        assert_eq!(p.tracks.len(), 3);
        assert_eq!(p.tracks[0].clips.len(), 8);
    }

    #[test]
    fn custom_instruments_serialize() {
        let mut p = Project::empty();
        p.add_track_with("Table", TrackKind::Synth, Instrument::preset("Hollow Table").unwrap());
        p.add_track_with("Organ", TrackKind::Synth, Instrument::preset("Jazz Organ").unwrap());
        let json = serde_json::to_string(&p).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert!(p == back);
    }
}
