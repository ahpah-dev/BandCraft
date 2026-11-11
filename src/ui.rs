//! BandCraft user interface (egui): app state, transport, arrangement and sounds browser.
//! The inspector and piano-roll editor live in `panels.rs`.

use crate::audio::{self, Recorder};
use crate::engine::{self, AudioCmd, MASTER_L, MASTER_R, METER_SLOTS, Meters};
use crate::project::{
    AudioData, CATEGORIES, Clip, ClipContent, DRUM_KIT, INSTRUMENT_CATEGORIES, INSTRUMENT_PRESETS, Instrument, KitStyle,
    LIBRARY, LoopDef, Note, SampleSlot,
    PEAK_BIN, Project, SYNTH_HIGH, SYNTH_LOW, TrackKind, drum_index, samples_per_beat,
};
use crate::theme::{self, Icon, kind_color, mix, track_color};
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Key, Margin, Painter, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};

pub(crate) const HEADER_W: f32 = 232.0;
pub(crate) const RULER_H: f32 = 26.0;
pub(crate) const LANE_H: f32 = 76.0;
pub(crate) const USER_INSTRUMENT_DIR: &str = "instruments";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct ClipRef {
    pub track: u32,
    pub clip: u32,
}

pub(crate) struct ClipDrag {
    pub clip: ClipRef,
    /// Beats between the pointer and the primary clip's start when the drag began.
    pub grab: f32,
    /// Start positions of every selected clip when the drag began.
    pub origins: Vec<(ClipRef, f32)>,
}

pub(crate) struct Marquee {
    pub start: Pos2,
    pub additive: bool,
}

pub(crate) struct Recording {
    pub track_id: u32,
    pub start_beat: f32,
    pub recorder: Recorder,
}

/// Deferred arrangement mutations collected while drawing.
pub(crate) enum Action {
    ClickClip { clip: ClipRef, shift: bool },
    MoveClips(Vec<(ClipRef, f32)>),
    MarqueeFinish { hits: Vec<ClipRef>, additive: bool },
    LaneClick { track: u32, beat: f32 },
    SelectTrack { id: u32, shift: bool },
    DeleteClips(Vec<ClipRef>),
    DeleteTracks,
    AddTrack(TrackKind),
    Seek(f32),
    ToggleMute(u32),
    ToggleSolo(u32),
}

pub struct BandCraftApp {
    pub(crate) project: Project,
    pub(crate) snapshot: Project,
    pub(crate) undo: Vec<Project>,
    pub(crate) redo: Vec<Project>,
    pub(crate) tx: Sender<AudioCmd>,
    pub(crate) pos: Arc<AtomicU64>,
    pub(crate) playing: Arc<AtomicBool>,
    pub(crate) _stream: Option<cpal::Stream>,
    pub(crate) sample_rate: u32,
    pub(crate) audio_error: Option<String>,
    pub(crate) recording: Option<Recording>,
    pub(crate) take_count: u32,
    /// Primary (inspector / editor) selection.
    pub(crate) selected_track: Option<u32>,
    pub(crate) selected_clip: Option<ClipRef>,
    /// Multi-selection.
    pub(crate) track_sel: Vec<u32>,
    pub(crate) clip_sel: Vec<ClipRef>,
    pub(crate) clip_drag: Option<ClipDrag>,
    pub(crate) marquee: Option<Marquee>,
    pub(crate) selected_loop: Option<usize>,
    pub(crate) px_per_beat: f32,
    pub(crate) loop_on: bool,
    pub(crate) metronome_on: bool,
    pub(crate) project_path: String,
    pub(crate) export_path: String,
    pub(crate) import_path: String,
    pub(crate) instrument_name: String,
    pub(crate) user_instruments: Vec<(String, PathBuf)>,
    /// Piano-roll state.
    pub(crate) note_sel: HashSet<usize>,
    pub(crate) note_sel_clip: Option<ClipRef>,
    pub(crate) note_op: Option<crate::roll::NoteOp>,
    pub(crate) tool: crate::roll::Tool,
    pub(crate) snap_div: u32,
    pub(crate) roll_ppb: f32,
    pub(crate) roll_rh: f32,
    pub(crate) note_len: f32,
    pub(crate) scale_idx: usize,
    pub(crate) scale_root: u8,
    pub(crate) roll_hover: f32,
    pub(crate) note_buf: Vec<Note>,
    pub(crate) roll_for_clip: Option<ClipRef>,
    pub(crate) meters: Arc<Meters>,
    pub(crate) meter_disp: Vec<f32>,
    pub(crate) sounds_tab: u8,
    pub(crate) instrument_search: String,
    pub(crate) editor_focus: bool,
    pub(crate) status: String,
}

