#![allow(dead_code)]

use crate::importer::{DbThread, SerializedThread};
use crate::renderer;
use crate::utils::{
    self, ExportConfig, ProcessResult, backup_database, extract_json_timestamp,
    parse_existing_frontmatter,
};
use eyre::{Context, Result, eyre};
use indicatif::{ProgressBar, ProgressStyle};
use rusqlite::Connection;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// The main entry point for the sequential export logic.
/// Handles snapshotting, migration, and the export loop.
pub fn execute(config: ExportConfig) -> Result<()> {
    let snapshot = backup_database(&config.db_path, config.quiet)?;
    run_internal(snapshot.path(), &config)
}

fn allocate_filename(id: &str, title: &str, registry: &mut HashMap<String, String>) -> String {
    let raw_slug = slug::slugify(title);
    // Truncate slug to 60 chars (slug output is ASCII-only, so byte == char)
    let slug = raw_slug[..raw_slug.len().min(60)]
        .trim_end_matches('-')
        .to_string();

    for &len in &[8usize, 12usize, id.len()] {
        let candidate = &id[..len.min(id.len())];
        match registry.get(candidate) {
            None => {
                registry.insert(candidate.to_string(), id.to_string());
                return if slug.is_empty() {
                    candidate.to_string()
                } else {
                    format!("{}_{}", candidate, slug)
                };
            }
            Some(existing) if existing == id => {
                return if slug.is_empty() {
                    candidate.to_string()
                } else {
                    format!("{}_{}", candidate, slug)
                };
            }
            Some(_) => continue,
        }
    }
    // Unreachable: full UUID is always unique per thread
    if slug.is_empty() {
        id.to_string()
    } else {
        format!("{}_{}", id, slug)
    }
}

/// Build an in-memory index of existing .md files: prefix → full path.
/// The prefix is the portion of the filename before the first '_' (or before '.md' if no '_').
fn build_file_index(target_dir: &Path) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    let Ok(entries) = fs::read_dir(target_dir) else {
        return map;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.ends_with(".md") {
            continue;
        }
        let stem = name_str.trim_end_matches(".md");
        let prefix = stem.split('_').next().unwrap_or(stem).to_string();
        if !prefix.is_empty() {
            map.insert(prefix, entry.path());
        }
    }
    map
}

#[allow(clippy::too_many_arguments)]
fn export_thread(
    id: &str,
    data_type: &str,
    raw_data: &[u8],
    title: &str,
    config: &ExportConfig,
    registry: &mut HashMap<String, String>,
    file_index: &mut HashMap<String, PathBuf>,
    pb: &ProgressBar,
) -> Result<ProcessResult> {
    let stem = allocate_filename(id, title, registry);

    // Extract prefix (everything before first '_')
    let prefix = stem.split('_').next().unwrap_or(&stem).to_string();

    let desired_path = config.target_dir.join(format!("{}.md", stem));
    let existing_path = file_index.get(&prefix).cloned();

    // Idempotency check — only decompress/parse if a file exists to compare against
    let mut cached_json: Option<Vec<u8>> = None;
    if !config.force
        && let Some(ref existing) = existing_path
            && let Some(fm) = parse_existing_frontmatter(existing) {
                let json_bytes: Vec<u8> = utils::decompress(data_type, raw_data)
                    .wrap_err("Failed to decompress data")?;
                if let Some(db_ts) = extract_json_timestamp(&json_bytes)
                    && fm.updated_at >= db_ts && fm.include_context == config.include_context {
                        if config.verbose {
                            pb.println(format!("Skipped:  {}.md", stem));
                        }
                        return Ok(ProcessResult::Skipped);
                    }
                cached_json = Some(json_bytes);
            }

    let json_bytes: Vec<u8> = match cached_json {
        Some(b) => b,
        None => utils::decompress(data_type, raw_data)
            .wrap_err("Failed to decompress data")?,
    };

    let result_variant = if existing_path.is_none() {
        ProcessResult::Created
    } else {
        ProcessResult::Updated
    };

    // Rename if slug changed (Scenario C)
    if let Some(ref existing) = existing_path
        && existing != &desired_path
            && let Err(e) = fs::rename(existing, &desired_path) {
                pb.println(format!(
                    "Warning: could not rename {} → {}: {}",
                    existing.display(),
                    desired_path.display(),
                    e
                ));
            }

    // Update the index so subsequent lookups reflect the rename
    file_index.insert(prefix, desired_path.clone());

    let md_file = File::create(&desired_path)
        .wrap_err_with(|| format!("Failed to create: {}", desired_path.display()))?;
    let mut writer = BufWriter::new(md_file);

    let tags = config.tags.as_deref();

    let assets = match serde_json::from_slice::<DbThread>(&json_bytes) {
        Ok(thread) => renderer::render_thread(
            &mut writer,
            id,
            &stem,
            &thread,
            tags,
            config.include_context,
        )
        .wrap_err("Failed to write DbThread markdown")?,
        Err(_) => match serde_json::from_slice::<SerializedThread>(&json_bytes) {
            Ok(thread) => {
                renderer::render_serialized_thread(&mut writer, id, &thread, tags)
                    .wrap_err("Failed to write SerializedThread markdown")?;
                None
            }
            Err(e) => {
                drop(writer);
                let _ = fs::remove_file(&desired_path);
                return Err(eyre!(
                    "Could not deserialize as DbThread or SerializedThread: {}",
                    e
                ));
            }
        },
    };

    writer.flush().wrap_err("Failed to flush markdown file")?;
    drop(writer);

    if let Some(asset_list) = assets {
        let assets_dir = config.target_dir.join("assets");
        for asset in asset_list {
            let asset_path = assets_dir.join(&asset.name);
            fs::write(&asset_path, &asset.data)
                .wrap_err_with(|| format!("Failed to write asset: {}", asset.name))?;
        }
    }

    if config.verbose {
        match result_variant {
            ProcessResult::Created => pb.println(format!("Created:  {}.md", stem)),
            ProcessResult::Updated => pb.println(format!("Updated:  {}.md", stem)),
            ProcessResult::Skipped => unreachable!(),
        }
    }

    Ok(result_variant)
}

