//! BandCraft: a simple, loop-based music studio in pure Rust.

// Hide the console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod dsp;
mod engine;
mod panels;
mod project;
mod roll;
mod theme;
mod ui;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("BandCraft")
            .with_inner_size([1440.0, 900.0])
            .with_maximized(true)
            .with_min_inner_size([1080.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "BandCraft",
        options,
        Box::new(|cc| {
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(ui::BandCraftApp::new()))
        }),
    )
}
