use async_trait::async_trait;
use eframe::egui::{vec2, CollapsingHeader, Color32, Ui};
use uuid::Uuid;

use super::{BarcodeData, BarcodeDecoder, BoxedBarcodeData};

#[derive(Debug)]
pub struct AamvaDecoder;

#[async_trait]
impl BarcodeDecoder for AamvaDecoder {
    fn name(&self) -> &'static str {
        "AAMVA"
    }

    fn settings(&self, _ui: &mut Ui) {}

    async fn decode(&self, input: &str) -> eyre::Result<BoxedBarcodeData> {
        let data =
            aamva::parse_barcode(input).map_err(|err| eyre::eyre!("failed to parse: {err}"))?;

        let decoded_data = serde_json::to_value(data.subfiles.clone())?;

        Ok(Box::new(AamvaData {
            id: Uuid::new_v4(),
            raw_data: decoded_data,
            decoded_data: data.into(),
        }))
    }
}

#[derive(Debug)]
pub struct AamvaData {
    id: Uuid,
    raw_data: serde_json::Value,
    decoded_data: aamva::DecodedData,
}

impl AamvaData {
    fn display_name(&self) -> String {
        if let Some(name) = &self.decoded_data.name {
            format!("{} {}", name.first, name.family)
        } else {
            "Unknown".to_string()
        }
    }
}

impl BarcodeData for AamvaData {
    fn id(&self) -> Uuid {
        self.id
    }

    fn summary(&self) -> String {
        self.display_name()
    }

    fn render(&self, ui: &mut Ui) {
        let now = time::OffsetDateTime::now_local();

        if let Some(birthday) = self.decoded_data.date_of_birth {
            let mut text = format!("🎂 {birthday}");

            if let Ok(today) = now {
                let elapsed_years = today.date().year() - birthday.year();
                text.push_str(&format!(" ({elapsed_years})"));
            }

            ui.label(text).on_hover_text("Birthday");
        }

        if let Some(expiration_date) = self.decoded_data.document_expiration_date {
            let text = format!("⏰ {expiration_date}");

            if matches!(now, Ok(today) if today.date() > expiration_date) {
                ui.colored_label(Color32::YELLOW, text)
            } else {
                ui.label(text)
            }
            .on_hover_text("Document Expiration Date");
        }

        if let Some(address) = &self.decoded_data.address {
            ui.horizontal_top(|ui| {
                ui.style_mut().spacing.item_spacing = vec2(0.0, 0.0);

                ui.label("🏠 ");
                ui.vertical(|ui| {
                    ui.label(&address.address_1);
                    if let Some(address_2) = &address.address_2 {
                        ui.label(address_2);
                    }
                    ui.label(format!(
                        "{}, {} {}",
                        address.city,
                        address.jurisdiction_code,
                        &address.postal_code[..5]
                    ));
                });
            });
        }

        CollapsingHeader::new("Decoded Data")
            .id_source(format!("{}-decoded", self.id))
            .show(ui, |ui| {
                let theme = egui_extras::syntax_highlighting::CodeTheme::from_memory(ui.ctx());
                egui_extras::syntax_highlighting::code_view_ui(
                    ui,
                    &theme,
                    &serde_json::to_string_pretty(&self.decoded_data)
                        .expect("could not reserialize data"),
                    "json",
                );
            });

        CollapsingHeader::new("Raw Data")
            .id_source(format!("{}-raw", self.id))
            .show(ui, |ui| {
                let theme = egui_extras::syntax_highlighting::CodeTheme::from_memory(ui.ctx());
                egui_extras::syntax_highlighting::code_view_ui(
                    ui,
                    &theme,
                    &serde_json::to_string_pretty(&self.raw_data)
                        .expect("could not reserialize data"),
                    "json",
                );
            });

        ui.end_row();
    }

    fn raw_data(&self) -> Option<&serde_json::Value> {
        Some(&self.raw_data)
    }
}
