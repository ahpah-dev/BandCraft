//! BandCraft user interface (egui): app state, transport, arrangement and sounds browser.
//! The inspector and piano-roll editor live in `panels.rs`.

use crate::audio::{self, Recorder};
use crate::midi::{self, MidiEvent, MidiIn};
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
pub(crate) const RULER_H: f32 = 28.0;
pub(crate) const LANE_H: f32 = 84.0;
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

pub(crate) struct ClipResize {
    pub clip: ClipRef,
    /// Beats between the pointer and the clip's right edge when the drag began.
    pub grab: f32,
}

/// A MIDI take in progress: notes land in `clip_id` on `track_id`.
pub(crate) struct MidiRec {
    pub track_id: u32,
    pub clip_id: u32,
    /// Held keys: pitch -> (start beat, velocity).
    pub pending: std::collections::HashMap<u8, (f32, u8)>,
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
    ResizeClip { clip: ClipRef, length: f32 },
    DeleteTrack(u32),
    DuplicateTrack(u32),
    DuplicateClips(Vec<ClipRef>),
    NewClip { track: u32, beat: f32 },
    Rename(Option<u32>),
    Record,
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
    pub(crate) clip_resize: Option<ClipResize>,
    pub(crate) midi: MidiIn,
    pub(crate) midi_rec: Option<MidiRec>,
    /// Keyboard notes currently sounding: pitch -> track they were sent to.
    pub(crate) live_notes: std::collections::HashMap<u8, u32>,
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
    /// True after a track header was clicked: Delete then removes tracks instead of clips.
    pub(crate) track_focus: bool,
    pub(crate) renaming: Option<u32>,
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
            clip_resize: None,
            midi: MidiIn::new(),
            midi_rec: None,
            live_notes: std::collections::HashMap::new(),
            marquee: None,
            selected_loop: None,
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
            track_focus: false,
            renaming: None,
            status: "Double-click a lane to add a clip, or click a loop then a lane. Click a track and press Delete to remove it.".into(),
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
        self.track_focus = false;
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
            self.status = format!(
                "'{}' is a {} loop: click a {} lane, or double-click the loop to give it a new track.",
                lp.name,
                kind_label(lp.kind()),
                kind_label(lp.kind())
            );
            return;
        }
        let start = (beat / 4.0).floor().max(0.0) * 4.0;
        let existing = self
            .project
            .track(track_id)
            .and_then(|t| t.clips.iter().find(|c| (c.start - start).abs() < 0.01 && c.name == lp.name).map(|c| c.id));
        let id = match existing {
            Some(id) => id,
            None => self.project.place_loop(track_id, lp, start),
        };
        self.select_clip(ClipRef { track: track_id, clip: id });
        self.status = format!("Placed '{}'. Keep clicking lanes to place more, Esc to stop.", lp.name);
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
                self.track_focus = false;
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
                self.track_focus = false;
                self.clip_sel.clear();
                self.editor_focus = false;
                match self.selected_loop {
                    Some(idx) => self.place_loop(track, idx, beat),
                    None => self.selected_clip = None,
                }
            }
            Action::SelectTrack { id, shift } => {
                self.track_focus = true;
                self.editor_focus = false;
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
                self.delete_tracks(&ids);
            }
            Action::DeleteTrack(id) => {
                let ids = if self.track_sel.len() > 1 && self.track_sel.contains(&id) { self.track_sel.clone() } else { vec![id] };
                self.delete_tracks(&ids);
            }
            Action::ResizeClip { clip, length } => {
                if let Some(c) = self
                    .project
                    .track_mut(clip.track)
                    .and_then(|t| t.clips.iter_mut().find(|k| k.id == clip.clip))
                {
                    if matches!(c.content, ClipContent::Notes(_)) {
                        c.length = length.clamp(1.0, 512.0);
                    }
                }
            }
            Action::DuplicateTrack(id) => self.duplicate_track(id),
            Action::DuplicateClips(list) => self.duplicate_clips(&list),
            Action::NewClip { track, beat } => self.new_clip(track, beat),
            Action::Rename(r) => self.renaming = r,
            Action::Record => self.toggle_record(),
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
    pub(crate) fn duplicate_selection(&mut self) {
        let list: Vec<ClipRef> = if self.clip_sel.is_empty() {
            self.selected_clip.into_iter().collect()
        } else {
            self.clip_sel.clone()
        };
        self.duplicate_clips(&list);
    }

    fn duplicate_clips(&mut self, list: &[ClipRef]) {
        let bpm = self.project.bpm;
        let mut new_sel = Vec::new();
        let sources: Vec<(ClipRef, Clip)> = list
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
        if let Some(last) = new_sel.last().copied() {
            self.selected_clip = Some(last);
            self.selected_track = Some(last.track);
            self.track_sel = vec![last.track];
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
                self.reset_ui_state();
                self.project = project;
                self.undo.clear();
                self.redo.clear();
                self.snapshot = self.project.clone();
                self.seek_beats(0.0);
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
        self.reset_ui_state();
        self.project = Project::empty();
        self.seek_beats(0.0);
        self.status = "New project. Add a track with + Track or pick an instrument on the left.".into();
    }

    /// Forgets every selection, drag and in-progress recording (used when the project is replaced).
    fn reset_ui_state(&mut self) {
        if let Some(rec) = self.recording.take() {
            let _ = rec.recorder.finish();
            self.send(AudioCmd::Stop);
        }
        self.clip_sel.clear();
        self.track_sel.clear();
        self.selected_clip = None;
        self.selected_track = None;
        self.clip_drag = None;
        self.clip_resize = None;
        self.stop_midi_record();
        self.marquee = None;
        self.note_sel.clear();
        self.note_sel_clip = None;
        self.note_op = None;
        self.roll_for_clip = None;
        self.renaming = None;
        self.track_focus = false;
        self.editor_focus = false;
    }

    // ----- panels --------------------------------------------------------------------

    fn menu_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                ui.set_min_width(210.0);
                if ui.button("New project").clicked() {
                    self.new_project();
                    ui.close();
                }
                if ui.button("Open").clicked() {
                    self.open();
                    ui.close();
                }
                if ui.add(egui::Button::new("Save").shortcut_text("Ctrl+S")).clicked() {
                    self.save();
                    ui.close();
                }
                if ui.button("Export WAV").clicked() {
                    self.export();
                    ui.close();
                }
                if ui.button("Export MIDI").clicked() {
                    self.export_midi();
                    ui.close();
                }
                ui.separator();
                if ui.button("Exit").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                ui.set_min_width(230.0);
                if ui.add(egui::Button::new("Undo").shortcut_text("Ctrl+Z")).clicked() {
                    self.undo();
                    ui.close();
                }
                if ui.add(egui::Button::new("Redo").shortcut_text("Ctrl+Y")).clicked() {
                    self.redo();
                    ui.close();
                }
                ui.separator();
                if ui.add(egui::Button::new("Select all").shortcut_text("Ctrl+A")).clicked() {
                    self.select_all();
                    ui.close();
                }
                if ui.add(egui::Button::new("Duplicate").shortcut_text("Ctrl+D")).clicked() {
                    if self.editor_focus {
                        self.duplicate_notes();
                    } else {
                        self.duplicate_selection();
                    }
                    ui.close();
                }
                if ui.add(egui::Button::new("Delete selection").shortcut_text("Del")).clicked() {
                    self.delete_selection();
                    ui.close();
                }
                if ui.button("Delete selected track(s)").clicked() {
                    let ids = self.selected_track_ids();
                    self.delete_tracks(&ids);
                    ui.close();
                }
                if ui.add(egui::Button::new("Clear selection").shortcut_text("Esc")).clicked() {
                    self.clear_selection();
                    ui.close();
                }
            });
            ui.menu_button("Track", |ui| {
                ui.set_min_width(200.0);
                if ui.button("Add synth track").clicked() {
                    self.add_track(TrackKind::Synth);
                    ui.close();
                }
                if ui.button("Add drum kit track").clicked() {
                    self.add_track(TrackKind::Drums);
                    ui.close();
                }
                if ui.add(egui::Button::new("Record audio").shortcut_text("R")).clicked() {
                    self.toggle_record();
                    ui.close();
                }
                if ui.add(egui::Button::new("Record MIDI").shortcut_text("Shift+R")).clicked() {
                    self.toggle_midi_record();
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
                if ui.button("Reset zoom").clicked() {
                    self.px_per_beat = 26.0;
                    ui.close();
                }
            });
            ui.menu_button("Settings", |ui| {
                self.midi_menu(ui);
                ui.separator();
                ui.label(format!("Output sample rate: {} Hz", self.sample_rate));
                match &self.audio_error {
                    Some(e) => ui.colored_label(egui::Color32::YELLOW, format!("Audio output: {e}")),
                    None => ui.label("Audio output: OK"),
                };
            });
            ui.menu_button("Help", |ui| {
                for line in [
                    "Space: play / pause      R: record",
                    "Double-click a lane: add an empty clip",
                    "Click a loop on the left, then click a lane to place it (Esc stops)",
                    "Click a track name to select it; Delete removes it",
                    "Trash icon / right-click a track: delete, rename, duplicate",
                    "Click: select   Shift+click: add/remove   Drag empty space: box select",
                    "Drag clips: move the whole selection   Ctrl+scroll: zoom",
                    "Ctrl+A select all   Ctrl+D duplicate   Esc clear",
                    "Piano roll: click to add notes, drag to box-select, right-click deletes",
                    "Piano roll: arrows nudge selected notes, Q quantizes",
                    "Drag a note clip's right edge to resize it",
                    "MIDI keyboard: plays the selected track; Shift+R records MIDI into it",
                    "Loops tab: import .mid files; File > Export MIDI saves one",
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

    // ----- MIDI ----------------------------------------------------------------------

    /// Menu content for choosing the MIDI input device.
    fn midi_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(220.0);
        ui.label(RichText::new("MIDI INPUT").size(10.0).strong().color(theme::TEXT_DIM));
        if self.midi.ports.is_empty() {
            ui.label("No MIDI keyboard found");
        }
        let ports = self.midi.ports.clone();
        for name in ports {
            let on = self.midi.connected.as_deref() == Some(name.as_str());
            if ui.selectable_label(on, &name).clicked() {
                self.midi.connect(&name);
                self.status = match &self.midi.error {
                    Some(e) => format!("MIDI: {e}"),
                    None => format!("MIDI keyboard connected: {name}"),
                };
                ui.close();
            }
        }
        if let Some(e) = &self.midi.error {
            ui.colored_label(Color32::YELLOW, e);
        }
        ui.separator();
        if ui.button("Rescan devices").clicked() {
            self.midi.rescan();
        }
        if self.midi.connected.is_some() && ui.button("Disconnect").clicked() {
            self.midi.disconnect();
            ui.close();
        }
    }

    /// Handles keyboard notes: plays them on the selected track and records them if a take is running.
    fn pump_midi(&mut self) {
        let events: Vec<MidiEvent> = self.midi.rx.try_iter().collect();
        if let Some(rec) = &self.midi_rec {
            // Keep the take long enough to cover the playhead.
            let beat = self.playhead_beats();
            let (tid, cid) = (rec.track_id, rec.clip_id);
            if let Some(clip) = self.project.track_mut(tid).and_then(|t| t.clips.iter_mut().find(|c| c.id == cid)) {
                let need = (((beat - clip.start) / 4.0).ceil() * 4.0).max(4.0);
                if need > clip.length {
                    clip.length = need;
                }
            }
        }
        if events.is_empty() {
            return;
        }
        let target = self
            .midi_rec
            .as_ref()
            .map(|r| r.track_id)
            .or_else(|| self.selected_track.filter(|t| self.project.track(*t).is_some_and(|tr| tr.kind != TrackKind::Audio)));
        let Some(track_id) = target else {
            return;
        };
        let drums = self.project.track(track_id).is_some_and(|t| t.kind == TrackKind::Drums);
        let beat = self.playhead_beats();
        for ev in events {
            let pitch = if drums {
                match midi::gm_drum_to_kit(ev.pitch) {
                    Some(p) => p,
                    None => continue,
                }
            } else {
                ev.pitch
            };
            if ev.on {
                self.live_notes.insert(pitch, track_id);
                self.send(AudioCmd::NoteOn { track: track_id, pitch, vel: ev.vel });
                if let Some(rec) = &mut self.midi_rec {
                    rec.pending.insert(pitch, (beat, ev.vel));
                }
            } else {
                let t = self.live_notes.remove(&pitch).unwrap_or(track_id);
                self.send(AudioCmd::NoteOff { track: t, pitch });
                self.finish_midi_note(pitch, beat);
            }
        }
    }

    fn finish_midi_note(&mut self, pitch: u8, end_beat: f32) {
        let Some(rec) = &mut self.midi_rec else {
            return;
        };
        let Some((start, vel)) = rec.pending.remove(&pitch) else {
            return;
        };
        let (tid, cid) = (rec.track_id, rec.clip_id);
        let drums = self.project.track(tid).is_some_and(|t| t.kind == TrackKind::Drums);
        if let Some(clip) = self.project.track_mut(tid).and_then(|t| t.clips.iter_mut().find(|c| c.id == cid)) {
            let rel = (start - clip.start).max(0.0);
            let len = if drums { 0.25 } else { (end_beat - start).max(0.05) };
            let stored = if drums { pitch } else { midi::fold_pitch(i32::from(pitch)) };
            if let ClipContent::Notes(notes) = &mut clip.content {
                notes.push(Note { pitch: stored, start: rel, length: len, velocity: vel.max(1) });
            }
            let need = ((rel + len) / 4.0).ceil() * 4.0;
            if need > clip.length {
                clip.length = need;
            }
        }
    }

    pub(crate) fn toggle_midi_record(&mut self) {
        if self.midi_rec.is_some() {
            self.stop_midi_record();
            return;
        }
        let Some(tid) = self
            .selected_track
            .filter(|t| self.project.track(*t).is_some_and(|tr| tr.kind != TrackKind::Audio))
        else {
            self.status = "Select a synth or drum track first, then press Shift+R to record MIDI.".into();
            return;
        };
        if self.midi.connected.is_none() {
            self.status = "No MIDI keyboard connected. Plug one in, then choose it in Settings > MIDI input.".into();
            return;
        }
        if self.recording.is_some() {
            self.stop_recording();
        }
        if self.playhead_beats() >= self.project.song_beats() {
            self.seek_beats(0.0);
        }
        let start = (self.playhead_beats() / 4.0).floor().max(0.0) * 4.0;
        let clip_id = self.project.alloc_id();
        let count = self.project.track(tid).map_or(0, |t| t.clips.len()) + 1;
        if let Some(t) = self.project.track_mut(tid) {
            t.clips.push(Clip {
                id: clip_id,
                name: format!("MIDI take {count}"),
                start,
                length: 4.0,
                content: ClipContent::Notes(Vec::new()),
            });
        }
        self.midi_rec = Some(MidiRec { track_id: tid, clip_id, pending: std::collections::HashMap::new() });
        self.select_clip(ClipRef { track: tid, clip: clip_id });
        if !self.is_playing() {
            self.send(AudioCmd::Play);
        }
        self.status = "Recording MIDI: play your keyboard. Press Shift+R or the MIDI button to stop.".into();
    }

    pub(crate) fn stop_midi_record(&mut self) {
        if self.midi_rec.is_none() {
            return;
        }
        let beat = self.playhead_beats();
        let held: Vec<u8> = self.midi_rec.as_ref().map(|r| r.pending.keys().copied().collect()).unwrap_or_default();
        for p in held {
            self.finish_midi_note(p, beat);
        }
        let Some(rec) = self.midi_rec.take() else {
            return;
        };
        self.send(AudioCmd::Stop);
        let count = self
            .project
            .track(rec.track_id)
            .and_then(|t| t.clips.iter().find(|c| c.id == rec.clip_id))
            .map(|c| match &c.content {
                ClipContent::Notes(n) => n.len(),
                ClipContent::Audio { .. } => 0,
            });
        match count {
            Some(0) => {
                self.project.remove_clip(rec.track_id, rec.clip_id);
                self.status = "No notes were played, so the MIDI take was discarded.".into();
            }
            Some(n) => self.status = format!("Recorded {n} MIDI note(s). Press Q in the piano roll to quantize."),
            None => {}
        }
    }

    pub(crate) fn import_midi(&mut self, path: &str) {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                self.status = format!("Cannot read {path}: {e}");
                return;
            }
        };
        let imp = match midi::import_smf(&bytes) {
            Ok(i) => i,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        if self.project.tracks.is_empty() {
            if let Some(b) = imp.bpm {
                self.project.bpm = b.round();
            }
        }
        let stem = Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("MIDI").to_string();
        let count = imp.tracks.len();
        let mut first = None;
        for t in imp.tracks {
            let (kind, inst) = if t.drums {
                (TrackKind::Drums, Instrument::default_drum_kit())
            } else {
                (TrackKind::Synth, Instrument::preset(midi::preset_for_program(t.program)).unwrap_or_default())
            };
            let id = self.project.add_track_with(&t.name, kind, inst);
            let end = t.notes.iter().map(|n| n.start + n.length).fold(0.0f32, f32::max);
            let length = ((end / 4.0).ceil() * 4.0).max(4.0);
            let clip_id = self.project.alloc_id();
            if let Some(track) = self.project.track_mut(id) {
                track.clips.push(Clip { id: clip_id, name: stem.clone(), start: 0.0, length, content: ClipContent::Notes(t.notes) });
            }
            first.get_or_insert(ClipRef { track: id, clip: clip_id });
        }
        if let Some(c) = first {
            self.select_clip(c);
        }
        self.seek_beats(0.0);
        self.status = format!("Imported {count} track(s) from {stem}.mid");
    }

    pub(crate) fn export_midi(&mut self) {
        let path = Path::new(&self.export_path).with_extension("mid");
        let bytes = midi::export_smf(&self.project);
        match std::fs::write(&path, bytes) {
            Ok(()) => self.status = format!("Exported MIDI to {}", path.display()),
            Err(e) => self.status = format!("MIDI export failed: {e}"),
        }
    }

    /// Deletes selected notes (when the piano roll has focus), else selected clips, else selected tracks.
    pub(crate) fn delete_selection(&mut self) {
        if self.editor_focus {
            if !self.note_sel.is_empty() {
                self.delete_selected_notes();
            }
            return;
        }
        let list: Vec<ClipRef> = if self.clip_sel.is_empty() {
            self.selected_clip.into_iter().collect()
        } else {
            self.clip_sel.clone()
        };
        if !list.is_empty() {
            self.delete_clips(&list);
        } else if self.track_focus {
            let ids = self.selected_track_ids();
            self.delete_tracks(&ids);
        }
    }

    pub(crate) fn delete_tracks(&mut self, ids: &[u32]) {
        if ids.is_empty() {
            return;
        }
        if self.recording.as_ref().is_some_and(|r| ids.contains(&r.track_id)) {
            if let Some(rec) = self.recording.take() {
                let _ = rec.recorder.finish();
            }
            self.send(AudioCmd::Stop);
        }
        if self.midi_rec.as_ref().is_some_and(|r| ids.contains(&r.track_id)) {
            self.stop_midi_record();
        }
        for &id in ids {
            self.project.remove_track(id);
        }
        self.track_sel.retain(|t| !ids.contains(t));
        if self.selected_track.is_some_and(|t| ids.contains(&t)) {
            self.selected_track = self.track_sel.last().copied();
        }
        self.clip_sel.retain(|c| !ids.contains(&c.track));
        if self.selected_clip.is_some_and(|c| ids.contains(&c.track)) {
            self.selected_clip = None;
        }
        self.track_focus = false;
        self.renaming = None;
        self.status = if ids.len() == 1 {
            "Track deleted. Ctrl+Z brings it back.".to_string()
        } else {
            format!("{} tracks deleted. Ctrl+Z brings them back.", ids.len())
        };
    }

    fn duplicate_track(&mut self, id: u32) {
        let Some(mut t) = self.project.track(id).cloned() else {
            return;
        };
        t.id = self.project.alloc_id();
        t.name = format!("{} copy", t.name);
        for c in &mut t.clips {
            c.id = self.project.alloc_id();
        }
        let new_id = t.id;
        let pos = self.project.tracks.iter().position(|x| x.id == id).map_or(self.project.tracks.len(), |p| p + 1);
        self.project.tracks.insert(pos, t);
        self.select_only_track(new_id);
        self.status = "Track duplicated".into();
    }

    /// Adds an empty one-bar-aligned, four-beat note clip and selects it so it opens in the piano roll.
    fn new_clip(&mut self, track_id: u32, beat: f32) {
        let Some(kind) = self.project.track(track_id).map(|t| t.kind) else {
            return;
        };
        if kind == TrackKind::Audio {
            self.select_only_track(track_id);
            self.status = "Audio tracks hold recordings: press R to record, or import a WAV.".into();
            return;
        }
        let start = (beat / 4.0).floor().max(0.0) * 4.0;
        let id = self.project.alloc_id();
        let count = self.project.track(track_id).map_or(0, |t| t.clips.len()) + 1;
        if let Some(t) = self.project.track_mut(track_id) {
            t.clips.push(Clip {
                id,
                name: format!("Pattern {count}"),
                start,
                length: 4.0,
                content: ClipContent::Notes(Vec::new()),
            });
        }
        self.select_clip(ClipRef { track: track_id, clip: id });
        self.status = "Empty clip added. Click in the piano roll below to write notes.".into();
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
            if let Some(last) = self.clip_sel.last().copied() {
                self.selected_clip = Some(last);
                self.selected_track = Some(last.track);
            }
        }
    }

    pub(crate) fn clear_selection(&mut self) {
        if self.selected_loop.take().is_some() {
            self.status = "Stopped placing loops".into();
        }
        self.clip_sel.clear();
        self.note_sel.clear();
        self.note_sel_clip = None;
        self.editor_focus = false;
        self.renaming = None;
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
            if theme::round_button(ui, Icon::Record, theme::RECORD, "Record audio from the microphone (R)", recording).clicked() {
                self.toggle_record();
            }
            ui.add_space(4.0);
            ui.vertical(|ui| {
                let on = self.midi_rec.is_some();
                let label = if on { "■ MIDI" } else { "● MIDI" };
                let btn = egui::Button::new(RichText::new(label).strong().size(11.5).color(if on { theme::WINDOW_BG } else { theme::RECORD }))
                    .fill(if on { theme::RECORD } else { theme::SURFACE })
                    .min_size(Vec2::new(64.0, 18.0));
                if ui
                    .add(btn)
                    .on_hover_text("Record notes from your MIDI keyboard into the selected synth or drum track (Shift+R)")
                    .clicked()
                {
                    self.toggle_midi_record();
                }
                let dot = if self.midi.connected.is_some() { theme::ACCENT } else { theme::TEXT_DIM };
                let name = self.midi.connected.clone().unwrap_or_else(|| "no keyboard".to_string());
                let short: String = name.chars().take(10).collect();
                let resp = ui.add(
                    egui::Button::new(RichText::new(format!("● {short}")).size(10.0).color(dot))
                        .frame(false)
                        .min_size(Vec2::new(64.0, 14.0)),
                );
                let resp = resp.on_hover_text("MIDI input: click to choose a device");
                egui::Popup::menu(&resp).show(|ui| self.midi_menu(ui));
            });

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
                ui.add(egui::TextEdit::singleline(&mut self.export_path).desired_width(120.0).hint_text("output.wav"));
                ui.label(RichText::new("Export to").small().color(theme::TEXT_DIM));
                ui.add_space(8.0);
                ui.add(egui::TextEdit::singleline(&mut self.project_path).desired_width(170.0));
                ui.label(RichText::new("Project file").small().color(theme::TEXT_DIM));
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

        theme::caption(ui, "Import audio / MIDI");
        ui.horizontal(|ui| {
            let row_w = ui.available_width();
            ui.add(
                egui::TextEdit::singleline(&mut self.import_path)
                    .hint_text("path to a .wav or .mid file")
                    .desired_width((row_w - 96.0).max(60.0)),
            );
            if ui.button("Import").clicked() {
                let path = self.import_path.clone();
                let lower = path.to_lowercase();
                if lower.ends_with(".mid") || lower.ends_with(".midi") {
                    self.import_midi(&path);
                } else {
                    self.import_wav(&path);
                }
            }
        });

        theme::caption(ui, "Loops");
        if let Some(i) = self.selected_loop {
            let mut stop = false;
            egui::Frame::new()
                .fill(mix(theme::CARD, theme::ACCENT, 0.16))
                .corner_radius(CornerRadius::same(9))
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("Placing: {}", LIBRARY[i].name)).strong().color(theme::ACCENT));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Stop").clicked() {
                                stop = true;
                            }
                        });
                    });
                    ui.label(RichText::new("Click a matching lane to drop it.").small().color(theme::TEXT_DIM));
                });
            if stop {
                self.selected_loop = None;
            }
        } else {
            ui.label(
                RichText::new("Click a loop to arm it, then click a lane. Double-click a loop to fill a new track.")
                    .small()
                    .color(theme::TEXT_DIM),
            );
        }
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
                        self.selected_loop = None;
                    }
                }
            }
        });
    }

    fn arrangement_ui(&mut self, ui: &mut egui::Ui) {
        let pointer_down = ui.input(|i| i.pointer.primary_down());
        if !pointer_down {
            self.clip_drag = None;
            self.clip_resize = None;
        }
        let zoom = ui.input(|i| i.zoom_delta());
        if (zoom - 1.0).abs() > 1e-4 && ui.rect_contains_pointer(ui.max_rect()) {
            self.px_per_beat = (self.px_per_beat * zoom).clamp(8.0, 80.0);
        }
        let shift = ui.input(|i| i.modifiers.shift);
        let ppb = self.px_per_beat;
        let bpm = self.project.bpm;
        let song_beats = self.project.song_beats();
        let track_count = self.project.tracks.len();
        let content_w = HEADER_W + (song_beats + 16.0) * ppb;
        let content_h = (RULER_H + track_count as f32 * LANE_H + 80.0).max(ui.available_height());
        let playhead = self.playhead_beats();
        let selected_track = self.selected_track;
        let track_sel = self.track_sel.clone();
        let levels = self.meter_disp.clone();
        let clip_sel = self.clip_sel.clone();
        let recording = self.recording.as_ref().map(|r| (r.track_id, r.start_beat));
        let pointer_pos = ui.input(|i| i.pointer.interact_pos());
        let renaming = self.renaming;
        let armed: Option<&'static LoopDef> = self.selected_loop.and_then(|i| LIBRARY.get(i));
        let project = &mut self.project;
        let clip_drag = &mut self.clip_drag;
        let clip_resize = &mut self.clip_resize;
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
                let bar_start = |beat: f32| (beat / 4.0).floor().max(0.0) * 4.0;

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
                        Stroke::new(1.0, Color32::from_black_alpha(110)),
                    );
                }
                let empty = Rect::from_min_max(
                    Pos2::new(timeline_left, lanes_bottom),
                    Pos2::new(origin.x + content_w, origin.y + content_h),
                );
                p.rect_filled(empty, 0.0, theme::WINDOW_BG);

                // Alternate bars are shaded a touch lighter; beat lines appear when zoomed in.
                for k in 0..=(max_beats / 8) {
                    let x0 = beat_x((2 * k + 1) as f32 * 4.0);
                    let x1 = beat_x((2 * k + 2) as f32 * 4.0);
                    p.rect_filled(
                        Rect::from_min_max(Pos2::new(x0, origin.y + RULER_H), Pos2::new(x1, lanes_bottom)),
                        0.0,
                        Color32::from_white_alpha(5),
                    );
                }
                for b in 0..=max_beats {
                    let bar = b % 4 == 0;
                    if !bar && ppb < 16.0 {
                        continue;
                    }
                    let x = beat_x(b as f32);
                    let col = if bar { theme::GRID_BAR } else { theme::GRID_BEAT };
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
                    p.line_segment(
                        [Pos2::new(song_end_x, origin.y + RULER_H), Pos2::new(song_end_x, lanes_bottom)],
                        Stroke::new(1.0, theme::GRID_BAR),
                    );
                }

                if track_count == 0 {
                    let c = origin + viewport.center().to_vec2() + Vec2::new(HEADER_W * 0.5, 0.0);
                    p.text(c - Vec2::new(0.0, 14.0), Align2::CENTER_CENTER, "Your song is empty", FontId::proportional(18.0), theme::TEXT);
                    p.text(
                        c + Vec2::new(0.0, 12.0),
                        Align2::CENTER_CENTER,
                        "Click an instrument on the left, or use + Track.",
                        FontId::proportional(12.5),
                        theme::TEXT_DIM,
                    );
                }

                // Lanes: click selects/places, double-click adds an empty clip, drag box-selects.
                for (i, track) in project.tracks.iter().enumerate() {
                    let r = Rect::from_min_size(Pos2::new(timeline_left, lane_top(i)), Vec2::new(timeline_w, LANE_H));
                    let resp = ui.interact(r, ui.scope_id().with(("lane", track.id)), Sense::click_and_drag());
                    let beat_key = ui.scope_id().with(("ctxbeat", track.id));
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
                    if resp.double_clicked() && armed.is_none() {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            actions.push(Action::NewClip { track: track.id, beat: beat_at(pos.x) });
                        }
                    }
                    if resp.secondary_clicked() {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            ui.data_mut(|d| d.insert_temp(beat_key, beat_at(pos.x)));
                        }
                    }
                    if resp.hovered() && !pointer_down && marquee.is_none() {
                        if let Some(pos) = resp.hover_pos() {
                            let start = bar_start(beat_at(pos.x));
                            match armed {
                                Some(lp) if lp.kind() == track.kind => {
                                    let g = Rect::from_min_size(
                                        Pos2::new(beat_x(start), r.top() + 6.0),
                                        Vec2::new((lp.beats() * ppb).max(8.0), LANE_H - 12.0),
                                    );
                                    p.rect_filled(g, 8.0, kind_color(lp.kind()).gamma_multiply(0.30));
                                    p.rect_stroke(g, 8.0, Stroke::new(1.5, kind_color(lp.kind())), StrokeKind::Inside);
                                    p.with_clip_rect(g).text(
                                        g.left_top() + Vec2::new(8.0, 4.0),
                                        Align2::LEFT_TOP,
                                        lp.name,
                                        FontId::proportional(11.0),
                                        Color32::WHITE,
                                    );
                                }
                                Some(_) => ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed),
                                None if track.kind != TrackKind::Audio => {
                                    let g = Rect::from_min_size(
                                        Pos2::new(beat_x(start), r.top() + 6.0),
                                        Vec2::new(4.0 * ppb, LANE_H - 12.0),
                                    );
                                    p.rect_stroke(g, 8.0, Stroke::new(1.0, Color32::from_white_alpha(34)), StrokeKind::Inside);
                                    if g.width() > 150.0 {
                                        p.text(
                                            g.center(),
                                            Align2::CENTER_CENTER,
                                            "+ Double-click to add a clip",
                                            FontId::proportional(11.5),
                                            Color32::from_white_alpha(64),
                                        );
                                    }
                                }
                                None => {}
                            }
                        }
                    }
                    let tid = track.id;
                    let tkind = track.kind;
                    let n_sel = if track_sel.contains(&tid) { track_sel.len() } else { 1 };
                    resp.context_menu(|ui| {
                        ui.set_min_width(190.0);
                        let beat: f32 = ui.data(|d| d.get_temp(beat_key)).unwrap_or(0.0);
                        if tkind != TrackKind::Audio && ui.button("New empty clip here").clicked() {
                            actions.push(Action::NewClip { track: tid, beat });
                            ui.close();
                        }
                        if let Some(lp) = armed {
                            if lp.kind() == tkind && ui.button(format!("Place '{}' here", lp.name)).clicked() {
                                actions.push(Action::LaneClick { track: tid, beat });
                                ui.close();
                            }
                        }
                        ui.separator();
                        track_menu(ui, tid, n_sel, &mut actions);
                    });
                }

                // Clips
                let mut clip_rects: Vec<(ClipRef, Rect)> = Vec::new();
                for (i, track) in project.tracks.iter().enumerate() {
                    let base = if track.mute {
                        mix(track_color(track.color), Color32::from_gray(78), 0.72)
                    } else {
                        track_color(track.color)
                    };
                    for clip in &track.clips {
                        let len = clip.length_beats(bpm);
                        let r = Rect::from_min_size(
                            Pos2::new(beat_x(clip.start), lane_top(i) + 6.0),
                            Vec2::new((len * ppb).max(4.0), LANE_H - 12.0),
                        );
                        let cref = ClipRef { track: track.id, clip: clip.id };
                        let selected = clip_sel.contains(&cref);
                        draw_clip(&p, r, clip, track.kind, base, ppb, bpm, selected);
                        clip_rects.push((cref, r));

                        let is_notes = matches!(clip.content, ClipContent::Notes(_));
                        let edge_x = r.right() - 9.0;
                        let can_resize = is_notes && r.width() > 24.0;
                        let resp = ui.interact(r, ui.scope_id().with(("clip", clip.id)), Sense::click_and_drag());
                        if resp.hovered() && !pointer_down {
                            let on_edge = can_resize && resp.hover_pos().is_some_and(|h| h.x >= edge_x);
                            ui.ctx().set_cursor_icon(if on_edge {
                                egui::CursorIcon::ResizeHorizontal
                            } else {
                                egui::CursorIcon::Grab
                            });
                        }
                        if resp.clicked() {
                            actions.push(Action::ClickClip { clip: cref, shift });
                        }
                        if resp.secondary_clicked() && !clip_sel.contains(&cref) {
                            actions.push(Action::ClickClip { clip: cref, shift: false });
                        }
                        if resp.drag_started() {
                            let press = ui.input(|i| i.pointer.press_origin());
                            if can_resize && press.is_some_and(|p| p.x >= edge_x) {
                                // Dragging the right edge resizes the clip.
                                if let Some(pos) = press {
                                    *clip_resize = Some(ClipResize { clip: cref, grab: beat_at(pos.x) - (clip.start + clip.length) });
                                }
                                *clip_drag = None;
                                if !clip_sel.contains(&cref) {
                                    actions.push(Action::ClickClip { clip: cref, shift: false });
                                }
                            } else {
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
                                if let Some(pos) = press {
                                    *clip_drag = Some(ClipDrag { clip: cref, grab: beat_at(pos.x) - clip.start, origins });
                                }
                            }
                        }
                        if resp.dragged() {
                            let resizing = clip_resize.as_ref().is_some_and(|z| z.clip == cref);
                            if let (true, Some(pos), Some(z)) = (resizing, resp.interact_pointer_pos(), clip_resize.as_ref()) {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                                let length = (beat_at(pos.x) - z.grab - clip.start).round().max(1.0);
                                actions.push(Action::ResizeClip { clip: cref, length });
                            } else if !resizing {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
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
                        }
                        resp.context_menu(|ui| {
                            ui.set_min_width(190.0);
                            let targets: Vec<ClipRef> = if clip_sel.contains(&cref) { clip_sel.clone() } else { vec![cref] };
                            let n = targets.len();
                            if is_notes && ui.button("Edit notes").clicked() {
                                actions.push(Action::ClickClip { clip: cref, shift: false });
                                ui.close();
                            }
                            let dup = if n > 1 { format!("Duplicate {n} clips") } else { "Duplicate".to_string() };
                            if ui.button(dup).clicked() {
                                actions.push(Action::DuplicateClips(targets.clone()));
                                ui.close();
                            }
                            ui.separator();
                            let del = if n > 1 { format!("Delete {n} clips") } else { "Delete clip".to_string() };
                            if ui.add(egui::Button::new(RichText::new(del).color(theme::RECORD))).clicked() {
                                actions.push(Action::DeleteClips(targets));
                                ui.close();
                            }
                        });
                    }
                }

                // Box selection overlay: finish on release.
                if let Some(m) = marquee.as_ref() {
                    let now = pointer_pos.unwrap_or(m.start);
                    let sel = Rect::from_two_pos(m.start, now);
                    p.rect_filled(sel, 3.0, Color32::from_rgba_unmultiplied(52, 211, 196, 36));
                    p.rect_stroke(sel, 3.0, Stroke::new(1.0, theme::ACCENT), StrokeKind::Inside);
                    if !pointer_down {
                        let hits = clip_rects
                            .iter()
                            .filter(|(_, r)| r.intersects(sel))
                            .map(|(c, _)| *c)
                            .collect();
                        actions.push(Action::MarqueeFinish { hits, additive: m.additive });
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
                        p.rect_filled(r, 8.0, Color32::from_rgb(110, 30, 40));
                        p.rect_stroke(r, 8.0, Stroke::new(1.5, theme::RECORD), StrokeKind::Inside);
                        p.text(
                            r.left_top() + Vec2::new(10.0, 6.0),
                            Align2::LEFT_TOP,
                            "● REC",
                            FontId::proportional(11.5),
                            theme::RECORD,
                        );
                    }
                }

                // Playhead
                let px = beat_x(playhead);
                if px >= timeline_left {
                    p.line_segment(
                        [Pos2::new(px, origin.y + RULER_H), Pos2::new(px, lanes_bottom)],
                        Stroke::new(5.0, Color32::from_white_alpha(18)),
                    );
                    p.line_segment(
                        [Pos2::new(px, origin.y + RULER_H), Pos2::new(px, lanes_bottom)],
                        Stroke::new(1.5, theme::PLAYHEAD),
                    );
                }

                // Ruler (sticky to the top of the viewport)
                let ruler_y = origin.y + viewport.min.y;
                let ruler = Rect::from_min_size(Pos2::new(timeline_left, ruler_y), Vec2::new(timeline_w, RULER_H));
                p.rect_filled(ruler, 0.0, theme::PANEL_BG);
                p.line_segment(
                    [Pos2::new(timeline_left, ruler_y + RULER_H), Pos2::new(origin.x + content_w, ruler_y + RULER_H)],
                    Stroke::new(1.0, Color32::from_black_alpha(160)),
                );
                for bar in 0..=(max_beats / 4) {
                    let x = beat_x(bar as f32 * 4.0);
                    p.line_segment(
                        [Pos2::new(x, ruler_y + 4.0), Pos2::new(x, ruler_y + RULER_H)],
                        Stroke::new(1.0, Color32::from_gray(96)),
                    );
                    p.text(
                        Pos2::new(x + 6.0, ruler_y + 6.0),
                        Align2::LEFT_TOP,
                        format!("{}", bar + 1),
                        FontId::proportional(11.5),
                        theme::TEXT_DIM,
                    );
                }
                if ppb >= 16.0 {
                    for b in (0..=max_beats).filter(|b| b % 4 != 0) {
                        let x = beat_x(b as f32);
                        p.line_segment(
                            [Pos2::new(x, ruler_y + RULER_H - 5.0), Pos2::new(x, ruler_y + RULER_H)],
                            Stroke::new(1.0, Color32::from_gray(62)),
                        );
                    }
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
                            Pos2::new(px - 6.0, ruler_y + 1.0),
                            Pos2::new(px + 6.0, ruler_y + 1.0),
                            Pos2::new(px, ruler_y + 10.0),
                        ],
                        theme::PLAYHEAD,
                        Stroke::NONE,
                    ));
                }

                // Track headers (sticky to the left edge of the viewport)
                let sx = origin.x + viewport.min.x;
                p.rect_filled(
                    Rect::from_min_max(Pos2::new(sx, lanes_bottom), Pos2::new(sx + HEADER_W, origin.y + content_h)),
                    0.0,
                    theme::PANEL_BG,
                );
                for (i, track) in project.tracks.iter_mut().enumerate() {
                    let top = lane_top(i);
                    let id = track.id;
                    let color = track_color(track.color);
                    let hr = Rect::from_min_size(Pos2::new(sx, top), Vec2::new(HEADER_W, LANE_H));
                    let sel = track_sel.contains(&id) || selected_track == Some(id);
                    p.rect_filled(hr, 0.0, mix(theme::CARD, color, if sel { 0.16 } else { 0.05 }));
                    p.rect_filled(
                        Rect::from_min_size(Pos2::new(sx + 4.0, top + 8.0), Vec2::new(4.0, LANE_H - 16.0)),
                        2.0,
                        color,
                    );
                    p.line_segment(
                        [Pos2::new(hr.right(), top), Pos2::new(hr.right(), top + LANE_H)],
                        Stroke::new(1.0, Color32::from_black_alpha(160)),
                    );
                    p.line_segment(
                        [Pos2::new(sx, top + LANE_H), Pos2::new(hr.right(), top + LANE_H)],
                        Stroke::new(1.0, Color32::from_black_alpha(110)),
                    );

                    // Whole header: click selects, double-click renames, right-click opens the menu.
                    let head = ui.interact(hr, ui.scope_id().with(("thead", id)), Sense::click());
                    if head.clicked() {
                        actions.push(Action::SelectTrack { id, shift });
                    }
                    if head.double_clicked() {
                        actions.push(Action::Rename(Some(id)));
                    }
                    let n_sel = if track_sel.contains(&id) { track_sel.len() } else { 1 };
                    head.context_menu(|ui| track_menu(ui, id, n_sel, &mut actions));

                    let name_rect = Rect::from_min_size(Pos2::new(sx + 16.0, top + 8.0), Vec2::new(HEADER_W - 16.0 - 66.0, 22.0));
                    if renaming == Some(id) {
                        let te = ui.put(
                            name_rect,
                            egui::TextEdit::singleline(&mut track.name).font(FontId::proportional(13.0)),
                        );
                        let key = ui.scope_id().with(("renamefocus", id));
                        let started: bool = ui.data(|d| d.get_temp(key).unwrap_or(false));
                        if !started {
                            te.request_focus();
                            ui.data_mut(|d| d.insert_temp(key, true));
                        }
                        if te.lost_focus() {
                            if track.name.trim().is_empty() {
                                track.name = kind_label(track.kind).to_string();
                            }
                            actions.push(Action::Rename(None));
                            ui.data_mut(|d| d.remove::<bool>(key));
                        }
                    } else {
                        p.with_clip_rect(name_rect).text(
                            Pos2::new(name_rect.left(), top + 10.0),
                            Align2::LEFT_TOP,
                            &track.name,
                            FontId::proportional(13.5),
                            theme::TEXT,
                        );
                    }
                    p.text(
                        Pos2::new(sx + 16.0, top + 31.0),
                        Align2::LEFT_TOP,
                        kind_label(track.kind),
                        FontId::proportional(10.5),
                        theme::TEXT_DIM,
                    );

                    // Delete (trash) and options (dots): both work with a plain left click.
                    let trash = Rect::from_min_size(Pos2::new(sx + HEADER_W - 34.0, top + 8.0), Vec2::splat(24.0));
                    let tr = ui.interact(trash, ui.scope_id().with(("tdel", id)), Sense::click());
                    if tr.hovered() {
                        p.rect_filled(trash, 6.0, Color32::from_rgba_unmultiplied(236, 72, 82, 46));
                    }
                    theme::paint_icon(&p, trash.shrink(5.0), Icon::Trash, if tr.hovered() { theme::RECORD } else { theme::TEXT_DIM });
                    if tr.clicked() {
                        actions.push(Action::DeleteTrack(id));
                    }
                    tr.on_hover_text(if n_sel > 1 { "Delete selected tracks" } else { "Delete track" });

                    let dots = Rect::from_min_size(Pos2::new(sx + HEADER_W - 60.0, top + 8.0), Vec2::splat(24.0));
                    let dr = ui.interact(dots, ui.scope_id().with(("tdots", id)), Sense::click());
                    if dr.hovered() {
                        p.rect_filled(dots, 6.0, theme::SURFACE_HI);
                    }
                    theme::paint_icon(&p, dots.shrink(5.0), Icon::Dots, if dr.hovered() { theme::TEXT } else { theme::TEXT_DIM });
                    egui::Popup::menu(&dr).show(|ui| track_menu(ui, id, n_sel, &mut actions));

                    let mute_r = Rect::from_min_size(Pos2::new(sx + 16.0, top + 54.0), Vec2::new(28.0, 20.0));
                    let solo_r = Rect::from_min_size(Pos2::new(sx + 50.0, top + 54.0), Vec2::new(28.0, 20.0));
                    for (r, label, on, on_color, key) in [
                        (mute_r, "M", track.mute, theme::MUTE_ON, "mute"),
                        (solo_r, "S", track.solo, theme::SOLO_ON, "solo"),
                    ] {
                        let resp = ui.interact(r, ui.scope_id().with((key, id)), Sense::click());
                        let off = if resp.hovered() { Color32::from_rgb(70, 70, 86) } else { Color32::from_rgb(52, 52, 66) };
                        p.rect_filled(r, 5.0, if on { on_color } else { off });
                        p.text(
                            r.center(),
                            Align2::CENTER_CENTER,
                            label,
                            FontId::proportional(11.5),
                            if on { Color32::from_rgb(18, 18, 22) } else { theme::TEXT_DIM },
                        );
                        if resp.clicked() {
                            actions.push(if key == "mute" { Action::ToggleMute(id) } else { Action::ToggleSolo(id) });
                        }
                        resp.on_hover_text(if key == "mute" { "Mute" } else { "Solo" });
                    }

                    let knob_r = Rect::from_min_size(Pos2::new(sx + 90.0, top + 51.0), Vec2::splat(26.0));
                    ui.put(knob_r, |ui: &mut egui::Ui| {
                        theme::knob(ui, &mut track.pan, -1.0, 1.0, 0.0, 26.0, "Pan (drag, double-click to centre)")
                    });
                    let fader_r = Rect::from_min_max(
                        Pos2::new(sx + 128.0, top + 58.0),
                        Pos2::new(sx + HEADER_W - 16.0, top + 70.0),
                    );
                    let vol_id = ui.scope_id().with(("vol", id));
                    theme::fader(ui, vol_id, fader_r, &mut track.volume, 0.0, 1.5, color);
                    let level = levels.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
                    let meter_bg = Rect::from_min_max(Pos2::new(fader_r.left(), top + 73.0), Pos2::new(fader_r.right(), top + 76.0));
                    p.rect_filled(meter_bg, 1.5, theme::WELL);
                    if level > 0.002 {
                        let mut fill = meter_bg;
                        fill.max.x = meter_bg.left() + meter_bg.width() * level;
                        p.rect_filled(fill, 1.5, mix(Color32::from_rgb(70, 200, 110), Color32::from_rgb(236, 72, 82), level));
                    }
                }

                // "Add track" row under the last header
                let add_r = Rect::from_min_size(Pos2::new(sx + 14.0, lanes_bottom + 14.0), Vec2::new(HEADER_W - 28.0, 36.0));
                let ar = ui.interact(add_r, ui.scope_id().with("addrow"), Sense::click());
                p.rect_filled(add_r, 10.0, if ar.hovered() { theme::SURFACE_HI } else { theme::SURFACE });
                p.rect_stroke(add_r, 10.0, Stroke::new(1.0, theme::BORDER), StrokeKind::Inside);
                theme::paint_icon(
                    &p,
                    Rect::from_center_size(add_r.left_center() + Vec2::new(22.0, 0.0), Vec2::splat(16.0)),
                    Icon::Plus,
                    theme::ACCENT,
                );
                p.text(
                    add_r.left_center() + Vec2::new(40.0, 0.0),
                    Align2::LEFT_CENTER,
                    "Add track",
                    FontId::proportional(13.0),
                    theme::TEXT,
                );
                egui::Popup::menu(&ar).show(|ui| add_track_menu(ui, &mut actions));

                // Header corner (always visible): title and a quick "+ Track" button
                p.rect_filled(
                    Rect::from_min_size(Pos2::new(sx, ruler_y), Vec2::new(HEADER_W, RULER_H)),
                    0.0,
                    theme::PANEL_BG,
                );
                p.line_segment(
                    [Pos2::new(sx, ruler_y + RULER_H), Pos2::new(sx + HEADER_W, ruler_y + RULER_H)],
                    Stroke::new(1.0, Color32::from_black_alpha(160)),
                );
                p.text(
                    Pos2::new(sx + 16.0, ruler_y + 8.0),
                    Align2::LEFT_TOP,
                    "TRACKS",
                    FontId::proportional(10.0),
                    theme::TEXT_DIM,
                );
                let plus = Rect::from_min_size(Pos2::new(sx + HEADER_W - 82.0, ruler_y + 3.0), Vec2::new(70.0, RULER_H - 6.0));
                let pr = ui.interact(plus, ui.scope_id().with("addtrack"), Sense::click());
                p.rect_filled(plus, 9.0, if pr.hovered() { mix(theme::ACCENT, Color32::WHITE, 0.2) } else { theme::ACCENT });
                p.text(plus.center(), Align2::CENTER_CENTER, "+ Track", FontId::proportional(11.5), theme::WINDOW_BG);
                egui::Popup::menu(&pr).show(|ui| add_track_menu(ui, &mut actions));

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

