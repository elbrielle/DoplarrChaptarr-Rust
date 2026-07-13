//! Configuration validation and backend startup shared by normal and preflight
//! modes. Keeping this boundary outside the Discord event loop lets operators
//! prove a deployment without opening a second bot session.

use crate::{
    config::{Backend, BackendConfig, Config},
    providers::{
        MediaBackend, chaptarr::Chaptarr, radarr::Radarr, seerr::Seerr as SeerrBackend,
        sonarr::Sonarr,
    },
};
use anyhow::{Result, bail};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackendCheck {
    media: String,
    provider: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compatibility: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct PreflightReport<'a> {
    status: &'static str,
    discord: &'static str,
    backends: &'a [BackendCheck],
}

pub struct ConnectedBackends {
    pub by_media: HashMap<String, Arc<dyn MediaBackend>>,
    pub media_types: HashSet<String>,
    checks: Vec<BackendCheck>,
}

impl ConnectedBackends {
    /// Print a stable, machine-readable report which deliberately excludes
    /// backend URLs, API keys, root paths, and profile names.
    pub fn print_preflight_report(&self) -> Result<()> {
        let unsupported = self
            .checks
            .iter()
            .any(|check| check.compatibility == Some("untested"));
        let report = PreflightReport {
            status: if unsupported { "unsupported" } else { "ok" },
            discord: "not_contacted",
            backends: &self.checks,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
        if unsupported {
            bail!(
                "Preflight rejected an untested backend version; update the compatibility contract before deployment"
            );
        }
        Ok(())
    }
}

pub async fn connect_backends(config: &Config) -> Result<ConnectedBackends> {
    if config.backends.is_empty() {
        bail!("At least one media backend is required!");
    }

    let mut media_types = HashSet::new();
    if !config
        .backends
        .iter()
        .all(|backend| media_types.insert(backend.media.clone()))
    {
        bail!("There must only be one of each media type");
    }

    let backend_http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()?;

    let mut by_media = HashMap::new();
    let mut checks = Vec::with_capacity(config.backends.len());

    for Backend { media, config } in &config.backends {
        let (backend, provider, version, compatibility): (
            Arc<dyn MediaBackend>,
            &'static str,
            Option<String>,
            Option<&'static str>,
        ) = match config {
            BackendConfig::Radarr { .. } => (
                Arc::new(Radarr::connect(config.clone(), backend_http.clone()).await?),
                "Radarr",
                None,
                None,
            ),
            BackendConfig::Sonarr { .. } => (
                Arc::new(Sonarr::connect(config.clone(), backend_http.clone()).await?),
                "Sonarr",
                None,
                None,
            ),
            BackendConfig::Seerr { .. } => (
                Arc::new(SeerrBackend::connect(config.clone(), backend_http.clone()).await?),
                "Seerr",
                None,
                None,
            ),
            BackendConfig::Chaptarr { .. } => {
                let backend = Chaptarr::connect(config.clone(), backend_http.clone()).await?;
                let version = Some(backend.server_version().to_string());
                let compatibility = Some(if backend.server_version_is_tested() {
                    "tested"
                } else {
                    "untested"
                });
                (Arc::new(backend), "Chaptarr", version, compatibility)
            }
        };

        by_media.insert(media.clone(), backend);
        checks.push(BackendCheck {
            media: media.clone(),
            provider,
            version,
            compatibility,
        });
    }

    Ok(ConnectedBackends {
        by_media,
        media_types,
        checks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitized_report_cannot_include_connection_details() {
        let report = PreflightReport {
            status: "ok",
            discord: "not_contacted",
            backends: &[BackendCheck {
                media: "book".into(),
                provider: "Chaptarr",
                version: Some("0.9.720.0".into()),
                compatibility: Some("tested"),
            }],
        };

        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("not_contacted"));
        assert!(json.contains("0.9.720.0"));
        assert!(!json.contains("url"));
        assert!(!json.contains("api_key"));
        assert!(!json.contains("root"));
        assert!(!json.contains("profile"));
    }

    #[test]
    fn an_untested_backend_version_fails_preflight() {
        let connected = ConnectedBackends {
            by_media: HashMap::new(),
            media_types: HashSet::new(),
            checks: vec![BackendCheck {
                media: "book".into(),
                provider: "Chaptarr",
                version: Some("0.9.999.0".into()),
                compatibility: Some("untested"),
            }],
        };

        assert!(connected.print_preflight_report().is_err());
    }
}
