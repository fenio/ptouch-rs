// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Main application struct and eframe::App implementation.

use std::sync::mpsc;

use log::{error, info};

use ptouch_render::text::TextRenderer;

use crate::panels;
use crate::printer_worker;
use crate::state::{AppState, PrinterEvent, PrinterResponse};

/// The main P-Touch GUI application.
pub struct PtouchApp {
    /// Application state shared across all panels.
    pub state: AppState,
    /// Text renderer instance for generating label bitmaps.
    renderer: TextRenderer,
    /// Receiver for responses from the printer worker thread.
    resp_rx: mpsc::Receiver<PrinterEvent>,
}

impl PtouchApp {
    /// Create a new application instance.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_fallback_fonts(&cc.egui_ctx);

        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (resp_tx, resp_rx) = mpsc::channel();

        let ctx = cc.egui_ctx.clone();
        std::thread::Builder::new()
            .name("printer-worker".to_string())
            .spawn(move || {
                printer_worker::printer_worker(cmd_rx, resp_tx, ctx);
            })
            .expect("failed to spawn printer worker thread");

        Self {
            state: AppState {
                available_fonts: ptouch_render::font::list_fonts(),
                printer_cmd_tx: Some(cmd_tx),
                ..AppState::default()
            },
            renderer: TextRenderer::new(),
            resp_rx,
        }
    }

    /// Re-render the preview bitmap from the current element list.
    pub fn update_preview(&mut self, ctx: &egui::Context) {
        self.state.needs_rerender = false;

        if self.state.elements.is_empty() {
            self.state.preview_bitmap = None;
            self.state.preview_texture = None;
            return;
        }

        let result = match ptouch_render::document::render_elements(
            &self.state.elements,
            self.state.tape_width_px,
            &self.state.font_name,
            self.state.font_margin,
            &mut self.renderer,
        ) {
            Ok(result) => result,
            Err(e) => {
                error!("Render failed: {}", e);
                self.state.status_message = format!("Render error: {}", e);
                None
            }
        };

        // Whole-label mirroring is applied once, after the elements are
        // composed, independently of any per-element flips.
        let result =
            result.map(|bmp| bmp.mirrored(self.state.overall_flip_h, self.state.overall_flip_v));

        if let Some(ref bitmap) = result {
            let rgba = bitmap.to_rgba_image();
            let max_side = ctx.input(|i| i.max_texture_side);

            let rgba = if rgba.width() as usize > max_side || rgba.height() as usize > max_side {
                let scale = max_side as f32 / rgba.width().max(rgba.height()) as f32;
                let new_w = (rgba.width() as f32 * scale).floor() as u32;
                let new_h = (rgba.height() as f32 * scale).floor() as u32;
                image::imageops::resize(
                    &rgba,
                    new_w.max(1),
                    new_h.max(1),
                    image::imageops::FilterType::Nearest,
                )
            } else {
                rgba
            };

            let size = [rgba.width() as usize, rgba.height() as usize];
            let pixels = rgba.into_raw();
            let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);

            let texture =
                ctx.load_texture("label_preview", color_image, egui::TextureOptions::NEAREST);
            self.state.preview_texture = Some(texture);
        } else {
            self.state.preview_texture = None;
        }

        self.state.preview_bitmap = result;
        info!("Preview updated");
    }

    fn drain_printer_responses(&mut self) {
        // Drain all pending responses from the printer worker
        while let Ok(event) = self.resp_rx.try_recv() {
            if event
                .target
                .as_ref()
                .is_some_and(|target| target != &self.state.printer_target)
            {
                continue;
            }
            match event.response {
                PrinterResponse::BluetoothDevices(devices) => {
                    self.state.bluetooth_targets = devices;
                }
                PrinterResponse::Connected {
                    model_name,
                    media_width,
                    media_type,
                    max_px,
                    dpi,
                    quality_modes,
                    tape_width_px,
                } => {
                    self.state.printer_connected = true;
                    self.state.connecting = false;
                    self.state.printer_max_px = max_px;
                    self.state.printer_dpi = dpi;
                    self.state.printer_quality_modes = quality_modes;
                    // A stale non-standard quality from a previous printer
                    // would make every print fail with the selector hidden.
                    if !quality_modes {
                        self.state.print_quality = ptouch_core::protocol::PrintQuality::Standard;
                    }
                    self.state.printer_model =
                        Some(format!("{}: {} mm {}", model_name, media_width, media_type));
                    self.state.printer_status = Some("Connected".to_string());
                    if media_width > 0 {
                        self.state.tape_width_mm = media_width;
                    }
                    // Use the printer's status-derived printable width. This
                    // differs from the transfer width on the PT-P300BT.
                    let old_px = self.state.tape_width_px;
                    self.state.tape_width_px = u32::from(tape_width_px);
                    if self.state.tape_width_px != old_px {
                        self.state.mark_dirty();
                    }
                }
                PrinterResponse::Disconnected => {
                    self.state.printer_status = Some("Disconnected".to_string());
                    self.state.printer_model = None;
                    self.state.printer_connected = false;
                    self.state.connecting = false;
                    // Keep printer_max_px, printer_dpi, and quality state:
                    // the canvas must not resize on a transient disconnect,
                    // and printing is gated on printer_connected anyway.
                }
                PrinterResponse::PrintDone => {
                    self.state.operation_in_progress = false;
                    self.state.status_message = "Print complete".to_string();
                }
                PrinterResponse::FeedAndCutDone => {
                    self.state.operation_in_progress = false;
                    self.state.status_message = "Feed & cut done".to_string();
                }
                PrinterResponse::Error(msg) => {
                    self.state.operation_in_progress = false;
                    self.state.status_message = msg;
                }
            }
        }
    }
}

