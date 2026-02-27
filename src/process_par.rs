use crate::exporter;
use crate::importer::{DbThread, SerializedThread};
use crate::process::ExportConfig;
use chrono::{DateTime, Utc};
use crossbeam_channel::{SendTimeoutError, bounded};
use eyre::{Context, Result, eyre};
use indicatif::{ProgressBar, ProgressStyle};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

struct GlobalState {
    registry: HashMap<String, String>,
}

#[derive(Clone, Copy)]
enum ProcessResult {
    Created,
    Updated,
    Skipped,
}

pub fn run(config: ExportConfig) -> Result<()> {
    fs::create_dir_all(&config.target_dir).wrap_err("Failed to create target dir")?;
    fs::create_dir_all(config.target_dir.join("assets")).wrap_err("Failed to create assets dir")?;

    if has_existing_exports(&config.target_dir) {
        run_incremental(&config)
    } else {
        run_fresh(&config)
    }
}

fn has_existing_exports(target_dir: &Path) -> bool {
    fs::read_dir(target_dir)
        .map(|d| {
            d.flatten()
                .any(|e| e.file_name().to_string_lossy().ends_with(".md"))
        })
        .unwrap_or(false)
}

fn open_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .wrap_err("Failed to open database")?;
    conn.execute_batch("PRAGMA cache_size = -16384;")
        .wrap_err("Failed to set cache_size")?;
    Ok(conn)
}

fn make_bar(total: Option<u64>, quiet: bool) -> ProgressBar {
    if quiet {
        return ProgressBar::hidden();
    }
    match total {
        Some(n) => {
            let bar = ProgressBar::new(n);
            bar.set_style(
                ProgressStyle::with_template(
                    "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {msg}",
                )
                .unwrap()
                .progress_chars("=>-"),
            );
            bar
        }
        None => {
            let s = ProgressBar::new_spinner();
            s.set_style(ProgressStyle::default_spinner());
            s.set_message("Exporting...");
            s.enable_steady_tick(Duration::from_millis(80));
            s
        }
    }
}

// ── Fresh path ────────────────────────────────────────────────────────────────

fn run_fresh(config: &ExportConfig) -> Result<()> {
    let ids: Vec<String> = {
        let conn = open_db(&config.db_path)?;
        let mut stmt = conn
            .prepare("SELECT id FROM threads")
            .wrap_err("Failed to prepare query")?;
        stmt.query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<_, _>>()
            .wrap_err("Failed to collect ids")?
    };

    let state = Mutex::new(GlobalState {
        registry: HashMap::new(),
    });
    let pb = make_bar(Some(ids.len() as u64), config.quiet);

    let (tx, rx) = bounded::<String>(512);
    let count_created = AtomicUsize::new(0);
    let count_errors = AtomicUsize::new(0);
    let n_workers = std::thread::available_parallelism()
        .map(|n| n.get() * 2)
        .unwrap_or(8);

    std::thread::scope(|s| {
        for _ in 0..n_workers {
            let rx = rx.clone();
            let (config, state, pb) = (&config, &state, &pb);
            let (count_created, count_errors) = (&count_created, &count_errors);

            s.spawn(move || {
                let conn = match open_db(&config.db_path) {
                    Ok(c) => c,
                    Err(e) => {
                        pb.println(format!("Worker DB open failed: {:#}", e));
                        return;
                    }
                };

                loop {
                    let id = match rx.recv() {
                        Ok(id) => id,
                        Err(_) => break,
                    };

                    let row_result = conn.query_row(
                        "SELECT data_type, data, summary FROM threads WHERE id = ?",
                        [&id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, Vec<u8>>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        },
                    );

                    match row_result {
                        Ok((data_type, data, summary)) => {
                            match process_thread(
                                &id, &data_type, &data, &summary, None, config, state, pb,
                            ) {
                                Ok(ProcessResult::Created) => {
                                    count_created.fetch_add(1, Ordering::Relaxed);
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    count_errors.fetch_add(1, Ordering::Relaxed);
                                    pb.println(format!(
                                        "Error [{}]: {:#}",
                                        &id[..8.min(id.len())],
                                        e
                                    ));
                                }
                            }
                        }
                        Err(e) => {
                            count_errors.fetch_add(1, Ordering::Relaxed);
                            pb.println(format!(
                                "Error fetching [{}]: {:#}",
                                &id[..8.min(id.len())],
                                e
                            ));
                        }
                    }
                    pb.inc(1);
                }
            });
        }

        drop(rx);

        'outer: for id in &ids {
            let mut pending = id.clone();
            loop {
                match tx.send_timeout(pending, Duration::from_millis(50)) {
                    Ok(()) => break,
                    Err(SendTimeoutError::Disconnected(_)) => break 'outer,
                    Err(SendTimeoutError::Timeout(r)) => {
                        pending = r;
                    }
                }
            }
        }

        drop(tx);
        Ok::<_, eyre::Error>(())
    })
    .wrap_err("Fresh pipeline failed")?;

    pb.finish_and_clear();

    if !config.quiet {
        eprintln!(
            "Done (Fresh). {} created. Errors: {}",
            count_created.load(Ordering::Relaxed),
            count_errors.load(Ordering::Relaxed),
        );
    }

    Ok(())
}