impl BandCraftApp {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let pos = Arc::new(AtomicU64::new(0));
        let playing = Arc::new(AtomicBool::new(false));
        let meters = Meters::new();
        let (stream, sample_rate, audio_error) =
            match audio::start_output(rx, Arc::clone(&pos), Arc::clone(&playing), Arc::clone(&meters)) {
                Ok((stream, sr)) => (Some(stream), sr, None),
                Err(e) => (None, 44100, Some(e)),
            };
        let project = Project::starter();
        let mut app = Self {
            snapshot: project.clone(),
            project,
            undo: Vec::new(),
            redo: Vec::new(),
            tx,
            pos,
            playing,
            _stream: stream,
            sample_rate,
            audio_error,
            recording: None,
            take_count: 0,
            selected_track: None,
            selected_clip: None,
            track_sel: Vec::new(),
            clip_sel: Vec::new(),
            clip_drag: None,
            marquee: None,
            selected_loop: Some(0),
            px_per_beat: 26.0,
            loop_on: true,
            metronome_on: false,
            project_path: "my_song.bandcraft.json".into(),
            export_path: "my_song.wav".into(),
            import_path: String::new(),
            instrument_name: "My Instrument".into(),
            user_instruments: Vec::new(),
            note_sel: HashSet::new(),
            note_sel_clip: None,
            note_op: None,
            tool: crate::roll::Tool::Draw,
            snap_div: 4,
            roll_ppb: 90.0,
            roll_rh: 16.0,
            note_len: 0.25,
            scale_idx: 0,
            scale_root: 0,
            roll_hover: 0.0,
            note_buf: Vec::new(),
            roll_for_clip: None,
            meters,
            meter_disp: vec![0.0; METER_SLOTS],
            sounds_tab: 1,
            instrument_search: String::new(),
            editor_focus: false,
            status: "Pick a loop, then click a track lane to place it. Shift-click to multi-select.".into(),
        };
        app.refresh_user_instruments();
        app
    }

    // ----- transport helpers ---------------------------------------------------

    pub(crate) fn spb(&self) -> f64 {
        samples_per_beat(self.project.bpm, f64::from(self.sample_rate))
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    pub(crate) fn playhead_beats(&self) -> f32 {
        (self.pos.load(Ordering::Relaxed) as f64 / self.spb()) as f32
    }

    pub(crate) fn playhead_secs(&self) -> f64 {
        self.pos.load(Ordering::Relaxed) as f64 / f64::from(self.sample_rate)
    }

    pub(crate) fn send(&self, cmd: AudioCmd) {
        // If the audio device failed to open the receiver is gone; editing still works.
        let _ = self.tx.send(cmd);
    }

    pub(crate) fn seek_beats(&self, beat: f32) {
        let samples = (f64::from(beat.max(0.0)) * self.spb()).round() as u64;
        self.send(AudioCmd::Seek(samples));
    }

    pub(crate) fn toggle_play(&self) {
        if self.is_playing() {
            self.send(AudioCmd::Stop);
        } else {
            self.send(AudioCmd::Play);
        }
    }

    // ----- recording -------------------------------------------------------------

    pub(crate) fn toggle_record(&mut self) {
        if self.recording.is_some() {
            self.stop_recording();
        } else {
            self.start_recording();
        }
    }

    fn start_recording(&mut self) {
        let recorder = match audio::start_input() {
            Ok(r) => r,
            Err(e) => {
                self.status = format!("Cannot record: {e}");
                return;
            }
        };
        if self.playhead_beats() >= self.project.song_beats() {
            self.seek_beats(0.0);
        }
        self.take_count += 1;
        let name = format!("Recording {}", self.take_count);
        let track_id = self.project.add_track(&name, TrackKind::Audio, "");
        let start_beat = self.playhead_beats();
        self.select_only_track(track_id);
        self.clip_sel.clear();
        self.selected_clip = None;
        let rate = recorder.sample_rate();
        self.recording = Some(Recording { track_id, start_beat, recorder });
        if !self.is_playing() {
            self.send(AudioCmd::Play);
        }
        self.status = format!("Recording at {rate} Hz. Press R or the red button to stop.");
    }

    fn stop_recording(&mut self) {
        let Some(rec) = self.recording.take() else {
            return;
        };
        let data = rec.recorder.finish();
        self.send(AudioCmd::Stop);
        let secs = data.duration_secs();
        if secs < 0.05 {
            self.project.remove_track(rec.track_id);
            self.status = "Recording was empty and was discarded.".into();
            return;
        }
        let clip_id = self.project.alloc_id();
        let length = secs * self.project.bpm / 60.0;
        if let Some(track) = self.project.track_mut(rec.track_id) {
            track.clips.push(Clip {
                id: clip_id,
                name: "Take".into(),
                start: rec.start_beat,
                length,
                content: ClipContent::Audio { path: String::new(), data: Some(Arc::new(data)) },
            });
        }
        self.select_clip(ClipRef { track: rec.track_id, clip: clip_id });
        self.status = format!("Recorded {secs:.1} s. Save the project to keep the audio file.");
    }

    // ----- selection helpers ---------------------------------------------------

    pub(crate) fn select_only_track(&mut self, id: u32) {
        self.selected_track = Some(id);
        self.track_sel = vec![id];
    }

    pub(crate) fn select_clip(&mut self, c: ClipRef) {
        self.clip_sel = vec![c];
        self.selected_clip = Some(c);
        self.selected_track = Some(c.track);
        self.track_sel = vec![c.track];
        self.editor_focus = false;
    }

    pub(crate) fn clip_exists(&self, c: ClipRef) -> bool {
        self.project
            .track(c.track)
            .is_some_and(|t| t.clips.iter().any(|k| k.id == c.clip))
    }

    /// Selected tracks, or the primary track when nothing else is selected.
    pub(crate) fn selected_track_ids(&self) -> Vec<u32> {
        if !self.track_sel.is_empty() {
            self.track_sel.clone()
        } else {
            self.selected_track.into_iter().collect()
        }
    }

    // ----- editing helpers ---------------------------------------------------------

    pub(crate) fn add_track(&mut self, kind: TrackKind) {
        let (base, preset) = match kind {
            TrackKind::Drums => ("Drum Kit", ""),
            TrackKind::Synth => ("Synth", "Lead"),
            TrackKind::Audio => ("Audio", ""),
        };
        let count = self.project.tracks.iter().filter(|t| t.kind == kind).count() + 1;
        let name = if count == 1 { base.to_string() } else { format!("{base} {count}") };
        let id = self.project.add_track(&name, kind, preset);
        self.select_only_track(id);
    }

    pub(crate) fn add_instrument_track(&mut self, name: &str, instrument: Instrument) -> u32 {
        let id = self.project.add_track_with(name, TrackKind::Synth, instrument);
        self.select_only_track(id);
        id
    }

    pub(crate) fn add_drum_kit(&mut self, style: KitStyle) {
        let count = self.project.tracks.iter().filter(|t| t.kind == TrackKind::Drums).count() + 1;
        let name = if count == 1 { "Drum Kit".to_string() } else { format!("Drum Kit {count}") };
        let inst = Instrument::DrumKit { style, slots: vec![SampleSlot::default(); DRUM_KIT.len()] };
        let id = self.project.add_track_with(&name, TrackKind::Drums, inst);
        self.select_only_track(id);
        self.status = format!("Added a {} drum kit", style.label());
    }

    fn add_loop_track(&mut self, idx: usize) {
        let lp: &LoopDef = &LIBRARY[idx];
        let instrument = Instrument::preset(lp.preset).unwrap_or_default();
        let id = if lp.kind() == TrackKind::Drums {
            self.project.add_track(lp.name, TrackKind::Drums, "")
        } else {
            self.project.add_track_with(lp.name, TrackKind::Synth, instrument)
        };
        self.project.fill_with_loop(id, lp);
        self.select_only_track(id);
        self.status = format!("Added '{}' as a new track, filling the song", lp.name);
    }

    fn place_loop(&mut self, track_id: u32, idx: usize, beat: f32) {
        let lp: &LoopDef = &LIBRARY[idx];
        let compatible = self.project.track(track_id).is_some_and(|t| t.kind == lp.kind());
        if !compatible {
            self.status = format!("'{}' needs a {} track", lp.name, kind_label(lp.kind()));
            return;
        }
        let id = self.project.place_loop(track_id, lp, beat.round().max(0.0));
        self.select_clip(ClipRef { track: track_id, clip: id });
    }

    pub(crate) fn import_wav(&mut self, path: &str) {
        match audio::load_wav(path) {
            Ok(data) => {
                let name = Path::new(path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Audio")
                    .to_string();
                let secs = data.duration_secs();
                let track_id = self.project.add_track(&name, TrackKind::Audio, "");
                let clip_id = self.project.alloc_id();
                let length = secs * self.project.bpm / 60.0;
                if let Some(track) = self.project.track_mut(track_id) {
                    track.clips.push(Clip {
                        id: clip_id,
                        name: name.clone(),
                        start: 0.0,
                        length,
                        content: ClipContent::Audio { path: path.to_string(), data: Some(Arc::new(data)) },
                    });
                }
                self.select_clip(ClipRef { track: track_id, clip: clip_id });
                self.status = format!("Imported '{name}' ({secs:.1} s)");
            }
            Err(e) => self.status = e,
        }
    }

    pub(crate) fn apply(&mut self, action: Action) {
        match action {
            Action::ClickClip { clip, shift } => {
                if shift {
                    if let Some(i) = self.clip_sel.iter().position(|c| *c == clip) {
                        self.clip_sel.remove(i);
                    } else {
                        self.clip_sel.push(clip);
                    }
                    self.selected_clip = Some(clip);
                    self.selected_track = Some(clip.track);
                    self.editor_focus = false;
                } else {
                    self.select_clip(clip);
                }
            }
            Action::MoveClips(moves) => {
                for (c, start) in moves {
                    if let Some(clip) = self
                        .project
                        .track_mut(c.track)
                        .and_then(|t| t.clips.iter_mut().find(|k| k.id == c.clip))
                    {
                        clip.start = start;
                    }
                }
            }
            Action::MarqueeFinish { hits, additive } => {
                if additive {
                    for h in hits {
                        if !self.clip_sel.contains(&h) {
                            self.clip_sel.push(h);
                        }
                    }
                } else {
                    self.clip_sel = hits;
                }
                self.selected_clip = self.clip_sel.last().copied();
                if let Some(c) = self.selected_clip {
                    self.selected_track = Some(c.track);
                }
                self.editor_focus = false;
            }
            Action::LaneClick { track, beat } => {
                self.select_only_track(track);
                self.clip_sel.clear();
                self.editor_focus = false;
                match self.selected_loop {
                    Some(idx) => self.place_loop(track, idx, beat),
                    None => self.selected_clip = None,
                }
            }
            Action::SelectTrack { id, shift } => {
                if shift {
                    if let Some(i) = self.track_sel.iter().position(|t| *t == id) {
                        self.track_sel.remove(i);
                    } else {
                        self.track_sel.push(id);
                    }
                    self.selected_track = Some(id);
                } else {
                    self.select_only_track(id);
                }
            }
            Action::DeleteClips(list) => self.delete_clips(&list),
            Action::DeleteTracks => {
                let ids = self.selected_track_ids();
                for id in ids {
                    self.project.remove_track(id);
                }
                self.track_sel.clear();
                self.selected_track = None;
                self.selected_clip = None;
                self.clip_sel.clear();
            }
            Action::AddTrack(kind) => self.add_track(kind),
            Action::Seek(beat) => self.seek_beats(beat),
            Action::ToggleMute(id) => {
                let targets = self.group_for(id);
                let new_value = self.project.track(id).is_some_and(|t| !t.mute);
                for t in targets {
                    if let Some(tr) = self.project.track_mut(t) {
                        tr.mute = new_value;
                    }
                }
            }
            Action::ToggleSolo(id) => {
                let targets = self.group_for(id);
                let new_value = self.project.track(id).is_some_and(|t| !t.solo);
                for t in targets {
                    if let Some(tr) = self.project.track_mut(t) {
                        tr.solo = new_value;
                    }
                }
            }
        }
    }

    /// If a track is part of a multi-selection, mute/solo applies to the whole selection.
    fn group_for(&self, id: u32) -> Vec<u32> {
        if self.track_sel.len() > 1 && self.track_sel.contains(&id) {
            self.track_sel.clone()
        } else {
            vec![id]
        }
    }

    pub(crate) fn delete_clips(&mut self, list: &[ClipRef]) {
        for c in list {
            self.project.remove_clip(c.track, c.clip);
        }
        self.clip_sel.retain(|c| !list.contains(c));
        if self.selected_clip.is_some_and(|c| list.contains(&c)) {
            self.selected_clip = None;
        }
    }

    /// Duplicates every selected clip directly after itself and selects the copies.
    fn duplicate_selection(&mut self) {
        let bpm = self.project.bpm;
        let mut new_sel = Vec::new();
        let sources: Vec<(ClipRef, Clip)> = self
            .clip_sel
            .iter()
            .filter_map(|c| {
                self.project
                    .track(c.track)?
                    .clips
                    .iter()
                    .find(|k| k.id == c.clip)
                    .map(|k| (*c, k.clone()))
            })
            .collect();
        for (c, mut clip) in sources {
            let id = self.project.alloc_id();
            clip.id = id;
            clip.start += clip.length_beats(bpm);
            if let Some(track) = self.project.track_mut(c.track) {
                track.clips.push(clip);
            }
            new_sel.push(ClipRef { track: c.track, clip: id });
        }
        if !new_sel.is_empty() {
            self.selected_clip = new_sel.last().copied();
            self.clip_sel = new_sel;
            self.status = format!("Duplicated {} clip(s)", self.clip_sel.len());
        }
    }

    pub(crate) fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.project, prev));
            self.snapshot = self.project.clone();
            self.status = "Undone".into();
        }
    }

    pub(crate) fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.project, next));
            self.snapshot = self.project.clone();
            self.status = "Redone".into();
        }
    }

    // ----- files -------------------------------------------------------------------

    pub(crate) fn refresh_user_instruments(&mut self) {
        self.user_instruments.clear();
        if let Ok(entries) = std::fs::read_dir(USER_INSTRUMENT_DIR) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "json") {
                    let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or("instrument").to_string();
                    self.user_instruments.push((name, p));
                }
            }
            self.user_instruments.sort_by(|a, b| a.0.cmp(&b.0));
        }
    }

    pub(crate) fn save_instrument(&mut self, inst: &Instrument) {
        let name: String = self
            .instrument_name
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        if name.is_empty() {
            self.status = "Give the instrument a name first".into();
            return;
        }
        let _ = std::fs::create_dir_all(USER_INSTRUMENT_DIR);
        let path = format!("{USER_INSTRUMENT_DIR}/{name}.json");
        match serde_json::to_string_pretty(inst).map_err(|e| e.to_string()).and_then(|j| std::fs::write(&path, j).map_err(|e| e.to_string())) {
            Ok(()) => {
                self.status = format!("Saved instrument to {path}");
                self.refresh_user_instruments();
            }
            Err(e) => self.status = format!("Could not save instrument: {e}"),
        }
    }

    pub(crate) fn load_user_instrument(&mut self, path: &Path) -> Option<Instrument> {
        let json = std::fs::read_to_string(path).ok()?;
        match serde_json::from_str::<Instrument>(&json) {
            Ok(inst) => Some(inst),
            Err(e) => {
                self.status = format!("Not an instrument file: {e}");
                None
            }
        }
    }

    fn save(&mut self) {
        // Recordings only live in memory until saved: write them next to the project.
        let stem = Path::new(&self.project_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("project")
            .to_string();
        let mut failed = None;
        for track in &mut self.project.tracks {
            for clip in &mut track.clips {
                let id = clip.id;
                if let ClipContent::Audio { path, data: Some(d) } = &mut clip.content {
                    if path.is_empty() {
                        let _ = std::fs::create_dir_all("recordings");
                        let file = format!("recordings/{stem}_{id}.wav");
                        match audio::write_wav(&file, &d.samples, d.sample_rate) {
                            Ok(()) => *path = file,
                            Err(e) => failed = Some(e),
                        }
                    }
                }
            }
        }
        if let Some(e) = failed {
            self.status = format!("Could not save a recording: {e}");
            return;
        }
        match serde_json::to_string_pretty(&self.project) {
            Ok(json) => match std::fs::write(&self.project_path, json) {
                Ok(()) => {
                    self.status = format!("Saved {}", self.project_path);
                    self.snapshot = self.project.clone();
                }
                Err(e) => self.status = format!("Save failed: {e}"),
            },
            Err(e) => self.status = format!("Save failed: {e}"),
        }
    }

    fn open(&mut self) {
        let json = match std::fs::read_to_string(&self.project_path) {
            Ok(s) => s,
            Err(e) => {
                self.status = format!("Open failed: {e}");
                return;
            }
        };
        match serde_json::from_str::<Project>(&json) {
            Ok(mut project) => {
                let mut missing = 0;
                for track in &mut project.tracks {
                    for clip in &mut track.clips {
                        if let ClipContent::Audio { path, data } = &mut clip.content {
                            match audio::load_wav(path.as_str()) {
                                Ok(d) => *data = Some(Arc::new(d)),
                                Err(_) => missing += 1,
                            }
                        }
                    }
                    load_instrument_samples(&mut track.instrument, &mut missing);
                }
                self.project = project;
                self.undo.clear();
                self.redo.clear();
                self.snapshot = self.project.clone();
                self.clip_sel.clear();
                self.track_sel.clear();
                self.selected_clip = None;
                self.selected_track = None;
                self.status = if missing > 0 {
                    format!("Opened, but {missing} audio file(s) could not be found")
                } else {
                    format!("Opened {}", self.project_path)
                };
            }
            Err(e) => self.status = format!("Not a BandCraft project: {e}"),
        }
    }

    fn export(&mut self) {
        const EXPORT_RATE: u32 = 44_100;
        let buf = engine::render_to_buffer(&self.project, EXPORT_RATE);
        let secs = buf.len() as f32 / 2.0 / EXPORT_RATE as f32;
        match audio::write_wav(&self.export_path, &buf, EXPORT_RATE) {
            Ok(()) => self.status = format!("Exported {} ({secs:.1} s, 24-bit stereo)", self.export_path),
            Err(e) => self.status = format!("Export failed: {e}"),
        }
    }

    fn new_project(&mut self) {
        self.project = Project::empty();
        self.track_sel.clear();
        self.clip_sel.clear();
        self.selected_track = None;
        self.selected_clip = None;
        self.status = "New project".into();
    }

    // ----- panels --------------------------------------------------------------------

    fn menu_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::MenuBar::new().ui(ui, |ui| {
            if ui.button("Exit").clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ui.menu_button("File", |ui| {
                if ui.button("New project").clicked() {
                    self.new_project();
                    ui.close();
                }
                if ui.button("Open").clicked() {
                    self.open();
                    ui.close();
                }
                if ui.button("Save").clicked() {
                    self.save();
                    ui.close();
                }
                if ui.button("Export WAV").clicked() {
                    self.export();
                    ui.close();
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui.button("Undo    Ctrl+Z").clicked() {
                    self.undo();
                    ui.close();
                }
                if ui.button("Redo    Ctrl+Y").clicked() {
                    self.redo();
                    ui.close();
                }
                if ui.button("Select all    Ctrl+A").clicked() {
                    self.select_all();
                    ui.close();
                }
                if ui.button("Duplicate    Ctrl+D").clicked() {
                    self.duplicate_selection();
                    ui.close();
                }
                if ui.button("Clear selection    Esc").clicked() {
                    self.clear_selection();
                    ui.close();
                }
            });
            ui.menu_button("View", |ui| {
                if ui.button("Zoom in").clicked() {
                    self.px_per_beat = (self.px_per_beat * 1.25).min(80.0);
                    ui.close();
                }
                if ui.button("Zoom out").clicked() {
                    self.px_per_beat = (self.px_per_beat / 1.25).max(8.0);
                    ui.close();
                }
            });
            ui.menu_button("Settings", |ui| {
                ui.label(format!("Output sample rate: {} Hz", self.sample_rate));
                match &self.audio_error {
                    Some(e) => ui.colored_label(egui::Color32::YELLOW, format!("Audio output: {e}")),
                    None => ui.label("Audio output: OK"),
                };
            });
            ui.menu_button("Help", |ui| {
                for line in [
                    "Space: play / pause      R: record",
                    "Click: select   Shift+click: add/remove",
                    "Drag empty space: box select",
                    "Drag clips: move the whole selection",
                    "Ctrl+A select all   Ctrl+D duplicate   Esc clear",
                    "Piano roll: click cell to add a note, drag to box-select",
                    "Piano roll: arrows nudge selected notes, Delete removes",
                    "Right-click a note to delete it",
                ] {
                    ui.label(line);
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.project.name)
                        .desired_width(200.0)
                        .font(FontId::proportional(13.0))
                        .hint_text("Project name"),
                );
            });
        });
    }

    pub(crate) fn select_all(&mut self) {
        if self.editor_focus {
            self.select_all_notes();
        } else {
            self.clip_sel = self
                .project
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter().map(move |c| ClipRef { track: t.id, clip: c.id }))
                .collect();
        }
    }

    pub(crate) fn clear_selection(&mut self) {
        self.clip_sel.clear();
        self.note_sel.clear();
        self.note_sel_clip = None;
        self.editor_focus = false;
    }

    fn transport_ui(&mut self, ui: &mut egui::Ui) {
        let playing = self.is_playing();
        let recording = self.recording.is_some();
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            if theme::icon_button(ui, Icon::Undo, "Undo (Ctrl+Z)", false).clicked() {
                self.undo();
            }
            if theme::icon_button(ui, Icon::Redo, "Redo (Ctrl+Y)", false).clicked() {
                self.redo();
            }
            ui.add_space(6.0);
            if theme::icon_button(ui, Icon::Loop, "Loop playback", self.loop_on).clicked() {
                self.loop_on = !self.loop_on;
                self.send(AudioCmd::SetLoop(self.loop_on));
            }
            if theme::icon_button(ui, Icon::Metronome, "Metronome click", self.metronome_on).clicked() {
                self.metronome_on = !self.metronome_on;
                self.send(AudioCmd::SetMetronome(self.metronome_on));
            }

            ui.add_space(18.0);
            if theme::icon_button(ui, Icon::Rewind, "Back to start", false).clicked() {
                self.seek_beats(0.0);
            }
            let play_icon = if playing { Icon::Pause } else { Icon::Play };
            if theme::round_button(ui, play_icon, theme::ACCENT, "Play / pause (Space)", playing).clicked() {
                self.toggle_play();
            }
            if theme::round_button(ui, Icon::Stop, theme::TEXT, "Stop and rewind", false).clicked() {
                self.send(AudioCmd::Stop);
                self.seek_beats(0.0);
            }
            if theme::round_button(ui, Icon::Record, theme::RECORD, "Record from input (R)", recording).clicked() {
                self.toggle_record();
            }

            ui.add_space(12.0);
            self.clock_ui(ui);
            ui.add_space(8.0);
            chip(ui, |ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("TEMPO").size(9.5).color(theme::TEXT_DIM));
                    ui.visuals_mut().widgets.inactive.weak_bg_fill = theme::WELL;
                    ui.visuals_mut().widgets.inactive.bg_fill = theme::WELL;
                    ui.add(
                        egui::DragValue::new(&mut self.project.bpm)
                            .range(40.0..=240.0)
                            .speed(0.5)
                            .fixed_decimals(0)
                            .suffix(" BPM"),
                    );
                });
            });
            ui.add_space(4.0);
            chip(ui, |ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("METER").size(9.5).color(theme::TEXT_DIM));
                    ui.label(RichText::new("4 / 4").size(14.0).strong().color(theme::TEXT));
                });
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let export = egui::Button::new(RichText::new("Export").strong().color(theme::WINDOW_BG))
                    .fill(theme::ACCENT)
                    .min_size(Vec2::new(86.0, 30.0));
                if ui.add(export).on_hover_text("Render the song to a WAV file").clicked() {
                    self.export();
                }
                if ui.add(egui::Button::new("Save").min_size(Vec2::new(66.0, 30.0))).clicked() {
                    self.save();
                }
                ui.add_space(8.0);
                let (rect, _) = ui.allocate_exact_size(Vec2::new(130.0, 16.0), Sense::hover());
                let id = ui.scope_id().with("master");