impl eframe::App for PtouchApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain_printer_responses();

        // Top toolbar
        egui::Panel::top("toolbar").show(ui, |ui| {
            panels::toolbar::show_toolbar(ui, &mut self.state);
        });

        // Bottom status bar
        egui::Panel::bottom("status_bar").show(ui, |ui| {
            panels::status_bar::show_status_bar(ui, &self.state);
        });

        // Left sidebar
        egui::Panel::left("sidebar")
            .default_size(200.0)
            .resizable(true)
            .show(ui, |ui| {
                panels::sidebar::show_sidebar(ui, &mut self.state);
            });

        // Right properties panel
        egui::Panel::right("properties")
            .default_size(250.0)
            .resizable(true)
            .show(ui, |ui| {
                panels::properties::show_properties(ui, &mut self.state);
            });

        // Central canvas
        egui::CentralPanel::default().show(ui, |ui| {
            panels::canvas::show_canvas(ui, &mut self.state);
        });

        // Re-render preview if dirty
        if self.state.needs_rerender {
            self.update_preview(&ctx);
        }

        // Periodic repaint so we pick up worker responses even when idle
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
    }
}

/// Register fallback fonts for CJK text and emoji rendering.
fn setup_fallback_fonts(ctx: &egui::Context) {
    use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};

    let lowest_both = vec![
        InsertFontFamily {
            family: egui::FontFamily::Proportional,
            priority: FontPriority::Lowest,
        },
        InsertFontFamily {
            family: egui::FontFamily::Monospace,
            priority: FontPriority::Lowest,
        },
    ];

    // CJK fallback (DroidSansFallback, Apache-2.0)
    ctx.add_font(FontInsert {
        name: "cjk_fallback".into(),
        data: egui::FontData::from_static(include_bytes!("../assets/fonts/DroidSansFallback.ttf")),
        families: lowest_both.clone(),
    });

    // Emoji fallback (NotoEmoji, OFL-1.1)
    ctx.add_font(FontInsert {
        name: "emoji_fallback".into(),
        data: egui::FontData::from_static(include_bytes!("../assets/fonts/NotoEmoji.ttf")),
        families: lowest_both,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::PrinterTarget;

    fn app(state: AppState) -> (PtouchApp, mpsc::Sender<PrinterEvent>) {
        let (tx, rx) = mpsc::channel();
        (
            PtouchApp {
                state,
                renderer: TextRenderer::new(),
                resp_rx: rx,
            },
            tx,
        )
    }

    fn usb_status() -> PrinterEvent {
        PrinterEvent {
            target: Some(PrinterTarget::Usb),
            response: PrinterResponse::Connected {
                model_name: "PT-P700".into(),
                media_width: 24,
                media_type: "Laminated tape".into(),
                max_px: 128,
                dpi: 180,
                quality_modes: false,
                tape_width_px: 128,
            },
        }
    }

    #[test]
    fn late_usb_poll_does_not_connect_selected_bluetooth_printer() {
        let (mut app, tx) = app(AppState {
            printer_target: PrinterTarget::Bluetooth {
                name: "PT-P300BT".into(),
                address: "AA:BB:CC:DD:EE:FF".into(),
            },
            connecting: true,
            ..AppState::default()
        });
        tx.send(usb_status()).unwrap();
        app.drain_printer_responses();
        assert!(!app.state.printer_connected);
        assert!(app.state.connecting);
        assert!(app.state.printer_model.is_none());
        tx.send(PrinterEvent {
            target: Some(PrinterTarget::Usb),
            response: PrinterResponse::Disconnected,
        })
        .unwrap();
        app.drain_printer_responses();
        assert!(app.state.connecting);

        tx.send(PrinterEvent {
            target: Some(app.state.printer_target.clone()),
            response: PrinterResponse::Connected {
                model_name: "PT-P300BT".into(),
                media_width: 12,
                media_type: "Laminated tape".into(),
                max_px: 128,
                dpi: 180,
                quality_modes: false,
                tape_width_px: 64,
            },
        })
        .unwrap();
        app.drain_printer_responses();
        assert!(app.state.printer_connected);
        assert!(!app.state.is_printer_busy());
        assert_eq!(app.state.tape_width_px, 64);
    }

    #[test]
    fn status_reply_does_not_finish_pending_print() {
        let (mut app, tx) = app(AppState {
            printer_connected: true,
            operation_in_progress: true,
            ..AppState::default()
        });
        tx.send(usb_status()).unwrap();
        app.drain_printer_responses();
        assert!(app.state.operation_in_progress);
        assert!(
            app.state
                .printer_model
                .as_ref()
                .unwrap()
                .starts_with("PT-P700")
        );
        tx.send(PrinterEvent {
            target: Some(PrinterTarget::Usb),
            response: PrinterResponse::Disconnected,
        })
        .unwrap();
        app.drain_printer_responses();
        assert!(!app.state.printer_connected);
        assert!(app.state.operation_in_progress);
        tx.send(PrinterEvent {
            target: Some(PrinterTarget::Usb),
            response: PrinterResponse::PrintDone,
        })
        .unwrap();
        app.drain_printer_responses();
        assert!(!app.state.operation_in_progress);
    }
}
