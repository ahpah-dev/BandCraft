//! Inspector: track card, mixer, instrument editor and effects.

use crate::project::{
    ClipContent, DRAWBARS, DRUM_KIT, INSTRUMENT_CATEGORIES, INSTRUMENT_PRESETS, Instrument, KitStyle, SampleSlot,
    SynthParams, TRACK_COLORS, TrackKind, Wave, note_name, wavetable_hollow, wavetable_saw, wavetable_sine,
};
use crate::theme::{self, track_color};
use crate::ui::{Action, BandCraftApp, empty_state, load_instrument_samples};
use eframe::egui::{self, Color32, Pos2, RichText, Sense, Stroke, Vec2};
use std::path::PathBuf;

/// Requests raised while drawing the inspector, applied once the borrow ends.
#[derive(Default)]
struct Pending {
    save: bool,
    user_load: Option<PathBuf>,
    reload: bool,
    make_sampler: bool,
}

impl BandCraftApp {
    pub(crate) fn inspector_ui(&mut self, ui: &mut egui::Ui) {
        let Some(tid) = self.selected_track.filter(|t| self.project.track(*t).is_some()) else {
            empty_state(ui, "No track selected", "Click a track name in the arrangement. Shift-click to select several.");
            return;
        };
        ui.set_max_width(ui.available_width());
        let group = self.track_sel.len();
        let user_list = self.user_instruments.clone();
        let mut pending = Pending::default();
        let mut delete = false;
        {
            let instrument_name = &mut self.instrument_name;
            let Some(track) = self.project.track_mut(tid) else {
                return;
            };
            if group > 1 {
                ui.label(
                    RichText::new(format!("{group} tracks selected · M/S and delete apply to all"))
                        .small()
                        .color(theme::ACCENT),
                );
                ui.add_space(4.0);
            }

            theme::card(ui, "Track", |ui| {
                ui.add(egui::TextEdit::singleline(&mut track.name).desired_width(ui.available_width() - 18.0));
                ui.label(RichText::new(crate::ui::kind_label(track.kind)).small().color(theme::TEXT_DIM));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    for c in 0..TRACK_COLORS as u8 {
                        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::click());
                        let color = track_color(c);
                        ui.painter().circle_filled(rect.center(), 8.0, color);
                        if track.color == c {
                            ui.painter().circle_stroke(rect.center(), 9.5, Stroke::new(2.0, Color32::WHITE));
                        } else if resp.hovered() {
                            ui.painter().circle_stroke(rect.center(), 9.5, Stroke::new(1.0, theme::TEXT_DIM));
                        }
                        if resp.clicked() {
                            track.color = c;
                        }
                    }
                });
            });

            theme::card(ui, "Mixer", |ui| {
                ui.label("Volume");
                ui.add(egui::Slider::new(&mut track.volume, 0.0..=1.5).show_value(false));
                ui.label("Pan");
                ui.add(egui::Slider::new(&mut track.pan, -1.0..=1.0).show_value(false));
                ui.horizontal(|ui| {
                    ui.toggle_value(&mut track.mute, RichText::new("Mute").strong());
                    ui.toggle_value(&mut track.solo, RichText::new("Solo").strong());
                });
            });

            match track.kind {
                TrackKind::Synth => theme::card(ui, "Instrument", |ui| {
                    instrument_section(ui, &mut track.instrument, instrument_name, &user_list, &mut pending);
                }),
                TrackKind::Drums => theme::card(ui, "Drum kit", |ui| {
                    instrument_section(ui, &mut track.instrument, instrument_name, &user_list, &mut pending);
                }),
                TrackKind::Audio => theme::card(ui, "Audio", |ui| {
                    if ui
                        .button("Make instrument from this recording")
                        .on_hover_text("Creates a sampler track that plays this clip across the keyboard")
                        .clicked()
                    {
                        pending.make_sampler = true;
                    }
                }),
            }

            theme::card(ui, "Effects", |ui| {
                ui.add(egui::Slider::new(&mut track.drive, 0.0..=1.0).text("Drive"));
                let delay_options: [(&str, f32); 5] =
                    [("Off", 0.0), ("1/16", 0.25), ("1/8", 0.5), ("1/8 dotted", 0.75), ("1/4", 1.0)];
                let current = delay_options
                    .iter()
                    .find(|(_, b)| (*b - track.delay_beats).abs() < 0.01)
                    .map_or("Off", |o| o.0);
                egui::ComboBox::from_id_salt("delay_time")
                    .selected_text(format!("Delay: {current}"))
                    .show_ui(ui, |ui| {
                        for (label, beats) in delay_options {
                            if ui.selectable_label((track.delay_beats - beats).abs() < 0.01, label).clicked() {
                                track.delay_beats = beats;
                            }
                        }
                    });
                ui.add(egui::Slider::new(&mut track.delay_feedback, 0.0..=0.9).text("Feedback"));
                ui.add(egui::Slider::new(&mut track.delay_mix, 0.0..=1.0).text("Delay mix"));
            });

            let label = if group > 1 { format!("Delete {group} tracks") } else { "Delete track".to_string() };
            if ui.add(egui::Button::new(RichText::new(label).color(theme::RECORD))).clicked() {
                delete = true;
            }
        }

        if pending.save {
            if let Some(inst) = self.project.track(tid).map(|t| t.instrument.clone()) {
                self.save_instrument(&inst);
            }
        }
        if let Some(path) = pending.user_load.take() {
            if let Some(inst) = self.load_user_instrument(&path) {
                if let Some(t) = self.project.track_mut(tid) {
                    t.instrument = inst;
                }
                pending.reload = true;
            }
        }
        if pending.reload {
            let mut missing = 0;
            if let Some(t) = self.project.track_mut(tid) {
                load_instrument_samples(&mut t.instrument, &mut missing);
            }
            self.status = if missing > 0 {
                format!("{missing} sample file(s) could not be loaded")
            } else {
                "Samples loaded".into()
            };
        }
        if pending.make_sampler {
            self.make_sampler_from_selected_clip(tid);
        }
        if delete {
            self.apply(Action::DeleteTracks);
        }
    }

    fn make_sampler_from_selected_clip(&mut self, tid: u32) {
        let clip = self
            .selected_clip
            .filter(|c| c.track == tid)
            .and_then(|c| self.project.track(tid)?.clips.iter().find(|k| k.id == c.clip).cloned())
            .or_else(|| self.project.track(tid)?.clips.first().cloned());
        let Some(clip) = clip else {
            self.status = "This track has no recording to sample".into();
            return;
        };
        let ClipContent::Audio { path, data: Some(d) } = clip.content else {
            self.status = "Select a recorded clip first".into();
            return;
        };
        let name = format!("{} sampler", clip.name);
        let id = self.project.add_track_with(
            &name,
            TrackKind::Synth,
            Instrument::Sampler { path, root: 60, data: Some(d) },
        );
        self.select_only_track(id);
        self.status = format!("Created sampler '{name}' from the recording");
    }
}

