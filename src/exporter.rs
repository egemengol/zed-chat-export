#![allow(dead_code)]

use crate::importer::{
    AgentMessageContent, DbThread, MentionUri, Message, Role, SerializedMessageSegment,
    SerializedThread, UserMessageContent,
};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Write;

pub struct Asset {
    pub name: String,
    pub data: Vec<u8>,
}

fn image_asset(thread_id: &str, b64: &str) -> Option<Asset> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let ext = infer::get(&bytes).map(|t| t.extension()).unwrap_or("bin");
    let hash = format!("{:.16x}", Sha256::digest(&bytes));
    let name = format!("{}.{}.{}", thread_id, hash, ext);
    Some(Asset { name, data: bytes })
}

#[derive(Serialize)]
struct Frontmatter {
    title: String,
    updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git: Option<GitMetadata>,
}

#[derive(Serialize)]
struct GitMetadata {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit: Option<String>,
}
pub fn write_db_thread_markdown<W: Write>(
    writer: &mut W,
    thread_id: &str,
    thread: &DbThread,
) -> std::io::Result<Option<Vec<Asset>>> {
    let model = thread
        .model
        .as_ref()
        .map(|slm| format!("{}/{}", slm.provider, slm.model));

    // Extract Git info from the first worktree snapshot if available
    let git_info = thread
        .initial_project_snapshot
        .as_ref()
        .and_then(|s| s.worktree_snapshots.first())
        .map(|wt| {
            let gs = wt.git_state.as_ref();
            GitMetadata {
                path: wt.worktree_path.clone(),
                remote: gs.and_then(|g| g.remote_url.clone()),
                branch: gs.and_then(|g| g.current_branch.clone()),
                commit: gs
                    .and_then(|g| g.head_sha.as_deref())
                    .map(|sha| sha.get(..6).unwrap_or(sha).to_string()),
            }
        });

    let fm = Frontmatter {
        title: thread.title.clone(),
        updated_at: thread.updated_at,
        model: model,
        git: git_info,
    };

    // 2. Write Frontmatter
    writeln!(writer, "---")?;
    // serde_yaml usually adds a leading "---", but inside a block it might not.
    // We'll rely on serde_yaml's output but trim the leading "---" if present to control formatting manually
    // or just let serde_yaml handle the body.
    let yaml = serde_yaml::to_string(&fm)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    // serde_yaml includes the initial "---"
    write!(writer, "{}", yaml)?;
    writeln!(writer, "---")?;
    writeln!(writer)?;

    let mut assets: Vec<Asset> = Vec::new();

    // 3. Write Messages
    for msg in &thread.messages {
        match msg {
            Message::User(user_msg) => {
                writeln!(writer, "## User")?;
                writeln!(writer)?;
                for content in &user_msg.content {
                    match content {
                        UserMessageContent::Text(text) => {
                            writeln!(writer, "{}", text)?;
                        }
                        UserMessageContent::Mention { uri, content } => {
                            let (path_str, lang_ext) = match uri {
                                MentionUri::File { abs_path } => (
                                    Some(abs_path.to_string_lossy().to_string()),
                                    abs_path.extension(),
                                ),
                                MentionUri::Directory { abs_path } => {
                                    (Some(abs_path.to_string_lossy().to_string()), None)
                                }
                                MentionUri::Symbol { abs_path, .. } => (
                                    Some(abs_path.to_string_lossy().to_string()),
                                    abs_path.extension(),
                                ),
                                MentionUri::Selection { abs_path, .. } => {
                                    if let Some(p) = abs_path {
                                        (Some(p.to_string_lossy().to_string()), p.extension())
                                    } else {
                                        (None, None)
                                    }
                                }
                                MentionUri::TextThread { path, .. } => {
                                    (Some(path.to_string_lossy().to_string()), path.extension())
                                }
                                MentionUri::Fetch { url } => (Some(url.to_string()), None),
                                MentionUri::Thread { name, .. } => (Some(name.to_string()), None),
                                MentionUri::Rule { name, .. } => (Some(name.to_string()), None),
                                MentionUri::PastedImage => (Some("image".to_string()), None),
                                MentionUri::Diagnostics { .. } => {
                                    (Some("diagnostics".to_string()), None)
                                }
                                MentionUri::TerminalSelection { .. } => {
                                    (Some("terminal".to_string()), None)
                                }
                            };

                            let header = match (lang_ext.and_then(|s| s.to_str()), path_str) {
                                (Some(l), Some(p)) => format!("{} {}", l, p),
                                (Some(l), None) => l.to_string(),
                                (None, Some(p)) => p,
                                (None, None) => "".to_string(),
                            };

                            let trimmed = content.trim_start();
                            if trimmed.starts_with("```") {
                                writeln!(writer, "{}", content)?;
                            } else {
                                writeln!(writer, "```{}", header)?;
                                writeln!(writer, "{}", content)?;
                                writeln!(writer, "```")?;
                            }
                        }
                        UserMessageContent::Image(img) => {
                            if let Some(asset) = image_asset(thread_id, &img.source) {
                                writeln!(writer, "![image](./{})", asset.name)?;
                                assets.push(asset);
                            }
                        }
                    }
                }
                writeln!(writer)?;
            }

            Message::Agent(agent_msg) => {
                writeln!(writer, "## Assistant")?;
                writeln!(writer)?;
                for content in &agent_msg.content {
                    if let AgentMessageContent::Text(text) = content {
                        writeln!(writer, "{}", text)?;
                    }
                }
                writeln!(writer)?;
            }
            Message::Resume => {
                // Ignore resume messages
            }
        }
    }

    Ok(if assets.is_empty() {
        None
    } else {
        Some(assets)
    })
}

pub fn write_serialized_thread_markdown<W: Write>(
    writer: &mut W,
    thread: &SerializedThread,
) -> std::io::Result<()> {
    let model = thread
        .model
        .as_ref()
        .map(|slm| format!("{}/{}", slm.provider, slm.model));

    let git_info = thread
        .initial_project_snapshot
        .as_ref()
        .and_then(|s| s.worktree_snapshots.first())
        .map(|wt| {
            let gs = wt.git_state.as_ref();
            GitMetadata {
                path: wt.worktree_path.clone(),
                remote: gs.and_then(|g| g.remote_url.clone()),
                branch: gs.and_then(|g| g.current_branch.clone()),
                commit: gs
                    .and_then(|g| g.head_sha.as_deref())
                    .map(|sha| sha.get(..6).unwrap_or(sha).to_string()),
            }
        });

    let fm = Frontmatter {
        title: thread.summary.clone(),
        updated_at: thread.updated_at,
        model,
        git: git_info,
    };

    writeln!(writer, "---")?;
    let yaml = serde_yaml::to_string(&fm)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    write!(writer, "{}", yaml)?;
    writeln!(writer, "---")?;
    writeln!(writer)?;

    for msg in &thread.messages {
        let role_name = match msg.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::System => "System",
        };

        writeln!(writer, "## {}", role_name)?;
        writeln!(writer)?;

        for segment in &msg.segments {
            if let SerializedMessageSegment::Text { text } = segment {
                writeln!(writer, "{}", text)?;
            }
        }
        writeln!(writer)?;
    }

    Ok(())
}