fn run_internal(snapshot_path: &Path, config: &ExportConfig) -> Result<()> {
    fs::create_dir_all(&config.target_dir).wrap_err_with(|| {
        format!(
            "Failed to create target directory: {}",
            config.target_dir.display()
        )
    })?;
    fs::create_dir_all(config.target_dir.join("assets"))
        .wrap_err("Failed to create assets directory")?;

    let conn = Connection::open(snapshot_path).wrap_err("Failed to open snapshot database")?;

    let total: u64 = conn
        .query_row("SELECT COUNT(*) FROM threads", [], |row| {
            row.get::<_, i64>(0)
        })
        .wrap_err("Failed to count threads")? as u64;

    let mut file_index = build_file_index(&config.target_dir);

    let pb = if config.quiet {
        ProgressBar::hidden()
    } else {
        let bar = ProgressBar::new(total);
        bar.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%)",
            )
            .unwrap()
            .progress_chars("=>-"),
        );
        bar.println(format!("Found {} threads.", total));
        bar
    };

    let mut stmt = conn
        .prepare("SELECT id, data_type, data, summary FROM threads ORDER BY updated_at DESC")
        .wrap_err("Failed to prepare query")?;

    let mut rows = stmt.query([]).wrap_err("Failed to execute query")?;

    let mut registry: HashMap<String, String> = HashMap::new();
    let mut count_created = 0usize;
    let mut count_updated = 0usize;
    let mut count_skipped = 0usize;
    let mut count_errors = 0usize;

    while let Some(row) = rows.next().wrap_err("Failed to read row")? {
        let id: String = row.get(0)?;
        let data_type: String = row.get(1)?;
        let raw_data: Vec<u8> = row.get(2)?;
        let summary: String = row.get(3)?;
        match export_thread(
            &id,
            &data_type,
            &raw_data,
            &summary,
            config,
            &mut registry,
            &mut file_index,
            &pb,
        ) {
            Ok(ProcessResult::Created) => count_created += 1,
            Ok(ProcessResult::Updated) => count_updated += 1,
            Ok(ProcessResult::Skipped) => {
                count_skipped += 1;
                // Since rows are ordered newest-first, once we skip a thread that's
                // already up-to-date the rest are also guaranteed up-to-date.
                // Drain the count and stop early.
                let remaining = total.saturating_sub(
                    (count_created + count_updated + count_skipped + count_errors) as u64,
                );
                count_skipped += remaining as usize;
                pb.inc(remaining + 1);
                break;
            }
            Err(e) => {
                count_errors += 1;
                pb.println(format!("Error [{}]: {:#}", &id[..8.min(id.len())], e));
            }
        }
        pb.inc(1);
    }

    pb.finish_and_clear();

    if !config.quiet {
        let mut summary = format!(
            "Done. {} created, {} updated, {} skipped.",
            count_created, count_updated, count_skipped
        );
        if count_errors > 0 {
            summary.push_str(&format!(" Completed with {} error(s).", count_errors));
        }
        eprintln!("{}", summary);
    }

    Ok(())
}
