# zed-chat-export

![License](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)

<!-- [![Crates.io](https://img.shields.io/crates/v/zed-chat-export.svg)](https://crates.io/crates/zed-chat-export) -->

Export your [Zed](https://zed.dev) AI conversations to local Markdown files.
Searchable with grep, browsable in Obsidian, consumable by LLM agents and RAG pipelines.

## What the Output Looks Like

Each conversation becomes a `.md` file. Frontmatter captures the model, timestamp, and the git state that was active during the conversation.

```md
---
title: Fix Deadlock in Connection Pool Under Load
updated_at: 2025-06-14T09:22:17.041832Z
model: anthropic/claude-sonnet-4-20250514
tags:
  - zed
  - ai-chat
git:
  path: /Users/dev/projects/warehouse-api
  remote: git@github.com:devco/warehouse-api.git
  branch: fix/pool-timeout
  commit: a3e71bf
id: 01941c3a-7f2e-7003-b8a2-4e9d1c08ef3a
---

## User

I'm seeing intermittent deadlocks when the connection pool is under load.
The pool is configured with max 10 connections but we're seeing threads
hang indefinitely when all 10 are checked out. Here's the relevant code:

`/src/db/pool.rs`

## Assistant

The issue is in your `acquire_connection` method. You're holding the
mutex guard across the `.await` point on line 47:

​`rust
let mut pool = self.inner.lock().await;  // guard held
let conn = pool.checkout().await;         // awaits while holding guard
​`

...
```

The conversation continues, but you get the idea. Months later, you can `grep "deadlock"` across your export directory — or point an agent at it — and recover the exact reasoning, the exact branch, the exact commit.

## Install

### Pre-built Binaries

Grab the latest from [GitHub Releases](https://github.com/egemengol/zed-chat-export/releases) for macOS (Intel/Apple Silicon) or Linux.

### Via Cargo

```/dev/null/install.sh#L1
cargo install zed-chat-export
```

## Usage

Point it at a directory. It finds your Zed database automatically.

```/dev/null/usage.sh#L1-2
# Export all conversations
zed-chat-export ~/notes/zed-chats
```

Run it again later — it only processes new or continued conversations by comparing content hashes.

### Options

```/dev/null/options.sh#L1-8
# Add tags to frontmatter (useful for Obsidian)
zed-chat-export ~/notes/zed-chats --tags zed,ai-chat

# Use a specific database path
zed-chat-export ~/notes/zed-chats --db-path /path/to/threads.db
```

### Config File

Persist preferences in `~/.config/zed-chat-export/config.toml` so you can run bare `zed-chat-export`:

```/dev/null/config.toml#L1-3
target_dir = "/Users/me/notes/zed-chats"
tags = ["zed", "ai-chat"]
# db_path = "/custom/path/to/threads.db"  # optional
```

## How It Works

Zed stores AI conversations in a SQLite database with Zstd-compressed message bodies. This tool:

1. Opens the database **read-only** — your data is never modified
2. Decompresses each conversation's message bodies
3. Renders them as Markdown with YAML frontmatter
4. Writes files to your target directory, one per conversation

On subsequent runs, it reads the frontmatter of existing files to detect what's changed, and skips anything that hasn't.

**Privacy:** Everything runs locally. No network calls, no telemetry, no data leaves your machine.

## Incremental Sync

The tool is designed to be run repeatedly (manually, via cron, etc.). It compares the database state against existing exported files using content hashes embedded in the frontmatter. Unchanged conversations are skipped entirely. New messages appended to an existing conversation trigger a re-export of that file.

## Limitations

- **Zed schema dependency:** This reads Zed's internal SQLite schema, which is undocumented and can change between Zed releases. If it breaks after a Zed update, open an issue. Last upstream version is `0.225.9`
- **Platform support:** Tested on macOS. Linux should work. Windows is untested.
- **Assets:** Images and slash-command outputs are referenced in the Markdown but not downloaded locally.
- **Not yet implemented:** file watching / live sync, content redaction, pruning of deleted conversations.

## License

AGPL-3.0-or-later
