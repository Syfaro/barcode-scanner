use async_trait::async_trait;
use eframe::egui::{CollapsingHeader, Ui};
use itertools::Itertools;
use uuid::Uuid;

use super::{BarcodeData, BarcodeDecoder, BoxedBarcodeData};

#[derive(Debug)]
pub(crate) struct GenericDataDecoder;

#[async_trait]
impl BarcodeDecoder for GenericDataDecoder {
    fn name(&self) -> &'static str {
        "Generic Data"
    }

    fn settings(&self, _ui: &mut Ui) {}

    async fn decode(&self, input: &str) -> eyre::Result<BoxedBarcodeData> {
        Ok(Box::new(GenericData {
            id: Uuid::new_v4(),
            data: input.to_string(),
        }))
    }
}

#[derive(Debug)]
struct GenericData {
    id: Uuid,
    data: String,
}

impl BarcodeData for GenericData {
    fn id(&self) -> Uuid {
        self.id
    }

    fn summary(&self) -> String {
        let trimmed_data = self.data.trim();

        let mut display = trimmed_data
            .chars()
            .filter(|c| !c.is_control())
            .take(50)
            .join("");
        if trimmed_data.len() > 50 {
            display.push('…');
        }
        display
    }

    fn render(&self, ui: &mut Ui) {
        CollapsingHeader::new("Raw Data")
            .id_source(format!("{}-data", self.id))
            .default_open(true)
            .show(ui, |ui| {
                let theme = egui_extras::syntax_highlighting::CodeTheme::from_memory(ui.ctx());
                egui_extras::syntax_highlighting::code_view_ui(ui, &theme, &self.data, "text");
            });
    }

    fn raw_data(&self) -> Option<&serde_json::Value> {
        None
    }
}