/// Context/popup menu for a track header.
fn track_menu(ui: &mut egui::Ui, id: u32, selected: usize, actions: &mut Vec<Action>) {
    ui.set_min_width(180.0);
    if ui.button("Rename").clicked() {
        actions.push(Action::Rename(Some(id)));
        ui.close();
    }
    if ui.button("Duplicate track").clicked() {
        actions.push(Action::DuplicateTrack(id));
        ui.close();
    }
    ui.separator();
    let label = if selected > 1 { format!("Delete {selected} tracks") } else { "Delete track".to_string() };
    if ui.add(egui::Button::new(RichText::new(label).color(theme::RECORD))).clicked() {
        actions.push(Action::DeleteTrack(id));
        ui.close();
    }
}

/// Menu used by every "+ Track" control.
fn add_track_menu(ui: &mut egui::Ui, actions: &mut Vec<Action>) {
    ui.set_min_width(180.0);
    if ui.button("Synth track").clicked() {
        actions.push(Action::AddTrack(TrackKind::Synth));
        ui.close();
    }
    if ui.button("Drum kit track").clicked() {
        actions.push(Action::AddTrack(TrackKind::Drums));
        ui.close();
    }
    if ui.button("Record audio (R)").clicked() {
        actions.push(Action::Record);
        ui.close();
    }
}

