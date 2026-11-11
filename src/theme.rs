//! Visual theme and custom widgets (icons, knobs, faders, round buttons).

use crate::project::TrackKind;
use std::sync::Arc;
use eframe::egui::{self, Color32, CornerRadius, Id, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2};
use std::f32::consts::PI;

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub const CARD: Color32 = rgb(36, 36, 43);
pub const WINDOW_BG: Color32 = rgb(20, 20, 24);
pub const PANEL_BG: Color32 = rgb(30, 30, 36);
pub const SURFACE: Color32 = rgb(40, 40, 47);
pub const SURFACE_HI: Color32 = rgb(54, 54, 63);
pub const WELL: Color32 = rgb(15, 15, 19);
pub const LANE_EVEN: Color32 = rgb(29, 29, 35);
pub const LANE_ODD: Color32 = rgb(25, 25, 31);
pub const LANE_SELECTED: Color32 = rgb(36, 36, 44);
pub const GRID_BEAT: Color32 = rgb(40, 40, 48);
pub const GRID_BAR: Color32 = rgb(64, 64, 74);
pub const TEXT: Color32 = rgb(232, 232, 238);
pub const TEXT_DIM: Color32 = rgb(138, 138, 150);
pub const ACCENT: Color32 = rgb(46, 200, 186);
pub const RECORD: Color32 = rgb(236, 72, 82);
pub const PLAYHEAD: Color32 = rgb(255, 255, 255);
pub const MUTE_ON: Color32 = rgb(74, 150, 255);
pub const SOLO_ON: Color32 = rgb(250, 196, 60);
const SYNTH: Color32 = rgb(160, 74, 204);
const DRUM: Color32 = rgb(212, 86, 150);
const AUDIO: Color32 = rgb(64, 118, 214);

/// Base colour for a track kind (used for clips, strips and faders).
pub fn kind_color(kind: TrackKind) -> Color32 {
    match kind {
        TrackKind::Drums => DRUM,
        TrackKind::Synth => SYNTH,
        TrackKind::Audio => AUDIO,
    }
}

/// Installs the dark BandCraft look.
pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    // Force dark so the system light theme can't override our visuals.
    ctx.set_theme(egui::ThemePreference::Dark);
    let mut v = egui::Visuals::dark();
    v.window_fill = PANEL_BG;
    v.panel_fill = PANEL_BG;
    v.extreme_bg_color = WELL;
    v.faint_bg_color = rgb(36, 36, 42);
    v.selection.bg_fill = Color32::from_rgba_unmultiplied(46, 200, 186, 70);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    let radius = CornerRadius::same(6);

    v.widgets.noninteractive.bg_fill = PANEL_BG;
    v.widgets.noninteractive.weak_bg_fill = PANEL_BG;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, rgb(44, 44, 52));
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.inactive.bg_fill = rgb(66, 66, 78);
    v.widgets.inactive.weak_bg_fill = SURFACE;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.hovered.bg_fill = SURFACE_HI;
    v.widgets.hovered.weak_bg_fill = SURFACE_HI;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, rgb(78, 78, 90));
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, TEXT);
    v.widgets.active.bg_fill = rgb(64, 64, 76);
    v.widgets.active.weak_bg_fill = rgb(64, 64, 76);
    v.widgets.active.fg_stroke = Stroke::new(1.5, TEXT);
    v.widgets.open.bg_fill = SURFACE_HI;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = radius;
    }
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.set_visuals_of(egui::Theme::Light, egui::Visuals::dark());
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = Vec2::new(8.0, 6.0);
        s.spacing.button_padding = Vec2::new(8.0, 4.0);
        s.spacing.interact_size.y = 24.0;
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Play,
    Pause,
    Stop,
    Record,
    Rewind,
    Loop,
    Undo,
    Redo,
    Metronome,
    Dots,
}

fn at(c: Pos2, x: f32, y: f32) -> Pos2 {
    Pos2::new(c.x + x, c.y + y)
}

