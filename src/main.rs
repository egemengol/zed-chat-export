mod exporter;
mod importer;

use crate::importer::{DbThread, SerializedThread};
use eyre::{Context, Result, eyre};
use rusqlite::{Connection, OpenFlags, backup::Backup};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::NamedTempFile;

struct Config {
    target_dir: PathBuf,
    db_path: PathBuf,
    tags: Option<Vec<String>>,
}

fn parse_args() -> Result<Config> {
    let mut args = std::env::args().skip(1);

    let target_dir = args.next().ok_or_else(|| {
        eyre!(
            "Usage: zed-export <TARGET_DIRECTORY> [--db <PATH>] [--tags <TAG1,TAG2,...>]\n\
             \n\
             Arguments:\n  \
             TARGET_DIRECTORY    Where to write exported .md files\n  \
             --db PATH           Path to threads.db (default: ~/Library/Application Support/Zed/threads/threads.db)\n  \
             --tags TAGS         Comma-separated tags injected into frontmatter"
        )
    })?;

    let mut db_path: Option<PathBuf> = None;
    let mut tags: Option<Vec<String>> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db" => {
                let val = args
                    .next()
                    .ok_or_else(|| eyre!("--db requires a path argument"))?;
                db_path = Some(PathBuf::from(val));
            }
            "--tags" => {
                let val = args
                    .next()
                    .ok_or_else(|| eyre!("--tags requires a comma-separated list"))?;
                let parsed: Vec<String> = val
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !parsed.is_empty() {
                    tags = Some(parsed);
                }
            }
            other => return Err(eyre!("Unknown argument: {}", other)),
        }
    }

    let db_path = match db_path {
        Some(p) => p,
        None => {
            let home = std::env::var("HOME")
                .wrap_err("Could not determine home directory (HOME env var not set)")?;
            PathBuf::from(home).join("Library/Application Support/Zed/threads/threads.db")
        }
    };

    if !db_path.exists() {
        return Err(eyre!(
            "Database not found at: {}\nUse --db to specify the path manually.",
            db_path.display()
        ));
    }

    Ok(Config {
        target_dir: PathBuf::from(target_dir),
        db_path,
        tags,
    })
}

fn create_snapshot(db_path: &Path) -> Result<NamedTempFile> {
    eprintln!("Creating snapshot of database...");

    let src = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .wrap_err_with(|| format!("Failed to open source database: {}", db_path.display()))?;

    let tmp = NamedTempFile::new().wrap_err("Failed to create temporary file")?;
    let mut dst =
        Connection::open(tmp.path()).wrap_err("Failed to open snapshot database connection")?;

    {
        let backup = Backup::new(&src, &mut dst).wrap_err("Failed to initialize backup")?;
        backup
            .run_to_completion(1000, Duration::from_millis(5), None)
            .wrap_err("Backup did not complete successfully")?;
    }

    drop(src);

    eprintln!("Snapshot created");
    Ok(tmp)
}

fn allocate_filename(id: &str, registry: &mut HashMap<String, String>) -> String {
    for &len in &[8usize, 12usize, id.len()] {
        let candidate = &id[..len.min(id.len())];
        match registry.get(candidate) {
            None => {
                registry.insert(candidate.to_string(), id.to_string());
                return candidate.to_string();
            }
            Some(existing) if existing == id => {
                return candidate.to_string();
            }
            Some(_) => continue,
        }
    }
    // Unreachable: full UUID is always unique per thread
    id.to_string()
}

fn process_thread(
    id: &str,
    data_type: &str,
    raw_data: &[u8],
    config: &Config,
    registry: &mut HashMap<String, String>,
) -> Result<()> {
    let json_bytes: Vec<u8> = match data_type {
        "zstd" => zstd::decode_all(raw_data).wrap_err("zstd decompression failed")?,
        "json" => raw_data.to_vec(),
        other => return Err(eyre!("Unknown data_type: {:?}", other)),
    };

    let stem = allocate_filename(id, registry);
    let md_path = config.target_dir.join(format!("{}.md", stem));

    let md_file = File::create(&md_path)
        .wrap_err_with(|| format!("Failed to create: {}", md_path.display()))?;
    let mut writer = BufWriter::new(md_file);

    let tags = config.tags.as_deref();

    let assets = match serde_json::from_slice::<DbThread>(&json_bytes) {
        Ok(thread) => exporter::write_db_thread_markdown(&mut writer, &stem, &thread, tags)
            .wrap_err("Failed to write DbThread markdown")?,
        Err(_) => match serde_json::from_slice::<SerializedThread>(&json_bytes) {
            Ok(thread) => {
                exporter::write_serialized_thread_markdown(&mut writer, &thread, tags)
                    .wrap_err("Failed to write SerializedThread markdown")?;
                None
            }
            Err(e) => {
                drop(writer);
                let _ = fs::remove_file(&md_path);
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
        for asset in asset_list {
            let asset_path = config.target_dir.join(&asset.name);
            fs::write(&asset_path, &asset.data)
                .wrap_err_with(|| format!("Failed to write asset: {}", asset.name))?;
        }
    }

    Ok(())
}

fn run_export(snapshot_path: &Path, config: &Config) -> Result<()> {
    fs::create_dir_all(&config.target_dir).wrap_err_with(|| {
        format!(
            "Failed to create target directory: {}",
            config.target_dir.display()
        )
    })?;

    eprintln!("Exporting to: {}", config.target_dir.display());

    let conn = Connection::open(snapshot_path).wrap_err("Failed to open snapshot database")?;

    let mut stmt = conn
        .prepare("SELECT id, data_type, data FROM threads")
        .wrap_err("Failed to prepare query")?;

    let rows: Vec<(String, String, Vec<u8>)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .wrap_err("Failed to execute query")?
        .collect::<rusqlite::Result<_>>()
        .wrap_err("Failed to collect rows")?;

    let mut registry: HashMap<String, String> = HashMap::new();
    let mut count_ok = 0usize;
    let mut count_skipped = 0usize;

    for (id, data_type, raw_data) in rows {
        match process_thread(&id, &data_type, &raw_data, config, &mut registry) {
            Ok(()) => {
                count_ok += 1;
                if count_ok % 10 == 0 {
                    eprint!(".");
                }
            }
            Err(e) => {
                count_skipped += 1;
                eprintln!("\nSkipped [{}]: {:#}", id, e);
            }
        }
    }

    if count_ok >= 10 {
        eprintln!();
    }
    eprintln!("Done. Exported: {}, Skipped: {}", count_ok, count_skipped);

    Ok(())
}

fn main() -> Result<()> {
    let config = parse_args()?;
    let snapshot = create_snapshot(&config.db_path)?;
    run_export(snapshot.path(), &config)
    // snapshot dropped here → temp file auto-deleted
}
