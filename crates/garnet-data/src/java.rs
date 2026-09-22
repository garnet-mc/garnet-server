//! Finding or installing a Java runtime. Java is only needed once per
//! version, to run Mojang's data generator.

use crate::mojang::{download_verified, VersionMeta};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const RUNTIME_MANIFEST_URL: &str =
    "https://launchermeta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";

/// Path to a `java` executable that satisfies the version's requirement.
#[derive(Debug, Clone)]
pub struct JavaRuntime {
    pub executable: PathBuf,
    pub major_version: u32,
}

/// Returns a usable Java, checking in this order:
/// 1. `GARNET_JAVA` environment variable,
/// 2. a runtime we downloaded earlier into `data_dir/java`,
/// 3. `java` on the PATH if it is new enough,
/// 4. Mojang's own runtime for this platform (downloaded).
pub async fn ensure_runtime(data_dir: &Path, meta: &VersionMeta) -> Result<JavaRuntime> {
    let required = meta.java_version.major_version;

    if let Ok(path) = std::env::var("GARNET_JAVA") {
        let exe = PathBuf::from(path);
        let major = probe_version(&exe).context("GARNET_JAVA does not point at a working java")?;
        if major < required {
            bail!("GARNET_JAVA is Java {major} but Minecraft {} needs Java {required}", meta.id);
        }
        return Ok(JavaRuntime { executable: exe, major_version: major });
    }

    let managed = managed_java_path(data_dir, &meta.java_version.component);
    if let Some(major) = probe_version(&managed) {
        if major >= required {
            return Ok(JavaRuntime { executable: managed, major_version: major });
        }
    }

    if let Some(major) = probe_version(Path::new("java")) {
        if major >= required {
            tracing::info!("using system Java {major}");
            return Ok(JavaRuntime { executable: PathBuf::from("java"), major_version: major });
        }
        tracing::info!("system Java is {major}, Minecraft {} needs {required}; downloading Mojang's runtime", meta.id);
    } else {
        tracing::info!("no system Java found; downloading Mojang's Java {required} runtime");
    }

    download_mojang_runtime(data_dir, &meta.java_version.component).await?;
    let major = probe_version(&managed).context("downloaded Java runtime does not run")?;
    Ok(JavaRuntime { executable: managed, major_version: major })
}

fn managed_java_path(data_dir: &Path, component: &str) -> PathBuf {
    let dir = data_dir.join("java").join(component);
    if cfg!(target_os = "macos") {
        dir.join("jre.bundle/Contents/Home/bin/java")
    } else if cfg!(windows) {
        dir.join("bin").join("java.exe")
    } else {
        dir.join("bin").join("java")
    }
}

/// Runs `java -version` and parses the major version, or `None` if it does
/// not run at all.
pub fn probe_version(exe: &Path) -> Option<u32> {
    let output = std::process::Command::new(exe).arg("-version").output().ok()?;
    let text = String::from_utf8_lossy(&output.stderr).to_string() + &String::from_utf8_lossy(&output.stdout);
    // Looks like: openjdk version "21.0.3" 2024-04-16  or  java version "1.8.0_392"
    let quoted = text.split('"').nth(1)?;
    let mut parts = quoted.split('.');
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

// ---- Mojang runtime manifest ----

#[derive(Debug, Deserialize)]
struct RuntimeComponent {
    manifest: RuntimeManifestRef,
}

#[derive(Debug, Deserialize)]
struct RuntimeManifestRef {
    url: String,
}

#[derive(Debug, Deserialize)]
struct RuntimeManifest {
    files: BTreeMap<String, RuntimeFile>,
}

#[derive(Debug, Deserialize)]
struct RuntimeFile {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    #[allow(dead_code)] // used on unix to set the mode bit
    executable: bool,
    downloads: Option<RuntimeDownloads>,
    target: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RuntimeDownloads {
    raw: RawDownload,
}

#[derive(Debug, Deserialize)]
struct RawDownload {
    url: String,
    sha1: String,
}

fn platform_key() -> Result<&'static str> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "windows-x64",
        ("windows", "aarch64") => "windows-arm64",
        ("windows", "x86") => "windows-x86",
        ("linux", "x86_64") => "linux",
        ("linux", "x86") => "linux-i386",
        ("macos", "x86_64") => "mac-os",
        ("macos", "aarch64") => "mac-os-arm64",
        (os, arch) => bail!("Mojang does not ship a Java runtime for {os}/{arch}; install Java and set GARNET_JAVA"),
    })
}

async fn download_mojang_runtime(data_dir: &Path, component: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let all: BTreeMap<String, BTreeMap<String, Vec<RuntimeComponent>>> =
        client.get(RUNTIME_MANIFEST_URL).send().await?.error_for_status()?.json().await?;
    let platform = platform_key()?;
    let component_list = all
        .get(platform)
        .and_then(|p| p.get(component))
        .with_context(|| format!("no Java runtime '{component}' for {platform}"))?;
    let entry = component_list
        .first()
        .with_context(|| format!("Java runtime '{component}' is empty for {platform}"))?;
    let manifest: RuntimeManifest = client.get(&entry.manifest.url).send().await?.error_for_status()?.json().await?;

    let root = data_dir.join("java").join(component);
    std::fs::create_dir_all(&root)?;
    let total = manifest.files.values().filter(|f| f.kind == "file").count();
    tracing::info!("downloading Java runtime '{component}' ({total} files) to {}", root.display());

    for (name, file) in &manifest.files {
        let path = root.join(name);
        match file.kind.as_str() {
            "directory" => std::fs::create_dir_all(&path)?,
            "file" => {
                let download = &file.downloads.as_ref().context("runtime file without download")?.raw;
                download_verified(&download.url, &download.sha1, &path).await?;
                #[cfg(unix)]
                if file.executable {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
                }
            }
            "link" => {
                #[cfg(unix)]
                if let Some(target) = &file.target {
                    let _ = std::fs::remove_file(&path);
                    std::os::unix::fs::symlink(target, &path)?;
                }
                #[cfg(not(unix))]
                let _ = &file.target;
            }
            other => tracing::warn!("unknown runtime entry type {other} for {name}"),
        }
    }
    Ok(())
}
