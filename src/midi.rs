//! MIDI: live keyboard input (midir) and Standard MIDI File import/export (midly).

use crate::project::{ClipContent, DRUM_KIT, INSTRUMENT_CATEGORIES, INSTRUMENT_PRESETS, Note, TrackKind, Project};
use midir::{Ignore, MidiInput, MidiInputConnection};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind, num::{u4, u7, u15, u24, u28}};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};

const EXPORT_TPB: u16 = 480;
const MAX_IMPORT_TRACKS: usize = 32;

/// A note message from a connected MIDI keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiEvent {
    pub on: bool,
    pub pitch: u8,
    pub vel: u8,
}

/// Live MIDI input. Events arrive on `rx` and are drained once per frame.
pub struct MidiIn {
    conn: Option<MidiInputConnection<()>>,
    tx: Sender<MidiEvent>,
    pub rx: Receiver<MidiEvent>,
    pub connected: Option<String>,
    pub ports: Vec<String>,
    pub error: Option<String>,
    /// Lets the MIDI thread wake the UI when a message arrives.
    wake: Arc<OnceLock<eframe::egui::Context>>,
}

impl MidiIn {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let mut m = Self { conn: None, tx, rx, connected: None, ports: Vec::new(), error: None, wake: Arc::new(OnceLock::new()) };
        m.rescan();
        if let Some(first) = m.ports.first().cloned() {
            m.connect(&first);
        }
        m
    }

    /// Refreshes the list of available MIDI input devices.
    pub fn rescan(&mut self) {
        match MidiInput::new("BandCraft") {
            Ok(input) => {
                self.ports = input.ports().iter().filter_map(|p| input.port_name(p).ok()).collect();
                self.error = None;
            }
            Err(e) => {
                self.ports.clear();
                self.error = Some(e.to_string());
            }
        }
    }

    pub fn connect(&mut self, name: &str) {
        self.conn = None;
        self.connected = None;
        let mut input = match MidiInput::new("BandCraft") {
            Ok(i) => i,
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        input.ignore(Ignore::None);
        let Some(port) = input.ports().into_iter().find(|p| input.port_name(p).is_ok_and(|n| n == name)) else {
            self.error = Some(format!("MIDI device '{name}' not found"));
            return;
        };
        let tx = self.tx.clone();
        let wake = Arc::clone(&self.wake);
        match input.connect(
            &port,
            "bandcraft-input",
            move |_stamp, msg, ()| {
                if let Some(ev) = parse_message(msg) {
                    let _ = tx.send(ev);
                    if let Some(ctx) = wake.get() {
                        ctx.request_repaint();
                    }
                }
            },
            (),
        ) {
            Ok(conn) => {
                self.conn = Some(conn);
                self.connected = Some(name.to_string());
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Gives the MIDI thread a handle to wake the UI (call once).
    pub fn set_context(&self, ctx: &eframe::egui::Context) {
        let _ = self.wake.set(ctx.clone());
    }

    pub fn disconnect(&mut self) {
        self.conn = None;
        self.connected = None;
    }
}

/// Parses a raw MIDI message into a note event (anything else is ignored).
pub fn parse_message(msg: &[u8]) -> Option<MidiEvent> {
    let (&status, rest) = msg.split_first()?;
    let (&pitch, rest) = rest.split_first()?;
    let vel = rest.first().copied().unwrap_or(0);
    match status & 0xF0 {
        0x90 if vel > 0 => Some(MidiEvent { on: true, pitch: pitch & 0x7F, vel: vel & 0x7F }),
        0x90 | 0x80 => Some(MidiEvent { on: false, pitch: pitch & 0x7F, vel: 0 }),
        _ => None,
    }
}

// ----- files -------------------------------------------------------------------------

/// One note track found in a MIDI file.
pub struct ImportedTrack {
    pub name: String,
    pub drums: bool,
    /// General MIDI program (0-127), used to pick a matching built-in instrument.
    pub program: u8,
    pub notes: Vec<Note>,
}

pub struct MidiImport {
    pub bpm: Option<f32>,
    pub tracks: Vec<ImportedTrack>,
}

/// Maps a General MIDI percussion key to the nearest pad of the built-in kit.
pub fn gm_drum_to_kit(key: u8) -> Option<u8> {
    Some(match key {
        35 | 36 => 36,
        37 => 37,
        38 | 40 => 38,
        39 => 39,
        42 | 44 => 42,
        46 => 46,
        41 | 43 | 45 | 47 | 48 | 50 => 45,
        49 | 51 | 52 | 53 | 55 | 57 | 59 => 46,
        54 | 56 | 58 => 56,
        _ => return None,
    })
}

/// Keeps melodic notes inside the range the piano roll can show.
pub fn fold_pitch(mut p: i32) -> u8 {
    while p < i32::from(crate::roll::PITCH_LOW) {
        p += 12;
    }
    while p > i32::from(crate::roll::PITCH_HIGH) {
        p -= 12;
    }
    p as u8
}

pub fn import_smf(bytes: &[u8]) -> Result<MidiImport, String> {
    let smf = Smf::parse(bytes).map_err(|e| format!("Not a MIDI file: {e}"))?;
    let tpb = match smf.header.timing {
        Timing::Metrical(t) => f32::from(t.as_int()).max(1.0),
        Timing::Timecode(fps, sub) => (fps.as_f32() * f32::from(sub)).max(1.0) / 2.0,
    };
    let mut bpm = None;
    let mut out: Vec<ImportedTrack> = Vec::new();
    for track in &smf.tracks {
        let mut tick: u64 = 0;
        let mut name = String::new();
        // (channel) -> pitch -> (start tick, velocity)
        let mut open: HashMap<(u8, u8), (u64, u8)> = HashMap::new();
        let mut by_channel: HashMap<u8, ImportedTrack> = HashMap::new();
        let mut programs: HashMap<u8, u8> = HashMap::new();
        let finish = |chan: u8, key: u8, start: u64, vel: u8, end: u64, by: &mut HashMap<u8, ImportedTrack>, programs: &HashMap<u8, u8>, name: &str| {
            let drums = chan == 9;
            let pitch = if drums {
                match gm_drum_to_kit(key) {
                    Some(p) => p,
                    None => return,
                }
            } else {
                fold_pitch(i32::from(key))
            };
            let entry = by.entry(chan).or_insert_with(|| ImportedTrack {
                name: if name.is_empty() { format!("MIDI ch {}", chan + 1) } else { name.to_string() },
                drums,
                program: programs.get(&chan).copied().unwrap_or(0),
                notes: Vec::new(),
            });
            let s = start as f32 / tpb;
            let len = if drums { 0.25 } else { ((end.saturating_sub(start)) as f32 / tpb).max(0.05) };
            entry.notes.push(Note { pitch, start: s, length: len, velocity: vel.max(1) });
        };
        for ev in track {
            tick += u64::from(ev.delta.as_int());
            match ev.kind {
                TrackEventKind::Meta(MetaMessage::TrackName(n)) if name.is_empty() => {
                    name = String::from_utf8_lossy(n).trim().to_string();
                }
                TrackEventKind::Meta(MetaMessage::Tempo(t)) if bpm.is_none() => {
                    let micros = t.as_int().max(1);
                    bpm = Some(60_000_000.0 / micros as f32);
                }
                TrackEventKind::Midi { channel, message } => {
                    let ch = channel.as_int();
                    match message {
                        MidiMessage::ProgramChange { program } => {
                            programs.insert(ch, program.as_int());
                        }
                        MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                            let k = key.as_int();
                            if let Some((s, v)) = open.insert((ch, k), (tick, vel.as_int())) {
                                finish(ch, k, s, v, tick, &mut by_channel, &programs, &name);
                            }
                        }
                        MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                            let k = key.as_int();
                            if let Some((s, v)) = open.remove(&(ch, k)) {
                                finish(ch, k, s, v, tick, &mut by_channel, &programs, &name);
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        let leftovers: Vec<((u8, u8), (u64, u8))> = open.into_iter().collect();
        for ((ch, k), (s, v)) in leftovers {
            finish(ch, k, s, v, tick, &mut by_channel, &programs, &name);
        }
        let mut channels: Vec<_> = by_channel.into_iter().collect();
        channels.sort_by_key(|(c, _)| *c);
        out.extend(channels.into_iter().map(|(_, t)| t));
    }
    out.retain(|t| !t.notes.is_empty());
    if out.is_empty() {
        return Err("The MIDI file has no notes".into());
    }
    out.truncate(MAX_IMPORT_TRACKS);
    for t in &mut out {
        t.notes.sort_by(|a, b| a.start.total_cmp(&b.start));
    }
    Ok(MidiImport { bpm: bpm.map(|b| b.clamp(40.0, 240.0)), tracks: out })
}

/// Picks a built-in instrument preset that roughly matches a General MIDI program number.
pub fn preset_for_program(program: u8) -> &'static str {
    let category = match program {
        0..=7 => "Keys",
        8..=15 => "Bells & Mallets",
        16..=23 => "Organs",
        24..=31 => "Plucked",
        32..=39 => "Bass",
        40..=55 | 88..=103 => "Pads",
        56..=87 => "Leads",
        _ => "Keys",
    };
    debug_assert!(INSTRUMENT_CATEGORIES.contains(&category));
    INSTRUMENT_PRESETS
        .iter()
        .find(|(_, c)| *c == category)
        .map_or("Keys", |(n, _)| *n)
}

/// Writes every note track of the project to a format-1 Standard MIDI File.
pub fn export_smf(project: &Project) -> Vec<u8> {
    let mut smf = Smf::new(Header::new(Format::Parallel, Timing::Metrical(u15::new(EXPORT_TPB))));
    let micros = (60_000_000.0 / project.bpm.max(1.0)) as u32;
    smf.tracks.push(vec![
        TrackEvent { delta: u28::new(0), kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(micros))) },
        TrackEvent { delta: u28::new(0), kind: TrackEventKind::Meta(MetaMessage::TimeSignature(4, 2, 24, 8)) },
        TrackEvent { delta: u28::new(0), kind: TrackEventKind::Meta(MetaMessage::EndOfTrack) },
    ]);
    let mut melodic_channel = 0u8;
    for track in &project.tracks {
        if track.kind == TrackKind::Audio {
            continue;
        }
        let drums = track.kind == TrackKind::Drums;
        let channel = if drums {
            9
        } else {
            let c = melodic_channel;
            melodic_channel = (melodic_channel + 1) % 9; // channels 0-8, channel 9 is percussion
            c
        };
        // (absolute tick, is_on, key, velocity)
        let mut events: Vec<(u64, bool, u8, u8)> = Vec::new();
        for clip in &track.clips {
            let ClipContent::Notes(notes) = &clip.content else {
                continue;
            };
            for n in notes {
                if n.start >= clip.length {
                    continue;
                }
                let on = ((f64::from(clip.start) + f64::from(n.start)) * f64::from(EXPORT_TPB)).round() as u64;
                let len = ((f64::from(n.length) * f64::from(EXPORT_TPB)).round() as u64).max(1);
                events.push((on, true, n.pitch.min(127), n.velocity.clamp(1, 127)));
                events.push((on + len, false, n.pitch.min(127), 0));
            }
        }
        // Note-offs sort before note-ons at the same tick so repeated notes stay separate.
        events.sort_by_key(|e| (e.0, e.1));
        let mut data: Vec<TrackEvent> = vec![TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::TrackName(track.name.as_bytes())),
        }];
        let mut last = 0u64;
        for (tick, on, key, vel) in events {
            let delta = (tick - last) as u32;
            last = tick;
            let message = if on {
                MidiMessage::NoteOn { key: u7::new(key), vel: u7::new(vel) }
            } else {
                MidiMessage::NoteOff { key: u7::new(key), vel: u7::new(0) }
            };
            data.push(TrackEvent {
                delta: u28::new(delta),
                kind: TrackEventKind::Midi { channel: u4::new(channel), message },
            });
        }
        data.push(TrackEvent { delta: u28::new(0), kind: TrackEventKind::Meta(MetaMessage::EndOfTrack) });
        smf.tracks.push(data);
    }
    let mut bytes = Vec::new();
    // Writing into a Vec cannot fail.
    let _ = smf.write_std(&mut bytes);
    bytes
}

/// General MIDI percussion key for a kit pad (the kit already uses GM numbers).
#[allow(dead_code)]
pub fn kit_pitch(slot: usize) -> u8 {
    DRUM_KIT[slot.min(DRUM_KIT.len() - 1)].0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_note_messages() {
        assert_eq!(parse_message(&[0x90, 60, 100]), Some(MidiEvent { on: true, pitch: 60, vel: 100 }));
        assert_eq!(parse_message(&[0x90, 60, 0]), Some(MidiEvent { on: false, pitch: 60, vel: 0 }));
        assert_eq!(parse_message(&[0x83, 61, 40]), Some(MidiEvent { on: false, pitch: 61, vel: 0 }));
        assert_eq!(parse_message(&[0xB0, 64, 127]), None);
    }

    #[test]
    fn export_then_import_keeps_notes() {
        let mut p = Project::starter();
        p.bpm = 100.0;
        let bytes = export_smf(&p);
        let back = import_smf(&bytes).expect("valid file");
        assert!((back.bpm.unwrap_or(0.0) - 100.0).abs() < 0.5);
        assert_eq!(back.tracks.len(), 3);
        assert!(back.tracks.iter().any(|t| t.drums));
        let original: usize = p.tracks[1]
            .clips
            .iter()
            .map(|c| match &c.content {
                ClipContent::Notes(n) => n.len(),
                ClipContent::Audio { .. } => 0,
            })
            .sum();
        assert_eq!(back.tracks[1].notes.len(), original);
    }

    #[test]
    fn rejects_garbage() {
        assert!(import_smf(b"not midi").is_err());
    }
}