theme::fader(ui, id, rect, &mut self.project.master_volume, 0.0, 1.5, theme::ACCENT);
                let (mrect, _) = ui.allocate_exact_size(Vec2::new(60.0, 14.0), Sense::hover());
                for (k, slot) in [MASTER_L, MASTER_R].iter().enumerate() {
                    let level = self.meter_disp[*slot].clamp(0.0, 1.0);
                    let bar = Rect::from_min_size(mrect.left_top() + Vec2::new(0.0, k as f32 * 7.0), Vec2::new(mrect.width(), 5.0));
                    ui.painter().rect_filled(bar, 2.0, theme::WELL);
                    let mut fill = bar;
                    fill.max.x = bar.left() + bar.width() * level;
                    ui.painter().rect_filled(fill, 2.0, mix(Color32::from_rgb(70, 200, 110), Color32::from_rgb(236, 72, 82), level));
                }
                ui.label(RichText::new("MASTER").size(10.0).color(theme::TEXT_DIM));
            });
        });
    }

    fn clock_ui(&self, ui: &mut egui::Ui) {
        let secs = self.playhead_secs();
        let beat = self.playhead_beats();
        let time = format!(
            "{:02}:{:02}.{}",
            (secs / 60.0) as u32,
            (secs % 60.0) as u32,
            ((secs.fract()) * 10.0) as u32
        );
        chip(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(time).monospace().size(20.0).color(theme::TEXT));
                ui.label(
                    RichText::new(format!("Bar {}  Beat {}", (beat / 4.0).floor() as u32 + 1, (beat % 4.0).floor() as u32 + 1))
                        .size(10.0)
                        .color(theme::TEXT_DIM),
                );
            });
        });
    }

    fn status_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(self.status.as_str()).color(theme::TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match &self.audio_error {
                    Some(_) => {
                        ui.colored_label(egui::Color32::YELLOW, "No audio output");
                    }
                    None => {
                        ui.label(RichText::new(format!("{} Hz", self.sample_rate)).small().color(theme::TEXT_DIM));
                    }
                }
                ui.label(RichText::new("Project").small().color(theme::TEXT_DIM));
                ui.add(egui::TextEdit::singleline(&mut self.project_path).desired_width(180.0));
                ui.add(egui::TextEdit::singleline(&mut self.export_path).desired_width(130.0).hint_text("output.wav"));
            });
        });
    }

    fn sounds_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 8.0) / 2.0;
            for (i, label) in ["Loops", "Instruments"].iter().enumerate() {
                let on = self.sounds_tab == i as u8;
                let btn = egui::Button::new(RichText::new(*label).strong().color(if on { theme::WINDOW_BG } else { theme::TEXT_DIM }))
                    .fill(if on { theme::ACCENT } else { theme::SURFACE })
                    .min_size(Vec2::new(w, 30.0));
                if ui.add(btn).clicked() {
                    self.sounds_tab = i as u8;
                }
            }
        });
        ui.add_space(4.0);
        if self.sounds_tab == 0 {
            self.loops_tab(ui);
        } else {
            self.instruments_tab(ui);
        }
    }

    fn instruments_tab(&mut self, ui: &mut egui::Ui) {
        ui.add(
            egui::TextEdit::singleline(&mut self.instrument_search)
                .hint_text("Search instruments")
                .desired_width(ui.available_width() - 14.0),
        );
        ui.label(
            RichText::new("Click to add a track. All sounds are generated live; no sample files are loaded.")
                .small()
                .color(theme::TEXT_DIM),
        );
        let query = self.instrument_search.to_lowercase();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let wants = |name: &str| query.is_empty() || name.to_lowercase().contains(&query);
            ui.add_space(4.0);
            ui.label(RichText::new("DRUM KITS").size(10.0).strong().color(theme::TEXT_DIM));
            for style in KitStyle::ALL {
                let label = style.label();
                if !wants(label) && !wants("drum kit") {
                    continue;
                }
                if sound_card(ui, label, "Drum kit · 8 pads", theme::PALETTE[1]).clicked() {
                    self.add_drum_kit(style);
                }
            }
            for category in INSTRUMENT_CATEGORIES {
                let items: Vec<&str> = INSTRUMENT_PRESETS
                    .iter()
                    .filter(|(n, c)| *c == category && (wants(n) || wants(category)))
                    .map(|(n, _)| *n)
                    .collect();
                if items.is_empty() {
                    continue;
                }
                ui.add_space(6.0);
                ui.label(RichText::new(category.to_uppercase()).size(10.0).strong().color(theme::TEXT_DIM));
                let color = match category {
                    "Keys" => theme::PALETTE[2],
                    "Bass" => theme::PALETTE[0],
                    "Leads" => theme::PALETTE[5],
                    "Pads" => theme::PALETTE[3],
                    "Plucked" => theme::PALETTE[4],
                    "Organs" => theme::PALETTE[6],
                    "Bells & Mallets" => theme::PALETTE[7],
                    _ => theme::PALETTE[1],
                };
                for name in items {
                    let engine = Instrument::preset(name).map_or("", |i| i.label());
                    if sound_card(ui, name, engine, color).clicked() {
                        let inst = Instrument::preset(name).unwrap_or_default();
                        let id = self.add_instrument_track(name, inst);
                        if let Some(t) = self.project.track_mut(id) {
                            t.color = PALETTE_FOR[category_index(category) % PALETTE_FOR.len()];
                        }
                    }
                }
            }
            if wants("sampler") || wants("custom") {
                ui.add_space(6.0);
                ui.label(RichText::new("CUSTOM").size(10.0).strong().color(theme::TEXT_DIM));
                if sound_card(ui, "Sampler", "Load your own WAV when you want it", theme::PALETTE[3]).clicked() {
                    self.add_instrument_track("Sampler", Instrument::Sampler { path: String::new(), root: 60, data: None });
                }
                for (name, path) in self.user_instruments.clone() {
                    if wants(&name) && sound_card(ui, &name, "My instrument", theme::PALETTE[6]).clicked() {
                        if let Some(inst) = self.load_user_instrument(&path) {
                            let mut missing = 0;
                            let mut inst = inst;
                            load_instrument_samples(&mut inst, &mut missing);
                            self.add_instrument_track(&name, inst);
                        }
                    }
                }
            }
        });
    }

    fn loops_tab(&mut self, ui: &mut egui::Ui) {
        theme::caption(ui, "Create");
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 8.0) / 2.0;
            if ui.add(egui::Button::new("+ Drum kit").min_size(Vec2::new(w, 30.0))).clicked() {
                self.add_track(TrackKind::Drums);
            }
            if ui.add(egui::Button::new("+ Synth").min_size(Vec2::new(w, 30.0))).clicked() {
                self.add_track(TrackKind::Synth);
            }
        });

        theme::caption(ui, "Import audio");
        ui.horizontal(|ui| {
            let row_w = ui.available_width();
            ui.add(
                egui::TextEdit::singleline(&mut self.import_path)
                    .hint_text("path to a .wav file")
                    .desired_width((row_w - 96.0).max(60.0)),
            );
            if ui.button("Import").clicked() {
                let path = self.import_path.clone();
                self.import_wav(&path);
            }
        });

        theme::caption(ui, "Loops");
        ui.label(
            RichText::new("Click a loop, then click a lane. Double-click to fill a new track.")
                .small()
                .color(theme::TEXT_DIM),
        );
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for category in CATEGORIES {
                ui.add_space(6.0);
                ui.label(RichText::new(category.to_uppercase()).size(10.0).strong().color(theme::TEXT_DIM));
                for (i, lp) in LIBRARY.iter().enumerate().filter(|(_, lp)| lp.category == category) {
                    let sub = format!("{} · {} bar{}", kind_label(lp.kind()), lp.bars, if lp.bars == 1 { "" } else { "s" });
                    let selected = self.selected_loop == Some(i);
                    let resp = sound_card_sel(ui, lp.name, &sub, kind_color(lp.kind()), selected);
                    if resp.clicked() {
                        self.selected_loop = Some(i);
                    }
                    if resp.double_clicked() {
                        self.add_loop_track(i);
                    }
                }
            }
        });
    }

    fn arrangement_ui(&mut self, ui: &mut egui::Ui) {
        let pointer_down = ui.input(|i| i.pointer.primary_down());
        if !pointer_down {
            self.clip_drag = None;
        }
        let shift = ui.input(|i| i.modifiers.shift);
        let ppb = self.px_per_beat;
        let bpm = self.project.bpm;
        let song_beats = self.project.song_beats();
        let track_count = self.project.tracks.len();
        let content_w = HEADER_W + (song_beats + 16.0) * ppb;
        let content_h = RULER_H + (track_count as f32 + 1.5) * LANE_H;
        let playhead = self.playhead_beats();
        let selected_track = self.selected_track;
        let track_sel = self.track_sel.clone();
        let levels = self.meter_disp.clone();
        let clip_sel = self.clip_sel.clone();
        let recording = self.recording.as_ref().map(|r| (r.track_id, r.start_beat));
        let pointer_pos = ui.input(|i| i.pointer.interact_pos());
        let project = &mut self.project;
        let clip_drag = &mut self.clip_drag;
        let marquee = &mut self.marquee;

        let actions = egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show_viewport(ui, |ui, viewport| {
                let mut actions: Vec<Action> = Vec::new();
                let (rect, _) = ui.allocate_exact_size(Vec2::new(content_w, content_h), Sense::hover());
                let origin = rect.min;
                let p = ui.painter_at(rect);
                let timeline_left = origin.x + HEADER_W;
                let timeline_w = content_w - HEADER_W;
                let beat_x = |b: f32| timeline_left + b * ppb;
                let beat_at = |x: f32| (x - timeline_left) / ppb;
                let lane_top = |i: usize| origin.y + RULER_H + i as f32 * LANE_H;
                let lanes_bottom = lane_top(track_count);
                let song_end_x = beat_x(song_beats);
                let max_beats = (timeline_w / ppb).ceil() as usize;

                // Lane backgrounds
                for (i, track) in project.tracks.iter().enumerate() {
                    let r = Rect::from_min_size(Pos2::new(timeline_left, lane_top(i)), Vec2::new(timeline_w, LANE_H));
                    let bg = if track_sel.contains(&track.id) {
                        theme::LANE_SELECTED
                    } else if i % 2 == 0 {
                        theme::LANE_EVEN
                    } else {
                        theme::LANE_ODD
                    };
                    p.rect_filled(r, 0.0, bg);
                    p.line_segment(
                        [Pos2::new(timeline_left, r.bottom()), Pos2::new(origin.x + content_w, r.bottom())],
                        Stroke::new(1.0, Color32::from_black_alpha(120)),
                    );
                }
                let empty = Rect::from_min_max(
                    Pos2::new(timeline_left, lanes_bottom),
                    Pos2::new(origin.x + content_w, origin.y + content_h),
                );
                p.rect_filled(empty, 0.0, theme::WINDOW_BG);

                for b in 0..=max_beats {
                    let x = beat_x(b as f32);
                    let col = if b % 4 == 0 { theme::GRID_BAR } else { theme::GRID_BEAT };
                    p.line_segment([Pos2::new(x, origin.y + RULER_H), Pos2::new(x, lanes_bottom)], Stroke::new(1.0, col));
                }
                if song_end_x < origin.x + content_w {
                    p.rect_filled(
                        Rect::from_min_max(
                            Pos2::new(song_end_x, origin.y + RULER_H),
                            Pos2::new(origin.x + content_w, lanes_bottom),
                        ),
                        0.0,
                        Color32::from_black_alpha(120),
                    );
                }

                // Lanes: click places/selects, drag starts a box selection (registered before clips).
                for (i, track) in project.tracks.iter().enumerate() {
                    let r = Rect::from_min_size(Pos2::new(timeline_left, lane_top(i)), Vec2::new(timeline_w, LANE_H));
                    let resp = ui.interact(r, ui.scope_id().with(("lane", track.id)), Sense::click_and_drag());
                    if resp.drag_started() {
                        if let Some(pos) = ui.input(|i| i.pointer.press_origin()) {
                            *marquee = Some(Marquee { start: pos, additive: shift });
                        }
                    }
                    if resp.clicked() {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            actions.push(Action::LaneClick { track: track.id, beat: beat_at(pos.x) });
                        }
                    }
                }

                // Clips
                let mut clip_rects: Vec<(ClipRef, Rect)> = Vec::new();
                for (i, track) in project.tracks.iter().enumerate() {
                    for clip in &track.clips {
                        let len = clip.length_beats(bpm);
                        let r = Rect::from_min_size(
                            Pos2::new(beat_x(clip.start), lane_top(i) + 6.0),
                            Vec2::new((len * ppb).max(4.0), LANE_H - 12.0),
                        );
                        let cref = ClipRef { track: track.id, clip: clip.id };
                        let selected = clip_sel.contains(&cref);
                        draw_clip(&p, r, clip, track.kind, track_color(track.color), ppb, bpm, selected);
                        clip_rects.push((cref, r));

                        let resp = ui.interact(r, ui.scope_id().with(("clip", clip.id)), Sense::click_and_drag());
                        if resp.clicked() {
                            actions.push(Action::ClickClip { clip: cref, shift });
                        }
                        if resp.drag_started() {
                            let mut origins = Vec::new();
                            // Dragging an unselected clip (without shift) replaces the selection.
                            let base_sel: Vec<ClipRef> = if clip_sel.contains(&cref) || shift {
                                clip_sel.clone()
                            } else {
                                vec![cref]
                            };
                            if !clip_sel.contains(&cref) && !shift {
                                actions.push(Action::ClickClip { clip: cref, shift: false });
                            }
                            for t in project.tracks.iter() {
                                for k in &t.clips {
                                    let kr = ClipRef { track: t.id, clip: k.id };
                                    if base_sel.contains(&kr) || kr == cref {
                                        origins.push((kr, k.start));
                                    }
                                }
                            }
                            if let Some(pos) = ui.input(|i| i.pointer.press_origin()) {
                                *clip_drag = Some(ClipDrag { clip: cref, grab: beat_at(pos.x) - clip.start, origins });
                            }
                        }
                        if resp.dragged() {
                            if let (Some(d), Some(pos)) = (clip_drag.as_ref(), resp.interact_pointer_pos()) {
                                if d.clip == cref {
                                    let primary_origin = d.origins.iter().find(|(c, _)| *c == cref).map_or(clip.start, |o| o.1);
                                    let new_primary = (beat_at(pos.x) - d.grab).round().max(0.0);
                                    let delta = new_primary - primary_origin;
                                    let moves = d
                                        .origins
                                        .iter()
                                        .map(|(c, s)| (*c, (s + delta).max(0.0)))
                                        .collect();
                                    actions.push(Action::MoveClips(moves));
                                }
                            }
                        }
                        resp.context_menu(|ui| {
                            let n = clip_sel.len().max(1);
                            let label = if n > 1 && clip_sel.contains(&cref) {
                                format!("Delete {n} clips")
                            } else {
                                "Delete clip".to_string()
                            };
                            if ui.button(label).clicked() {
                                if clip_sel.contains(&cref) {
                                    actions.push(Action::DeleteClips(clip_sel.clone()));
                                } else {
                                    actions.push(Action::DeleteClips(vec![cref]));
                                }
                                ui.close();
                            }
                        });
                    }
                }

                // Box selection overlay: finish on release.
                if let Some(m) = marquee.as_ref() {
                    if let Some(now) = pointer_pos {
                        let sel = Rect::from_two_pos(m.start, now);
                        p.rect_filled(sel, 2.0, Color32::from_rgba_unmultiplied(46, 200, 186, 40));
                        p.rect_stroke(sel, 2.0, Stroke::new(1.0, theme::ACCENT), StrokeKind::Inside);
                        if !pointer_down {
                            let hits = clip_rects
                                .iter()
                                .filter(|(_, r)| r.intersects(sel))
                                .map(|(c, _)| *c)
                                .collect();
                            actions.push(Action::MarqueeFinish { hits, additive: m.additive });
                        }
                    }
                    if !pointer_down {
                        *marquee = None;
                    }
                }

                // Live recording take
                if let Some((tid, start)) = recording {
                    if let Some(i) = project.tracks.iter().position(|t| t.id == tid) {
                        let x0 = beat_x(start);
                        let x1 = beat_x(playhead.max(start)).max(x0 + 4.0);
                        let r = Rect::from_min_max(Pos2::new(x0, lane_top(i) + 6.0), Pos2::new(x1, lane_top(i) + LANE_H - 6.0));
                        p.rect_filled(r, 6.0, Color32::from_rgb(120, 34, 42));
                        p.rect_stroke(r, 6.0, Stroke::new(1.5, theme::RECORD), StrokeKind::Inside);
                        p.text(
                            r.left_top() + Vec2::new(8.0, 6.0),
                            Align2::LEFT_TOP,
                            "● REC",
                            FontId::proportional(11.0),
                            theme::RECORD,
                        );
                    }
                }

                // Playhead
                let px = beat_x(playhead);
                if px >= timeline_left {
                    p.line_segment(
                        [Pos2::new(px, origin.y + RULER_H), Pos2::new(px, lanes_bottom)],
                        Stroke::new(1.5, theme::PLAYHEAD),
                    );
                }

                // Add-track row below the last lane
                let add_rect = Rect::from_min_size(
                    Pos2::new(timeline_left + 12.0, lanes_bottom + 14.0),
                    Vec2::new(150.0, 28.0),
                );
                if ui
                    .put(add_rect, egui::Button::new(RichText::new("+ Add synth track").color(theme::TEXT_DIM)))
                    .clicked()
                {
                    actions.push(Action::AddTrack(TrackKind::Synth));
                }

                // Ruler (sticky to the top of the viewport)
                let ruler_y = origin.y + viewport.min.y;
                let ruler = Rect::from_min_size(Pos2::new(timeline_left, ruler_y), Vec2::new(timeline_w, RULER_H));
                p.rect_filled(ruler, 0.0, theme::PANEL_BG);
                for bar in 0..=(max_beats / 4) {
                    let x = beat_x(bar as f32 * 4.0);
                    p.line_segment(
                        [Pos2::new(x, ruler_y), Pos2::new(x, ruler_y + RULER_H)],
                        Stroke::new(1.0, Color32::from_gray(90)),
                    );
                    p.text(
                        Pos2::new(x + 5.0, ruler_y + 6.0),
                        Align2::LEFT_TOP,
                        format!("{}", bar + 1),
                        FontId::proportional(11.5),
                        theme::TEXT_DIM,
                    );
                }
                for b in 0..=max_beats {
                    let x = beat_x(b as f32);
                    p.line_segment(
                        [Pos2::new(x, ruler_y + RULER_H - 6.0), Pos2::new(x, ruler_y + RULER_H)],
                        Stroke::new(1.0, Color32::from_gray(70)),
                    );
                }
                let ruler_resp = ui.interact(ruler, ui.scope_id().with("ruler"), Sense::click_and_drag());
                if ruler_resp.clicked() || ruler_resp.dragged() {
                    if let Some(pos) = ruler_resp.interact_pointer_pos() {
                        actions.push(Action::Seek(beat_at(pos.x).max(0.0)));
                    }
                }
                if px >= timeline_left {
                    p.add(egui::Shape::convex_polygon(
                        vec![
                            Pos2::new(px - 6.0, ruler_y),
                            Pos2::new(px + 6.0, ruler_y),
                            Pos2::new(px, ruler_y + 9.0),
                        ],
                        theme::PLAYHEAD,
                        Stroke::NONE,
                    ));
                }

                // Track headers (sticky to the left edge of the viewport)
                let sx = origin.x + viewport.min.x;
                p.rect_filled(
                    Rect::from_min_size(Pos2::new(sx, ruler_y), Vec2::new(HEADER_W, RULER_H)),
                    0.0,
                    theme::PANEL_BG,
                );
                p.text(
                    Pos2::new(sx + 14.0, ruler_y + 7.0),
                    Align2::LEFT_TOP,
                    "TRACKS",
                    FontId::proportional(10.0),
                    theme::TEXT_DIM,
                );
                for (i, track) in project.tracks.iter_mut().enumerate() {
                    let top = lane_top(i);
                    let id = track.id;
                    let hr = Rect::from_min_size(Pos2::new(sx, top), Vec2::new(HEADER_W, LANE_H));
                    let sel = track_sel.contains(&id) || selected_track == Some(id);
                    p.rect_filled(hr, 0.0, if sel { Color32::from_rgb(46, 46, 56) } else { Color32::from_rgb(36, 36, 43) });
                    p.rect_filled(Rect::from_min_size(Pos2::new(sx, top), Vec2::new(5.0, LANE_H)), 0.0, track_color(track.color));
                    p.line_segment(
                        [Pos2::new(hr.right(), top), Pos2::new(hr.right(), top + LANE_H)],
                        Stroke::new(1.0, Color32::from_black_alpha(140)),
                    );
                    p.text(Pos2::new(sx + 16.0, top + 10.0), Align2::LEFT_TOP, &track.name, FontId::proportional(13.0), theme::TEXT);
                    p.text(
                        Pos2::new(sx + 16.0, top + 28.0),
                        Align2::LEFT_TOP,
                        kind_label(track.kind),
                        FontId::proportional(10.5),
                        theme::TEXT_DIM,
                    );

                    let name_rect = Rect::from_min_size(Pos2::new(sx, top), Vec2::new(HEADER_W - 36.0, 46.0));
                    if ui.interact(name_rect, ui.scope_id().with(("tname", id)), Sense::click()).clicked() {
                        actions.push(Action::SelectTrack { id, shift });
                    }

                    let dots = Rect::from_min_size(Pos2::new(sx + HEADER_W - 28.0, top + 10.0), Vec2::splat(18.0));
                    let dr = ui.interact(dots, ui.scope_id().with(("tdots", id)), Sense::click());
                    theme::paint_icon(&p, dots, Icon::Dots, if dr.hovered() { theme::TEXT } else { theme::TEXT_DIM });
                    dr.on_hover_text("Right-click for track options").context_menu(|ui| {
                        let n = track_sel.len().max(1);
                        let label = if n > 1 { format!("Delete {n} tracks") } else { "Delete track".to_string() };
                        if ui.button(label).clicked() {
                            actions.push(Action::DeleteTracks);
                            ui.close();
                        }
                    });

                    let mute_r = Rect::from_min_size(Pos2::new(sx + 16.0, top + 52.0), Vec2::new(26.0, 19.0));
                    let solo_r = Rect::from_min_size(Pos2::new(sx + 48.0, top + 52.0), Vec2::new(26.0, 19.0));
                    for (r, label, on, on_color) in [
                        (mute_r, "M", track.mute, theme::MUTE_ON),
                        (solo_r, "S", track.solo, theme::SOLO_ON),
                    ] {
                        p.rect_filled(r, 4.0, if on { on_color } else { Color32::from_rgb(52, 52, 62) });
                        p.text(
                            r.center(),
                            Align2::CENTER_CENTER,
                            label,
                            FontId::proportional(11.0),
                            if on { Color32::from_rgb(18, 18, 22) } else { theme::TEXT_DIM },
                        );
                    }
                    if ui.interact(mute_r, ui.scope_id().with(("mute", id)), Sense::click()).clicked() {
                        actions.push(Action::ToggleMute(id));
                    }
                    if ui.interact(solo_r, ui.scope_id().with(("solo", id)), Sense::click()).clicked() {
                        actions.push(Action::ToggleSolo(id));
                    }

                    let knob_r = Rect::from_min_size(Pos2::new(sx + 84.0, top + 49.0), Vec2::splat(24.0));
                    ui.put(knob_r, |ui: &mut egui::Ui| {
                        theme::knob(ui, &mut track.pan, -1.0, 1.0, 0.0, 24.0, "Pan (drag, double-click to centre)")
                    });
                    let fader_r = Rect::from_min_max(
                        Pos2::new(sx + 120.0, top + 55.0),
                        Pos2::new(sx + HEADER_W - 14.0, top + 67.0),
                    );
                    let vol_id = ui.scope_id().with(("vol", id));
                    theme::fader(ui, vol_id, fader_r, &mut track.volume, 0.0, 1.5, track_color(track.color));
                    let level = levels.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
                    let meter_bg = Rect::from_min_max(Pos2::new(fader_r.left(), top + 69.0), Pos2::new(fader_r.right(), top + 72.0));
                    p.rect_filled(meter_bg, 1.5, theme::WELL);
                    if level > 0.002 {
                        let mut fill = meter_bg;
                        fill.max.x = meter_bg.left() + meter_bg.width() * level;
                        p.rect_filled(fill, 1.5, mix(Color32::from_rgb(70, 200, 110), Color32::from_rgb(236, 72, 82), level));
                    }
                }

                actions
            })
            .inner;

        let mut touched_editor = false;
        for action in actions {
            if matches!(action, Action::ClickClip { .. } | Action::MarqueeFinish { .. } | Action::LaneClick { .. }) {
                touched_editor = true;
            }
            self.apply(action);
        }
        if touched_editor {
            self.editor_focus = false;
        }
    }
}

