use std::num::NonZeroUsize;
use std::{collections::VecDeque, fmt::Debug};

use eframe::egui::Grid;
use eframe::{
    egui::{menu, pos2, vec2, CentralPanel, Rect, SidePanel, TopBottomPanel, Window},
    run_native, App, NativeOptions,
};
use ring_channel::ring_channel;
use tokio_util::sync::CancellationToken;

use self::state_worker::StateWorker;

mod scanner_settings;
mod state_worker;

#[derive(Debug, Default)]
struct State {
    scanner_settings_open: bool,
    scanner_settings: scanner_settings::State,
    barcode_history: VecDeque<String>,
}

#[derive(Debug)]
enum Action {
    ScannerSettings(scanner_settings::Action),
}

struct Application {
    state: State,
    // We need to keep this handle so the worker doesn't get cancelled.
    #[allow(dead_code)]
    worker: StateWorker<Action>,

    rx: ring_channel::RingReceiver<Action>,

    scanner_settings: scanner_settings::BarcodeSettings,
}

impl Application {
    fn new(
        cc: &eframe::CreationContext,
        state: State,
        rx: ring_channel::RingReceiver<Action>,
        mut worker: StateWorker<Action>,
    ) -> Self {
        worker.set_egui_ctx(cc.egui_ctx.clone());

        Application {
            state,
            rx,
            scanner_settings: scanner_settings::BarcodeSettings {
                worker: worker.scoped(Action::ScannerSettings),
            },
            worker,
        }
    }
}

impl App for Application {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        if let Ok(action) = self.rx.try_recv() {
            tracing::debug!(?action, "got action");

            match action {
                Action::ScannerSettings(scanner_settings) => {
                    match &scanner_settings {
                        scanner_settings::Action::ScannedBarcode(value) => {
                            self.state.barcode_history.push_front(value.clone());
                            self.state.barcode_history.truncate(100);
                        }
                        _ => (),
                    }

                    self.scanner_settings
                        .update(&mut self.state.scanner_settings, scanner_settings);
                }
            }

            tracing::debug!(state = ?self.state, "built new state");
        }

        Window::new("Scanner Settings")
            .open(&mut self.state.scanner_settings_open)
            .resizable(false)
            .default_rect(Rect::from_min_size(pos2(10.0, 80.0), vec2(160.0, 300.0)))
            .show(ctx, |ui| {
                self.scanner_settings
                    .render(&mut self.state.scanner_settings, ui)
            });

        TopBottomPanel::top("top_panel").show(ctx, |ui| {
            menu::bar(ui, |ui| {
                ui.menu_button("Settings", |ui| {
                    if ui.button("Scanner Device").clicked() {
                        self.state.scanner_settings_open = true;
                    }
                });
            });
        });

        SidePanel::right("right_panel")
            .min_width(160.0)
            .show(ctx, |ui| {
                ui.heading("Scanner History");

                Grid::new("scanner_history")
                    .striped(true)
                    .num_columns(1)
                    .spacing([40.0, 4.0])
                    .show(ui, |ui| {
                        for entry in self.state.barcode_history.iter() {
                            ui.label(entry.trim());
                            ui.end_row();
                        }
                    });
            });

        CentralPanel::default().show(ctx, |ui| {
            ui.heading("Hello, world!");
        });

        ctx.request_repaint();
    }
}

pub(crate) fn show_ui() -> eyre::Result<()> {
    let (tx, rx) = ring_channel::<Action>(NonZeroUsize::new(1).unwrap());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let token = CancellationToken::new();
    let worker = StateWorker::new(rt.handle().clone(), tx, token);

    let state = State::default();

    // Load devices immediately.
    worker.send(Action::ScannerSettings(
        scanner_settings::Action::ReloadDevices,
    ));

    run_native(
        "Scanner",
        NativeOptions::default(),
        Box::new(move |cc| Box::new(Application::new(cc, state, rx, worker))),
    )
    .map_err(|err| eyre::eyre!("egui error: {err}"))?;

    Ok(())
}