/// Instrument type, presets, user instruments and per-engine controls.
fn instrument_section(
    ui: &mut egui::Ui,
    inst: &mut Instrument,
    instrument_name: &mut String,
    user_list: &[(String, PathBuf)],
    pending: &mut Pending,
) {
    if let Instrument::DrumKit { style, slots } = inst {
        drum_kit_section(ui, style, slots, pending);
        return;
    }

    ui.horizontal_wrapped(|ui| {
        egui::ComboBox::from_id_salt("inst_type")
            .selected_text(inst.label())
            .show_ui(ui, |ui| {
                let choices: [(&str, &str); 6] = [
                    ("Subtractive", "Pluck"),
                    ("Wavetable", "Saw Table"),
                    ("FM", "E-Piano"),
                    ("Pluck", "String"),
                    ("Organ", "Full Drawbar"),
                    ("Sampler", ""),
                ];
                for (label, preset) in choices {
                    if ui.selectable_label(inst.label() == label, label).clicked() {
                        *inst = if preset.is_empty() {
                            Instrument::Sampler { path: String::new(), root: 60, data: None }
                        } else {
                            Instrument::preset(preset).unwrap_or_default()
                        };
                    }
                }
            });
        egui::ComboBox::from_id_salt("inst_preset")
            .selected_text("Presets")
            .show_ui(ui, |ui| {
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    for category in INSTRUMENT_CATEGORIES {
                        ui.label(RichText::new(category).small().strong().color(theme::TEXT_DIM));
                        for (name, cat) in INSTRUMENT_PRESETS.iter() {
                            if *cat == category && ui.button(*name).clicked() {
                                *inst = Instrument::preset(name).unwrap_or_default();
                            }
                        }
                    }
                });
            });
        egui::ComboBox::from_id_salt("inst_user")
            .selected_text("My instruments")
            .show_ui(ui, |ui| {
                if user_list.is_empty() {
                    ui.label(RichText::new("Save one below").small().color(theme::TEXT_DIM));
                }
                for (name, path) in user_list {
                    if ui.button(name).clicked() {
                        pending.user_load = Some(path.clone());
                    }
                }
            });
    });
    ui.add_space(4.0);

    match inst {
        Instrument::Subtractive(p) => subtractive_controls(ui, p),
        Instrument::Wavetable { table, env, cutoff, resonance } => {
            wavetable_controls(ui, table, env, cutoff, resonance);
        }
        Instrument::Fm { ratio, index, mod_decay, env } => {
            ui.add(egui::Slider::new(ratio, 0.5..=8.0).text("Ratio"));
            ui.add(egui::Slider::new(index, 0.0..=10.0).text("Brightness"));
            ui.add(egui::Slider::new(mod_decay, 0.1..=10.0).logarithmic(true).text("Brightness decay"));
            env_controls(ui, env);
        }
        Instrument::Pluck { decay, brightness } => {
            ui.add(egui::Slider::new(decay, 0.9..=0.9999).text("Sustain"));
            ui.add(egui::Slider::new(brightness, 0.0..=1.0).text("Brightness"));
        }
        Instrument::Organ { drawbars } => organ_controls(ui, drawbars),
        Instrument::Sampler { path, root, data } => {
            ui.label("Sample file");
            ui.horizontal(|ui| {
                let row_w = ui.available_width();
                ui.add(egui::TextEdit::singleline(path).hint_text("path to a .wav").desired_width((row_w - 100.0).max(60.0)));
                if ui.button("Load").clicked() {
                    pending.reload = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Root note");
                ui.add(egui::DragValue::new(root).range(24..=96).custom_formatter(|v, _| note_name(v as u8)));
                if data.is_some() {
                    ui.label(RichText::new("loaded").small().color(theme::ACCENT));
                }
            });
        }
        Instrument::DrumKit { .. } => {}
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let row_w = ui.available_width();
        ui.add(egui::TextEdit::singleline(instrument_name).desired_width((row_w - 100.0).max(60.0)));
        if ui.button("Save").on_hover_text("Save this sound to the instruments folder").clicked() {
            pending.save = true;
        }
    });
}