impl eframe::App for BandCraftApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let pointer_down = ctx.input(|i| i.pointer.any_down());
        if !pointer_down && self.project != self.snapshot {
            let previous = std::mem::replace(&mut self.snapshot, self.project.clone());
            self.undo.push(previous);
            if self.undo.len() > 100 {
                self.undo.remove(0);
            }
            self.redo.clear();
        }

        if !ctx.egui_wants_keyboard_input() {
            let keys = ctx.input(|i| {
                let cmd = i.modifiers.command;
                let shift = i.modifiers.shift;
                Keys {
                    space: i.key_pressed(Key::Space),
                    undo: cmd && !shift && i.key_pressed(Key::Z),
                    redo: (cmd && i.key_pressed(Key::Y)) || (cmd && shift && i.key_pressed(Key::Z)),
                    delete: i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace),
                    record: i.key_pressed(Key::R) && !cmd,
                    save: cmd && i.key_pressed(Key::S),
                    home: i.key_pressed(Key::Home),
                    select_all: cmd && i.key_pressed(Key::A),
                    duplicate: cmd && i.key_pressed(Key::D),
                    escape: i.key_pressed(Key::Escape),
                    quantize: i.key_pressed(Key::Q) && !cmd,
                    left: i.key_pressed(Key::ArrowLeft),
                    right: i.key_pressed(Key::ArrowRight),
                    up: i.key_pressed(Key::ArrowUp),
                    down: i.key_pressed(Key::ArrowDown),
                }
            });
            if keys.space {
                self.toggle_play();
            }
            if keys.undo {
                self.undo();
            }
            if keys.redo {
                self.redo();
            }
            if keys.record {
                self.toggle_record();
            }
            if keys.save {
                self.save();
            }
            if keys.home {
                self.seek_beats(0.0);
            }
            if keys.select_all {
                self.select_all();
            }
            if keys.duplicate {
                if self.editor_focus {
                    self.duplicate_notes();
                } else {
                    self.duplicate_selection();
                }
            }
            if keys.quantize && self.editor_focus {
                self.quantize_notes();
            }
            let (copy, paste) = ctx.input(|i| {
                (
                    i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
                    i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))),
                )
            });
            if self.editor_focus {
                if copy {
                    self.copy_notes();
                }
                if paste {
                    self.paste_notes();
                }
            }
            if keys.escape {
                self.clear_selection();
            }
            if keys.delete {
                if self.editor_focus && !self.note_sel.is_empty() {
                    self.delete_selected_notes();
                } else {
                    let list = if self.clip_sel.is_empty() {
                        self.selected_clip.into_iter().collect()
                    } else {
                        self.clip_sel.clone()
                    };
                    self.delete_clips(&list);
                }
            }
            let (dx, dy) = (
                i32::from(keys.right) - i32::from(keys.left),
                i32::from(keys.up) - i32::from(keys.down),
            );
            if self.editor_focus && (dx != 0 || dy != 0) {
                self.nudge_selected_notes(dy, dx);
            }
        }

        if let Some(c) = self.selected_clip {
            if !self.clip_exists(c) {
                self.selected_clip = None;
            }
        }
        self.clip_sel.retain(|c| self.project.track(c.track).is_some_and(|t| t.clips.iter().any(|k| k.id == c.clip)));
        self.track_sel.retain(|t| self.project.track(*t).is_some());
        if self.selected_track.is_some_and(|t| self.project.track(t).is_none()) {
            self.selected_track = None;
        }

        for i in 0..METER_SLOTS {
            let v = self.meters.get(i);
            self.meter_disp[i] = v.max(self.meter_disp[i] * 0.9);
        }
        if let Some(rec) = &mut self.recording {
            rec.recorder.drain();
            ctx.request_repaint();
        }

        self.send(AudioCmd::SetProject(Arc::new(self.project.clone())));
        if self.is_playing() {
            ctx.request_repaint();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("menu").show(ui, |ui| self.menu_ui(ui));
        egui::Panel::top("transport").show(ui, |ui| {
            ui.add_space(4.0);
            self.transport_ui(ui);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_ui(ui));
        egui::Panel::bottom("editor")
            .resizable(true)
            .default_size(380.0)
            .min_size(240.0)
            .show(ui, |ui| self.editor_ui(ui));
        egui::Panel::left("sounds")
            .resizable(true)
            .default_size(240.0)
            .min_size(200.0)
            .max_size(340.0)
            .show(ui, |ui| self.sounds_ui(ui));
        egui::Panel::right("inspector")
            .resizable(true)
            .default_size(290.0)
            .min_size(220.0)
            .max_size(420.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.inspector_ui(ui));
            });
        egui::CentralPanel::default().show(ui, |ui| self.arrangement_ui(ui));
    }
}

