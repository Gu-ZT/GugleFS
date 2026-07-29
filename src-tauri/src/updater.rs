use std::time::Duration;

use semver::Version;
use serde::{Deserialize, Serialize};

const RELEASES_PAGE: &str = "https://github.com/Gu-ZT/GugleFS/releases";
const UPDATE_API: &str = "https://api.github.com/repos/Gu-ZT/GugleFS/releases?per_page=1";
const UPDATE_FALLBACK: &str = "https://gh-proxy.com/https://raw.githubusercontent.com/Gu-ZT/GugleFS/main/src-tauri/tauri.conf.json";

type UpdateResult<T> = Result<T, String>;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    current_version: String,
    latest_version: String,
    update_available: bool,
    download_url: &'static str,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
}

#[derive(Deserialize)]
struct TauriConfig {
    version: String,
}

#[tauri::command]
pub async fn check_for_updates() -> UpdateResult<UpdateInfo> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| "failed to initialize update client".to_string())?;
    let latest_version = fetch_latest_version(&client)
        .await
        .map_err(|_| "update service is unavailable".to_string())?;
    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let update_available = is_newer(&latest_version, &current_version)?;

    Ok(UpdateInfo {
        current_version,
        latest_version,
        update_available,
        download_url: RELEASES_PAGE,
    })
}

async fn fetch_latest_version(client: &reqwest::Client) -> UpdateResult<String> {
    match fetch_github_version(client).await {
        Ok(version) => Ok(version),
        Err(_) => fetch_fallback_version(client).await,
    }
}

async fn fetch_github_version(client: &reqwest::Client) -> UpdateResult<String> {
    let response = client
        .get(UPDATE_API)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header(reqwest::header::USER_AGENT, "GugleFS update checker")
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("GitHub returned {}", response.status()));
    }
    let body = response.text().await.map_err(|error| error.to_string())?;
    let releases: Vec<Release> = serde_json::from_str(&body).map_err(|error| error.to_string())?;
    releases
        .into_iter()
        .next()
        .map(|release| release.tag_name)
        .ok_or_else(|| "GitHub returned no releases".to_string())
}

async fn fetch_fallback_version(client: &reqwest::Client) -> UpdateResult<String> {
    let response = client
        .get(UPDATE_FALLBACK)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("fallback returned {}", response.status()));
    }
    let body = response.text().await.map_err(|error| error.to_string())?;
    let config: TauriConfig = serde_json::from_str(&body).map_err(|error| error.to_string())?;
    Ok(config.version)
}

fn is_newer(remote: &str, current: &str) -> UpdateResult<bool> {
    let mut remote = parse_version(remote)?;
    let mut current = parse_version(current)?;
    remote.build = semver::BuildMetadata::EMPTY;
    current.build = semver::BuildMetadata::EMPTY;
    Ok(remote > current)
}

fn parse_version(value: &str) -> UpdateResult<Version> {
    Version::parse(value.trim().trim_start_matches(['v', 'V'])).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_semantic_versions_and_accepts_release_tags() {
        assert!(is_newer("v0.12.0", "0.11.0").unwrap());
        assert!(is_newer("0.12.0-rc.2", "0.12.0-rc.1").unwrap());
        assert!(!is_newer("0.11.0", "0.11.0").unwrap());
        assert!(!is_newer("0.10.9", "0.11.0").unwrap());
    }

    #[test]
    fn ignores_build_metadata_for_update_precedence() {
        assert!(!is_newer("0.11.0+build.20", "0.11.0+build.10").unwrap());
        assert!(!is_newer("0.11.0+build.20", "0.11.0").unwrap());
    }
}
