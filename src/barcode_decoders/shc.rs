use std::{
    borrow::Cow,
    collections::HashMap,
    fmt::Debug,
    io::Read,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use eframe::egui::{
    CollapsingHeader, Color32, Grid, Hyperlink, Label, ProgressBar, RichText, Ui, Window,
};
use egui_extras::{Column, TableBuilder};
use futures::{StreamExt, TryStreamExt};
use icu::{casemap::TitlecaseMapper, locid::LanguageIdentifier};
use itertools::Itertools;
use jsonwebtoken::jwk::JwkSet;
use lexical_sort::natural_lexical_cmp;
use serde::Deserialize;
use sqlx::{SqliteExecutor, SqlitePool};
use time::macros::format_description;
use uuid::Uuid;

use crate::ui::state_worker::StateWorker;

use super::{BarcodeData, BarcodeDecoder, BoxedBarcodeData};

pub(crate) struct SmartHealthCardDecoder {
    client: reqwest::Client,
    pool: SqlitePool,
    cvx_codes: Arc<HashMap<String, String>>,
    sorted_cvx_codes: Arc<Vec<(String, String)>>,
    ui_state: Arc<Mutex<UiState>>,
    state_worker: StateWorker<Action>,
}

impl Debug for SmartHealthCardDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmartHealthCardDecoder")
            .field("ui_state", &self.ui_state)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct UiState {
    showing_cvx_codes: bool,
    showing_vci_issuers: bool,
    vci_issuers_loading: bool,
    vci_issuer_loaded: usize,
    vci_issuer_total: usize,
    vci_issuers: Vec<VciIssuerMeta>,
}

impl UiState {
    fn add_issuer(&mut self, meta: VciIssuerMeta) {
        self.vci_issuers.push(meta);
        self.vci_issuers
            .sort_by(|a, b| natural_lexical_cmp(&a.name, &b.name));
    }
}

impl Debug for UiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiState")
            .field("showing_cvx_codes", &self.showing_cvx_codes)
            .field("showing_vci_issuers", &self.showing_vci_issuers)
            .field("vci_issuers_loading", &self.vci_issuers_loading)
            .field("vci_issuer_loaded", &self.vci_issuer_loaded)
            .field("vci_issuer_total", &self.vci_issuer_total)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Deserialize)]
struct VciIssuers {
    participating_issuers: Vec<VciIssuer>,
}

#[derive(Debug, Deserialize)]
struct VciIssuer {
    iss: String,
    name: String,
    website: Option<String>,
    canonical_iss: Option<String>,
}

#[derive(Debug)]
struct VciIssuerMeta {
    name: String,
    iss: String,
    error: bool,
    keys: i64,
    jwk_set: Option<JwkSet>,
}

#[derive(Debug, Deserialize)]
struct FhirBundleEntry {
    #[serde(rename = "fullUrl")]
    full_url: String,
    resource: FhirBundleEntryResource,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "resourceType", rename_all_fields = "camelCase")]
enum FhirBundleEntryResource {
    Patient {
        birth_date: String,
        name: Vec<PatientName>,
    },
    Immunization {
        lot_number: Option<String>,
        occurrence_date_time: String,
        patient: Reference,
        performer: Vec<Performer>,
        status: String,
        vaccine_code: VaccineCode,
    },
    #[serde(untagged)]
    Other(serde_json::Value),
}

#[derive(Debug, Deserialize)]
struct PatientName {
    family: String,
    given: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Reference {
    reference: String,
}

#[derive(Debug, Deserialize)]
struct Performer {
    actor: Actor,
}

#[derive(Debug, Deserialize)]
struct Actor {
    display: String,
}

#[derive(Debug, Deserialize)]
struct VaccineCode {
    coding: Vec<Coding>,
}

#[derive(Debug, Deserialize)]
struct Coding {
    code: String,
    system: String,
}

#[derive(Debug, Clone)]
pub(crate) enum Action {
    VciRefresh,
    VciLoading,
    VciLoadingFinished(Option<String>),
}

impl SmartHealthCardDecoder {
    const ISSUER_URL: &'static str =
        "https://raw.githubusercontent.com/the-commons-project/vci-directory/main/vci-issuers.json";
    const CVX_CODES: &'static str =
        "https://www2a.cdc.gov/vaccines/iis/iisstandards/downloads/cvx.txt";