/// Keyboard state gathered in one pass.
struct Keys {
    space: bool,
    undo: bool,
    redo: bool,
    delete: bool,
    record: bool,
    save: bool,
    home: bool,
    select_all: bool,
    duplicate: bool,
    escape: bool,
    quantize: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,
}

/// Loads any WAV files referenced by an instrument (sampler and custom drum pads).
pub(crate) fn load_instrument_samples(inst: &mut Instrument, missing: &mut usize) {
    match inst {
        Instrument::Sampler { path, data, .. } => {
            if !path.is_empty() {
                match audio::load_wav(path.as_str()) {
                    Ok(d) => *data = Some(Arc::new(d)),
                    Err(_) => *missing += 1,
                }
            }
        }
        Instrument::DrumKit { slots, .. } => {
            for slot in slots.iter_mut() {
                if !slot.path.is_empty() {
                    match audio::load_wav(slot.path.as_str()) {
                        Ok(d) => slot.data = Some(Arc::new(d)),
                        Err(_) => *missing += 1,
                    }
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn kind_label(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Drums => "Drum kit",
        TrackKind::Synth => "Synth",
        TrackKind::Audio => "Audio",
    }
}

/// Centred placeholder shown when nothing is selected.
pub(crate) fn empty_state(ui: &mut egui::Ui, title: &str, body: &str) {
    ui.add_space(18.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(title).size(14.0).strong().color(theme::TEXT_DIM));
        ui.add_space(4.0);
        ui.label(RichText::new(body).size(11.5).color(theme::TEXT_DIM));
    });
}

/// Rounded dark container used for the clock, tempo and meter.
fn chip(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(theme::WELL)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 5))
        .show(ui, add);
}

