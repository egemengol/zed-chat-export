mod exporter;
mod importer;

use crate::importer::{DbThread, SerializedThread};
use chrono::{DateTime, Utc};
use eyre::{Context, Result, eyre};
use rusqlite::{Connection, OpenFlags, backup::Backup};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::NamedTempFile;

struct Config {
    target_dir: PathBuf,
    db_path: PathBuf,
    tags: Option<Vec<String>>,
    force: bool,
    verbose: bool,
    quiet: bool,
}

#[derive(Deserialize, Default)]
struct FileConfig {
    target_dir: Option<PathBuf>,
    db_path: Option<PathBuf>,
    tags: Option<Vec<String>>,
}

fn default_db_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("Zed/threads/threads.db"))
}

fn load_file_config(explicit_path: Option<&Path>) -> Result<FileConfig> {
    let path = if let Some(p) = explicit_path {
        if !p.exists() {
            return Err(eyre!("Config file not found: {}", p.display()));
        }
        Some(p.to_path_buf())
    } else {
        // Search: XDG/OS config dir, then nothing
        dirs::config_dir()
            .map(|d| d.join("zed-export/config.toml"))
            .filter(|p| p.exists())
    };

    match path {
        None => Ok(FileConfig::default()),
        Some(p) => {
            let content = fs::read_to_string(&p)
                .wrap_err_with(|| format!("Failed to read config: {}", p.display()))?;
            toml::from_str(&content)
                .wrap_err_with(|| format!("Failed to parse config: {}", p.display()))
        }
    }
}

fn parse_args() -> Result<Config> {
    let mut args = std::env::args().skip(1);

    let mut positional_target: Option<String> = None;
    let mut cli_db_path: Option<PathBuf> = None;
    let mut cli_tags: Option<Vec<String>> = None;
    let mut config_path: Option<PathBuf> = None;
    let mut force = false;
    let mut verbose = false;
    let mut quiet = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db" => {
                let val = args
                    .next()
                    .ok_or_else(|| eyre!("--db requires a path argument"))?;
                cli_db_path = Some(PathBuf::from(val));
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
                    cli_tags = Some(parsed);
                }
            }
            "--config" => {
                let val = args
                    .next()
                    .ok_or_else(|| eyre!("--config requires a path argument"))?;
                config_path = Some(PathBuf::from(val));
            }
            "--force" | "-f" => force = true,
            "--verbose" | "-v" => verbose = true,
            "--quiet" | "-q" => quiet = true,
            other if !other.starts_with('-') => {
                if positional_target.is_none() {
                    positional_target = Some(other.to_string());
                } else {
                    return Err(eyre!("Unexpected argument: {}", other));
                }
            }
            other => return Err(eyre!("Unknown argument: {}", other)),
        }
    }

    let file_cfg = load_file_config(config_path.as_deref())?;

    // target_dir: CLI positional > config file
    let target_dir = match positional_target.or_else(|| file_cfg.target_dir.map(|p| p.to_string_lossy().into_owned())) {
        Some(p) => PathBuf::from(p),
        None => return Err(eyre!(
            "Usage: zed-export [TARGET_DIRECTORY] [--db <PATH>] [--config <PATH>] [--tags <TAG1,TAG2,...>] [--force|-f] [--verbose|-v] [--quiet|-q]\n\
             \n\
             Arguments:\n  \
             TARGET_DIRECTORY    Where to write exported .md files (or set target_dir in config.toml)\n  \
             --db PATH           Path to threads.db\n  \
             --config PATH       Path to config.toml\n  \
             --tags TAGS         Comma-separated tags injected into frontmatter\n  \
             --force, -f         Overwrite all files regardless of timestamp\n  \
             --verbose, -v       Print each file written or skipped\n  \
             --quiet, -q         No output on success\n\
             \n\
             Config file searched at: $XDG_CONFIG_HOME/zed-export/config.toml"
        )),
    };

    // db_path: CLI > config file > OS default
    let db_path = cli_db_path
        .or(file_cfg.db_path)
        .or_else(default_db_path)
        .ok_or_else(|| eyre!("Could not determine database path. Set db_path in config.toml or use --db."))?;

    if !db_path.exists() {
        return Err(eyre!(
            "Database not found at: {}\nUse --db to specify the path manually, or set db_path in config.toml.",
            db_path.display()
        ));
    }

    // tags: CLI > config file
    let tags = cli_tags.or(file_cfg.tags);

    Ok(Config {
        target_dir,
        db_path,
        tags,
        force,
        verbose,
        quiet,
    })
}