    pub(crate) async fn new(
        client: reqwest::Client,
        pool: SqlitePool,
        state_worker: StateWorker<Action>,
    ) -> eyre::Result<Self> {
        let cvx_codes = Arc::new(Self::update_cvx_codes(&client, &pool).await?);

        let mut sorted_cvx_codes: Vec<_> = cvx_codes
            .iter()
            .map(|(code, name)| (code.to_string(), name.to_string()))
            .collect();
        sorted_cvx_codes
            .sort_by(|(code_a, _), (code_b, _)| lexical_sort::natural_lexical_cmp(code_a, code_b));

        let ui_state: Arc<Mutex<UiState>> = Default::default();

        let shc = Self {
            client,
            pool,
            cvx_codes,
            sorted_cvx_codes: Arc::new(sorted_cvx_codes),
            ui_state,
            state_worker,
        };

        shc.refresh_vci(false);

        Ok(shc)
    }

    async fn update_cvx_codes(
        client: &reqwest::Client,
        pool: &SqlitePool,
    ) -> eyre::Result<HashMap<String, String>> {
        let data = if let Some(data) = sqlx::query_scalar!(
            "SELECT value
                FROM expiring_cache
                WHERE key = 'cvx_codes' AND expires_at >= date('now')
                ORDER BY expires_at DESC
                LIMIT 1"
        )
        .fetch_optional(pool)
        .await?
        {
            tracing::debug!("using cached cvx codes");
            data
        } else {
            tracing::debug!("updating cvx code cache");

            let data = client.get(Self::CVX_CODES).send().await?.text().await?;

            sqlx::query!(
                "INSERT INTO expiring_cache (key, value, expires_at)
                    VALUES ('cvx_codes', $1, date('now', '+1 day')) ON CONFLICT DO UPDATE SET
                        value = EXCLUDED.value,
                        expires_at = EXCLUDED.expires_at",
                data
            )
            .execute(pool)
            .await?;

            data
        };

        let mut cvx_codes = HashMap::new();

        for line in data.lines() {
            let parts: Vec<_> = line.split('|').collect();

            let code = parts[0].trim();

            static DATE_FORMAT: &[time::format_description::FormatItem<'_>] =
                format_description!("[year]/[month]/[day]");
            let last_updated = time::Date::parse(parts[6], &DATE_FORMAT)?;

            sqlx::query!(
                "INSERT INTO cvx_code (code, short_description, full_name, notes, vaccine_status, last_updated)
                    VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (code) DO UPDATE SET
                        short_description = EXCLUDED.short_description,
                        full_name = EXCLUDED.full_name,
                        notes = EXCLUDED.notes,
                        vaccine_status = EXCLUDED.vaccine_status,
                        last_updated = EXCLUDED.last_updated",
                code,
                parts[1],
                parts[2],
                parts[3],
                parts[5],
                last_updated,
            )
            .execute(pool)
            .await?;

            cvx_codes.insert(code.to_string(), parts[1].to_string());
        }

        Ok(cvx_codes)
    }

    fn refresh_vci(&self, ignore_cache: bool) {
        self.state_worker.send(Action::VciRefresh);

        let client = self.client.clone();
        let pool = self.pool.clone();
        let state_worker = self.state_worker.clone();
        let ui_state = self.ui_state.clone();

        self.state_worker.perform(async move {
            let err = Self::update_vci_issuers(client, pool, state_worker, ui_state, ignore_cache)
                .await
                .err()
                .map(|err| err.to_string());

            Action::VciLoadingFinished(err)
        });
    }

    async fn update_vci_issuers(
        client: reqwest::Client,
        pool: SqlitePool,
        state_worker: StateWorker<Action>,
        ui_state: Arc<Mutex<UiState>>,
        ignore_cache: bool,
    ) -> eyre::Result<()> {
        tracing::info!("updating vci issuers");

        if ui_state.lock().unwrap().vci_issuers_loading {
            tracing::warn!("already loading, skipping");
            return Ok(());
        }

        let vci_issuers: VciIssuers = client.get(Self::ISSUER_URL).send().await?.json().await?;

        let total = vci_issuers.participating_issuers.len();

        {
            let mut ui_state = ui_state.lock().unwrap();
            ui_state.vci_issuers_loading = true;
            ui_state.vci_issuer_loaded = 0;
            ui_state.vci_issuer_total = total;
            ui_state.vci_issuers = Vec::with_capacity(total);
        }

        state_worker.send(Action::VciLoading);

        let futs = futures::stream::iter(
            vci_issuers
                .participating_issuers
                .into_iter()
                .map(|issuer| Self::update_vci_issuer(&client, &pool, issuer, ignore_cache)),
        );

        let mut issuers = futs.buffer_unordered(4);

        while let Some(meta) = issuers.try_next().await? {
            let mut ui_state = ui_state.lock().unwrap();
            ui_state.vci_issuer_loaded += 1;
            ui_state.add_issuer(meta);

            state_worker.send(Action::VciLoading);
        }

        ui_state.lock().unwrap().vci_issuers_loading = false;
        state_worker.send(Action::VciLoading);

        Ok(())
    }

    #[tracing::instrument(skip_all, fields(issuer_name = issuer.name, ignore_cache))]
    async fn update_vci_issuer(
        client: &reqwest::Client,
        pool: &SqlitePool,
        issuer: VciIssuer,
        ignore_cache: bool,
    ) -> eyre::Result<VciIssuerMeta> {
        tracing::debug!("checking issuer");

        let mut issuer_meta = VciIssuerMeta {
            name: issuer.name.clone(),
            iss: issuer.iss.clone(),
            error: false,
            keys: 0,
            jwk_set: None,
        };

        let record = sqlx::query!(
            "SELECT error, updated_at, count(DISTINCT vci_issuer_key.id) key_count
                FROM vci_issuer
                LEFT JOIN vci_issuer_key ON vci_issuer_key.vci_issuer_id = vci_issuer.id
                WHERE vci_issuer.iss = $1
                GROUP BY vci_issuer.iss",
            issuer.iss
        )
        .fetch_optional(pool)
        .await?;

        match record {
            None => tracing::debug!("new record"),
            Some(record)
                if ignore_cache
                    || time::OffsetDateTime::now_utc() - record.updated_at
                        > time::Duration::days(7) =>
            {
                tracing::debug!(ignore_cache, updated_at = %record.updated_at, "data was stale or ignoring cache");
                issuer_meta.keys = record.key_count;
            }
            Some(record) => {
                tracing::debug!("data was fresh, ignoring");
                issuer_meta.error = record.error;
                issuer_meta.keys = record.key_count;
                return Ok(issuer_meta);
            }
        }

        let key_set_resp = match client
            .get(format!("{}/.well-known/jwks.json", issuer.iss))
            .send()
            .await
        {
            Ok(key_set_resp) => key_set_resp,
            Err(err) => {
                tracing::warn!("could not request jwks: {err}");
                Self::save_vci_issuer(pool, issuer, true).await?;
                issuer_meta.error = true;
                return Ok(issuer_meta);
            }
        };

        let key_set: JwkSet = match key_set_resp.json().await {
            Ok(key_set) => key_set,
            Err(err) => {
                tracing::warn!("jwks endpoint returned invalid data: {err}");
                Self::save_vci_issuer(pool, issuer, true).await?;
                issuer_meta.error = true;
                return Ok(issuer_meta);
            }
        };

        tracing::info!("updated issuer, found {} keys", key_set.keys.len());
        issuer_meta.keys = key_set.keys.len() as i64;

        let mut tx = pool.begin().await?;

        let id = Self::save_vci_issuer(&mut *tx, issuer, false).await?;

        for key in key_set.keys.iter() {
            let key_id = key.common.key_id.as_deref().unwrap();
            let key_data = serde_json::to_value(key)?;
            sqlx::query!(
                "INSERT INTO vci_issuer_key (vci_issuer_id, key_id, data)
                    VALUES ($1, $2, $3) ON CONFLICT (vci_issuer_id, key_id) DO UPDATE SET
                        data = EXCLUDED.data",
                id,
                key_id,
                key_data
            )
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        issuer_meta.jwk_set = Some(key_set);
        Ok(issuer_meta)
    }

    async fn save_vci_issuer<'a, E>(
        executor: E,
        issuer: VciIssuer,
        error: bool,
    ) -> eyre::Result<i64>
    where
        E: SqliteExecutor<'a>,
    {
        sqlx::query_scalar!(
            "INSERT INTO vci_issuer (iss, name, website, canonical_iss, error)
                VALUES ($1, $2, $3, $4, $5) ON CONFLICT (iss) DO UPDATE SET
                    name = EXCLUDED.name,
                    website = EXCLUDED.website,
                    canonical_iss = EXCLUDED.canonical_iss,
                    updated_at = CURRENT_TIMESTAMP,
                    error = EXCLUDED.error
                RETURNING id",
            issuer.iss,
            issuer.name,
            issuer.website,
            issuer.canonical_iss,
            error
        )
        .fetch_one(executor)
        .await
        .map_err(Into::into)
    }

    fn decode_qr_data(input: &str) -> eyre::Result<String> {
        let data = input
            .trim()
            .strip_prefix("shc:/")
            .ok_or_else(|| eyre::eyre!("missing SHC prefix"))?;
        eyre::ensure!(data.len() % 2 == 0, "data length must be even");

        let mut payload = String::with_capacity(data.len() / 2);

        for pos in 0..(data.len() / 2) {
            let chunk = &data[pos * 2..=pos * 2 + 1];
            let num: u8 = chunk.parse()?;
            let ch = (num + 45) as char;
            payload.push(ch);
        }

        Ok(payload)
    }

    fn decompress_data<'a>(payload_parts: &[&'a str]) -> eyre::Result<Cow<'a, str>> {
        let header_data: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload_parts[0])?)?;

        let data = if header_data["zip"].as_str() == Some("DEF") {
            tracing::trace!("data was compressed");

            let main_data = URL_SAFE_NO_PAD.decode(payload_parts[1])?;

            let mut deflater = flate2::read::DeflateDecoder::new(main_data.as_slice());
            let mut decompressed_data = String::new();
            deflater.read_to_string(&mut decompressed_data)?;

            decompressed_data.into()
        } else {
            payload_parts[1].into()
        };

        Ok(data)
    }
}