fn draw_clip(p: &Painter, r: Rect, clip: &Clip, kind: TrackKind, base: Color32, ppb: f32, bpm: f32, selected: bool) {
    let body = mix(Color32::from_rgb(20, 20, 26), base, 0.42);
    p.rect_filled(r.translate(Vec2::new(0.0, 1.5)), 6.0, Color32::from_black_alpha(70));
    p.rect_filled(r, 6.0, body);
    let head = Rect::from_min_size(r.min, Vec2::new(r.width(), 18.0));
    p.rect_filled(head, CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, base);
    p.line_segment(
        [head.left_bottom(), head.right_bottom()],
        Stroke::new(1.0, Color32::from_black_alpha(60)),
    );
    let cp = p.with_clip_rect(r);
    cp.text(
        r.left_top() + Vec2::new(8.0, 3.0),
        Align2::LEFT_TOP,
        &clip.name,
        FontId::proportional(11.0),
        Color32::WHITE,
    );
    let area = Rect::from_min_max(r.min + Vec2::new(0.0, 20.0), r.max - Vec2::new(0.0, 4.0));
    match &clip.content {
        ClipContent::Notes(notes) => draw_notes(&cp, area, notes, kind, ppb),
        ClipContent::Audio { data: Some(d), .. } => draw_wave(&cp, area, d, bpm, ppb),
        ClipContent::Audio { data: None, .. } => {}
    }
    let stroke = if selected {
        Stroke::new(2.0, Color32::WHITE)
    } else {
        Stroke::new(1.0, Color32::from_black_alpha(110))
    };
    p.rect_stroke(r, 6.0, stroke, StrokeKind::Inside);
}

