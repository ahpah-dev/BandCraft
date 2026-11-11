//! Audio engine: turns a `Project` into interleaved stereo samples.
//!
//! The same `Renderer` drives realtime playback (from the audio callback) and
//! offline export (`render_to_buffer`).

use crate::dsp::{FilterCoef, NoteVoice, Rng};
use crate::project::{AudioData, ClipContent, Instrument, Project, Track, TrackKind, samples_per_beat};
use std::f32::consts::PI;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;

/// Track id used internally for the metronome.
pub const METRONOME_ID: u32 = u32::MAX;
/// Maximum delay line length (also bounds the usable delay time).
pub const DELAY_SECONDS: f32 = 2.0;
const MAX_VOICES: usize = 48;

/// Number of level-meter slots: one per track (by position) plus master left/right.
pub const METER_SLOTS: usize = 66;
pub const MASTER_L: usize = 64;
pub const MASTER_R: usize = 65;

/// Peak levels published by the audio thread for the UI.
pub struct Meters {
    levels: Vec<AtomicU32>,
}

impl Meters {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { levels: (0..METER_SLOTS).map(|_| AtomicU32::new(0)).collect() })
    }

    pub fn set(&self, i: usize, v: f32) {
        if let Some(l) = self.levels.get(i) {
            l.store(v.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn get(&self, i: usize) -> f32 {
        self.levels.get(i).map_or(0.0, |l| f32::from_bits(l.load(Ordering::Relaxed)))
    }
}

/// Messages sent from the UI thread to the audio thread.
pub enum AudioCmd {
    /// Audition a note on a track.
    Preview { track: u32, pitch: u8 },
    /// Live keyboard note: sounds until the matching `NoteOff`.
    NoteOn { track: u32, pitch: u8, vel: u8 },
    NoteOff { track: u32, pitch: u8 },
    SetProject(Arc<Project>),
    Play,
    Stop,
    Seek(u64),
    SetLoop(bool),
    SetMetronome(bool),
}

/// A note-on scheduled at a sample offset inside the current block.
struct Event {
    offset: usize,
    track: u32,
    voice: Option<NoteVoice>,
}

struct TrackState {
    id: u32,
    voices: Vec<NoteVoice>,
    delay_l: Vec<f32>,
    delay_r: Vec<f32>,
    delay_w: usize,
    rng: Rng,
    bus_l: Vec<f32>,
    bus_r: Vec<f32>,
    peak: f32,
}

impl TrackState {
    fn new(id: u32) -> Self {
        Self {
            id,
            voices: Vec::new(),
            delay_l: Vec::new(),
            delay_r: Vec::new(),
            delay_w: 0,
            rng: Rng::new(id.wrapping_add(1)),
            bus_l: Vec::new(),
            bus_r: Vec::new(),
            peak: 0.0,
        }
    }

    fn silence(&mut self) {
        self.voices.clear();
        self.delay_l.fill(0.0);
        self.delay_r.fill(0.0);
    }
}

/// Sample-accurate renderer with transport state.
pub struct Renderer {
    sr: f32,
    sr64: f64,
    /// Playhead position in samples.
    pub pos: u64,
    pub playing: bool,
    pub looping: bool,
    pub metronome: bool,
    tracks: Vec<TrackState>,
    events: Vec<Event>,
    rng: Rng,
}

impl Renderer {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sr: sample_rate as f32,
            sr64: f64::from(sample_rate),
            pos: 0,
            playing: false,
            looping: false,
            metronome: false,
            tracks: vec![TrackState::new(METRONOME_ID)],
            events: Vec::new(),
            rng: Rng::new(12345),
        }
    }

    pub fn play(&mut self, project: &Project) {
        if self.pos >= song_end(project, self.sr64) {
            self.pos = 0;
        }
        self.playing = true;
    }

    /// Plays a short audition note on a track, even when the transport is stopped.
    pub fn preview(&mut self, project: &Project, track_id: u32, pitch: u8) {
        self.sync(project);
        let Some(track) = project.track(track_id) else { return };
        if track.kind == TrackKind::Audio {
            return;
        }
        let hold = (self.sr * 0.35) as u32;
        let voice = NoteVoice::new(&track.instrument, pitch, 0.8, hold, self.sr, &mut self.rng);
        if let Some(v) = voice {
            self.push_voice(track_id, v);
        }
    }

    /// Starts a held keyboard note (released by `note_off`, or after at most 20 seconds).
    pub fn note_on(&mut self, project: &Project, track_id: u32, pitch: u8, vel: u8) {
        self.sync(project);
        let Some(track) = project.track(track_id) else { return };
        if track.kind == TrackKind::Audio {
            return;
        }
        let one_shot = matches!(track.instrument, Instrument::DrumKit { .. });
        let max_hold = (self.sr * 20.0) as u32;
        let hold = if one_shot { (self.sr * 0.35) as u32 } else { max_hold };
        let voice = NoteVoice::new(&track.instrument, pitch, f32::from(vel.max(1)) / 127.0, hold, self.sr, &mut self.rng);
        if let Some(mut v) = voice {
            if !one_shot {
                v.make_live(pitch, max_hold);
            }
            self.push_voice(track_id, v);
        }
    }

    pub fn note_off(&mut self, track_id: u32, pitch: u8) {
        if let Some(rt) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            for v in &mut rt.voices {
                v.release_key(pitch);
            }
        }
    }

    /// Returns and clears the loudest output sample of a track since the last call.
    pub fn take_peak(&mut self, track_id: u32) -> f32 {
        self.tracks
            .iter_mut()
            .find(|t| t.id == track_id)
            .map_or(0.0, |t| std::mem::take(&mut t.peak))
    }

    pub fn stop(&mut self) {
        self.playing = false;
        for t in &mut self.tracks {
            t.silence();
        }
    }

    pub fn seek(&mut self, pos: u64) {
        self.pos = pos;
        for t in &mut self.tracks {
            t.silence();
        }
    }

    /// Renders `out.len() / 2` stereo frames (interleaved L/R).
    pub fn render(&mut self, project: &Project, out: &mut [f32]) {
        self.sync(project);
        let total = out.len() / 2;
        let mut f = 0usize;
        while f < total {
            let spb = samples_per_beat(project.bpm, self.sr64);
            let end = song_end(project, self.sr64);
            if self.playing && self.pos >= end {
                if self.looping {
                    self.pos = 0;
                } else {
                    self.playing = false;
                }
            }
            let n = if self.playing {
                ((end - self.pos) as usize).min(total - f)
            } else {
                total - f
            };
            let start = self.pos;
            self.collect_events(project, start, n, spb);
            self.render_block(project, start, &mut out[f * 2..(f + n) * 2]);
            if self.playing {
                self.pos += n as u64;
            }
            f += n;
        }
    }

    fn sync(&mut self, project: &Project) {
        self.tracks
            .retain(|t| t.id == METRONOME_ID || project.tracks.iter().any(|p| p.id == t.id));
        for p in &project.tracks {
            if !self.tracks.iter().any(|t| t.id == p.id) {
                self.tracks.push(TrackState::new(p.id));
            }
        }
    }

    fn collect_events(&mut self, project: &Project, start: u64, n: usize, spb: f64) {
        self.events.clear();
        if !self.playing {
            return;
        }
        let end = start + n as u64;
        for track in &project.tracks {
            if track.kind == TrackKind::Audio {
                continue;
            }
            for clip in &track.clips {
                let ClipContent::Notes(notes) = &clip.content else {
                    continue;
                };
                for note in notes {
                    if note.start >= clip.length {
                        continue; // trimmed off the end of the clip
                    }
                    let s = ((f64::from(clip.start) + f64::from(note.start)) * spb).round() as u64;
                    if s < start || s >= end {
                        continue;
                    }
                    let vel = f32::from(note.velocity) / 127.0;
                    let hold = ((f64::from(note.length) * spb).round() as u32).max(1);
                    let voice = NoteVoice::new(&track.instrument, note.pitch, vel, hold, self.sr, &mut self.rng);
                    self.events.push(Event { offset: (s - start) as usize, track: track.id, voice });
                }
            }
        }
        if self.metronome {
            let mut k = (start as f64 / spb).ceil() as u64;
            loop {
                let s = (k as f64 * spb).round() as u64;
                if s >= end {
                    break;
                }
                if s >= start {
                    self.events.push(Event {
                        offset: (s - start) as usize,
                        track: METRONOME_ID,
                        voice: Some(NoteVoice::click(k % 4 == 0)),
                    });
                }
                k += 1;
            }
        }
        self.events.sort_by_key(|e| e.offset);
    }

    /// Renders `out` (n frames) starting at absolute sample `start`, splitting at events.
    fn render_block(&mut self, project: &Project, start: u64, out: &mut [f32]) {
        out.fill(0.0);
        let n = out.len() / 2;
        let mut cur = 0usize;
        let mut ei = 0usize;
        while cur < n {
            let next = self.events.get(ei).map_or(n, |e| e.offset);
            if next > cur {
                self.mix(project, start + cur as u64, &mut out[cur * 2..next * 2]);
                cur = next;
            }
            while ei < self.events.len() && self.events[ei].offset == cur {
                let track = self.events[ei].track;
                if let Some(voice) = self.events[ei].voice.take() {
                    self.push_voice(track, voice);
                }
                ei += 1;
            }
        }
    }

    fn push_voice(&mut self, track: u32, voice: NoteVoice) {
        if let Some(rt) = self.tracks.iter_mut().find(|t| t.id == track) {
            if rt.voices.len() >= MAX_VOICES {
                rt.voices.remove(0);
            }
            rt.voices.push(voice);
        }
    }

    /// Mixes all tracks for the absolute range starting at `abs` into `out` (adds to it).
    fn mix(&mut self, project: &Project, abs: u64, out: &mut [f32]) {
        let len = out.len() / 2;
        let sr = self.sr;
        let sr64 = self.sr64;
        let playing = self.playing;
        let spb = samples_per_beat(project.bpm, sr64);
        let any_solo = project.tracks.iter().any(|t| t.solo);

        for track in &project.tracks {
            let Some(rt) = self.tracks.iter_mut().find(|t| t.id == track.id) else {
                continue;
            };
            let audible = !track.mute && (!any_solo || track.solo);
            let gain = if audible { track.volume } else { 0.0 };
            rt.bus_l.clear();
            rt.bus_l.resize(len, 0.0);
            rt.bus_r.clear();
            rt.bus_r.resize(len, 0.0);

            render_voices(rt, &track.instrument, sr, len);

            if playing {
                for clip in &track.clips {
                    if let ClipContent::Audio { data: Some(d), .. } = &clip.content {
                        let clip_start = (f64::from(clip.start) * spb).round() as u64;
                        add_audio(d, clip_start, abs, sr64, &mut rt.bus_l, &mut rt.bus_r);
                    }
                }
            }

            apply_track_fx(rt, track, out, gain, spb, sr);
        }

        // Metronome (not a project track; always audible when enabled)
        if let Some(rt) = self.tracks.iter_mut().find(|t| t.id == METRONOME_ID) {
            rt.bus_l.clear();
            rt.bus_l.resize(len, 0.0);
            rt.bus_r.clear();
            rt.bus_r.resize(len, 0.0);
            render_voices(rt, &Instrument::default(), sr, len);
            for i in 0..len {
                out[2 * i] += rt.bus_l[i] * 0.6;
                out[2 * i + 1] += rt.bus_r[i] * 0.6;
            }
        }
    }
}

