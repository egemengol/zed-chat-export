mod exporter;
mod importer;

use crate::importer::{DbThread, SerializedThread};
use eyre::{Context, Result};
use rusqlite::{Connection, params};
use std::env;
use std::fs::{self, File};
use std::io::{BufWriter, Write};

fn main() -> Result<()> {
    // 1. Parse Arguments
    let args: Vec<String> = env::args().collect();
    let db_path = if args.len() > 1 {
        &args[1]
    } else {
        eprintln!("usage: zed-export-rs <path-to-db.sqlite>");
        std::process::exit(2);
    };

    // 2. Open Database
    println!("Opening database at {}", db_path);
    let conn = Connection::open(db_path).wrap_err("Failed to open database")?;

    // 3. Prepare Scratch Directory
    fs::create_dir_all("scratch").wrap_err("Failed to create scratch directory")?;

    // 4. Query 20 Random Rows
    // We select id, data_type (json/zstd), and the raw blob
    let mut stmt =
        conn.prepare("SELECT id, data_type, data FROM threads ORDER BY RANDOM() LIMIT 10")?;

    let rows = stmt.query_map(params![], |row| {
        let id: String = row.get(0)?;
        let data_type: String = row.get(1)?;
        let data: Vec<u8> = row.get(2)?;
        Ok((id, data_type, data))
    })?;

    for row in rows {
        let (id, data_type, raw_data) = row?;
        println!("Processing ID: {}", id);

        // 5. Decompress Data if necessary
        let json_bytes = match data_type.as_str() {
            "zstd" => {
                zstd::decode_all(raw_data.as_slice()).wrap_err("zstd decompression failed")?
            }
            "json" => raw_data,
            other => {
                eprintln!("  Skipping {}: Unknown data_type '{}'", id, other);
                continue;
            }
        };

        // 6. Write JSON to scratch folder
        let json_path = format!("scratch/files/{}.json", id);
        let mut json_file =
            File::create(&json_path).wrap_err("Failed to create JSON output file")?;
        json_file.write_all(&json_bytes)?;
        println!("  -> Wrote JSON: {}", json_path);

        // 7. Deserialize and Write Markdown
        // Try DbThread (v0.3.0) first; fall back to SerializedThread (v0.1.0 / v0.2.0).
        let md_path = format!("scratch/files/{}.md", id);
        let mut md_file =
            BufWriter::new(File::create(&md_path).wrap_err("Failed to create MD output file")?);

        let wrote = match serde_json::from_slice::<DbThread>(&json_bytes) {
            Ok(thread) => {
                if let Some(assets) =
                    exporter::write_db_thread_markdown(&mut md_file, &id, &thread)?
                {
                    for asset in assets {
                        let asset_path = format!("scratch/files/{}", asset.name);
                        fs::write(&asset_path, &asset.data).wrap_err("Failed to write asset")?;
                        println!("  -> Wrote Asset: {}", asset_path);
                    }
                }
                true
            }
            Err(_) => match serde_json::from_slice::<SerializedThread>(&json_bytes) {
                Ok(thread) => {
                    exporter::write_serialized_thread_markdown(&mut md_file, &thread)?;
                    true
                }
                Err(e) => {
                    let v_check: Option<serde_json::Value> =
                        serde_json::from_slice(&json_bytes).ok();
                    let version = v_check
                        .as_ref()
                        .and_then(|v| v.get("version"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("none");
                    eprintln!(
                        "  -> Skipped Markdown: both DbThread and SerializedThread failed (version: {}). Error: {}",
                        version, e
                    );
                    false
                }
            },
        };

        if wrote {
            println!("  -> Wrote Markdown: {}", md_path);
        }
    }

    println!("Done.");
    Ok(())
}