#[async_trait]
impl BarcodeDecoder for SmartHealthCardDecoder {
    fn name(&self) -> &'static str {
        "SMART Health Card"
    }

    fn settings(&self, ui: &mut Ui) {
        let mut ui_state = self.ui_state.lock().unwrap();

        ui.separator();

        if ui_state.vci_issuers_loading {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "Loading VCI Issuers {}/{}",
                        ui_state.vci_issuer_loaded, ui_state.vci_issuer_total
                    ));
                    ui.spinner();
                });

                ui.add(
                    ProgressBar::new(
                        ui_state.vci_issuer_loaded as f32 / ui_state.vci_issuer_total as f32,
                    )
                    .show_percentage(),
                );
            });
        } else {
            ui.label(format!("VCI Issuers {}", ui_state.vci_issuer_total));

            if ui.button("Refresh VCI Issuers").clicked() {
                self.refresh_vci(true);
            }
        }

        if ui.button("VCI Issuer Database").clicked() {
            ui_state.showing_vci_issuers = true;
        }

        ui.separator();

        ui.add_enabled_ui(!ui_state.showing_cvx_codes, |ui| {
            if ui.button("CVX Code Database").clicked() {
                ui_state.showing_cvx_codes = true;
            }
        });

        Window::new("CVX Code Database")
            .open(&mut ui_state.showing_cvx_codes)
            .resizable(true)
            .default_width(700.0)
            .show(ui.ctx(), |ui| {
                TableBuilder::new(ui)
                    .column(Column::initial(50.0))
                    .column(Column::remainder())
                    .auto_shrink([false, false])
                    .header(18.0, |mut header| {
                        header.col(|ui| {
                            ui.heading("Code");
                        });

                        header.col(|ui| {
                            ui.heading("Name");
                        });
                    })
                    .body(|body| {
                        body.rows(18.0, self.sorted_cvx_codes.len(), |mut row| {
                            let (code, name) = &self.sorted_cvx_codes[row.index()];

                            row.col(|ui| {
                                ui.monospace(code);
                            });

                            row.col(|ui| {
                                ui.label(name);
                            });
                        });
                    });
            });

        let mut showing = ui_state.showing_vci_issuers;

        Window::new("VCI Issuer Database")
            .open(&mut showing)
            .resizable(true)
            .default_width(750.0)
            .show(ui.ctx(), |ui| {
                TableBuilder::new(ui)
                    .column(Column::initial(240.0))
                    .column(Column::auto())
                    .column(Column::initial(60.0))
                    .column(Column::remainder())
                    .auto_shrink([false, false])
                    .header(18.0, |mut header| {
                        header.col(|ui| {
                            ui.heading("Name");
                        });

                        header.col(|ui| {
                            ui.heading("Loaded");
                        });

                        header.col(|ui| {
                            ui.heading("# Keys");
                        });

                        header.col(|ui| {
                            ui.heading("Link");
                        });
                    })
                    .body(|body| {
                        body.rows(18.0, ui_state.vci_issuers.len(), |mut row| {
                            let meta = &ui_state.vci_issuers[row.index()];

                            row.col(|ui| {
                                if meta.name.len() > 36 {
                                    ui.label(meta.name.chars().take(36).join("") + "…")
                                        .on_hover_text(&meta.name);
                                } else {
                                    ui.label(&meta.name);
                                }
                            });

                            row.col(|ui| {
                                ui.label(if meta.error { "❌" } else { "✅" });
                            });

                            row.col(|ui| {
                                ui.label(meta.keys.to_string());
                            });

                            row.col(|ui| {
                                if let Ok(url) = url::Url::parse(&meta.iss) {
                                    ui.hyperlink_to(
                                        url.domain().unwrap_or(url.as_str()),
                                        &meta.iss,
                                    );
                                } else {
                                    ui.label("Invalid URL");
                                }
                            });
                        });
                    });
            });

        ui_state.showing_vci_issuers = showing;
    }

    async fn decode(&self, input: &str) -> eyre::Result<BoxedBarcodeData> {
        let qr_data = Self::decode_qr_data(input)?;
        tracing::trace!(input, "got payload data");

        let header = jsonwebtoken::decode_header(&qr_data)?;
        tracing::trace!(?header, "got jwt header");

        let payload_parts: Vec<_> = qr_data.split('.').collect();
        eyre::ensure!(
            payload_parts.len() == 3,
            "payload should have exactly three parts"
        );

        let decompressed_data = Self::decompress_data(&payload_parts)?;
        let data: serde_json::Value = serde_json::from_str(&decompressed_data)?;
        tracing::trace!(?data, "extracted data");

        let iss = data["iss"]
            .as_str()
            .ok_or_else(|| eyre::eyre!("data was missing issuer"))?;

        let kid = header.kid.unwrap();
        let jwk = sqlx::query_scalar!(
            r#"SELECT data "data: sqlx::types::Json<jsonwebtoken::jwk::Jwk>" FROM vci_issuer_key WHERE key_id = $1"#,
            kid
        )
        .fetch_optional(&self.pool)
        .await?;

        let message = &qr_data[..qr_data.rfind('.').expect("jwt must have delimiters")];

        let verified = if let Some(jwk) = jwk {
            tracing::debug!(?jwk, "found key");
            let key = jsonwebtoken::DecodingKey::from_jwk(&jwk)?;
            jsonwebtoken::crypto::verify(
                payload_parts[2],
                message.as_bytes(),
                &key,
                jsonwebtoken::Algorithm::ES256,
            )?
        } else {
            tracing::warn!(kid, "unable to find jwk for key, attempting to load");

            // Double-check we don't have a name for this issuer and the lack of
            // key isn't a cache or previous network issue.
            let name = sqlx::query_scalar!("SELECT name FROM vci_issuer WHERE iss = $1", iss)
                .fetch_optional(&self.pool)
                .await?
                .unwrap_or_else(|| "Unknown Issuer".to_string());

            let meta = Self::update_vci_issuer(
                &self.client,
                &self.pool,
                VciIssuer {
                    iss: iss.to_string(),
                    name,
                    website: None,
                    canonical_iss: None,
                },
                false,
            )
            .await?;

            let successful = if let Some(keys) = &meta.jwk_set {
                if let Some(key) = keys.find(&kid) {
                    tracing::debug!("found key in issuer jwks");
                    let key = jsonwebtoken::DecodingKey::from_jwk(key)?;
                    jsonwebtoken::crypto::verify(
                        payload_parts[2],
                        message.as_bytes(),
                        &key,
                        header.alg,
                    )?
                } else {
                    tracing::warn!("issuer did not have key");
                    false
                }
            } else {
                tracing::debug!("could not find keys for issuer");
                false
            };

            self.ui_state.lock().unwrap().add_issuer(meta);

            successful
        };

        let relevant_data: Vec<FhirBundleEntry> =
            serde_json::from_value(data["vc"]["credentialSubject"]["fhirBundle"]["entry"].clone())?;

        tracing::info!(verified, "processed smart health card");

        let issuer = sqlx::query!(
            "SELECT iss, name, website, canonical_iss FROM vci_issuer WHERE iss = $1",
            iss
        )
        .map(|issuer| VciIssuer {
            iss: issuer.iss,
            name: issuer.name,
            website: issuer.website,
            canonical_iss: issuer.canonical_iss,
        })
        .fetch_optional(&self.pool)
        .await?;

        Ok(Box::new(SmartHealthCardData {
            id: Uuid::new_v4(),
            verified,
            issuer,
            relevant_data,
            cvx_codes: self.cvx_codes.clone(),
            raw_data: data,
        }))
    }
}