fn env_controls(ui: &mut egui::Ui, env: &mut crate::project::Env) {
    ui.add(egui::Slider::new(&mut env.attack, 0.001..=2.0).logarithmic(true).text("Attack"));
    ui.add(egui::Slider::new(&mut env.decay, 0.01..=3.0).logarithmic(true).text("Decay"));
    ui.add(egui::Slider::new(&mut env.sustain, 0.0..=1.0).text("Sustain"));
    ui.add(egui::Slider::new(&mut env.release, 0.01..=3.0).logarithmic(true).text("Release"));
}

fn subtractive_controls(ui: &mut egui::Ui, p: &mut SynthParams) {
    egui::ComboBox::from_id_salt("synth_wave")
        .selected_text(p.wave.label())
        .show_ui(ui, |ui| {
            for w in Wave::ALL {
                ui.selectable_value(&mut p.wave, w, w.label());
            }
        });
    ui.add(egui::Slider::new(&mut p.attack, 0.001..=2.0).logarithmic(true).text("Attack"));
    ui.add(egui::Slider::new(&mut p.decay, 0.01..=2.0).logarithmic(true).text("Decay"));
    ui.add(egui::Slider::new(&mut p.sustain, 0.0..=1.0).text("Sustain"));
    ui.add(egui::Slider::new(&mut p.release, 0.01..=3.0).logarithmic(true).text("Release"));
    ui.add(egui::Slider::new(&mut p.cutoff, 80.0..=7000.0).logarithmic(true).text("Filter Hz"));
    ui.add(egui::Slider::new(&mut p.resonance, 0.0..=1.0).text("Resonance"));
    ui.add(egui::Slider::new(&mut p.detune, 0.0..=0.03).text("Detune"));
}

