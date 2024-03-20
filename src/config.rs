use std::{borrow::Cow, collections::HashMap, path::PathBuf, sync::Arc};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::RwLock,
};

pub trait ConfigLoaderObject {
    fn key(&self) -> Cow<'static, str>;

    fn save(&self) -> eyre::Result<serde_json::Value>;
    fn restore(&mut self, value: serde_json::Value) -> eyre::Result<()>;
}

#[derive(Debug, Clone)]
pub struct ConfigLoader {
    file_path: PathBuf,
    config: Arc<RwLock<HashMap<Cow<'static, str>, serde_json::Value>>>,
}

impl ConfigLoader {
    pub fn empty(file_path: PathBuf) -> Self {
        Self {
            file_path,
            config: Default::default(),
        }
    }

    pub async fn read(file_path: PathBuf) -> eyre::Result<Self> {
        let mut file = tokio::fs::File::open(&file_path).await?;

        let mut buf = String::new();
        let _size = file.read_to_string(&mut buf).await?;

        let config = serde_json::from_str(&buf)?;

        tracing::debug!(file_path = %file_path.display(), "created new config loader");

        Ok(Self {
            file_path,
            config: Arc::new(RwLock::new(config)),
        })
    }

    pub async fn save(&self) -> eyre::Result<()> {
        let config = self.config.read().await;

        let data = serde_json::to_vec_pretty(&*config)?;

        match self.file_path.parent() {
            Some(parent) if !parent.exists() => tokio::fs::create_dir_all(parent).await?,
            _ => tracing::debug!("file path does not have parent"),
        }

        tracing::debug!(file_path = %self.file_path.display(), "saving config");

        let mut file = tokio::fs::File::create(&self.file_path).await?;
        file.write_all(&data).await?;

        Ok(())
    }

    #[tracing::instrument(skip_all)]
    pub fn get(&self) -> eyre::Result<serde_json::Value> {
        serde_json::to_value(&*self.config.blocking_read()).map_err(Into::into)
    }

    #[tracing::instrument(skip_all, fields(key = %object.key()))]
    pub fn save_object<T>(&self, object: &T) -> eyre::Result<()>
    where
        T: ConfigLoaderObject,
    {
        tracing::debug!("saving object data");

        let data = object.save()?;
        self.config.blocking_write().insert(object.key(), data);

        Ok(())
    }

    #[tracing::instrument(skip_all, fields(key = %object.key()))]
    pub fn restore_object<T>(&self, object: &mut T) -> eyre::Result<()>
    where
        T: ConfigLoaderObject,
    {
        tracing::debug!("loading object data");

        let data = match self.config.blocking_read().get(&object.key()) {
            Some(data) => data.to_owned(),
            None => {
                tracing::trace!("no data found for object");
                return Ok(());
            }
        };

        object.restore(data)?;

        Ok(())
    }
}
