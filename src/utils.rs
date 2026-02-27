use chrono::{DateTime, Utc};
use eyre::{Context, Result, eyre};
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Configuration required to run the export process.
/// This decouples the logic from how the arguments were parsed (CLI/Config file).
#[derive(Clone)]
pub struct ExportConfig {
    pub target_dir: std::path::PathBuf,
    pub db_path: std::path::PathBuf,
    pub tags: Option<Vec<String>>,
    pub force: bool,
    pub verbose: bool,
    pub quiet: bool,
    pub include_context: bool,
}

#[derive(Clone, Copy)]
pub enum ProcessResult {
    Created,
    Updated,
    Skipped,
}

#[derive(Clone)]
pub struct FileFrontmatter {
    pub id: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub include_context: bool,
}

/// Create a read-only backup of the database to a temporary file.
#[cfg(feature = "sequential")]
pub fn backup_database(db_path: &Path, quiet: bool) -> Result<NamedTempFile> {
    use rusqlite::{Connection, OpenFlags, backup::Backup};
    use std::time::Duration;
    use tempfile::NamedTempFile;
    let spinner = if quiet {
        indicatif::ProgressBar::hidden()
    } else {
        let s = indicatif::ProgressBar::new_spinner();
        s.set_style(
            indicatif::ProgressStyle::with_template("{spinner:.green} {msg}")
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

/// Cheaply extract `updated_at` from JSON without full deserialization.
pub fn extract_json_timestamp(json_bytes: &[u8]) -> Option<DateTime<Utc>> {
    #[derive(Deserialize)]
    struct Minimal {
        updated_at: DateTime<Utc>,
    }
    serde_json::from_slice::<Minimal>(json_bytes)
        .ok()
        .map(|m| m.updated_at)
}

/// Decompress data bytes based on the data type.
pub fn decompress(data_type: &str, raw_data: &[u8]) -> Result<Vec<u8>> {
    match data_type {
        "zstd" => zstd::decode_all(raw_data).wrap_err("zstd decompression failed"),
        "json" => Ok(raw_data.to_vec()),
        other => Err(eyre!("Unknown data_type: {:?}", other)),
    }
}

/// Read the YAML frontmatter from an existing .md file and extract relevant fields.
pub fn parse_existing_frontmatter(path: &Path) -> Option<FileFrontmatter> {
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
