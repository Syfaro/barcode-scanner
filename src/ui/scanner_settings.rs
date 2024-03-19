use eframe::egui::{ComboBox, Grid, Ui};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::barcode_scanner::{Device, DeviceType, HidType};

use super::StateWorker;

#[derive(Debug)]
pub(crate) enum Action {
    ReloadDevices,
    LoadedDevices(Vec<Device>),
    SelectedDevice(Option<Device>),
    ConnectDevice,
    DisconnectDevice,
    ScannedBarcode(eyre::Result<String>),
}

#[derive(Debug, Default)]
pub(crate) struct State {
    devices: Vec<Device>,
    baud_rate: u32,
    hid_type: HidType,
    selected_device: Option<Device>,
    connected_scanner_token: Option<CancellationToken>,
    pub saved_config: Option<SavedConfig>,
}

impl State {
    pub(crate) fn saved(&self) -> SavedConfig {
        SavedConfig {
            baud_rate: Some(self.baud_rate),
            hid_type: Some(self.hid_type),
            selected_device: self.selected_device.clone(),
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SavedConfig {
    baud_rate: Option<u32>,
    hid_type: Option<HidType>,
    selected_device: Option<Device>,
}

impl State {
    fn selected_device_name(&self) -> &str {
        match self.selected_device.as_ref() {
            Some(device) => &device.name,
            _ => "None",
        }
    }

    fn is_hid_device(&self) -> bool {
        matches!(
            self.selected_device,
            Some(Device {
                device_type: DeviceType::Hid { .. },
                ..
            })
        )
    }

    fn is_serial_device(&self) -> bool {
        matches!(
            self.selected_device,
            Some(Device {
                device_type: DeviceType::Serial { .. },
                ..
            })
        )
    }

    fn can_connect(&self) -> bool {
        match self.selected_device {
            Some(Device {
                device_type: DeviceType::Serial { .. },
                ..
            }) if self.baud_rate > 0 => true,
            Some(Device {
                device_type: DeviceType::Hid { .. },
                ..
            }) => true,
            _ => false,
        }
    }

    fn is_connected(&self) -> bool {
        self.connected_scanner_token.is_some()
    }
}

pub(crate) struct BarcodeSettings {
    pub(crate) worker: StateWorker<Action>,
}

impl BarcodeSettings {
    pub(crate) fn update(&self, mut state: &mut State, action: Action) {
        self.worker
            .apply(&mut state, action, |state, action| match action {
                Action::ReloadDevices => {
                    self.worker.perform(async move {
                        let devices = crate::barcode_scanner::list_devices().await.unwrap();
                        Action::LoadedDevices(devices)
                    });
                }
                Action::LoadedDevices(devices) => {
                    state.devices = devices;

                    if let Some(saved_config) = state.saved_config.take() {
                        state.baud_rate = saved_config.baud_rate.unwrap_or_default();
                        state.hid_type = saved_config.hid_type.unwrap_or_default();

                        if let Some(saved_device) = saved_config.selected_device {
                            if state.devices.iter().any(|device| device == &saved_device) {
                                state.selected_device = Some(saved_device);
                                self.worker.send(Action::ConnectDevice);
                            }
                        }
                    } else if let Some(selected_device) = &state.selected_device {
                        if !state.devices.iter().any(|device| device == selected_device) {
                            state.selected_device = None;
                        }
                    }
                }
                Action::SelectedDevice(Some(device)) => {
                    if let DeviceType::Hid { usage_id, .. } = device.device_type {
                        state.hid_type = if usage_id == 6 {
                            HidType::Keyboard
                        } else {
                            HidType::Pos
                        };
                    }
                }
                Action::SelectedDevice(_) => (),
                Action::ConnectDevice => {
                    let Some(device) = state.selected_device.clone() else {
                        return;
                    };

                    let baud = Some(state.baud_rate);
                    let hid_type = Some(state.hid_type);

                    let token = CancellationToken::new();
                    state.connected_scanner_token = Some(token.clone());

                    self.worker.stream(async move {
                        let tx = crate::barcode_scanner::start_scanner(
                            token,
                            device.device_type,
                            baud,
                            hid_type,
                        )
                        .await
                        .expect("could not start scanner");

                        tokio_stream::wrappers::ReceiverStream::from(tx).map(Action::ScannedBarcode)
                    });
                }
                Action::DisconnectDevice => {
                    if let Some(token) = state.connected_scanner_token.take() {
                        token.cancel();
                    }
                }
                Action::ScannedBarcode(Err(_)) => {
                    if let Some(token) = state.connected_scanner_token.take() {
                        token.cancel();
                    }

                    self.worker.send(Action::ReloadDevices);
                }
                Action::ScannedBarcode(_) => (),
            });
    }

    pub(crate) fn render(&mut self, state: &mut State, ui: &mut Ui) {
        ui.add_enabled_ui(!state.is_connected(), |ui| {
            Grid::new("barcode_scanner_settings")
                .num_columns(2)
                .spacing([40.0, 4.0])
                .show(ui, |ui| {
                    self.settings_grid(state, ui);
                });
        });

        if state.is_connected() {
            if ui.button("Disconnect").clicked() {
                self.worker.send(Action::DisconnectDevice);
            }
        } else {
            ui.add_enabled_ui(state.can_connect(), |ui| {
                if ui.button("Connect").clicked() {
                    self.worker.send(Action::ConnectDevice);
                }
            });
        }

        ui.separator();

        if ui.button("Reload device list").clicked() {
            self.worker.send(Action::ReloadDevices);
        }
    }

    fn settings_grid(&self, state: &mut State, ui: &mut Ui) {
        ui.label("Devices");
        ComboBox::from_label("Devices")
            .selected_text(state.selected_device_name())
            .show_ui(ui, |ui| {
                ui.style_mut().wrap = Some(false);
                ui.set_min_width(60.0);
                ui.selectable_value(&mut state.selected_device, None, "None");
                for device in state.devices.iter().cloned() {
                    ui.selectable_value(
                        &mut state.selected_device,
                        Some(device.clone()),
                        device.name,
                    )
                    .changed()
                    .then(|| {
                        self.worker
                            .send(Action::SelectedDevice(state.selected_device.clone()));
                    });
                }
            });
        ui.end_row();

        ui.label("HID Type");
        ui.add_enabled_ui(state.is_hid_device(), |ui| {
            ComboBox::from_label("HID Type")
                .selected_text(state.hid_type.to_string())
                .show_ui(ui, |ui| {
                    ui.style_mut().wrap = Some(false);
                    ui.set_min_width(60.0);
                    for hid_type in enum_iterator::all::<HidType>() {
                        ui.selectable_value(&mut state.hid_type, hid_type, hid_type.to_string());
                    }
                });
        });
        ui.end_row();

        ui.label("Baud Rate");
        ui.add_enabled_ui(state.is_serial_device(), |ui| {
            ComboBox::from_label("Baud Rate")
                .selected_text(state.baud_rate.to_string())
                .show_ui(ui, |ui| {
                    ui.style_mut().wrap = Some(false);
                    ui.set_min_width(60.0);
                    for rate in [9600, 115_200] {
                        ui.selectable_value(&mut state.baud_rate, rate, rate.to_string());
                    }
                });
        });
        ui.end_row();
    }
}