fn render_voices(rt: &mut TrackState, inst: &Instrument, sr: f32, len: usize) {
    let (cutoff, resonance) = inst.filter().unwrap_or((8000.0, 0.0));
    let fc = FilterCoef::new(cutoff, resonance, sr);
    rt.voices.retain(|v| v.matches(inst) && !v.is_done());
    for v in rt.voices.iter_mut() {
        for i in 0..len {
            let s = v.next(inst, sr, &fc, &mut rt.rng);
            rt.bus_l[i] += s[0];
            rt.bus_r[i] += s[1];
        }
    }
    rt.voices.retain(|v| !v.is_done());
}

fn add_audio(d: &AudioData, clip_start: u64, abs: u64, sr: f64, bl: &mut [f32], br: &mut [f32]) {
    let frames = d.samples.len() / 2;
    if frames < 2 {
        return;
    }
    // Varispeed read: linear interpolation handles any mismatch between file and engine rates.
    let ratio = f64::from(d.sample_rate) / sr;
    let s = &d.samples;
    for i in 0..bl.len() {
        let t = abs + i as u64;
        if t < clip_start {
            continue;
        }
        let p = (t - clip_start) as f64 * ratio;
        let i0 = p as usize;
        if i0 + 1 >= frames {
            break;
        }
        let fr = (p - i0 as f64) as f32;
        bl[i] += s[i0 * 2] + (s[i0 * 2 + 2] - s[i0 * 2]) * fr;
        br[i] += s[i0 * 2 + 1] + (s[i0 * 2 + 3] - s[i0 * 2 + 1]) * fr;
    }
}