#[derive(Debug)]
struct SmartHealthCardData {
    id: Uuid,
    verified: bool,
    issuer: Option<VciIssuer>,
    relevant_data: Vec<FhirBundleEntry>,
    cvx_codes: Arc<HashMap<String, String>>,
    raw_data: serde_json::Value,
}

impl SmartHealthCardData {
    fn patient_name(&self) -> String {
        let patients: Vec<_> = self
            .relevant_data
            .iter()
            .filter_map(|entry| match &entry.resource {
                FhirBundleEntryResource::Patient { name, .. } => name.first(),
                _ => None,
            })
            .collect();

        match patients.first() {
            Some(patient) if patients.len() == 1 => {
                format!("{} {}", patient.given.join(" "), patient.family)
            }
            Some(_) => "Multiple Patients".to_string(),
            None => "No Patients".to_string(),
        }
    }

    fn verified_widget(&self, ui: &mut Ui) {
        match &self.issuer {
            Some(issuer) if self.verified => {
                let text = RichText::new(format!("✅ Verified by {}", issuer.name));

                if issuer.name == "Unknown Issuer" {
                    ui.add(Hyperlink::from_label_and_url(
                        text.color(Color32::YELLOW),
                        &issuer.iss,
                    ));
                } else {
                    ui.add(Label::new(text.color(Color32::GREEN)));
                }
            }
            Some(issuer) => {
                ui.add(Label::new(
                    RichText::new(format!("❌ NOT Verified by {}", issuer.name))
                        .color(Color32::RED),
                ));
            }
            None => {
                ui.add(Label::new(
                    RichText::new("❌ NOT Verified").color(Color32::RED),
                ));
            }
        }
    }
}