// ── Incremental path ──────────────────────────────────────────────────────────

fn run_incremental(config: &ExportConfig) -> Result<()> {
    let ordered_ids: Vec<String> = {
        let conn = open_db(&config.db_path)?;
        let mut stmt = conn
            .prepare("SELECT id FROM threads ORDER BY updated_at DESC")
            .wrap_err("Failed to prepare id query")?;
        stmt.query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<_, _>>()
            .wrap_err("Failed to collect ids")?
    };

    let total = ordered_ids.len() as u64;
    let state = Mutex::new(GlobalState {
        registry: HashMap::new(),
    });
    let pb = make_bar(Some(total), config.quiet);

    let (tx, rx) = bounded::<String>(64);
    let count_created = AtomicUsize::new(0);
    let count_updated = AtomicUsize::new(0);
    let count_skipped = AtomicUsize::new(0);
    let count_errors = AtomicUsize::new(0);
    let should_stop = AtomicBool::new(false);
    let n_workers = std::thread::available_parallelism()
        .map(|n| n.get() * 2)
        .unwrap_or(8);

    std::thread::scope(|s| {
        for _ in 0..n_workers {
            let rx = rx.clone();
            let (config, state, pb) = (&config, &state, &pb);
            let (count_created, count_updated, count_skipped, count_errors) = (
                &count_created,
                &count_updated,
                &count_skipped,
                &count_errors,
            );
            let should_stop = &should_stop;

            s.spawn(move || {
                let conn = match open_db(&config.db_path) {
                    Ok(c) => c,
                    Err(e) => {
                        pb.println(format!("Worker DB open failed: {:#}", e));
                        return;
                    }
                };

                loop {
                    if should_stop.load(Ordering::Relaxed) {
                        break;
                    }

                    let id = match rx.recv() {
                        Ok(id) => id,
                        Err(_) => break,
                    };

                    if should_stop.load(Ordering::Relaxed) {
                        break;
                    }

                    let row_result = conn.query_row(
                        "SELECT data_type, data, summary FROM threads WHERE id = ?",
                        [&id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, Vec<u8>>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        },
                    );

                    let (data_type, data, summary) = match row_result {
                        Ok(r) => r,
                        Err(e) => {
                            count_errors.fetch_add(1, Ordering::Relaxed);
                            pb.println(format!(
                                "Error fetching [{}]: {:#}",
                                &id[..8.min(id.len())],
                                e
                            ));
                            pb.inc(1);
                            continue;
                        }
                    };

                    let existing_path = find_existing_file(&config.target_dir, &id);

                    match process_thread(
                        &id,
                        &data_type,
                        &data,
                        &summary,
                        existing_path,
                        config,
                        state,
                        pb,
                    ) {
                        Ok(ProcessResult::Created) => {
                            count_created.fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(ProcessResult::Updated) => {
                            count_updated.fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(ProcessResult::Skipped) => {
                            count_skipped.fetch_add(1, Ordering::Relaxed);
                            pb.inc(1);
                            should_stop.store(true, Ordering::Relaxed);
                            break;
                        }
                        Err(e) => {
                            count_errors.fetch_add(1, Ordering::Relaxed);
                            pb.println(format!("Error [{}]: {:#}", &id[..8.min(id.len())], e));
                        }
                    }
                    pb.inc(1);
                }
            });
        }

        drop(rx);

        'outer: for id in &ordered_ids {
            if should_stop.load(Ordering::Relaxed) {
                break;
            }
            let mut pending = id.clone();
            loop {
                match tx.send_timeout(pending, Duration::from_millis(50)) {
                    Ok(()) => break,
                    Err(SendTimeoutError::Disconnected(_)) => break 'outer,
                    Err(SendTimeoutError::Timeout(r)) => {
                        pending = r;
                        if should_stop.load(Ordering::Relaxed) {
                            break 'outer;
                        }
                    }
                }
            }
        }

        drop(tx);
        Ok::<_, eyre::Error>(())
    })
    .wrap_err("Incremental pipeline failed")?;

    pb.finish_and_clear();

    if !config.quiet {
        eprintln!(
            "Done (Incremental). {} created, {} updated, {} skipped. Errors: {}",
            count_created.load(Ordering::Relaxed),
            count_updated.load(Ordering::Relaxed),
            count_skipped.load(Ordering::Relaxed),
            count_errors.load(Ordering::Relaxed),
        );
    }

    Ok(())
}

// ── Shared processing ─────────────────────────────────────────────────────────

// Find a file whose name starts with the first 8 chars of the UUID,
// then confirm ownership by reading the `id:` field from its frontmatter.
// Handles the rare collision case where multiple files share an 8-char prefix.
fn find_existing_file(target_dir: &Path, id: &str) -> Option<PathBuf> {
    let prefix = &id[..8.min(id.len())];
    fs::read_dir(target_dir)
        .ok()?
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let s = n.to_string_lossy();
            s.ends_with(".md") && s.starts_with(prefix)
        })
        .find_map(|e| {
            let path = e.path();
            let fm = read_file_frontmatter(&path)?;
            if fm.id.as_deref() == Some(id) {
                Some(path)
            } else {
                None
            }
        })
}