fn apply_track_fx(rt: &mut TrackState, track: &Track, out: &mut [f32], gain: f32, spb: f64, sr: f32) {
    let len = rt.bus_l.len();
    let angle = (track.pan.clamp(-1.0, 1.0) + 1.0) * PI / 4.0;
    let (gl, gr) = (angle.cos() * gain, angle.sin() * gain);

    let delay_on = track.delay_beats > 0.0;
    let delay_len = (DELAY_SECONDS * sr) as usize;
    if delay_on && rt.delay_l.len() != delay_len {
        rt.delay_l = vec![0.0; delay_len];
        rt.delay_r = vec![0.0; delay_len];
        rt.delay_w = 0;
    }
    let d_samples = ((f64::from(track.delay_beats) * spb) as usize).clamp(1, delay_len - 1);
    let fb = track.delay_feedback.clamp(0.0, 0.9);
    let mix = track.delay_mix.clamp(0.0, 1.0);
    let drive = track.drive.clamp(0.0, 1.0);

    for i in 0..len {
        let l = shape(rt.bus_l[i], drive);
        let r = shape(rt.bus_r[i], drive);
        let (mut wl, mut wr) = (0.0, 0.0);
        if delay_on {
            let w = rt.delay_w;
            let read = (w + delay_len - d_samples) % delay_len;
            wl = rt.delay_l[read];
            wr = rt.delay_r[read];
            rt.delay_l[w] = l + wl * fb;
            rt.delay_r[w] = r + wr * fb;
            rt.delay_w = (w + 1) % delay_len;
        }
        let (ol, or) = ((l + wl * mix) * gl, (r + wr * mix) * gr);
        out[2 * i] += ol;
        out[2 * i + 1] += or;
        rt.peak = rt.peak.max(ol.abs()).max(or.abs());
    }
}