/// Hand-drawn single-cycle waveform: drag across the box to draw.
fn wavetable_controls(
    ui: &mut egui::Ui,
    table: &mut Vec<f32>,
    env: &mut crate::project::Env,
    cutoff: &mut f32,
    resonance: &mut f32,
) {
    ui.label(RichText::new("Draw a waveform — drag inside the box").small().color(theme::TEXT_DIM));
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 120.0), Sense::click_and_drag());
    let n = table.len().max(1);
    let last_key = ui.scope_id().with("wt_last");
    if resp.dragged() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let i = (((pos.x - rect.left()) / rect.width()) * n as f32).clamp(0.0, (n - 1) as f32) as usize;
            let v = ((rect.center().y - pos.y) / (rect.height() * 0.5)).clamp(-1.0, 1.0);
            let prev: Option<usize> = ui.data(|d| d.get_temp(last_key));
            let (a, b) = match prev {
                Some(p) => (p.min(i), p.max(i)),
                None => (i, i),
            };
            let start_v = table.get(a).copied().unwrap_or(v);
            for k in a..=b {
                if k < n {
                    let t = if b == a { 1.0 } else { (k - a) as f32 / (b - a) as f32 };
                    table[k] = start_v + (v - start_v) * t;
                }
            }
            ui.data_mut(|d| d.insert_temp(last_key, i));
        }
    }
    if !resp.dragged() {
        ui.data_mut(|d| d.remove::<usize>(last_key));
    }
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 6.0, theme::WELL);
    p.line_segment(
        [Pos2::new(rect.left(), rect.center().y), Pos2::new(rect.right(), rect.center().y)],
        Stroke::new(1.0, Color32::from_gray(60)),
    );
    let pts: Vec<Pos2> = table
        .iter()
        .enumerate()
        .map(|(i, v)| {
            Pos2::new(
                rect.left() + rect.width() * i as f32 / n as f32,
                rect.center().y - v * rect.height() * 0.5,
            )
        })
        .collect();
    if pts.len() > 1 {
        p.add(egui::Shape::line(pts, Stroke::new(1.8, theme::ACCENT)));
    }
    ui.horizontal(|ui| {
        if ui.small_button("Sine").clicked() {
            *table = wavetable_sine();
        }
        if ui.small_button("Saw").clicked() {
            *table = wavetable_saw();
        }
        if ui.small_button("Hollow").clicked() {
            *table = wavetable_hollow();
        }
    });
    env_controls(ui, env);
    ui.add(egui::Slider::new(cutoff, 80.0..=7000.0).logarithmic(true).text("Filter Hz"));
    ui.add(egui::Slider::new(resonance, 0.0..=1.0).text("Resonance"));
}

fn organ_controls(ui: &mut egui::Ui, drawbars: &mut [f32; DRAWBARS]) {
    const LABELS: [&str; DRAWBARS] = ["16", "5.3", "8", "4", "2.7", "2", "1.6", "1.3", "1"];
    ui.label(RichText::new("Drawbars").small().color(theme::TEXT_DIM));
    ui.horizontal(|ui| {
        for (i, label) in LABELS.iter().enumerate() {
            ui.vertical(|ui| {
                ui.add(egui::Slider::new(&mut drawbars[i], 0.0..=1.0).vertical().show_value(false));
                ui.label(RichText::new(*label).small().color(theme::TEXT_DIM));
            });
        }
    });
}

fn drum_kit_section(ui: &mut egui::Ui, style: &mut KitStyle, slots: &mut Vec<SampleSlot>, pending: &mut Pending) {
    egui::ComboBox::from_id_salt("kit_style")
        .selected_text(style.label())
        .show_ui(ui, |ui| {
            for s in KitStyle::ALL {
                ui.selectable_value(style, s, s.label());
            }
        });
    ui.label(RichText::new("Pads use built-in sounds. Load your own WAV only if you want to replace one.").small().color(theme::TEXT_DIM));
    if slots.len() < DRUM_KIT.len() {
        slots.resize(DRUM_KIT.len(), SampleSlot::default());
    }
    for (k, slot) in slots.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            let row_w = ui.available_width();
            ui.add_sized([62.0, 18.0], egui::Label::new(RichText::new(DRUM_KIT[k].1).small()));
            ui.add(
                egui::TextEdit::singleline(&mut slot.path)
                    .hint_text("built-in")
                    .desired_width((row_w - 62.0 - 115.0).max(60.0)),
            );
            if ui.small_button("Load").clicked() {
                pending.reload = true;
            }
            if ui.small_button("×").on_hover_text("Back to built-in sound").clicked() {
                slot.path.clear();
                slot.data = None;
            }
        });
    }
}
