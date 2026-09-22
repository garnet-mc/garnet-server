//! Talking to Mojang's public download servers (piston-meta).

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

const VERSION_MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

#[derive(Debug, Deserialize)]
pub struct VersionManifest {
    pub latest: LatestVersions,
    pub versions: Vec<VersionSummary>,
}

#[derive(Debug, Deserialize)]
pub struct LatestVersions {
    pub release: String,
    pub snapshot: String,
}

#[derive(Debug, Deserialize)]
pub struct VersionSummary {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
}

/// The per-version JSON (only the parts we use).
#[derive(Debug, Clone, Deserialize)]
pub struct VersionMeta {
    pub id: String,
    pub downloads: Downloads,
    #[serde(rename = "javaVersion")]
    pub java_version: JavaVersion,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Downloads {
    pub server: Option<Download>,
    pub client: Option<Download>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Download {
    pub url: String,
    pub sha1: String,
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JavaVersion {
    pub component: String,
    #[serde(rename = "majorVersion")]
    pub major_version: u32,
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("garnet-server/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// Fetches the version manifest, caching it in `data_dir`.
pub async fn fetch_manifest(data_dir: &Path) -> Result<VersionManifest> {
    let cache = data_dir.join("version_manifest_v2.json");
    let text = match client()?.get(VERSION_MANIFEST_URL).send().await {
        Ok(resp) if resp.status().is_success() => {
            let text = resp.text().await?;
            std::fs::create_dir_all(data_dir)?;
            std::fs::write(&cache, &text)?;
            text
        }
        Ok(resp) => {
            tracing::warn!("version manifest returned HTTP {}, using cached copy", resp.status());
            std::fs::read_to_string(&cache).context("no cached version manifest")?
        }
        Err(err) => {
            tracing::warn!("could not reach Mojang ({err}), using cached version manifest");
            std::fs::read_to_string(&cache).context("no cached version manifest and Mojang is unreachable")?
        }
    };
    Ok(serde_json::from_str(&text)?)
}

pub async fn latest_release(data_dir: &Path) -> Result<String> {
    Ok(fetch_manifest(data_dir).await?.latest.release)
}

/// Fetches the JSON for one version, caching it next to the jar.
pub async fn fetch_version(data_dir: &Path, version_id: &str) -> Result<VersionMeta> {
    let cache = data_dir.join("versions").join(version_id).join("mojang.json");
    if let Ok(text) = std::fs::read_to_string(&cache) {
        if let Ok(meta) = serde_json::from_str(&text) {
            return Ok(meta);
        }
    }
    let manifest = fetch_manifest(data_dir).await?;
    let summary = manifest
        .versions
        .iter()
        .find(|v| v.id == version_id)
        .with_context(|| format!("Minecraft version {version_id} does not exist in Mojang's manifest"))?;
    let text = client()?.get(&summary.url).send().await?.error_for_status()?.text().await?;
    std::fs::create_dir_all(cache.parent().unwrap())?;
    std::fs::write(&cache, &text)?;
    Ok(serde_json::from_str(&text)?)
}

pub async fn download_server_jar(meta: &VersionMeta, target: &Path) -> Result<()> {
    let download = meta
        .downloads
        .server
        .as_ref()
        .with_context(|| format!("Minecraft {} has no server download", meta.id))?;
    download_verified(&download.url, &download.sha1, target).await
}

/// Downloads `url` to `target` unless the file already exists with the right
/// SHA-1. Writes to a temporary file first so a crash never leaves a
/// half-written file in place.
pub async fn download_verified(url: &str, sha1: &str, target: &Path) -> Result<()> {
    if target.exists() && file_sha1(target)? == sha1 {
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let size_hint = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    tracing::info!("downloading {size_hint} from {url}");

    let file_name = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let tmp: PathBuf = target.with_file_name(format!("{file_name}.part"));
    let mut resp = client()?.get(url).send().await?.error_for_status()?;
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut hasher = Sha1::new();
    while let Some(chunk) = resp.chunk().await? {
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);

    let actual = hex::encode(hasher.finalize());
    if actual != sha1 {
        let _ = std::fs::remove_file(&tmp);
        bail!("checksum mismatch for {url}: expected {sha1}, got {actual}");
    }
    std::fs::rename(&tmp, target)?;
    Ok(())
}

pub fn file_sha1(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}