/// Soft saturation blended with the clean signal.
fn shape(x: f32, drive: f32) -> f32 {
    if drive <= 0.0 {
        x
    } else {
        x * (1.0 - drive) + (x * (1.0 + 5.0 * drive)).tanh() * drive
    }
}

/// End of the song in samples (never zero).
pub fn song_end(project: &Project, sr: f64) -> u64 {
    (f64::from(project.song_beats()) * samples_per_beat(project.bpm, sr))
        .round()
        .max(1.0) as u64
}

/// Realtime wrapper: drains commands, renders, and publishes the playhead.
pub struct Engine {
    rx: Receiver<AudioCmd>,
    renderer: Renderer,
    project: Arc<Project>,
    pos: Arc<AtomicU64>,
    playing: Arc<AtomicBool>,
    meters: Arc<Meters>,
}

impl Engine {
    pub fn new(
        sample_rate: u32,
        rx: Receiver<AudioCmd>,
        pos: Arc<AtomicU64>,
        playing: Arc<AtomicBool>,
        meters: Arc<Meters>,
    ) -> Self {
        Self {
            meters,
            rx,
            renderer: Renderer::new(sample_rate),
            project: Arc::new(Project::empty()),
            pos,
            playing,
        }
    }

    pub fn process(&mut self, out: &mut [f32]) {
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                AudioCmd::SetProject(p) => self.project = p,
                AudioCmd::Preview { track, pitch } => self.renderer.preview(&self.project, track, pitch),
                AudioCmd::NoteOn { track, pitch, vel } => self.renderer.note_on(&self.project, track, pitch, vel),
                AudioCmd::NoteOff { track, pitch } => self.renderer.note_off(track, pitch),
                AudioCmd::Play => self.renderer.play(&self.project),
                AudioCmd::Stop => self.renderer.stop(),
                AudioCmd::Seek(p) => self.renderer.seek(p),
                AudioCmd::SetLoop(on) => self.renderer.looping = on,
                AudioCmd::SetMetronome(on) => self.renderer.metronome = on,
            }
        }
        self.renderer.render(&self.project, out);
        for (i, t) in self.project.tracks.iter().enumerate().take(MASTER_L) {
            let peak = self.renderer.take_peak(t.id);
            self.meters.set(i, peak);
        }
        let (mut ml, mut mr) = (0.0f32, 0.0f32);
        for frame in out.chunks_exact(2) {
            ml = ml.max(frame[0].abs());
            mr = mr.max(frame[1].abs());
        }
        self.meters.set(MASTER_L, ml);
        self.meters.set(MASTER_R, mr);
        self.pos.store(self.renderer.pos, Ordering::Relaxed);
        self.playing.store(self.renderer.playing, Ordering::Relaxed);
    }
}