fn create_snapshot(db_path: &Path, quiet: bool) -> Result<NamedTempFile> {
    if !quiet {
        eprintln!("Creating snapshot of database...");
    }

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

/// Read just the YAML frontmatter from an existing .md file and extract `updated_at`.
fn read_file_updated_at(path: &Path) -> Option<DateTime<Utc>> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    // First line must be "---"
    let first = lines.next()?.ok()?;
    if first.trim() != "---" {
        return None;
    }

    let mut bytes_read = 0usize;
    for line in lines {
        let line = line.ok()?;
        bytes_read += line.len() + 1;
        if bytes_read > 2048 {
            break;
        }
        if line.trim() == "---" {
            break;
        }
        if let Some(rest) = line.strip_prefix("updated_at:") {
            let val = rest.trim().trim_matches('\'').trim_matches('"');
            return DateTime::parse_from_rfc3339(val)
                .ok()
                .map(|dt| dt.with_timezone(&Utc));
        }
    }
    None
}

enum ProcessResult {
    Written,
    Skipped,
}

fn process_thread(
    id: &str,
    data_type: &str,
    raw_data: &[u8],
    config: &Config,
    registry: &mut HashMap<String, String>,
) -> Result<ProcessResult> {
    let json_bytes: Vec<u8> = match data_type {
        "zstd" => zstd::decode_all(raw_data).wrap_err("zstd decompression failed")?,
        "json" => raw_data.to_vec(),
        other => return Err(eyre!("Unknown data_type: {:?}", other)),
    };

    let stem = allocate_filename(id, registry);
    let md_path = config.target_dir.join(format!("{}.md", stem));

    // Idempotency check
    if !config.force && md_path.exists() {
        // We need the thread's updated_at to compare. Peek at the DB-side timestamp.
        // We'll deserialize just enough to get it.
        let db_updated_at = get_updated_at(&json_bytes);
        if let Some(db_ts) = db_updated_at {
            if let Some(file_ts) = read_file_updated_at(&md_path) {
                if file_ts >= db_ts {
                    return Ok(ProcessResult::Skipped);
                }
            }
        }
    }

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

    Ok(ProcessResult::Written)
}

/// Cheaply extract `updated_at` from JSON without full deserialization.
fn get_updated_at(json_bytes: &[u8]) -> Option<DateTime<Utc>> {
    // Try both thread formats
    #[derive(Deserialize)]
    struct Minimal {
        updated_at: DateTime<Utc>,
    }
    serde_json::from_slice::<Minimal>(json_bytes)
        .ok()
        .map(|m| m.updated_at)
}

fn run_export(snapshot_path: &Path, config: &Config) -> Result<()> {
    fs::create_dir_all(&config.target_dir).wrap_err_with(|| {
        format!(
            "Failed to create target directory: {}",
            config.target_dir.display()
        )
    })?;

    if !config.quiet {
        eprintln!("Exporting to: {}", config.target_dir.display());
    }

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
    let mut count_written = 0usize;
    let mut count_skipped = 0usize;
    let mut count_errors = 0usize;

    for (id, data_type, raw_data) in rows {
        match process_thread(&id, &data_type, &raw_data, config, &mut registry) {
            Ok(ProcessResult::Written) => {
                count_written += 1;
                if config.verbose {
                    let stem = registry.iter().find(|(_, v)| *v == &id).map(|(k, _)| k.as_str()).unwrap_or(&id);
                    eprintln!("Written: {}.md", stem);
                }
            }
            Ok(ProcessResult::Skipped) => {
                count_skipped += 1;
                if config.verbose {
                    eprintln!("Skipped: {}", &id[..8.min(id.len())]);
                }
            }
            Err(e) => {
                count_errors += 1;
                if !config.quiet {
                    eprintln!("Error [{}]: {:#}", &id[..8.min(id.len())], e);
                }
            }
        }
    }

    if !config.quiet {
        eprintln!(
            "Synced {}, Skipped {}, Errors {}",
            count_written, count_skipped, count_errors
        );
    }

    Ok(())
}

fn main() -> Result<()> {
    let config = parse_args()?;
    let snapshot = create_snapshot(&config.db_path, config.quiet)?;
    run_export(snapshot.path(), &config)
    // snapshot dropped here → temp file auto-deleted
}
