//! # zed-chat-export
//!
//! A CLI tool that exports [Zed](https://zed.dev) AI chat conversations to local Markdown files.
//!
//! ## What it does
//!
//! Zed stores AI conversations in a SQLite database (`threads.db`) with Zstd-compressed
//! message bodies. This tool reads that database, decompresses the messages, and writes
//! each conversation as a standalone Markdown file with YAML frontmatter containing
//! metadata like the model used, timestamps, and the git context that was active during
//! the conversation.
//!
//! The database is opened **read-only** — your data is never modified.
//!
//! ## Incremental export
//!
//! On repeated runs, existing files are checked against the database state using content
//! hashes embedded in the frontmatter. Unchanged conversations are skipped. Conversations
//! with new messages are re-exported in place.
//!
//! ## Usage
//!
//! ```sh
//! # Export all conversations to a directory
//! zed-chat-export ~/notes/zed-chats
//!
//! # With tags for Obsidian and a custom DB path
//! zed-chat-export ~/notes/zed-chats --tags zed,ai-chat --db /path/to/threads.db
//! ```
//!
//! Preferences can be persisted in `~/.config/zed-chat-export/config.toml`.
//!
//! ## Compatibility
//!
//! Tracks Zed's internal (undocumented) SQLite schema. Last verified against Zed `0.225.9`.
//! If a Zed update breaks the schema, please [open an issue](https://github.com/egemengol/zed-chat-export/issues).