/// Offline render of the whole song plus a 3-second tail, as interleaved stereo.
pub fn render_to_buffer(project: &Project, sample_rate: u32) -> Vec<f32> {
    let mut r = Renderer::new(sample_rate);
    r.playing = true;
    let end = song_end(project, f64::from(sample_rate)) as usize;
    let tail = sample_rate as usize * 3;
    let mut out = vec![0.0f32; (end + tail) * 2];
    r.render(project, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{LIBRARY, TrackKind};

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0f32, |m, v| m.max(v.abs()))
    }

    #[test]
    fn empty_project_is_silent() {
        assert_eq!(peak(&render_to_buffer(&Project::empty(), 44100)), 0.0);
    }

    #[test]
    fn drum_loop_makes_sound() {
        let mut p = Project::empty();
        let id = p.add_track("Drums", TrackKind::Drums, "");
        p.place_loop(id, &LIBRARY[0], 0.0);
        let pk = peak(&render_to_buffer(&p, 44100));
        assert!(pk > 0.1 && pk.is_finite(), "peak {pk}");
    }

    #[test]
    fn every_instrument_family_is_audible_in_a_project() {
        for preset in ["Pad", "E-Piano", "String", "Full Drawbar", "Hollow Table"] {
            let mut p = Project::empty();
            let id = p.add_track("T", TrackKind::Synth, preset);
            p.place_loop(id, &LIBRARY[7], 0.0);
            let pk = peak(&render_to_buffer(&p, 44100));
            assert!(pk > 0.01, "{preset} silent");
        }
    }

    #[test]
    fn muted_track_is_silent() {
        let mut p = Project::empty();
        let id = p.add_track("Bass", TrackKind::Synth, "Bass");
        p.place_loop(id, &LIBRARY[4], 0.0);
        p.track_mut(id).unwrap().mute = true;
        assert_eq!(peak(&render_to_buffer(&p, 44100)), 0.0);
    }

    #[test]
    fn export_length_includes_tail() {
        let p = Project::starter();
        let sr = 44100;
        let buf = render_to_buffer(&p, sr);
        let spb = samples_per_beat(p.bpm, f64::from(sr));
        let expected = (f64::from(p.song_beats()) * spb).round() as usize + 3 * sr as usize;
        assert_eq!(buf.len(), expected * 2);
    }

    #[test]
    fn loop_wraps_playhead() {
        let mut p = Project::empty();
        p.bars = 1;
        p.bpm = 120.0;
        let mut r = Renderer::new(1000);
        r.playing = true;
        r.looping = true;
        let mut out = vec![0.0f32; 5000 * 2];
        r.render(&p, &mut out);
        assert_eq!(r.pos, 1000);
        assert!(r.playing);
    }

    #[test]
    fn playhead_stops_at_end_without_loop() {
        let mut p = Project::empty();
        p.bars = 1;
        let mut r = Renderer::new(1000);
        r.playing = true;
        let end = song_end(&p, 1000.0);
        let mut out = vec![0.0f32; (end as usize + 100) * 2];
        r.render(&p, &mut out);
        assert!(!r.playing);
        assert_eq!(r.pos, end);
    }
}