/// Draws an icon centred in `r`.
pub fn paint_icon(p: &Painter, r: Rect, icon: Icon, color: Color32) {
    let c = r.center();
    let h = r.width().min(r.height()) * 0.5;
    let stroke = Stroke::new(1.6, color);
    match icon {
        Icon::Play => {
            p.add(Shape::convex_polygon(
                vec![at(c, -h * 0.4, -h * 0.6), at(c, -h * 0.4, h * 0.6), at(c, h * 0.6, 0.0)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Pause => {
            for x in [-0.32, 0.32] {
                p.rect_filled(Rect::from_center_size(at(c, x * h, 0.0), Vec2::new(h * 0.36, h * 1.2)), 1.0, color);
            }
        }
        Icon::Stop => {
            p.rect_filled(Rect::from_center_size(c, Vec2::splat(h * 1.1)), 2.0, color);
        }
        Icon::Record => {
            p.circle_filled(c, h * 0.62, color);
        }
        Icon::Rewind => {
            p.rect_filled(Rect::from_center_size(at(c, -h * 0.62, 0.0), Vec2::new(h * 0.2, h * 1.2)), 1.0, color);
            p.add(Shape::convex_polygon(
                vec![at(c, h * 0.55, -h * 0.6), at(c, h * 0.55, h * 0.6), at(c, -h * 0.4, 0.0)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Loop => {
            p.line_segment([at(c, -h * 0.7, -h * 0.25), at(c, h * 0.6, -h * 0.25)], stroke);
            p.line_segment([at(c, h * 0.7, h * 0.25), at(c, -h * 0.6, h * 0.25)], stroke);
            p.add(Shape::convex_polygon(
                vec![at(c, h * 0.95, -h * 0.25), at(c, h * 0.5, -h * 0.6), at(c, h * 0.5, h * 0.1)],
                color,
                Stroke::NONE,
            ));
            p.add(Shape::convex_polygon(
                vec![at(c, -h * 0.95, h * 0.25), at(c, -h * 0.5, h * 0.6), at(c, -h * 0.5, -h * 0.1)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Undo => arc_arrow(p, c, h * 0.7, 0.0, PI, color),
        Icon::Redo => arc_arrow(p, c, h * 0.7, PI, 0.0, color),
        Icon::Metronome => {
            let tri = [at(c, -h * 0.5, h * 0.8), at(c, -h * 0.2, -h * 0.8), at(c, h * 0.2, -h * 0.8), at(c, h * 0.5, h * 0.8)];
            p.line_segment([tri[0], tri[1]], stroke);
            p.line_segment([tri[1], tri[2]], stroke);
            p.line_segment([tri[2], tri[3]], stroke);
            p.line_segment([tri[3], tri[0]], stroke);
            p.line_segment([at(c, 0.0, h * 0.6), at(c, h * 0.25, -h * 0.4)], stroke);
            p.circle_filled(at(c, h * 0.25, -h * 0.4), h * 0.18, color);
        }
        Icon::Dots => {
            for y in [-0.6, 0.0, 0.6] {
                p.circle_filled(at(c, 0.0, y * h), h * 0.17, color);
            }
        }
    }
}

/// Curved arrow from angle `from` to `to` (radians, counter-clockwise, screen-y flipped).
fn arc_arrow(p: &Painter, c: Pos2, r: f32, from: f32, to: f32, color: Color32) {
    let center = at(c, 0.0, r * 0.2);
    let n = 18;
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let t = from + (to - from) * i as f32 / n as f32;
            Pos2::new(center.x + r * t.cos(), center.y - r * t.sin())
        })
        .collect();
    let end_angle = to;
    let dir = Vec2::new(-r * end_angle.sin(), -r * end_angle.cos()) * (to - from).signum();
    let d = dir.normalized();
    let perp = Vec2::new(-d.y, d.x);
    let tip = *pts.last().unwrap_or(&center);
    let size = r * 0.85;
    p.add(Shape::line(pts, Stroke::new(1.6, color)));
    p.add(Shape::convex_polygon(
        vec![tip + d * size * 0.45, tip - d * size * 0.35 + perp * size * 0.55, tip - d * size * 0.35 - perp * size * 0.55],
        color,
        Stroke::NONE,
    ));
}

/// Square toolbar button with an icon.
pub fn icon_button(ui: &mut Ui, icon: Icon, tooltip: &str, active: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
    let p = ui.painter();
    let bg = if active {
        rgb(58, 58, 68)
    } else if resp.hovered() {
        SURFACE_HI
    } else {
        Color32::TRANSPARENT
    };
    p.rect_filled(rect, 6.0, bg);
    let color = if active {
        ACCENT
    } else if resp.hovered() {
        TEXT
    } else {
        TEXT_DIM
    };
    paint_icon(p, rect.shrink(7.0), icon, color);
    resp.on_hover_text(tooltip)
}

/// Large round transport button.
pub fn round_button(ui: &mut Ui, icon: Icon, fill: Color32, tooltip: &str, active: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::click());
    let p = ui.painter();
    let ring = if active || resp.hovered() { fill } else { rgb(70, 70, 82) };
    p.circle_filled(rect.center(), 20.0, rgb(44, 44, 52));
    p.circle_stroke(rect.center(), 19.0, Stroke::new(1.5, ring));
    if active && icon == Icon::Record {
        p.circle_filled(rect.center(), 20.0, rgb(92, 26, 32));
    }
    let color = if icon == Icon::Record { RECORD } else if active { fill } else { TEXT };
    paint_icon(p, rect.shrink(12.0), icon, color);
    resp.on_hover_text(tooltip)
}

/// Rotary knob for values in `min..=max`. Drag vertically; double-click resets to `default`.
pub fn knob(ui: &mut Ui, value: &mut f32, min: f32, max: f32, default: f32, size: f32, tooltip: &str) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click_and_drag());
    if resp.double_clicked() {
        *value = default;
        resp.mark_changed();
    }
    if resp.dragged() {
        let dy = resp.drag_delta().y;
        *value = (*value - dy * (max - min) / 120.0).clamp(min, max);
        resp.mark_changed();
    }
    let p = ui.painter();
    let c = rect.center();
    let r = size * 0.36;
    p.circle_filled(c, r + 2.0, rgb(18, 18, 22));
    p.circle_filled(c, r, rgb(56, 56, 66));
    let t = ((*value - min) / (max - min)).clamp(0.0, 1.0);
    let start = 135f32.to_radians();
    let sweep = 270f32.to_radians();
    let arc: Vec<Pos2> = (0..=24)
        .map(|i| {
            let a = start + sweep * t * i as f32 / 24.0;
            Pos2::new(c.x + (r + 2.0) * a.cos(), c.y + (r + 2.0) * a.sin())
        })
        .collect();
    if arc.len() > 1 {
        p.add(Shape::line(arc, Stroke::new(2.0, ACCENT)));
    }
    let a = start + sweep * t;
    p.line_segment(
        [
            Pos2::new(c.x + r * 0.2 * a.cos(), c.y + r * 0.2 * a.sin()),
            Pos2::new(c.x + r * 0.85 * a.cos(), c.y + r * 0.85 * a.sin()),
        ],
        Stroke::new(1.8, TEXT),
    );
    resp.on_hover_text(tooltip)
}

/// Thin horizontal fader drawn inside `rect`. Click or drag to set `value`.
pub fn fader(ui: &mut Ui, id: Id, rect: Rect, value: &mut f32, min: f32, max: f32, color: Color32) -> Response {
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    if resp.dragged() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let t = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            *value = min + t * (max - min);
        }
    }
    let p = ui.painter();
    let mid = rect.center().y;
    let track = Rect::from_min_max(Pos2::new(rect.left(), mid - 2.0), Pos2::new(rect.right(), mid + 2.0));
    p.rect_filled(track, 2.0, WELL);
    let t = ((*value - min) / (max - min)).clamp(0.0, 1.0);
    let x = rect.left() + t * rect.width();
    p.rect_filled(
        Rect::from_min_max(track.min, Pos2::new(x, track.max.y)),
        2.0,
        color,
    );
    p.circle_filled(Pos2::new(x, mid), if resp.hovered() || resp.dragged() { 5.0 } else { 4.0 }, TEXT);
    resp
}

/// Draws a small caps-style section caption.
pub fn caption(ui: &mut Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(text.to_uppercase()).size(10.5).strong().color(TEXT_DIM));
}

/// Track colour palette (BandLab-style variety).
pub const PALETTE: [Color32; 8] = [
    rgb(168, 85, 214),
    rgb(222, 88, 150),
    rgb(70, 124, 224),
    rgb(40, 176, 176),
    rgb(88, 188, 100),
    rgb(238, 134, 66),
    rgb(226, 184, 54),
    rgb(226, 88, 88),
];

pub fn track_color(i: u8) -> Color32 {
    PALETTE[usize::from(i) % PALETTE.len()]
}

/// Linear blend between two colours (t = 0 gives `a`).
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Rounded section card with a small caption.
pub fn card(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(title.to_uppercase()).size(10.0).strong().color(TEXT_DIM));
            ui.add_space(2.0);
            add(ui);
        });
    ui.add_space(8.0);
}

/// Uses Segoe UI when available (Windows) for crisper native-looking text.
fn install_fonts(ctx: &egui::Context) {
    let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("segoe".to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
    if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        list.insert(0, "segoe".to_owned());
    }
    ctx.set_fonts(fonts);
}