impl BarcodeData for SmartHealthCardData {
    fn id(&self) -> Uuid {
        self.id
    }

    fn summary(&self) -> String {
        self.patient_name()
    }

    fn render(&self, ui: &mut Ui) {
        self.verified_widget(ui);

        let cm = TitlecaseMapper::new();
        let root: LanguageIdentifier = sys_locale::get_locale()
            .and_then(|locale| locale.parse().ok())
            .unwrap_or_default();

        static DATE_FORMAT: &[time::format_description::FormatItem<'_>] =
            format_description!("[year]-[month]-[day]");
        let calendar = icu::datetime::DateFormatter::try_new_with_length(
            &root.clone().into(),
            icu::datetime::options::length::Date::Medium,
        )
        .expect("formatter should exist");

        Grid::new(self.id)
            .num_columns(3)
            .striped(true)
            .spacing([40.0, 4.0])
            .show(ui, |ui| {
                for record in self.relevant_data.iter() {
                    match &record.resource {
                        FhirBundleEntryResource::Patient { birth_date, name } => {
                            let name = name.first().unwrap();

                            ui.strong("Patient");
                            ui.label(&record.full_url);
                            ui.vertical(|ui| {
                                ui.strong(format!("{} {}", name.given.join(" "), name.family));
                                ui.label(format!("🎂 {birth_date}"));
                            });
                        }
                        FhirBundleEntryResource::Immunization {
                            occurrence_date_time,
                            performer,
                            vaccine_code,
                            status,
                            patient,
                            lot_number,
                        } => {
                            let Some(coding) = vaccine_code.coding.first() else {
                                continue;
                            };

                            let name: Cow<'_, str> =
                                if coding.system == "http://hl7.org/fhir/sid/cvx" {
                                    let code_name = self.cvx_codes.get(&coding.code);
                                    code_name.unwrap_or(&coding.code).into()
                                } else {
                                    format!("{} - {}", coding.system, coding.code).into()
                                };

                            let performer = performer
                                .iter()
                                .map(|performer| &performer.actor.display)
                                .join(", ");

                            ui.strong("Immunization");
                            ui.label(&patient.reference);
                            ui.vertical(|ui| {
                                ui.strong(name);

                                if let Some(lot_number) = lot_number {
                                    ui.label(format!("{performer} — {lot_number}"));
                                } else {
                                    ui.label(performer);
                                }

                                let occurrence: Cow<'_, str> = if let Ok(date) =
                                    time::Date::parse(occurrence_date_time, DATE_FORMAT)
                                {
                                    let date_iso = icu::calendar::Date::try_new_iso_date(
                                        date.year(),
                                        date.month().into(),
                                        date.day(),
                                    )
                                    .expect("valid date should parse")
                                    .to_any();

                                    calendar
                                        .format_to_string(&date_iso)
                                        .expect("should be able to format")
                                        .into()
                                } else {
                                    occurrence_date_time.into()
                                };

                                let status = cm.titlecase_segment_to_string(
                                    status,
                                    &root,
                                    Default::default(),
                                );

                                ui.label(format!("{status} {occurrence}"));
                            });
                        }
                        FhirBundleEntryResource::Other(_) => {
                            ui.label("Unknown Record");
                        }
                    }

                    ui.end_row();
                }
            });

        CollapsingHeader::new("Raw Data")
            .id_source(format!("{}-data", self.id))
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