impl eframe::App for BandCraftApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // The song always covers every clip, so nothing placed late is silently skipped.
        let bpm = self.project.bpm;
        let end = self
            .project
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| c.start + c.length_beats(bpm))
            .fold(0.0f32, f32::max);
        let needed = (end / 4.0).ceil() as u32;
        if needed > self.project.bars {
            self.project.bars = needed;
        }
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
                    record: i.key_pressed(Key::R) && !cmd && !shift,
                    midi_record: i.key_pressed(Key::R) && !cmd && shift,
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
            if keys.midi_record {
                self.toggle_midi_record();
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
                self.delete_selection();
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

        self.midi.set_context(ctx);
        self.pump_midi();
        self.send(AudioCmd::SetProject(Arc::new(self.project.clone())));
        if self.is_playing() {
            ctx.request_repaint();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("menu")
            .frame(egui::Frame::new().fill(theme::WINDOW_BG).inner_margin(Margin::symmetric(8, 3)))
            .show(ui, |ui| self.menu_ui(ui));
        egui::Panel::top("transport")
            .frame(egui::Frame::new().fill(theme::PANEL_BG).inner_margin(Margin::symmetric(12, 8)))
            .show(ui, |ui| self.transport_ui(ui));
        egui::Panel::bottom("status")
            .frame(egui::Frame::new().fill(theme::WINDOW_BG).inner_margin(Margin::symmetric(12, 4)))
            .show(ui, |ui| self.status_ui(ui));
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
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::WINDOW_BG))
            .show(ui, |ui| self.arrangement_ui(ui));
    }
}