fn draw_notes(p: &Painter, r: Rect, notes: &[Note], kind: TrackKind, ppb: f32) {
    let rows = if kind == TrackKind::Drums {
        DRUM_KIT.len()
    } else {
        usize::from(SYNTH_HIGH - SYNTH_LOW) + 1
    };
    let row_h = (r.height() / rows as f32).max(1.5);
    for n in notes {
        let row = if kind == TrackKind::Drums {
            drum_index(n.pitch).unwrap_or(0)
        } else {
            usize::from(SYNTH_HIGH.saturating_sub(n.pitch))
        }
        .min(rows - 1);
        let y = r.top() + row as f32 * row_h;
        let x = r.left() + n.start * ppb;
        let w = (n.length * ppb - 1.5).max(2.0);
        p.rect_filled(
            Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, row_h.min(4.0))),
            1.5,
            Color32::from_white_alpha(215),
        );
    }
}

fn draw_wave(p: &Painter, r: Rect, d: &AudioData, bpm: f32, ppb: f32) {
    if d.peaks.is_empty() {
        return;
    }
    let mid = r.center().y;
    let half = r.height() * 0.45;
    let secs_per_px = 60.0 / (bpm * ppb);
    let stroke = Stroke::new(1.0, Color32::from_white_alpha(190));
    let mut x = 0.0f32;
    while x < r.width() {
        let frame = (x * secs_per_px * d.sample_rate as f32) as usize;
        let Some(&peak) = d.peaks.get(frame / PEAK_BIN) else {
            break;
        };
        let a = peak * half;
        let px = r.left() + x;
        p.line_segment([Pos2::new(px, mid - a), Pos2::new(px, mid + a)], stroke);
        x += 1.0;
    }
}

