mod exporter;
mod importer;

use crate::importer::{DbThread, SerializedThread};
use chrono::{DateTime, Utc};
use eyre::{Context, Result, eyre};
use indicatif::{ProgressBar, ProgressStyle};
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
    let spinner = if quiet {
        ProgressBar::hidden()
    } else {
        let s = ProgressBar::new_spinner();
        s.set_style(
            ProgressStyle::with_template("{spinner:.green} {msg}")
                .unwrap()
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
        );
        s.set_message("Snapshotting database...");
        s.enable_steady_tick(Duration::from_millis(80));
        s
    };

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
    spinner.finish_and_clear();
    Ok(tmp)
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

/// Cheaply extract `updated_at` from JSON without full deserialization.
fn get_updated_at(json_bytes: &[u8]) -> Option<DateTime<Utc>> {
    #[derive(Deserialize)]
    struct Minimal {
        updated_at: DateTime<Utc>,
    }
    serde_json::from_slice::<Minimal>(json_bytes).ok().map(|m| m.updated_at)
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

enum ProcessResult {
    Created,
    Updated,
    Skipped,
}

fn process_thread(
    id: &str,
    data_type: &str,
    raw_data: &[u8],
    title: &str,
    config: &Config,
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
    if !config.force {
        if let Some(ref existing) = existing_path {
            if let Some(file_ts) = read_file_updated_at(existing) {
                let json_bytes: Vec<u8> = match data_type {
                    "zstd" => zstd::decode_all(raw_data).wrap_err("zstd decompression failed")?,
                    "json" => raw_data.to_vec(),
                    other => return Err(eyre!("Unknown data_type: {:?}", other)),
                };
                if let Some(db_ts) = get_updated_at(&json_bytes) {
                    if file_ts >= db_ts {
                        if config.verbose {
                            pb.println(format!("Skipped:  {}.md", stem));
                        }
                        return Ok(ProcessResult::Skipped);
                    }
                }
                cached_json = Some(json_bytes);
            }
        }
    }

    let json_bytes: Vec<u8> = match cached_json {
        Some(b) => b,
        None => match data_type {
            "zstd" => zstd::decode_all(raw_data).wrap_err("zstd decompression failed")?,
            "json" => raw_data.to_vec(),
            other => return Err(eyre!("Unknown data_type: {:?}", other)),
        },
    };

    let result_variant = if existing_path.is_none() {
        ProcessResult::Created
    } else {
        ProcessResult::Updated
    };

    // Rename if slug changed (Scenario C)
    if let Some(ref existing) = existing_path {
        if existing != &desired_path {
            if let Err(e) = fs::rename(existing, &desired_path) {
                pb.println(format!(
                    "Warning: could not rename {} → {}: {}",
                    existing.display(),
                    desired_path.display(),
                    e
                ));
            }
        }
    }

    // Update the index so subsequent lookups reflect the rename
    file_index.insert(prefix, desired_path.clone());

    let md_file = File::create(&desired_path)
        .wrap_err_with(|| format!("Failed to create: {}", desired_path.display()))?;
    let mut writer = BufWriter::new(md_file);

    let tags = config.tags.as_deref();

    let assets = match serde_json::from_slice::<DbThread>(&json_bytes) {
        Ok(thread) => exporter::write_db_thread_markdown(&mut writer, id, &stem, &thread, tags)
            .wrap_err("Failed to write DbThread markdown")?,
        Err(_) => match serde_json::from_slice::<SerializedThread>(&json_bytes) {
            Ok(thread) => {
                exporter::write_serialized_thread_markdown(&mut writer, id, &thread, tags)
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

fn run_export(snapshot_path: &Path, config: &Config) -> Result<()> {
    fs::create_dir_all(&config.target_dir).wrap_err_with(|| {
        format!(
            "Failed to create target directory: {}",
            config.target_dir.display()
        )
    })?;
    fs::create_dir_all(config.target_dir.join("assets")).wrap_err("Failed to create assets directory")?;

    let conn = Connection::open(snapshot_path).wrap_err("Failed to open snapshot database")?;

    let total: u64 = conn
        .query_row("SELECT COUNT(*) FROM threads", [], |row| row.get::<_, i64>(0))
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

    let mut rows = stmt
        .query([])
        .wrap_err("Failed to execute query")?;

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
        match process_thread(&id, &data_type, &raw_data, &summary, config, &mut registry, &mut file_index, &pb) {
            Ok(ProcessResult::Created) => count_created += 1,
            Ok(ProcessResult::Updated) => count_updated += 1,
            Ok(ProcessResult::Skipped) => {
                count_skipped += 1;
                // Since rows are ordered newest-first, once we skip a thread that's
                // already up-to-date the rest are also guaranteed up-to-date.
                // Drain the count and stop early.
                let remaining = total.saturating_sub(
                    (count_created + count_updated + count_skipped + count_errors) as u64
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

fn main() -> Result<()> {
    let config = parse_args()?;
    let snapshot = create_snapshot(&config.db_path, config.quiet)?;
    run_export(snapshot.path(), &config)
    // snapshot dropped here → temp file auto-deleted
}
