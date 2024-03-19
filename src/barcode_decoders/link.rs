use async_trait::async_trait;
use eframe::egui::Ui;
use itertools::Itertools;
use uuid::Uuid;

use super::{BarcodeData, BarcodeDecoder, BoxedBarcodeData};

#[derive(Debug)]
pub(crate) struct LinkDecoder;

#[async_trait]
impl BarcodeDecoder for LinkDecoder {
    fn name(&self) -> &'static str {
        "Link"
    }

    fn settings(&self, _ui: &mut Ui) {}

    async fn decode(&self, input: &str) -> eyre::Result<BoxedBarcodeData> {
        let url = url::Url::parse(input)?;

        eyre::ensure!(
            matches!(url.scheme(), "http" | "https"),
            "must be http or https url"
        );

        Ok(Box::new(Link {
            id: Uuid::new_v4(),
            url,
        }))
    }
}

#[derive(Debug)]
struct Link {
    id: Uuid,
    url: url::Url,
}

impl BarcodeData for Link {
    fn id(&self) -> Uuid {
        self.id
    }

    fn summary(&self) -> String {
        let data = self.url.as_str();

        if data.len() > 50 {
            data.chars().take(50).join("")
        } else {
            data.to_string()
        }
    }

    fn render(&self, ui: &mut Ui) {
        if ui.link(self.url.as_str()).clicked() {
            if let Err(err) = open::that(self.url.as_str()) {
                tracing::error!("could not open link: {err}");
            }
        }
    }

    fn raw_data(&self) -> Option<&serde_json::Value> {
        None
    }
}