const PALETTE_FOR: [u8; 8] = [2, 0, 5, 3, 4, 6, 7, 1];

fn category_index(category: &str) -> usize {
    INSTRUMENT_CATEGORIES.iter().position(|c| *c == category).unwrap_or(0)
}

/// Card used for browsing sounds. Returns the click response.
pub(crate) fn sound_card(ui: &mut egui::Ui, name: &str, sub: &str, color: Color32) -> egui::Response {
    sound_card_sel(ui, name, sub, color, false)
}

pub(crate) fn sound_card_sel(ui: &mut egui::Ui, name: &str, sub: &str, color: Color32, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::click());
    let p = ui.painter();
    let bg = if selected {
        theme::SURFACE_HI
    } else if resp.hovered() {
        theme::SURFACE
    } else {
        theme::CARD
    };
    p.rect_filled(rect, 8.0, bg);
    p.rect_filled(
        Rect::from_min_size(rect.left_top() + Vec2::new(0.0, 8.0), Vec2::new(4.0, rect.height() - 16.0)),
        2.0,
        color,
    );
    p.text(rect.left_top() + Vec2::new(14.0, 9.0), Align2::LEFT_TOP, name, FontId::proportional(13.0), theme::TEXT);
    p.text(rect.left_top() + Vec2::new(14.0, 26.0), Align2::LEFT_TOP, sub, FontId::proportional(10.5), theme::TEXT_DIM);
    if selected {
        p.rect_stroke(rect, 8.0, Stroke::new(1.0, theme::ACCENT), StrokeKind::Inside);
    }
    ui.add_space(3.0);
    resp
}
