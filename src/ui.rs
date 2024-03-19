use std::borrow::Cow;
use std::ops::Not;
use std::{collections::VecDeque, fmt::Debug};

use eframe::egui::{
    Button, CollapsingHeader, Event, Key, KeyboardShortcut, Modifiers, ScrollArea, SidePanel,
};
use eframe::{
    egui::{menu, pos2, vec2, CentralPanel, Rect, TopBottomPanel, Window},
    run_native, App, NativeOptions,
};
use egui_modal::Modal;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio_util::sync::CancellationToken;

use crate::barcode_decoders::{self, BoxedBarcodeData};
use crate::config::{ConfigLoader, ConfigLoaderObject};

use self::state_worker::StateWorker;

mod scanner_settings;
pub mod state_worker;

#[derive(Debug, Default)]
struct State {
    scanner_settings_open: bool,
    pub scanner_settings: scanner_settings::State,
    decoders: barcode_decoders::BarcodeDecoders,
    decoded_history: VecDeque<(&'static str, BoxedBarcodeData)>,
    enabled_decoders: Vec<bool>,
    previous_scan: Option<String>,
    error: Option<(Cow<'static, str>, String)>,
    decoder_loading: usize,
}

impl State {
    fn clear_history(&mut self) {
        self.decoded_history.clear();
        self.previous_scan = None;
    }
}

#[derive(Debug)]
enum Action {
    Saved,
    ScannerSettings(scanner_settings::Action),
    GotBarcodeData(Option<(&'static str, BoxedBarcodeData)>),
    DecoderToggled,
    Decoder(barcode_decoders::Action),
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Config {
    scanner: Option<scanner_settings::SavedConfig>,
    disabled_decoders: Option<Vec<String>>,
}

struct Application {
    state: State,

    // We need to keep this handle so the worker doesn't get cancelled.
    #[allow(dead_code)]
    worker: StateWorker<Action>,
    rx: UnboundedReceiver<Action>,
    config_loader: ConfigLoader,

    scanner_settings: scanner_settings::BarcodeSettings,
}

impl ConfigLoaderObject for Application {
    fn key(&self) -> std::borrow::Cow<'static, str> {
        "application".into()
    }

    fn save(&self) -> eyre::Result<serde_json::Value> {
        let config = Config {
            scanner: self.state.scanner_settings.saved_config.clone(),
            disabled_decoders: Some(
                self.state
                    .enabled_decoders
                    .iter()
                    .zip(self.state.decoders.list().iter())
                    .flat_map(|(enabled, decoder)| {
                        enabled.not().then(|| decoder.name().to_string())
                    })
                    .collect(),
            ),
        };

        serde_json::to_value(config).map_err(Into::into)
    }

    fn restore(&mut self, value: serde_json::Value) -> eyre::Result<()> {
        let config: Config = serde_json::from_value(value)?;
        let disabled_decoders = config.disabled_decoders.unwrap_or_default();

        self.state.scanner_settings.saved_config = config.scanner;
        self.state.enabled_decoders = self
            .state
            .decoders
            .list()
            .iter()
            .map(|decoder| !disabled_decoders.contains(&decoder.name().to_string()))
            .collect();

        for disabled_decoder in disabled_decoders {
            self.worker.inner.handle.block_on({
                let decoders = self.state.decoders.clone();
                async move {
                    decoders.toggle_decoder(&disabled_decoder, false).await;
                }
            });
        }

        Ok(())
    }
}

impl Application {
    fn new(
        cc: &eframe::CreationContext,
        state: State,
        rx: UnboundedReceiver<Action>,
        mut worker: StateWorker<Action>,
        config_loader: ConfigLoader,
    ) -> Self {
        worker.set_egui_ctx(cc.egui_ctx.clone());

        let mut app = Application {
            state,
            rx,
            scanner_settings: scanner_settings::BarcodeSettings {
                worker: worker.scoped(Action::ScannerSettings),
            },
            worker,
            config_loader: config_loader.clone(),
        };

        if let Err(err) = config_loader.restore_object(&mut app) {
            app.state.error = Some(("Config Error".into(), err.to_string()));
        }

        app
    }
}

impl Application {
    fn save_config(&mut self) {
        let config_loader = self.config_loader.clone();
        self.state.scanner_settings.saved_config = Some(self.state.scanner_settings.saved());
        if let Err(err) = config_loader.save_object(self) {
            self.state.error = Some(("Config Error".into(), err.to_string()));
        }
        self.worker.perform(async move {
            config_loader.save("settings.json").await.unwrap();
            Action::Saved
        });
    }
}

impl App for Application {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(action) = self.rx.try_recv() {
            tracing::debug!(?action, "got action");

            match action {
                Action::Saved => (),
                Action::ScannerSettings(scanner_settings) => {
                    match &scanner_settings {
                        scanner_settings::Action::ScannedBarcode(Ok(value)) => {
                            if self.state.previous_scan.as_ref() != Some(value) {
                                self.state.previous_scan = Some(value.clone());

                                let value = value.clone();
                                let decoders = self.state.decoders.clone();

                                self.state.decoder_loading += 1;

                                self.worker.perform(async move {
                                    Action::GotBarcodeData(decoders.decode(&value).await)
                                });
                            }
                        }
                        scanner_settings::Action::ScannedBarcode(Err(err)) => {
                            self.state.error = Some(("Scanner Error".into(), err.to_string()));
                        }
                        _ => (),
                    }

                    self.scanner_settings
                        .update(&mut self.state.scanner_settings, scanner_settings);
                }
                Action::GotBarcodeData(data) => {
                    self.state.decoder_loading -= 1;

                    if let Some(data) = data {
                        self.state.decoded_history.push_front(data);
                        self.state.decoded_history.truncate(20);
                    }
                }
                Action::DecoderToggled | Action::Decoder(_) => (),
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
            const MAIN_KEY: Modifiers = if cfg!(target_os = "macos") {
                Modifiers::MAC_CMD
            } else {
                Modifiers::CTRL
            };

            const SAVE_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(MAIN_KEY, Key::S);
            const CLEAR_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(MAIN_KEY, Key::R);
            const PASTE_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(MAIN_KEY, Key::V);

            if ui.input_mut(|i| i.consume_shortcut(&SAVE_SHORTCUT)) {
                self.save_config();
            }

            if ui.input_mut(|i| i.consume_shortcut(&CLEAR_SHORTCUT)) {
                self.state.clear_history();
            }

            if let Some(paste) = ui.input(|i| {
                i.events.iter().find_map(|ev| match ev {
                    Event::Paste(value) => Some(value.clone()),
                    _ => None,
                })
            }) {
                self.worker.send(Action::ScannerSettings(
                    scanner_settings::Action::ScannedBarcode(Ok(paste)),
                ));
            }

            menu::bar(ui, |ui| {
                eframe::egui::global_dark_light_mode_switch(ui);

                ui.separator();

                ui.menu_button("File", |ui| {
                    if ui
                        .add(
                            Button::new("Save Settings")
                                .shortcut_text(ui.ctx().format_shortcut(&SAVE_SHORTCUT)),
                        )
                        .clicked()
                    {
                        self.save_config();
                    }

                    ui.separator();

                    if ui
                        .add(
                            Button::new("Clear History")
                                .shortcut_text(ui.ctx().format_shortcut(&CLEAR_SHORTCUT)),
                        )
                        .clicked()
                    {
                        self.state.clear_history();
                    }

                    ui.add_enabled_ui(false, |ui| {
                        ui.add(
                            Button::new("Scan from Clipboard")
                                .shortcut_text(ui.ctx().format_shortcut(&PASTE_SHORTCUT)),
                        )
                        .on_disabled_hover_text("Use the paste shortcut");
                    });
                });

                ui.menu_button("Settings", |ui| {
                    if ui.button("Scanner Setup").clicked() {
                        self.state.scanner_settings_open = true;
                    }
                });
            });
        });

        SidePanel::right("decoder_settings").show(ctx, |ui| {
            ui.heading("Decoder Settings");

            for (index, decoder) in self.state.decoders.list().iter().enumerate() {
                ui.collapsing(decoder.name(), |ui| {
                    ui.checkbox(&mut self.state.enabled_decoders[index], "Enabled")
                        .changed()
                        .then(|| {
                            let decoders = self.state.decoders.clone();
                            let name = decoder.name();
                            let enabled = self.state.enabled_decoders[index];

                            self.worker.perform(async move {
                                decoders.toggle_decoder(name, enabled).await;
                                Action::DecoderToggled
                            });
                        });

                    ui.add_enabled_ui(self.state.enabled_decoders[index], |ui| {
                        decoder.settings(ui);
                    });
                });
            }
        });

        CentralPanel::default().show(ctx, |ui| {
            ScrollArea::vertical().show(ui, |ui| {
                ui.heading("Decoded Barcodes");
                ui.set_width(ui.available_width());

                if self.state.decoded_history.is_empty() && self.state.decoder_loading == 0 {
                    ui.label("Nothing scanned yet!");
                } else if self.state.decoder_loading > 0 {
                    ui.horizontal(|ui| {
                        if self.state.decoder_loading == 1 {
                            ui.label("Processing barcode");
                        } else {
                            ui.label(format!(
                                "Processing {} barcodes",
                                self.state.decoder_loading
                            ));
                        }
                        ui.spinner();
                    });
                }

                for (decoder_name, data) in self.state.decoded_history.iter() {
                    ui.label(*decoder_name);

                    CollapsingHeader::new(data.summary())
                        .id_source(data.id())
                        .show(ui, |ui| {
                            data.render(ui);
                        });
                }
            });

            let mut modal = Modal::new(ctx, "error_message");

            if let Some((title, body)) = self.state.error.take() {
                modal
                    .dialog()
                    .with_title(title)
                    .with_body(body)
                    .with_icon(egui_modal::Icon::Error)
                    .open();

                ctx.request_repaint();
            }

            modal.show_dialog();
        });
    }
}

pub(crate) fn show_ui() -> eyre::Result<()> {
    let (tx, rx) = unbounded_channel();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let token = CancellationToken::new();
    let worker = StateWorker::new(rt.handle().clone(), tx, token);

    let worker_clone = worker.clone();
    let decoders = rt.block_on(async move {
        crate::barcode_decoders::BarcodeDecoders::new(worker_clone.scoped(Action::Decoder)).await
    })?;
    let enabled_decoders = std::iter::repeat(true)
        .take(decoders.list().len())
        .collect();

    let state = State {
        decoders,
        enabled_decoders,
        ..Default::default()
    };

    let config_loader = rt.block_on(async move {
        ConfigLoader::read("settings.json")
            .await
            .unwrap_or_default()
    });

    // Load devices immediately.
    worker.send(Action::ScannerSettings(
        scanner_settings::Action::ReloadDevices,
    ));

    run_native(
        "Scanner",
        NativeOptions::default(),
        Box::new(move |cc| Box::new(Application::new(cc, state, rx, worker, config_loader))),
    )
    .map_err(|err| eyre::eyre!("egui error: {err}"))?;

    Ok(())
}
