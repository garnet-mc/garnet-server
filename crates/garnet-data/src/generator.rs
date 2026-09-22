//! Runs Mojang's data generator, which is bundled inside every server jar.
//!
//! `java -DbundlerMainClass=net.minecraft.data.Main -jar server.jar --reports`
//! writes `generated/reports/{blocks,registries,packets,...}.json`.

use crate::java::JavaRuntime;
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Stdio;

pub async fn run(java: &JavaRuntime, server_jar: &Path, output_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(output_dir)?;
    let work_dir = server_jar.parent().context("jar has no parent directory")?;
    tracing::info!("running Mojang's data generator (this takes a minute the first time)");

    let output = tokio::process::Command::new(&java.executable)
        .current_dir(work_dir)
        .arg("-DbundlerMainClass=net.minecraft.data.Main")
        .arg("-jar")
        .arg(server_jar)
        .arg("--reports")
        .arg("--output")
        .arg(output_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("launching {}", java.executable.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "data generator failed with {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            output.status,
            tail(&stdout, 40),
            tail(&stderr, 40)
        );
    }

    let reports = output_dir.join("reports");
    for required in ["packets.json", "blocks.json", "registries.json"] {
        if !reports.join(required).exists() {
            bail!("data generator finished but {} is missing", reports.join(required).display());
        }
    }
    // The generator also leaves logs and a libraries folder next to the jar.
    // They are harmless, but the `logs` dir is noise, so tidy it.
    let _ = std::fs::remove_dir_all(work_dir.join("logs"));
    Ok(())
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].join("\n")
}
