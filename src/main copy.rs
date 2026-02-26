mod exporter;
mod importer;

use crate::importer::{DbThread, SerializedThread};
use rusqlite::{Connection, params};
use std::env;

fn main() {
    let path = env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: zed-export-rs <path-to-db.sqlite>");
        std::process::exit(2);
    });

    let conn = Connection::open(&path).unwrap_or_else(|e| {
        eprintln!("failed to open {}: {}", path, e);
        std::process::exit(1);
    });

    let mut stmt = conn
        .prepare("SELECT id, data_type, data FROM threads")
        .unwrap();

    let rows = stmt
        .query_map(params![], |row| {
            let id: String = row.get(0)?;
            let data_type_str: String = row.get(1)?;
            let data: Vec<u8> = row.get(2)?;
            Ok((id, data_type_str, data))
        })
        .unwrap();

    let mut total = 0usize;
    let mut failed = 0usize;

    let mut v010 = 0usize;
    let mut v020 = 0usize;
    let mut v030 = 0usize;
    let mut v_none = 0usize;
    let mut v_unknown = 0usize;

    for row in rows {
        let (id, data_type_str, data) = row.unwrap();
        total += 1;

        let json_bytes = match data_type_str.as_str() {
            "zstd" => match zstd::decode_all(data.as_slice()) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("[{}] zstd decompress failed: {}", id, e);
                    failed += 1;
                    continue;
                }
            },
            "json" => data,
            other => {
                eprintln!("[{}] unknown data_type: {:?}", id, other);
                failed += 1;
                continue;
            }
        };

        let json_str = match std::str::from_utf8(&json_bytes) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[{}] invalid utf8 after decompress: {}", id, e);
                failed += 1;
                continue;
            }
        };

        let version = serde_json::from_str::<serde_json::Value>(json_str)
            .ok()
            .and_then(|v| v.get("version").and_then(|v| v.as_str()).map(String::from));

        match version.as_deref() {
            Some("0.3.0") => {
                v030 += 1;
                match serde_json::from_str::<DbThread>(json_str) {
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("[{}] DbThread deserialize failed: {}", id, e);
                        eprintln!("  version field: {:?}", version);
                        eprintln!("  json snippet: {:.300}", json_str);
                        failed += 1;
                    }
                }
            }
            None => {
                v_none += 1;
                match serde_json::from_str::<DbThread>(json_str) {
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("[{}] DbThread deserialize failed: {}", id, e);
                        eprintln!("  version field: {:?}", version);
                        eprintln!("  json snippet: {:.300}", json_str);
                        failed += 1;
                    }
                }
            }
            Some("0.1.0") => {
                v010 += 1;
                match serde_json::from_str::<SerializedThread>(json_str) {
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("[{}] SerializedThread deserialize failed: {}", id, e);
                        eprintln!("  version field: {:?}", version);
                        eprintln!("  json snippet: {:.300}", json_str);
                        failed += 1;
                    }
                }
            }
            Some("0.2.0") => {
                v020 += 1;
                match serde_json::from_str::<SerializedThread>(json_str) {
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("[{}] SerializedThread deserialize failed: {}", id, e);
                        eprintln!("  version field: {:?}", version);
                        eprintln!("  json snippet: {:.300}", json_str);
                        failed += 1;
                    }
                }
            }
            Some(v) => {
                v_unknown += 1;
                eprintln!("[{}] unknown version: {:?}", id, v);
                eprintln!("  json snippet: {:.300}", json_str);
                failed += 1;
            }
        }
    }

    println!("total: {}", total);
    println!("versions:");
    println!("  0.1.0:   {}", v010);
    println!("  0.2.0:   {}", v020);
    println!("  0.3.0:   {}", v030);
    println!("  (none):  {}", v_none);
    println!("  unknown: {}", v_unknown);
    println!("failed: {}", failed);
}
