//! World backups: a zip of the world folder in `backups/`, oldest pruned.

use crate::server::Server;
use anyhow::{Context, Result};
use garnet_admin::api::BackupInfo;
use std::io::Write;
use std::path::PathBuf;

pub fn backup_dir(server: &Server) -> PathBuf {
    server.root.join(&server.config().backups.dir)
}

/// Saves the world, then zips it. Blocking; run it on the blocking pool.
pub fn create(server: &Server, source: &str) -> Result<PathBuf> {
    server.save_everything("backup");
    let dir = backup_dir(server);
    std::fs::create_dir_all(&dir)?;
    let world_name = server.config().world.name.clone();
    let world_dir = server.root.join(&world_name);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let target = dir.join(format!("{world_name}-{stamp}.zip"));
    let tmp = target.with_extension("zip.part");

    let started = std::time::Instant::now();
    {
        let file = std::fs::File::create(&tmp)?;
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut stack = vec![world_dir.clone()];
        while let Some(current) = stack.pop() {
            for entry in std::fs::read_dir(&current)? {
                let path = entry?.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().map(|e| e == "tmp" || e == "part").unwrap_or(false) {
                    continue;
                }
                let name = path.strip_prefix(&server.root)?.to_string_lossy().replace('\\', "/");
                zip.start_file(name, options)?;
                let mut reader = std::fs::File::open(&path)?;
                std::io::copy(&mut reader, &mut zip)?;
            }
        }
        zip.finish()?.flush()?;
    }
    std::fs::rename(&tmp, &target)?;
    let size = std::fs::metadata(&target)?.len();
    tracing::info!(
        "backup ({source}) written to {} ({:.1} MB in {} s)",
        target.display(),
        size as f64 / 1048576.0,
        started.elapsed().as_secs()
    );
    server.audit.record(source, "backup", target.file_name().unwrap().to_string_lossy());
    prune(server)?;
    Ok(target)
}

fn prune(server: &Server) -> Result<()> {
    let keep = server.config().backups.keep.max(1);
    let mut backups = list(server)?;
    backups.sort_by(|a, b| b.created.cmp(&a.created));
    for old in backups.iter().skip(keep) {
        let path = backup_dir(server).join(&old.file);
        if let Err(err) = std::fs::remove_file(&path) {
            tracing::warn!("could not delete old backup {}: {err}", path.display());
        }
    }
    Ok(())
}

pub fn list(server: &Server) -> Result<Vec<BackupInfo>> {
    let dir = backup_dir(server);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).context("listing backups")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map(|e| e == "zip").unwrap_or(false) {
            let meta = entry.metadata()?;
            let created: chrono::DateTime<chrono::Local> = meta.modified()?.into();
            out.push(BackupInfo {
                file: path.file_name().unwrap().to_string_lossy().to_string(),
                size_bytes: meta.len(),
                created: created.format("%Y-%m-%d %H:%M:%S").to_string(),
            });
        }
    }
    out.sort_by(|a, b| b.created.cmp(&a.created));
    Ok(out)
}
