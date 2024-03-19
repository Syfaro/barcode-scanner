use std::{fmt::Debug, sync::Arc};

use async_trait::async_trait;
use eframe::egui::{ahash::HashSet, Ui};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::ui::state_worker::StateWorker;

mod aamva;
mod generic;
mod link;
pub mod shc;

pub trait BarcodeData: Debug + Send + Sync {
    fn id(&self) -> Uuid;
    fn summary(&self) -> String;
    fn render(&self, ui: &mut Ui);
    fn raw_data(&self) -> Option<&serde_json::Value>;
}

pub type BoxedBarcodeData = Box<dyn BarcodeData>;

#[async_trait]
pub trait BarcodeDecoder: Debug + Send + Sync {
    fn name(&self) -> &'static str;
    fn settings(&self, ui: &mut Ui);

    async fn decode(&self, input: &str) -> eyre::Result<BoxedBarcodeData>;
}

pub type BoxedBarcodeDecoder = Box<dyn BarcodeDecoder>;

#[derive(Debug, Default, Clone)]
pub struct BarcodeDecoders {
    decoders: Arc<Vec<Box<dyn BarcodeDecoder>>>,
    disabled_decoders: Arc<RwLock<HashSet<String>>>,
}

#[derive(Debug)]
pub enum Action {
    SmartHealthCard(shc::Action),
}

impl BarcodeDecoders {
    pub async fn new(state_worker: StateWorker<Action>) -> eyre::Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();

        let path = std::env::var("DATABASE_URL").unwrap();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect(&path)
            .await?;

        sqlx::migrate!().run(&pool).await?;

        let decoders: Vec<BoxedBarcodeDecoder> = vec![
            Box::new(
                shc::SmartHealthCardDecoder::new(
                    client,
                    pool,
                    state_worker.scoped(Action::SmartHealthCard),
                )
                .await?,
            ),
            Box::new(aamva::AamvaDecoder),
            Box::new(link::LinkDecoder),
            Box::new(generic::GenericDataDecoder),
        ];

        Ok(BarcodeDecoders {
            decoders: Arc::new(decoders),
            disabled_decoders: Default::default(),
        })
    }

    #[tracing::instrument(skip(self))]
    pub async fn decode(&self, input: &str) -> Option<(&'static str, BoxedBarcodeData)> {
        let disabled_decoders = self.disabled_decoders.read().await;

        for decoder in self.decoders.iter() {
            if disabled_decoders.contains(decoder.name()) {
                continue;
            }

            match decoder.decode(input).await {
                Ok(data) => return Some((decoder.name(), data)),
                Err(err) => {
                    tracing::trace!(name = decoder.name(), "could not decode: {err}");
                }
            }
        }

        None
    }

    pub fn list(&self) -> &[BoxedBarcodeDecoder] {
        &self.decoders
    }

    pub async fn toggle_decoder(&self, name: &str, enabled: bool) {
        let mut disabled_decoders = self.disabled_decoders.write().await;

        if enabled {
            disabled_decoders.remove(name);
        } else {
            disabled_decoders.insert(name.to_string());
        }
    }
}