fn process_thread(
    id: &str,
    data_type: &str,
    raw_data: &[u8],
    title: &str,
    existing_path: Option<PathBuf>,
    config: &ExportConfig,
    state: &Mutex<GlobalState>,
    pb: &ProgressBar,
) -> Result<ProcessResult> {
    let mut cached_json: Option<Vec<u8>> = None;
    if !config.force {
        if let Some(ref existing) = existing_path {
            if let Some(fm) = read_file_frontmatter(existing) {
                let json_bytes = decompress(data_type, raw_data)?;
                if let Some(db_ts) = get_updated_at(&json_bytes) {
                    if fm.updated_at >= db_ts && fm.include_context == config.include_context {
                        if config.verbose {
                            pb.println(format!("Skipped: {}", id));
                        }
                        return Ok(ProcessResult::Skipped);
                    }
                }
                cached_json = Some(json_bytes);
            }
        }
    }

    let json_bytes = match cached_json {
        Some(b) => b,
        None => decompress(data_type, raw_data)?,
    };

    let (parsed_db_thread, parsed_serialized_thread) =
        match serde_json::from_slice::<DbThread>(&json_bytes) {
            Ok(t) => (Some(t), None),
            Err(_) => match serde_json::from_slice::<SerializedThread>(&json_bytes) {
                Ok(t) => (None, Some(t)),
                Err(e) => return Err(eyre!("Deserialization failed: {}", e)),
            },
        };

    let (stem, desired_path, result_variant) = {
        let mut guard = state.lock().unwrap();
        let stem = allocate_filename(id, title, &mut guard.registry);
        let desired = config.target_dir.join(format!("{}.md", stem));
        let variant = if existing_path.is_none() {
            ProcessResult::Created
        } else {
            ProcessResult::Updated
        };
        (stem, desired, variant)
    };

    if let Some(ref old_path) = existing_path {
        if old_path != &desired_path {
            if let Err(e) = fs::rename(old_path, &desired_path) {
                pb.println(format!(
                    "Warning: rename failed {} -> {}: {}",
                    old_path.display(),
                    desired_path.display(),
                    e
                ));
            }
        }
    }

    let md_file = File::create(&desired_path)
        .wrap_err_with(|| format!("Failed to create: {}", desired_path.display()))?;
    let mut writer = BufWriter::new(md_file);
    let tags = config.tags.as_deref();

    let assets = if let Some(thread) = parsed_db_thread {
        exporter::write_db_thread_markdown(
            &mut writer,
            id,
            &stem,
            &thread,
            tags,
            config.include_context,
        )?
    } else if let Some(thread) = parsed_serialized_thread {
        exporter::write_serialized_thread_markdown(&mut writer, id, &thread, tags)?;
        None
    } else {
        unreachable!()
    };

    writer.flush()?;
    drop(writer);

    if let Some(asset_list) = assets {
        let assets_dir = config.target_dir.join("assets");
        for asset in asset_list {
            fs::write(assets_dir.join(&asset.name), &asset.data)
                .wrap_err_with(|| format!("Failed to write asset: {}", asset.name))?;
        }
    }

    if config.verbose {
        match result_variant {
            ProcessResult::Created => pb.println(format!("Created: {}.md", stem)),
            ProcessResult::Updated => pb.println(format!("Updated: {}.md", stem)),
            ProcessResult::Skipped => {}
        }
    }

    Ok(result_variant)
}

