use crate::importer::{DbThread, SerializedThread};
use crate::renderer;
use crate::utils::{
    ExportConfig, ProcessResult, decompress, extract_json_timestamp,
    parse_existing_frontmatter,
};
use crossbeam_channel::{SendTimeoutError, bounded};
use eyre::{Context, Result, eyre};
use rusqlite::{Connection, OpenFlags};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

pub fn execute(config: ExportConfig) -> Result<()> {
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

    let (tx, rx) = bounded::<String>(512);
    let count_created = AtomicUsize::new(0);
    let count_errors = AtomicUsize::new(0);
    let n_workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);

    std::thread::scope(|s| {
        for _ in 0..n_workers {
            let rx = rx.clone();
            let (config, count_created, count_errors) = (&config, &count_created, &count_errors);

            s.spawn(move || {
                let conn = match open_db(&config.db_path) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("Worker DB open failed: {:#}", e);
                        return;
                    }
                };

                while let Ok(id) = rx.recv() {
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
                            match export_thread(&id, &data_type, &data, &summary, None, config) {
                                Ok(ProcessResult::Created) => {
                                    count_created.fetch_add(1, Ordering::Relaxed);
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    count_errors.fetch_add(1, Ordering::Relaxed);
                                    eprintln!("Error [{}]: {:#}", &id[..8.min(id.len())], e);
                                }
                            }
                        }
                        Err(e) => {
                            count_errors.fetch_add(1, Ordering::Relaxed);
                            eprintln!("Error fetching [{}]: {:#}", &id[..8.min(id.len())], e);
                        }
                    }
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

    let (tx, rx) = bounded::<String>(32);
    let count_created = AtomicUsize::new(0);
    let count_updated = AtomicUsize::new(0);
    let count_skipped = AtomicUsize::new(0);
    let count_errors = AtomicUsize::new(0);
    let should_stop = AtomicBool::new(false);
    let n_workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);

    std::thread::scope(|s| {
        for _ in 0..n_workers {
            let rx = rx.clone();
            let (config, count_created, count_updated, count_skipped, count_errors) = (
                &config,
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
                        eprintln!("Worker DB open failed: {:#}", e);
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
                            eprintln!("Error fetching [{}]: {:#}", &id[..8.min(id.len())], e);
                            continue;
                        }
                    };

                    let existing_path = find_existing_file(&config.target_dir, &id);

                    match export_thread(&id, &data_type, &data, &summary, existing_path, config) {
                        Ok(ProcessResult::Created) => {
                            count_created.fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(ProcessResult::Updated) => {
                            count_updated.fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(ProcessResult::Skipped) => {
                            count_skipped.fetch_add(1, Ordering::Relaxed);
                            should_stop.store(true, Ordering::Relaxed);
                            break;
                        }
                        Err(e) => {
                            count_errors.fetch_add(1, Ordering::Relaxed);
                            eprintln!("Error [{}]: {:#}", &id[..8.min(id.len())], e);
                        }
                    }
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
            let fm = parse_existing_frontmatter(&path)?;
            if fm.id.as_deref() == Some(id) {
                Some(path)
            } else {
                None
            }
        })
}

fn export_thread(
    id: &str,
    data_type: &str,
    raw_data: &[u8],
    title: &str,
    existing_path: Option<PathBuf>,
    config: &ExportConfig,
) -> Result<ProcessResult> {
    let mut cached_json: Option<Vec<u8>> = None;
    if !config.force
        && let Some(ref existing) = existing_path
            && let Some(fm) = parse_existing_frontmatter(existing) {
                let json_bytes = decompress(data_type, raw_data)?;
                if let Some(db_ts) = extract_json_timestamp(&json_bytes)
                    && fm.updated_at >= db_ts && fm.include_context == config.include_context {
                        if config.verbose {
                            eprintln!("Skipped: {}", id);
                        }
                        return Ok(ProcessResult::Skipped);
                    }
                cached_json = Some(json_bytes);
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

    let stem = allocate_filename(id, title, &config.target_dir);
    let desired_path = config.target_dir.join(format!("{}.md", stem));
    let result_variant = if existing_path.is_none() {
        ProcessResult::Created
    } else {
        ProcessResult::Updated
    };

    if let Some(ref old_path) = existing_path
        && old_path != &desired_path
            && let Err(e) = fs::rename(old_path, &desired_path) {
                eprintln!(
                    "Warning: rename failed {} -> {}: {}",
                    old_path.display(),
                    desired_path.display(),
                    e
                );
            }

    let md_file = File::create(&desired_path)
        .wrap_err_with(|| format!("Failed to create: {}", desired_path.display()))?;
    let mut writer = BufWriter::new(md_file);
    let tags = config.tags.as_deref();

    let assets = if let Some(thread) = parsed_db_thread {
        renderer::render_thread(
            &mut writer,
            id,
            &stem,
            &thread,
            tags,
            config.include_context,
        )?
    } else if let Some(thread) = parsed_serialized_thread {
        renderer::render_serialized_thread(&mut writer, id, &thread, tags)?;
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
            ProcessResult::Created => eprintln!("Created: {}.md", stem),
            ProcessResult::Updated => eprintln!("Updated: {}.md", stem),
            ProcessResult::Skipped => {}
        }
    }

    Ok(result_variant)
}

// Optimistically allocate a filename stem for the given id+title pair.
// For each prefix length [8, 12, full id], we check the filesystem:
//   - File absent  → claim it (the caller will create it immediately after)
//   - File present and owned by this id → reuse it (incremental update / slug unchanged)
//   - File present and owned by another id → try a longer prefix
// No shared lock is needed. The only race is two threads simultaneously claiming
// the same 8-char UUID prefix for *different* IDs — astronomically rare in practice.
fn allocate_filename(id: &str, title: &str, target_dir: &Path) -> String {
    let raw_slug = slug::slugify(title);
    let slug = raw_slug[..raw_slug.len().min(60)]
        .trim_end_matches('-')
        .to_string();

    for &len in &[8usize, 12usize, id.len()] {
        let prefix = &id[..len.min(id.len())];
        let stem = if slug.is_empty() {
            prefix.to_string()
        } else {
            format!("{}_{}", prefix, slug)
        };
        let path = target_dir.join(format!("{}.md", stem));
        match path.try_exists() {
            Ok(false) => return stem,
            Ok(true) => {
                if let Some(fm) = parse_existing_frontmatter(&path)
                    && fm.id.as_deref() == Some(id) {
                        return stem;
                    }
                // Taken by another id — fall through to try a longer prefix
            }
            Err(_) => return stem,
        }
    }

    // Full-id fallback; a UUID is unique so this is always safe
    if slug.is_empty() {
        id.to_string()
    } else {
        format!("{}_{}", id, slug)
    }
}
