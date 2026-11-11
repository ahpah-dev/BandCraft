//! Piano roll: note editor with variable lengths, snapping, piano keys, velocity lane,
//! box selection, scale highlighting and note audition.

use crate::engine::AudioCmd;
use crate::project::{ClipContent, DRUM_KIT, Note, TrackKind, note_name};
use crate::theme::{self, mix, track_color};
use crate::ui::{Action, BandCraftApp, ClipRef, empty_state};
use eframe::egui::{self, Align2, Color32, CursorIcon, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};
use std::collections::HashSet;

pub(crate) const PITCH_LOW: u8 = 24;
pub(crate) const PITCH_HIGH: u8 = 96;
const KEYS_W: f32 = 66.0;
const RULER_H: f32 = 22.0;
const VEL_H: f32 = 62.0;
const EDGE: f32 = 7.0;

/// Snap options: (label, subdivisions per beat).
pub(crate) const SNAPS: [(&str, u32); 4] = [("1/4", 1), ("1/8", 2), ("1/16", 4), ("1/32", 8)];

/// Scale highlighting: (name, semitone offsets from the root).
pub(crate) const SCALES: [(&str, &[u8]); 6] = [
    ("No scale", &[]),
    ("Major", &[0, 2, 4, 5, 7, 9, 11]),
    ("Minor", &[0, 2, 3, 5, 7, 8, 10]),
    ("Pentatonic", &[0, 3, 5, 7, 10]),
    ("Blues", &[0, 3, 5, 6, 7, 10]),
    ("Dorian", &[0, 2, 3, 5, 7, 9, 10]),
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tool {
    Draw,
    Select,
}

/// Mouse operations in progress inside the piano roll. Indices refer to the clip's note list.
pub(crate) enum NoteOp {
    Move { grab_beat: f32, grab_row: i32, idxs: Vec<usize>, originals: Vec<Note> },
    Resize { grab_beat: f32, idxs: Vec<usize>, originals: Vec<Note> },
    Create { idx: usize },
    Marquee { start: Pos2, additive: bool },
}

fn rows_for(drums: bool) -> Vec<u8> {
    if drums {
        DRUM_KIT.iter().map(|d| d.0).collect()
    } else {
        (PITCH_LOW..=PITCH_HIGH).rev().collect()
    }
}

fn is_black(pitch: u8) -> bool {
    matches!(pitch % 12, 1 | 3 | 6 | 8 | 10)
}

impl BandCraftApp {
    /// Runs `f` on the notes of the clip being edited.
    #[allow(clippy::type_complexity)]
    fn with_clip_notes<R>(
        &mut self,
        f: impl FnOnce(&mut Vec<Note>, &mut HashSet<usize>, f32, bool, f32) -> R,
    ) -> Option<R> {
        let c = self.selected_clip?;
        let unit = 1.0 / self.snap_div as f32;
        let sel = &mut self.note_sel;
        let track = self.project.track_mut(c.track)?;
        let drums = track.kind == TrackKind::Drums;
        let clip = track.clips.iter_mut().find(|k| k.id == c.clip)?;
        let len = clip.length;
        let ClipContent::Notes(notes) = &mut clip.content else {
            return None;
        };
        sel.retain(|&i| i < notes.len());
        Some(f(notes, sel, len, drums, unit))
    }

    pub(crate) fn delete_selected_notes(&mut self) {
        self.with_clip_notes(|notes, sel, _, _, _| {
            let mut idxs: Vec<usize> = sel.drain().collect();
            idxs.sort_unstable_by(|a, b| b.cmp(a));
            for i in idxs {
                if i < notes.len() {
                    notes.remove(i);
                }
            }
        });
    }

    pub(crate) fn select_all_notes(&mut self) {
        self.with_clip_notes(|notes, sel, _, _, _| {
            *sel = (0..notes.len()).collect();
        });
        self.note_sel_clip = self.selected_clip;
    }

    /// Moves selected notes by `dpitch` rows/semitones and `dsteps` snap units.
    pub(crate) fn nudge_selected_notes(&mut self, dpitch: i32, dsteps: i32) {
        self.with_clip_notes(|notes, sel, len, drums, unit| {
            let rows = rows_for(drums);
            for &i in sel.iter() {
                let n = &mut notes[i];
                if drums {
                    let row = rows.iter().position(|&p| p == n.pitch).unwrap_or(0) as i32;
                    let new_row = (row - dpitch).clamp(0, rows.len() as i32 - 1);
                    n.pitch = rows[new_row as usize];
                } else {
                    n.pitch = (i32::from(n.pitch) + dpitch).clamp(i32::from(PITCH_LOW), i32::from(PITCH_HIGH)) as u8;
                }
                n.start = (n.start + dsteps as f32 * unit).clamp(0.0, (len - n.length).max(0.0));
            }
        });
    }

    /// Snaps the start of selected notes (or all notes when none are selected) to the grid.
    pub(crate) fn quantize_notes(&mut self) {
        self.with_clip_notes(|notes, sel, len, _, unit| {
            for (i, n) in notes.iter_mut().enumerate() {
                if sel.is_empty() || sel.contains(&i) {
                    n.start = ((n.start / unit).round() * unit).clamp(0.0, (len - unit).max(0.0));
                    n.length = ((n.length / unit).round() * unit).max(unit);
                }
            }
        });
        self.status = "Quantized notes to the grid".into();
    }

    pub(crate) fn transpose_notes(&mut self, semitones: i32) {
        self.with_clip_notes(|notes, sel, _, drums, _| {
            if drums {
                return;
            }
            for (i, n) in notes.iter_mut().enumerate() {
                if sel.is_empty() || sel.contains(&i) {
                    n.pitch = (i32::from(n.pitch) + semitones).clamp(i32::from(PITCH_LOW), i32::from(PITCH_HIGH)) as u8;
                }
            }
        });
    }

    pub(crate) fn copy_notes(&mut self) {
        let copied = self.with_clip_notes(|notes, sel, _, _, _| {
            let picked: Vec<Note> = notes.iter().enumerate().filter(|(i, _)| sel.contains(i)).map(|(_, n)| *n).collect();
            let min = picked.iter().map(|n| n.start).fold(f32::MAX, f32::min);
            picked.into_iter().map(|mut n| {
                n.start -= min;
                n
            }).collect::<Vec<_>>()
        });
        if let Some(buf) = copied {
            if !buf.is_empty() {
                self.status = format!("Copied {} note(s)", buf.len());
                self.note_buf = buf;
            }
        }
    }

    /// Pastes copied notes at the last hovered position.
    pub(crate) fn paste_notes(&mut self) {
        let buf = self.note_buf.clone();
        let at = self.roll_hover.max(0.0);
        self.with_clip_notes(|notes, sel, len, _, unit| {
            let offset = (at / unit).round() * unit;
            sel.clear();
            for mut n in buf {
                n.start = (n.start + offset).clamp(0.0, (len - n.length).max(0.0));
                notes.push(n);
                sel.insert(notes.len() - 1);
            }
        });
    }

    /// Duplicates the selection directly after itself.
    pub(crate) fn duplicate_notes(&mut self) {
        self.with_clip_notes(|notes, sel, len, _, unit| {
            let picked: Vec<Note> = notes.iter().enumerate().filter(|(i, _)| sel.contains(i)).map(|(_, n)| *n).collect();
            if picked.is_empty() {
                return;
            }
            let min = picked.iter().map(|n| n.start).fold(f32::MAX, f32::min);
            let max = picked.iter().map(|n| n.start + n.length).fold(0.0, f32::max);
            let span = ((max - min) / unit).ceil() * unit;
            sel.clear();
            for mut n in picked {
                n.start = (n.start + span).clamp(0.0, (len - n.length).max(0.0));
                notes.push(n);
                sel.insert(notes.len() - 1);
            }
        });
    }

    // ----- piano roll panel ----------------------------------------------------------

    pub(crate) fn editor_ui(&mut self, ui: &mut egui::Ui) {
        let Some(cref) = self.selected_clip else {
            empty_state(
                ui,
                "Piano roll",
                "Click a clip in the arrangement to edit its notes here. Pick a loop on the left and click a lane to create one.",
            );
            return;
        };
        let info = self.project.track(cref.track).and_then(|t| {
            t.clips
                .iter()
                .find(|c| c.id == cref.clip)
                .map(|c| (t.kind, t.name.clone(), t.color, c.name.clone(), matches!(c.content, ClipContent::Notes(_))))
        });
        let Some((kind, track_name, color_idx, clip_name, is_notes)) = info else {
            ui.label("Clip not found.");
            return;
        };
        if kind == TrackKind::Audio || !is_notes {
            empty_state(ui, "Audio clip", "Audio clips play as recorded. Drag them in the arrangement to move them.");
            return;
        }
        if self.note_sel_clip != Some(cref) {
            self.note_sel.clear();
            self.note_sel_clip = Some(cref);
        }
        let accent = track_color(color_idx);
        self.roll_toolbar(ui, cref, &track_name, &clip_name, accent);
        self.roll_grid(ui, cref, kind, accent);
    }

    fn roll_toolbar(&mut self, ui: &mut egui::Ui, cref: ClipRef, track_name: &str, clip_name: &str, accent: Color32) {
        let mut delete_clip = false;
        ui.horizontal_wrapped(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 5.0, accent);
            ui.label(RichText::new(format!("{track_name}  ·  {clip_name}")).strong());
            ui.separator();
            ui.selectable_value(&mut self.tool, Tool::Draw, "Draw").on_hover_text("Click or drag to draw notes");
            ui.selectable_value(&mut self.tool, Tool::Select, "Select").on_hover_text("Drag to box-select notes");
            ui.separator();
            let snap_label = SNAPS.iter().find(|(_, d)| *d == self.snap_div).map_or("1/16", |s| s.0);
            egui::ComboBox::from_id_salt("roll_snap")
                .selected_text(format!("Snap {snap_label}"))
                .width(90.0)
                .show_ui(ui, |ui| {
                    for (label, div) in SNAPS {
                        ui.selectable_value(&mut self.snap_div, div, label);
                    }
                });
            ui.label(RichText::new("Zoom").small().color(theme::TEXT_DIM));
            ui.add(egui::Slider::new(&mut self.roll_ppb, 30.0..=260.0).show_value(false));
            ui.label(RichText::new("Height").small().color(theme::TEXT_DIM));
            ui.add(egui::Slider::new(&mut self.roll_rh, 10.0..=28.0).show_value(false));
            ui.separator();
            egui::ComboBox::from_id_salt("roll_scale")
                .selected_text(SCALES[self.scale_idx].0)
                .width(100.0)
                .show_ui(ui, |ui| {
                    for (i, (name, _)) in SCALES.iter().enumerate() {
                        ui.selectable_value(&mut self.scale_idx, i, *name);
                    }
                });
            if self.scale_idx > 0 {
                egui::ComboBox::from_id_salt("roll_root")
                    .selected_text(format!("Key {}", &note_name(self.scale_root)[..note_name(self.scale_root).len() - 1]))
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        for root in 0..12u8 {
                            let name = note_name(root + 12);
                            ui.selectable_value(&mut self.scale_root, root, &name[..name.len() - 1]);
                        }
                    });
            }
            ui.separator();
            if ui.button("Quantize").on_hover_text("Snap notes to the grid (Q)").clicked() {
                self.quantize_notes();
            }
            if ui.button("Oct −").clicked() {
                self.transpose_notes(-12);
            }
            if ui.button("Oct +").clicked() {
                self.transpose_notes(12);
            }
            if ui.button("Select all").clicked() {
                self.select_all_notes();
            }
            ui.separator();
            ui.label("Length");
            if let Some(clip) = self
                .project
                .track_mut(cref.track)
                .and_then(|t| t.clips.iter_mut().find(|c| c.id == cref.clip))
            {
                let mut bars = clip.length / 4.0;
                let unit = if (bars - 1.0).abs() < 0.01 { " bar" } else { " bars" };
                if ui
                    .add(egui::DragValue::new(&mut bars).range(1.0..=16.0).speed(0.1).fixed_decimals(0).suffix(unit))
                    .changed()
                {
                    clip.length = bars.round().max(1.0) * 4.0;
                }
            }
            if ui.button("Clear").clicked() {
                if let Some(clip) = self
                    .project
                    .track_mut(cref.track)
                    .and_then(|t| t.clips.iter_mut().find(|c| c.id == cref.clip))
                {
                    if let ClipContent::Notes(n) = &mut clip.content {
                        n.clear();
                    }
                }
                self.note_sel.clear();
            }
            if ui.button("Delete clip").clicked() {
                delete_clip = true;
            }
            ui.label(RichText::new(format!("{} selected", self.note_sel.len())).small().color(theme::TEXT_DIM));
        });
        if delete_clip {
            self.apply(Action::DeleteClips(vec![cref]));
        }
    }

    fn roll_grid(&mut self, ui: &mut egui::Ui, cref: ClipRef, kind: TrackKind, accent: Color32) {
        let drums = kind == TrackKind::Drums;
        let rows = rows_for(drums);
        let rh = if drums { 30.0 } else { self.roll_rh };
        let ppb = self.roll_ppb;
        let div = self.snap_div.max(1);
        let unit = 1.0 / div as f32;
        let tool = self.tool;
        let shift = ui.input(|i| i.modifiers.shift);
        let playhead = self.playhead_beats();
        let scale_set: &[u8] = SCALES[self.scale_idx].1;
        let scale_root = self.scale_root;
        let first_open = self.roll_for_clip != Some(cref);
        self.roll_for_clip = Some(cref);
        let avail_h = ui.available_height();
        let default_vel: u8 = if drums { 110 } else { 100 };
        let mut preview: Option<u8> = None;

        {
            let sel = &mut self.note_sel;
            let op = &mut self.note_op;
            let note_len = &mut self.note_len;
            let editor_focus = &mut self.editor_focus;
            let hover_store = &mut self.roll_hover;
            let Some(track) = self.project.track_mut(cref.track) else {
                return;
            };
            let Some(clip) = track.clips.iter_mut().find(|c| c.id == cref.clip) else {
                return;
            };
            let clip_start = clip.start;
            let clip_len = clip.length;
            let ClipContent::Notes(notes) = &mut clip.content else {
                return;
            };
            sel.retain(|&i| i < notes.len());

            let mut area = egui::ScrollArea::both().auto_shrink([false, false]);
            if first_open {
                let y = if drums || notes.is_empty() {
                    let target = if drums { 0 } else { rows.iter().position(|&p| p == 60).unwrap_or(0) };
                    (target as f32 * rh - (avail_h - RULER_H - VEL_H) * 0.5).max(0.0)
                } else {
                    let mean = notes.iter().map(|n| f32::from(n.pitch)).sum::<f32>() / notes.len() as f32;
                    let row = f32::from(PITCH_HIGH) - mean;
                    (row * rh - (avail_h - RULER_H - VEL_H) * 0.5).max(0.0)
                };
                area = area.vertical_scroll_offset(y).horizontal_scroll_offset(0.0);
            }

            area.show_viewport(ui, |ui, viewport| {
                let content = Vec2::new(KEYS_W + clip_len * ppb + 80.0, RULER_H + rows.len() as f32 * rh + VEL_H + 6.0);
                let (rect, _) = ui.allocate_exact_size(content, Sense::hover());
                let o = rect.min;
                let p = ui.painter_at(rect);
                let gx0 = o.x + KEYS_W;
                let gw = clip_len * ppb;
                let gy0 = o.y + RULER_H;
                let grid_rect = Rect::from_min_size(Pos2::new(gx0, gy0), Vec2::new(gw, rows.len() as f32 * rh));
                let beat_x = |b: f32| gx0 + b * ppb;
                let beat_at = |x: f32| (x - gx0) / ppb;
                let row_y = |r: usize| gy0 + r as f32 * rh;
                let row_at = |y: f32| ((y - gy0) / rh).floor() as i32;
                let snap_r = |b: f32| (b * div as f32).round() / div as f32;
                let snap_f = |b: f32| (b * div as f32).floor() / div as f32;
                let snap_c = |b: f32| (b * div as f32).ceil() / div as f32;
                let note_rect = |n: &Note| -> Option<Rect> {
                    let r = rows.iter().position(|&p| p == n.pitch)?;
                    Some(Rect::from_min_size(
                        Pos2::new(beat_x(n.start), row_y(r) + 1.0),
                        Vec2::new((n.length * ppb).max(4.0), rh - 2.0),
                    ))
                };

                // Sticky regions (viewport is in content coordinates).
                let sx = o.x + viewport.min.x;
                let sy = o.y + viewport.min.y;
                let by = o.y + viewport.max.y;
                let vw = viewport.width();
                let ruler_rect = Rect::from_min_size(Pos2::new(sx, sy), Vec2::new(vw, RULER_H));
                let keys_rect = Rect::from_min_size(Pos2::new(sx, gy0), Vec2::new(KEYS_W, rows.len() as f32 * rh));
                let vel_rect = Rect::from_min_max(Pos2::new(sx, by - VEL_H), Pos2::new(sx + vw, by));
                let blocked = |pos: Pos2| ruler_rect.contains(pos) || keys_rect.contains(pos) || vel_rect.contains(pos);

                // Row backgrounds
                for (r, &pitch) in rows.iter().enumerate() {
                    let y = row_y(r);
                    let rr = Rect::from_min_size(Pos2::new(gx0, y), Vec2::new(gw, rh));
                    let mut bg = if drums {
                        if r % 2 == 0 { Color32::from_rgb(33, 33, 39) } else { Color32::from_rgb(29, 29, 35) }
                    } else if is_black(pitch) {
                        Color32::from_rgb(26, 26, 31)
                    } else {
                        Color32::from_rgb(35, 35, 42)
                    };
                    if !drums && !scale_set.is_empty() {
                        let deg = (i32::from(pitch) - i32::from(scale_root)).rem_euclid(12) as u8;
                        if scale_set.contains(&deg) {
                            bg = mix(bg, accent, if deg == 0 { 0.28 } else { 0.10 });
                        } else {
                            bg = mix(bg, Color32::BLACK, 0.3);
                        }
                    }
                    p.rect_filled(rr, 0.0, bg);
                    p.line_segment(
                        [Pos2::new(gx0, y + rh), Pos2::new(gx0 + gw, y + rh)],
                        Stroke::new(1.0, Color32::from_black_alpha(70)),
                    );
                }

                // Grid lines
                let lines = (clip_len * div as f32).round() as usize;
                let y1 = gy0 + rows.len() as f32 * rh;
                for i in 0..=lines {
                    let x = gx0 + i as f32 / div as f32 * ppb;
                    let col = if i % (4 * div as usize) == 0 {
                        Color32::from_white_alpha(46)
                    } else if i % div as usize == 0 {
                        Color32::from_white_alpha(24)
                    } else {
                        Color32::from_white_alpha(10)
                    };
                    p.line_segment([Pos2::new(x, gy0), Pos2::new(x, y1)], Stroke::new(1.0, col));
                }

                let grid_resp = ui.interact(grid_rect, ui.scope_id().with("roll_grid"), Sense::click_and_drag());
                let hover = grid_resp.hover_pos().filter(|h| !blocked(*h));
                let hit_at = |notes: &[Note], pos: Pos2| -> Option<(usize, bool)> {
                    for (i, n) in notes.iter().enumerate().rev() {
                        if let Some(r) = note_rect(n) {
                            if r.contains(pos) {
                                return Some((i, r.width() > 14.0 && pos.x > r.right() - EDGE));
                            }
                        }
                    }
                    None
                };

                if let Some(h) = hover {
                    *hover_store = beat_at(h.x);
                    let icon = match hit_at(notes, h) {
                        Some((_, true)) => CursorIcon::ResizeHorizontal,
                        Some((_, false)) => CursorIcon::Grab,
                        None if tool == Tool::Draw => CursorIcon::Crosshair,
                        None => CursorIcon::Default,
                    };
                    ui.ctx().set_cursor_icon(icon);
                }

                if grid_resp.clicked() || grid_resp.drag_started() || grid_resp.secondary_clicked() {
                    *editor_focus = true;
                }

                // Right-click deletes a note.
                if grid_resp.secondary_clicked() {
                    if let Some(pos) = grid_resp.interact_pointer_pos().filter(|p| !blocked(*p)) {
                        if let Some((i, _)) = hit_at(notes, pos) {
                            notes.remove(i);
                            sel.clear();
                        }
                    }
                }

                // Press + drag: move/resize notes, draw a note, or box-select.
                if grid_resp.drag_started() {
                    if let Some(pos) = ui.input(|i| i.pointer.press_origin()).filter(|p| !blocked(*p) && grid_rect.contains(*p)) {
                        match hit_at(notes, pos) {
                            Some((i, edge)) => {
                                if !sel.contains(&i) {
                                    if !shift {
                                        sel.clear();
                                    }
                                    sel.insert(i);
                                }
                                let mut idxs: Vec<usize> = sel.iter().copied().filter(|&k| k < notes.len()).collect();
                                idxs.sort_unstable();
                                let originals: Vec<Note> = idxs.iter().map(|&k| notes[k]).collect();
                                let grab_beat = beat_at(pos.x);
                                *op = Some(if edge {
                                    NoteOp::Resize { grab_beat, idxs, originals }
                                } else {
                                    NoteOp::Move { grab_beat, grab_row: row_at(pos.y), idxs, originals }
                                });
                                preview = Some(notes[i].pitch);
                            }
                            None if tool == Tool::Draw && !shift => {
                                let r = row_at(pos.y);
                                if r >= 0 && (r as usize) < rows.len() {
                                    let start = snap_f(beat_at(pos.x)).clamp(0.0, (clip_len - unit).max(0.0));
                                    let pitch = rows[r as usize];
                                    notes.push(Note { pitch, start, length: unit, velocity: default_vel });
                                    sel.clear();
                                    sel.insert(notes.len() - 1);
                                    *op = Some(NoteOp::Create { idx: notes.len() - 1 });
                                    preview = Some(pitch);
                                }
                            }
                            None => *op = Some(NoteOp::Marquee { start: pos, additive: shift }),
                        }
                    }
                }

                if grid_resp.dragged() {
                    if let Some(pos) = grid_resp.interact_pointer_pos() {
                        let cur_beat = beat_at(pos.x);
                        match op.as_ref() {
                            Some(NoteOp::Move { grab_beat, grab_row, idxs, originals }) => {
                                let dbeat = snap_r(cur_beat - grab_beat);
                                let drow = row_at(pos.y) - grab_row;
                                let mut first_pitch = None;
                                for (k, &i) in idxs.iter().enumerate() {
                                    if i >= notes.len() {
                                        continue;
                                    }
                                    let o = originals[k];
                                    let orow = rows.iter().position(|&p| p == o.pitch).unwrap_or(0) as i32;
                                    let nrow = (orow + drow).clamp(0, rows.len() as i32 - 1) as usize;
                                    notes[i].pitch = rows[nrow];
                                    notes[i].start = (o.start + dbeat).clamp(0.0, (clip_len - o.length).max(0.0));
                                    if first_pitch.is_none() {
                                        first_pitch = Some(rows[nrow]);
                                    }
                                }
                                if let Some(pp) = first_pitch {
                                    let key = ui.scope_id().with("roll_last_pitch");
                                    let last: Option<u8> = ui.data(|d| d.get_temp(key));
                                    if last != Some(pp) {
                                        ui.data_mut(|d| d.insert_temp(key, pp));
                                        if last.is_some() {
                                            preview = Some(pp);
                                        }
                                    }
                                }
                            }
                            Some(NoteOp::Resize { grab_beat, idxs, originals }) => {
                                let dbeat = snap_r(cur_beat - grab_beat);
                                for (k, &i) in idxs.iter().enumerate() {
                                    if i < notes.len() {
                                        let o = originals[k];
                                        notes[i].length = (o.length + dbeat).clamp(unit, (clip_len - o.start).max(unit));
                                    }
                                }
                            }
                            Some(NoteOp::Create { idx }) => {
                                if *idx < notes.len() {
                                    let start = notes[*idx].start;
                                    notes[*idx].length = (snap_c(cur_beat) - start).clamp(unit, (clip_len - start).max(unit));
                                }
                            }
                            _ => {}
                        }
                    }
                }

                if grid_resp.drag_stopped() {
                    match op.take() {
                        Some(NoteOp::Create { idx }) => {
                            if idx < notes.len() && !drums {
                                *note_len = notes[idx].length;
                            }
                        }
                        Some(NoteOp::Marquee { start, additive }) => {
                            if let Some(now) = ui.input(|i| i.pointer.latest_pos()) {
                                let sel_rect = Rect::from_two_pos(start, now);
                                if !additive {
                                    sel.clear();
                                }
                                for (i, n) in notes.iter().enumerate() {
                                    if note_rect(n).is_some_and(|r| r.intersects(sel_rect)) {
                                        sel.insert(i);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    ui.data_mut(|d| d.remove::<u8>(ui.scope_id().with("roll_last_pitch")));
                }

                // Plain click: select a note, or add one.
                if grid_resp.clicked() {
                    if let Some(pos) = grid_resp.interact_pointer_pos().filter(|p| !blocked(*p)) {
                        match hit_at(notes, pos) {
                            Some((i, _)) => {
                                if shift {
                                    if !sel.remove(&i) {
                                        sel.insert(i);
                                    }
                                } else {
                                    sel.clear();
                                    sel.insert(i);
                                }
                                preview = Some(notes[i].pitch);
                            }
                            None if tool == Tool::Draw && !shift => {
                                let r = row_at(pos.y);
                                if r >= 0 && (r as usize) < rows.len() {
                                    let start = snap_f(beat_at(pos.x)).clamp(0.0, (clip_len - unit).max(0.0));
                                    let length = if drums { unit } else { (*note_len).max(unit).min((clip_len - start).max(unit)) };
                                    let pitch = rows[r as usize];
                                    notes.push(Note { pitch, start, length, velocity: default_vel });
                                    sel.clear();
                                    sel.insert(notes.len() - 1);
                                    preview = Some(pitch);
                                }
                            }
                            None => {
                                if !shift {
                                    sel.clear();
                                }
                            }
                        }
                    }
                }

                // Notes
                for (i, n) in notes.iter().enumerate() {
                    let Some(r) = note_rect(n) else { continue };
                    let selected = sel.contains(&i);
                    let t = f32::from(n.velocity) / 127.0;
                    let mut fill = mix(Color32::from_rgb(34, 34, 40), accent, 0.45 + 0.55 * t);
                    if selected {
                        fill = mix(fill, Color32::WHITE, 0.5);
                    }
                    p.rect_filled(r, 4.0, fill);
                    p.rect_stroke(
                        r,
                        4.0,
                        if selected { Stroke::new(1.5, Color32::WHITE) } else { Stroke::new(1.0, Color32::from_black_alpha(140)) },
                        StrokeKind::Inside,
                    );
                    if r.width() > 16.0 {
                        let handle = Rect::from_min_max(Pos2::new(r.right() - 4.0, r.top() + 3.0), Pos2::new(r.right() - 2.0, r.bottom() - 3.0));
                        p.rect_filled(handle, 1.0, Color32::from_white_alpha(90));
                    }
                    if r.width() > 34.0 && rh >= 13.0 && !drums {
                        p.with_clip_rect(r).text(
                            r.left_center() + Vec2::new(5.0, 0.0),
                            Align2::LEFT_CENTER,
                            note_name(n.pitch),
                            FontId::proportional(10.0),
                            if selected { Color32::from_rgb(20, 20, 24) } else { Color32::from_white_alpha(220) },
                        );
                    }
                }

                // Ghost note under the cursor
                if let (Some(h), None) = (hover, op.as_ref()) {
                    if tool == Tool::Draw && hit_at(notes, h).is_none() {
                        let r = row_at(h.y);
                        if r >= 0 && (r as usize) < rows.len() {
                            let start = snap_f(beat_at(h.x)).clamp(0.0, (clip_len - unit).max(0.0));
                            let gr = Rect::from_min_size(
                                Pos2::new(beat_x(start), row_y(r as usize) + 1.0),
                                Vec2::new(((if drums { unit } else { *note_len }) * ppb).max(6.0), rh - 2.0),
                            );
                            p.rect_filled(gr, 4.0, accent.gamma_multiply(0.35));
                        }
                    }
                }

                // Playhead
                let ph = playhead - clip_start;
                if (0.0..=clip_len).contains(&ph) {
                    let x = beat_x(ph);
                    p.line_segment([Pos2::new(x, gy0), Pos2::new(x, y1)], Stroke::new(1.5, theme::PLAYHEAD));
                }

                // Box selection overlay
                if let Some(NoteOp::Marquee { start, .. }) = op.as_ref() {
                    if let Some(now) = ui.input(|i| i.pointer.latest_pos()) {
                        let s = Rect::from_two_pos(*start, now);
                        p.rect_filled(s, 2.0, Color32::from_rgba_unmultiplied(46, 200, 186, 40));
                        p.rect_stroke(s, 2.0, Stroke::new(1.0, theme::ACCENT), StrokeKind::Inside);
                    }
                }

                // Sticky ruler
                p.rect_filled(ruler_rect, 0.0, theme::PANEL_BG);
                p.line_segment(
                    [Pos2::new(sx, sy + RULER_H), Pos2::new(sx + vw, sy + RULER_H)],
                    Stroke::new(1.0, Color32::from_black_alpha(140)),
                );
                let beats = clip_len.round() as usize;
                for b in 0..=beats {
                    let x = beat_x(b as f32);
                    let bar = b % 4 == 0;
                    p.line_segment(
                        [Pos2::new(x, sy + if bar { 2.0 } else { RULER_H - 7.0 }), Pos2::new(x, sy + RULER_H)],
                        Stroke::new(1.0, Color32::from_gray(if bar { 110 } else { 70 })),
                    );
                    if bar {
                        p.text(
                            Pos2::new(x + 5.0, sy + 5.0),
                            Align2::LEFT_TOP,
                            format!("{}", b / 4 + 1),
                            FontId::proportional(11.0),
                            theme::TEXT_DIM,
                        );
                    }
                }
                ui.interact(ruler_rect, ui.scope_id().with("roll_ruler"), Sense::click());

                // Sticky piano keys / drum names
                p.rect_filled(Rect::from_min_max(Pos2::new(sx, gy0), Pos2::new(sx + KEYS_W, y1)), 0.0, theme::WELL);
                for (r, &pitch) in rows.iter().enumerate() {
                    let kr = Rect::from_min_size(Pos2::new(sx, row_y(r)), Vec2::new(KEYS_W, rh));
                    if drums {
                        p.text(
                            kr.left_center() + Vec2::new(10.0, 0.0),
                            Align2::LEFT_CENTER,
                            DRUM_KIT[r].1,
                            FontId::proportional(11.5),
                            theme::TEXT,
                        );
                        p.line_segment([kr.left_bottom(), kr.right_bottom()], Stroke::new(1.0, Color32::from_black_alpha(90)));
                    } else {
                        p.rect_filled(kr.shrink2(Vec2::new(0.0, 0.5)), 0.0, Color32::from_rgb(226, 226, 234));
                        if is_black(pitch) {
                            p.rect_filled(
                                Rect::from_min_size(kr.min, Vec2::new(KEYS_W * 0.62, rh)).shrink2(Vec2::new(0.0, 0.5)),
                                2.0,
                                Color32::from_rgb(40, 40, 48),
                            );
                        } else if pitch % 12 == 0 {
                            p.text(
                                kr.right_center() - Vec2::new(5.0, 0.0),
                                Align2::RIGHT_CENTER,
                                note_name(pitch),
                                FontId::proportional(10.0),
                                Color32::from_rgb(60, 60, 70),
                            );
                        }
                    }
                }
                let keys_resp = ui.interact(keys_rect, ui.scope_id().with("roll_keys"), Sense::click_and_drag());
                if keys_resp.clicked() || keys_resp.drag_started() {
                    if let Some(pos) = keys_resp.interact_pointer_pos() {
                        let r = row_at(pos.y);
                        if r >= 0 && (r as usize) < rows.len() {
                            preview = Some(rows[r as usize]);
                        }
                    }
                }
                if keys_resp.dragged() {
                    if let Some(pos) = keys_resp.interact_pointer_pos() {
                        let r = row_at(pos.y);
                        let key = ui.scope_id().with("roll_keys_last");
                        let last: Option<i32> = ui.data(|d| d.get_temp(key));
                        if r >= 0 && (r as usize) < rows.len() && last != Some(r) {
                            ui.data_mut(|d| d.insert_temp(key, r));
                            preview = Some(rows[r as usize]);
                        }
                    }
                } else {
                    ui.data_mut(|d| d.remove::<i32>(ui.scope_id().with("roll_keys_last")));
                }

                // Sticky velocity lane
                p.rect_filled(vel_rect, 0.0, Color32::from_rgb(22, 22, 27));
                p.line_segment(
                    [vel_rect.left_top(), vel_rect.right_top()],
                    Stroke::new(1.0, Color32::from_black_alpha(160)),
                );
                let lane_h = VEL_H - 16.0;
                for i in 0..=3 {
                    let y = vel_rect.bottom() - 6.0 - lane_h * i as f32 / 3.0;
                    p.line_segment(
                        [Pos2::new(gx0.max(sx + KEYS_W), y), Pos2::new(sx + vw, y)],
                        Stroke::new(1.0, Color32::from_white_alpha(10)),
                    );
                }
                for (i, n) in notes.iter().enumerate() {
                    let x = beat_x(n.start) + 2.0;
                    if x < sx + KEYS_W || x > sx + vw {
                        continue;
                    }
                    let h = lane_h * f32::from(n.velocity) / 127.0;
                    let col = if sel.contains(&i) { Color32::WHITE } else { accent };
                    let top = vel_rect.bottom() - 6.0 - h;
                    p.line_segment([Pos2::new(x, vel_rect.bottom() - 6.0), Pos2::new(x, top)], Stroke::new(2.0, col.gamma_multiply(0.8)));
                    p.circle_filled(Pos2::new(x, top), 3.5, col);
                }
                p.rect_filled(Rect::from_min_size(vel_rect.min, Vec2::new(KEYS_W, VEL_H)), 0.0, theme::WELL);
                p.text(
                    vel_rect.left_top() + Vec2::new(10.0, 8.0),
                    Align2::LEFT_TOP,
                    "VELOCITY",
                    FontId::proportional(9.5),
                    theme::TEXT_DIM,
                );
                let vel_resp = ui.interact(vel_rect, ui.scope_id().with("roll_vel"), Sense::click_and_drag());
                if vel_resp.clicked() || vel_resp.dragged() || vel_resp.drag_started() {
                    if let Some(pos) = vel_resp.interact_pointer_pos() {
                        let target = notes
                            .iter()
                            .enumerate()
                            .map(|(i, n)| (i, (beat_x(n.start) + 2.0 - pos.x).abs()))
                            .filter(|(_, d)| *d <= 9.0)
                            .min_by(|a, b| a.1.total_cmp(&b.1))
                            .map(|(i, _)| i);
                        if let Some(i) = target {
                            let v = (((vel_rect.bottom() - 6.0 - pos.y) / lane_h).clamp(0.03, 1.0) * 127.0).round() as u8;
                            if sel.contains(&i) {
                                for &k in sel.iter() {
                                    if k < notes.len() {
                                        notes[k].velocity = v;
                                    }
                                }
                            } else {
                                notes[i].velocity = v;
                            }
                            *editor_focus = true;
                        }
                    }
                }
            });
        }

        if let Some(pitch) = preview {
            self.send(AudioCmd::Preview { track: cref.track, pitch });
        }
    }
}