/// Keyboard state gathered in one pass.
struct Keys {
    space: bool,
    undo: bool,
    redo: bool,
    delete: bool,
    record: bool,
    midi_record: bool,
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
    p.rect_filled(r.translate(Vec2::new(0.0, 2.0)), 8.0, Color32::from_black_alpha(85));
    p.rect_filled(r, 8.0, mix(Color32::from_rgb(22, 22, 29), base, 0.34));
    let head = Rect::from_min_size(r.min, Vec2::new(r.width(), 20.0f32.min(r.height())));
    p.rect_filled(head, CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 }, base);
    if r.width() > 20.0 {
        p.line_segment(
            [head.left_top() + Vec2::new(8.0, 1.0), head.right_top() + Vec2::new(-8.0, 1.0)],
            Stroke::new(1.0, Color32::from_white_alpha(46)),
        );
    }
    let cp = p.with_clip_rect(r);
    cp.text(
        r.left_top() + Vec2::new(9.0, 4.0),
        Align2::LEFT_TOP,
        &clip.name,
        FontId::proportional(11.5),
        Color32::WHITE,
    );
    let area = Rect::from_min_max(r.min + Vec2::new(0.0, 22.0), r.max - Vec2::new(0.0, 5.0));
    match &clip.content {
        ClipContent::Notes(notes) => {
            draw_notes(&cp, area, notes, kind, ppb);
            if notes.is_empty() && r.width() > 110.0 {
                cp.text(
                    area.center(),
                    Align2::CENTER_CENTER,
                    "Empty: click to edit",
                    FontId::proportional(11.0),
                    Color32::from_white_alpha(90),
                );
            }
        }
        ClipContent::Audio { data: Some(d), .. } => draw_wave(&cp, area, d, bpm, ppb),
        ClipContent::Audio { data: None, .. } => {}
    }
    if matches!(clip.content, ClipContent::Notes(_)) && r.width() > 24.0 {
        let x = r.right() - 5.0;
        let cy = r.center().y + 9.0;
        for dx in [-1.5, 1.5] {
            p.line_segment(
                [Pos2::new(x + dx, cy - 7.0), Pos2::new(x + dx, cy + 7.0)],
                Stroke::new(1.0, Color32::from_white_alpha(96)),
            );
        }
    }
    if selected {
        p.rect_stroke(r.expand(2.5), 10.0, Stroke::new(2.0, Color32::from_rgba_unmultiplied(52, 211, 196, 120)), StrokeKind::Outside);
        p.rect_stroke(r, 8.0, Stroke::new(1.8, Color32::WHITE), StrokeKind::Inside);
    } else {
        p.rect_stroke(r, 8.0, Stroke::new(1.0, Color32::from_black_alpha(120)), StrokeKind::Inside);
    }
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
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 46.0), Sense::click());
    let p = ui.painter();
    let bg = if selected {
        mix(theme::SURFACE_HI, color, 0.18)
    } else if resp.hovered() {
        theme::SURFACE
    } else {
        theme::CARD
    };
    p.rect_filled(rect, 10.0, bg);
    p.rect_filled(
        Rect::from_min_size(rect.left_top() + Vec2::new(8.0, 10.0), Vec2::new(4.0, rect.height() - 20.0)),
        2.0,
        color,
    );
    p.text(rect.left_top() + Vec2::new(22.0, 9.0), Align2::LEFT_TOP, name, FontId::proportional(13.0), theme::TEXT);
    p.text(rect.left_top() + Vec2::new(22.0, 27.0), Align2::LEFT_TOP, sub, FontId::proportional(10.5), theme::TEXT_DIM);
    if selected {
        p.rect_stroke(rect, 10.0, Stroke::new(1.2, color), StrokeKind::Inside);
    } else if resp.hovered() {
        p.rect_stroke(rect, 10.0, Stroke::new(1.0, theme::BORDER), StrokeKind::Inside);
    }
    ui.add_space(2.0);
    resp
}