fn decompress(data_type: &str, raw_data: &[u8]) -> Result<Vec<u8>> {
    match data_type {
        "zstd" => zstd::decode_all(raw_data).wrap_err("zstd decompression failed"),
        "json" => Ok(raw_data.to_vec()),
        other => Err(eyre!("Unknown data_type: {:?}", other)),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn allocate_filename(id: &str, title: &str, registry: &mut HashMap<String, String>) -> String {
    let raw_slug = slug::slugify(title);
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
    if slug.is_empty() {
        id.to_string()
    } else {
        format!("{}_{}", id, slug)
    }
}

struct FileFrontmatter {
    id: Option<String>,
    updated_at: DateTime<Utc>,
    include_context: bool,
}

fn read_file_frontmatter(path: &Path) -> Option<FileFrontmatter> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let first = lines.next()?.ok()?;
    if first.trim() != "---" {
        return None;
    }

    let mut id: Option<String> = None;
    let mut updated_at: Option<DateTime<Utc>> = None;
    let mut include_context = false;
    let mut bytes_read = 0usize;

    for line in lines {
        let line = line.ok()?;
        bytes_read += line.len() + 1;
        if bytes_read > 2048 || line.trim() == "---" {
            break;
        }
        if let Some(rest) = line.strip_prefix("id:") {
            id = Some(rest.trim().trim_matches('\'').trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("updated_at:") {
            let val = rest.trim().trim_matches('\'').trim_matches('"');
            updated_at = DateTime::parse_from_rfc3339(val)
                .ok()
                .map(|dt| dt.with_timezone(&Utc));
        } else if let Some(rest) = line.strip_prefix("include_context:") {
            include_context = rest.trim() == "true";
        }
    }
    updated_at.map(|ts| FileFrontmatter {
        id,
        updated_at: ts,
        include_context,
    })
}

fn get_updated_at(json_bytes: &[u8]) -> Option<DateTime<Utc>> {
    #[derive(Deserialize)]
    struct Minimal {
        updated_at: DateTime<Utc>,
    }
    serde_json::from_slice::<Minimal>(json_bytes)
        .ok()
        .map(|m| m.updated_at)
}
