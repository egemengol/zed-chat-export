mod importer;
mod parallel;
mod renderer;
#[cfg(feature = "sequential")]
mod sequential;
mod utils;

use clap::Parser;
use eyre::{Context, Result, eyre};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

/// Export Zed editor AI chat history to Markdown files.
/// Up to date with 0.225.9
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Directory to export markdown files.
    /// Defaults to ./zed-chat-export if not set in config.
    #[arg(value_name = "TARGET_DIR")]
    target_dir: Option<PathBuf>,

    /// Path to Zed SQLite DB (threads.db).
    /// Auto-detected if omitted.
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,

    /// Path to a specific configuration file.
    /// Defaults to $XDG_CONFIG_HOME/zed-export/config.toml
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Comma-separated tags to add to frontmatter (e.g. "zed,llm").
    #[arg(long, value_name = "TAGS", value_delimiter = ',')]
    tags: Option<Vec<String>>,

    /// Overwrite existing files even if they are newer.
    #[arg(short, long)]
    force: bool,

    /// Print each file written or skipped.
    #[arg(short, long)]
    verbose: bool,

    /// Suppress standard output (progress bars).
    #[arg(short, long)]
    quiet: bool,

    /// Include @-mention context blocks (file, symbol, selection, etc.) in output.
    #[arg(long)]
    include_context: bool,
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
            .map(|d| d.join("zed-chat-export/config.toml"))
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    // 1. Load config file (CLI path > default path)
    let file_cfg = load_file_config(cli.config.as_deref())?;

    // 2. Resolve target_dir (CLI > Config > Default)
    let target_dir = cli
        .target_dir
        .or(file_cfg.target_dir)
        .unwrap_or_else(|| PathBuf::from("zed-chat-export"));

    // 3. Resolve db_path (CLI > Config > Auto-detect)
    let db_path = cli
        .db
        .or(file_cfg.db_path)
        .or_else(default_db_path)
        .ok_or_else(|| {
            eyre!("Could not determine database path.\nUse --db to specify manually, or set db_path in config.toml.")
        })?;

    if !db_path.exists() {
        return Err(eyre!(
            "Database not found at: {}\nUse --db to specify the path manually.",
            db_path.display()
        ));
    }

    // 4. Resolve tags (CLI > Config)
    let tags = cli.tags.or(file_cfg.tags);

    // 5. Build the Export Config
    let config = utils::ExportConfig {
        target_dir,
        db_path,
        tags,
        force: cli.force,
        verbose: cli.verbose,
        quiet: cli.quiet,
        include_context: cli.include_context,
    };

    // 6. Run the Business Logic
    #[cfg(feature = "sequential")]
    return sequential::execute(config);

    #[cfg(not(feature = "sequential"))]
    parallel::execute(config)
}
