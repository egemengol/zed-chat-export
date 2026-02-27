#![allow(dead_code)]

/// Complete, self-contained type definitions for the Zed agent thread database schema.
///
/// Storage format: Zstd-compressed JSON (compression level 3) in a SQLite `threads` table.
/// Current JSON version: `0.3.0`
/// Current Zed version: `0.225.9`
///
/// Table schema:
/// ```sql
/// CREATE TABLE IF NOT EXISTS threads (
///     id        TEXT PRIMARY KEY,
///     parent_id TEXT,
///     summary   TEXT NOT NULL,
///     updated_at TEXT NOT NULL,
///     data_type  TEXT NOT NULL,   -- "json" | "zstd"
///     data       BLOB NOT NULL
/// );
/// ```
///
/// Source files:
/// - `crates/agent/src/db.rs`            – `DbThread`, `DbThreadMetadata`, `SharedThread`, `DataType`
/// - `crates/agent/src/thread.rs`         – `Message`, `UserMessage`, `UserMessageContent`, `AgentMessage`, `AgentMessageContent`, `SubagentContext`, `PromptId`
/// - `crates/agent/src/legacy_thread.rs`  – `SerializedThread` (v0.1.0/v0.2.0), `SerializedMessage`, `SerializedMessageSegment`, `SerializedToolUse`, `SerializedToolResult`, `SerializedCrease`, `DetailedSummaryState`, `MessageId`, `SerializedLanguageModel`
/// - `crates/language_model/src/language_model.rs` – `TokenUsage`, `LanguageModelToolUse`, `LanguageModelToolUseId`
/// - `crates/language_model/src/request.rs`        – `LanguageModelImage`, `LanguageModelToolResult`, `LanguageModelToolResultContent`
/// - `crates/language_model/src/role.rs`           – `Role`
/// - `crates/acp_thread/src/mention.rs`            – `MentionUri`
/// - `crates/acp_thread/src/connection.rs`         – `UserMessageId`
/// - `crates/agent_settings/src/agent_settings.rs` – `AgentProfileId`
/// - `crates/agent/src/agent.rs`                   – `ProjectSnapshot`
/// - `crates/project/src/telemetry_snapshot.rs`    – `TelemetryWorktreeSnapshot`, `GitState`
/// - `crates/agent_client_protocol` (external crate, crates.io) – `SessionId`
use std::{collections::HashMap, ops::RangeInclusive, path::PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Primitive / ID types
// ---------------------------------------------------------------------------

/// Opaque session identifier. Wraps an `Arc<str>`.
///
/// Source: `crates/agent_client_protocol` (published crate, `agent-client-protocol-schema`)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// ID for a user-submitted message (one per Enter press).
///
/// Source: `crates/acp_thread/src/connection.rs`
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UserMessageId(String);

impl UserMessageId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl Default for UserMessageId {
    fn default() -> Self {
        Self::new()
    }
}

/// ID for a tool use invocation.
///
/// Source: `crates/language_model/src/language_model.rs`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LanguageModelToolUseId(String);

impl LanguageModelToolUseId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for LanguageModelToolUseId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl<T: Into<String>> From<T> for LanguageModelToolUseId {
    fn from(value: T) -> Self {
        Self(value.into())
    }
}

/// Profile identifier (kebab-cased string, e.g. `"write"`).
///
/// Source: `crates/agent_settings/src/agent_settings.rs`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentProfileId(String);

impl AgentProfileId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for AgentProfileId {
    fn default() -> Self {
        Self("write".into())
    }
}

impl std::fmt::Display for AgentProfileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Role
// ---------------------------------------------------------------------------

/// Conversation turn role.
///
/// Source: `crates/language_model/src/role.rs`
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}

// ---------------------------------------------------------------------------
// Token usage
// ---------------------------------------------------------------------------

/// Token counts for a single LLM request/response pair.
///
/// All fields default to 0 and are omitted from JSON when zero.
///
/// Source: `crates/language_model/src/language_model.rs`
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub input_tokens: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub cache_creation_input_tokens: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub cache_read_input_tokens: u64,
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

/// A base64-encoded PNG image, optionally annotated with its pixel dimensions.
///
/// The `source` field contains raw base64 bytes (no `data:` prefix).
///
/// Source: `crates/language_model/src/request.rs`
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LanguageModelImage {
    /// Base64-encoded PNG bytes (no `data:image/png;base64,` prefix).
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<ImageSize>,
}

impl std::fmt::Debug for LanguageModelImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LanguageModelImage")
            .field("source", &format!("<{} bytes>", self.source.len()))
            .field("size", &self.size)
            .finish()
    }
}

/// Pixel dimensions of an image (device pixels).
///
/// Mirrors `gpui::Size<DevicePixels>` but without the gpui dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ImageSize {
    pub width: i32,
    pub height: i32,
}

// ---------------------------------------------------------------------------
// Tool use / results
// ---------------------------------------------------------------------------

/// A single tool invocation emitted by the model.
///
/// Source: `crates/language_model/src/language_model.rs`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LanguageModelToolUse {
    pub id: LanguageModelToolUseId,
    pub name: String,
    /// Raw JSON string as received from the model.
    pub raw_input: String,
    /// Parsed JSON input.
    pub input: serde_json::Value,
    pub is_input_complete: bool,
    /// Extended thinking signature (Anthropic-specific). Must be echoed back in history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

/// The content of a tool result, either text or an image.
///
/// Custom deserializer handles multiple legacy wire formats:
/// - plain string
/// - `{"type": "text", "text": "..."}` (Anthropic-style)
/// - `{"Text": "..."}` / `{"text": "..."}` (single-field wrapped)
/// - image object `{"source": "...", "size": {...}}`
/// - wrapped image `{"Image": {...}}` / `{"image": {...}}`
///
/// Source: `crates/language_model/src/request.rs`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub enum LanguageModelToolResultContent {
    Text(String),
    Image(LanguageModelImage),
}

impl<'de> Deserialize<'de> for LanguageModelToolResultContent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;

        let value = serde_json::Value::deserialize(deserializer)?;

        if let Ok(text) = serde_json::from_value::<String>(value.clone()) {
            return Ok(Self::Text(text));
        }

        if let Some(obj) = value.as_object() {
            fn get_ci<'a>(
                obj: &'a serde_json::Map<String, serde_json::Value>,
                field: &str,
            ) -> Option<&'a serde_json::Value> {
                obj.iter()
                    .find(|(k, _)| k.to_lowercase() == field.to_lowercase())
                    .map(|(_, v)| v)
            }

            // {"type": "text", "text": "..."}
            if let (Some(t), Some(txt)) = (get_ci(obj, "type"), get_ci(obj, "text"))
                && t.as_str().map(|s| s.to_lowercase()) == Some("text".into())
                && let Some(s) = txt.as_str()
            {
                return Ok(Self::Text(s.to_owned()));
            }

            // {"text": "..."} (single-field)
            if let Some((_, v)) = obj.iter().find(|(k, _)| k.to_lowercase() == "text")
                && obj.len() == 1
                && let Some(s) = v.as_str()
            {
                return Ok(Self::Text(s.to_owned()));
            }

            // {"image": {...}} or {"Image": {...}}
            if let Some((_, v)) = obj.iter().find(|(k, _)| k.to_lowercase() == "image")
                && obj.len() == 1
                && let Some(img_obj) = v.as_object()
                && let Some(image) = parse_lm_image(img_obj)
            {
                return Ok(Self::Image(image));
            }

            // Direct image object
            if let Some(image) = parse_lm_image(obj) {
                return Ok(Self::Image(image));
            }
        }

        Err(D::Error::custom(format!(
            "data did not match any variant of LanguageModelToolResultContent. Got: {}",
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
        )))
    }
}

fn parse_lm_image(obj: &serde_json::Map<String, serde_json::Value>) -> Option<LanguageModelImage> {
    let mut source = None;
    let mut size_obj = None;

    for (k, v) in obj.iter() {
        match k.to_lowercase().as_str() {
            "source" => source = v.as_str(),
            "size" => size_obj = v.as_object(),
            _ => {}
        }
    }

    let source = source?;
    let size_obj = size_obj?;

    let mut width = None;
    let mut height = None;
    for (k, v) in size_obj.iter() {
        match k.to_lowercase().as_str() {
            "width" => width = v.as_i64().map(|w| w as i32),
            "height" => height = v.as_i64().map(|h| h as i32),
            _ => {}
        }
    }

    Some(LanguageModelImage {
        source: source.to_owned(),
        size: Some(ImageSize {
            width: width?,
            height: height?,
        }),
    })
}

/// The result of a tool invocation, associated with a specific tool use ID.
///
/// Source: `crates/language_model/src/request.rs`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LanguageModelToolResult {
    pub tool_use_id: LanguageModelToolUseId,
    pub tool_name: String,
    pub is_error: bool,
    pub content: LanguageModelToolResultContent,
    /// Optional structured output (debug/display purposes).
    pub output: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Mention URIs
// ---------------------------------------------------------------------------

/// A URI that identifies context attached to a user message (@-mention).
///
/// Source: `crates/acp_thread/src/mention.rs`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MentionUri {
    /// An absolute path to a file on disk.
    File { abs_path: PathBuf },

    /// A pasted image (no path; the image data is carried separately).
    PastedImage,

    /// An absolute path to a directory.
    Directory { abs_path: PathBuf },

    /// A named symbol within a file, with a line range.
    Symbol {
        abs_path: PathBuf,
        name: String,
        line_range: RangeInclusive<u32>,
    },

    /// A reference to another agent thread by session ID.
    Thread { id: SessionId, name: String },

    /// A reference to a plain-text thread stored at a local path.
    TextThread { path: PathBuf, name: String },

    /// A user-defined prompt/rule by its UUID-based ID.
    Rule { id: String, name: String },

    /// Diagnostic output (errors and/or warnings).
    Diagnostics {
        #[serde(default = "default_include_errors")]
        include_errors: bool,
        #[serde(default)]
        include_warnings: bool,
    },

    /// A line-range selection within an optional file.
    Selection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        abs_path: Option<PathBuf>,
        line_range: RangeInclusive<u32>,
    },

    /// A fetched URL, included as context.
    Fetch { url: Url },

    /// A selection from a terminal buffer.
    TerminalSelection { line_count: u32 },
}

fn default_include_errors() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Message content
// ---------------------------------------------------------------------------

/// The content of a user message.
///
/// Source: `crates/agent/src/thread.rs`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserMessageContent {
    /// Plain text typed by the user.
    Text(String),

    /// An @-mention with expanded context.
    Mention { uri: MentionUri, content: String },

    /// An image pasted or dropped into the input.
    Image(LanguageModelImage),
}

/// The content of an agent (assistant) message.
///
/// Source: `crates/agent/src/thread.rs`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentMessageContent {
    /// A prose text segment.
    Text(String),

    /// Extended thinking (visible reasoning).
    Thinking {
        text: String,
        /// Anthropic signature that must be echoed back in conversation history.
        signature: Option<String>,
    },

    /// Redacted/encrypted thinking block (Anthropic-specific).
    RedactedThinking(String),

    /// A tool invocation.
    ToolUse(LanguageModelToolUse),
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// A user message, keyed by its unique ID.
///
/// Source: `crates/agent/src/thread.rs`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserMessage {
    pub id: UserMessageId,
    pub content: Vec<UserMessageContent>,
}

/// An agent (assistant) message including any tool invocations and their results.
///
/// `tool_results` is an ordered map from tool use ID to result; ordering matches
/// the order tool calls were issued.
///
/// Source: `crates/agent/src/thread.rs`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessage {
    pub content: Vec<AgentMessageContent>,
    /// Results keyed by tool use ID, in insertion order.
    pub tool_results: std::collections::BTreeMap<String, LanguageModelToolResult>,
    /// Opaque reasoning metadata (provider-specific, e.g. o1 reasoning tokens).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<serde_json::Value>,
}

/// A single turn in a conversation thread.
///
/// `Resume` is a synthetic user message ("Continue where you left off") injected
/// when a paused session is resumed.
///
/// Source: `crates/agent/src/thread.rs`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    User(UserMessage),
    Agent(AgentMessage),
    Resume,
}

// ---------------------------------------------------------------------------
// Subagent context
// ---------------------------------------------------------------------------

/// Lifecycle context carried by a subagent thread.
///
/// Source: `crates/agent/src/thread.rs`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentContext {
    /// Session ID of the parent thread that spawned this subagent.
    pub parent_thread_id: SessionId,

    /// Nesting depth (0 = root agent, 1 = first-level subagent, …).
    pub depth: u8,
}

// ---------------------------------------------------------------------------
// Project snapshot
// ---------------------------------------------------------------------------

/// Snapshot of the project state at thread creation time (telemetry/context).
///
/// Source: `crates/agent/src/agent.rs`, `crates/project/src/telemetry_snapshot.rs`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectSnapshot {
    pub worktree_snapshots: Vec<TelemetryWorktreeSnapshot>,
    pub timestamp: DateTime<Utc>,
}

/// Per-worktree slice of a project snapshot.
///
/// Source: `crates/project/src/telemetry_snapshot.rs`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TelemetryWorktreeSnapshot {
    pub worktree_path: String,
    pub git_state: Option<GitState>,
}

/// Git repository state captured at snapshot time.
///
/// Source: `crates/project/src/telemetry_snapshot.rs`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitState {
    pub remote_url: Option<String>,
    pub head_sha: Option<String>,
    pub current_branch: Option<String>,
    pub diff: Option<String>,
}

// ---------------------------------------------------------------------------
// Language model identity
// ---------------------------------------------------------------------------

/// Provider + model pair recorded alongside a thread.
///
/// Source: `crates/agent/src/legacy_thread.rs` (`SerializedLanguageModel`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerializedLanguageModel {
    pub provider: String,
    pub model: String,
}

// ---------------------------------------------------------------------------
// Top-level DB structs
// ---------------------------------------------------------------------------

/// The full thread document stored (compressed) in the SQLite `data` column.
///
/// Serialized with a `"version": "0.3.0"` field injected at write time (see
/// `save_thread_sync` in `db.rs`).
///
/// Source: `crates/agent/src/db.rs`
#[derive(Debug, Serialize, Deserialize)]
pub struct DbThread {
    pub title: String,
    pub messages: Vec<Message>,
    pub updated_at: DateTime<Utc>,

    /// Human-readable AI-generated detailed summary, if generated.
    #[serde(default)]
    pub detailed_summary: Option<String>,

    /// Snapshot of worktrees at session start.
    #[serde(default)]
    pub initial_project_snapshot: Option<ProjectSnapshot>,

    /// Cumulative token usage across the entire thread.
    #[serde(default)]
    pub cumulative_token_usage: TokenUsage,

    /// Per-user-message token usage, keyed by `UserMessageId`.
    #[serde(default)]
    pub request_token_usage: HashMap<String, TokenUsage>,

    /// The language model active at save time.
    #[serde(default)]
    pub model: Option<SerializedLanguageModel>,

    /// The agent profile active at save time.
    #[serde(default)]
    pub profile: Option<AgentProfileId>,

    /// `true` when this thread was imported from an external share link.
    #[serde(default)]
    pub imported: bool,

    /// Present only for subagent threads.
    #[serde(default)]
    pub subagent_context: Option<SubagentContext>,
}

/// Lightweight row returned by `SELECT id, parent_id, summary, updated_at FROM threads`.
///
/// Source: `crates/agent/src/db.rs`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbThreadMetadata {
    pub id: SessionId,
    pub parent_session_id: Option<SessionId>,
    /// Displayed title / summary of the thread.
    #[serde(alias = "summary")]
    pub title: String,
    pub updated_at: DateTime<Utc>,
}

/// The data type stored in the `data_type` SQLite column.
///
/// Source: `crates/agent/src/db.rs`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataType {
    #[serde(rename = "json")]
    Json,
    #[serde(rename = "zstd")]
    Zstd,
}

/// Thread document used for external sharing (exported as zstd-compressed JSON).
///
/// Versioned independently from `DbThread` (`SharedThread::VERSION = "1.0.0"`).
///
/// Source: `crates/agent/src/db.rs`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedThread {
    pub title: String,
    pub messages: Vec<Message>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub model: Option<SerializedLanguageModel>,
    pub version: String,
}

// ---------------------------------------------------------------------------
// Legacy formats (v0.1.0 / v0.2.0) – needed to deserialize old threads
// ---------------------------------------------------------------------------

/// Detailed-summary lifecycle state stored in legacy v0.1.0/v0.2.0 threads.
///
/// Source: `crates/agent/src/legacy_thread.rs`
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum DetailedSummaryState {
    #[default]
    NotGenerated,
    Generating,
    Generated {
        text: String,
    },
}

/// Sequential message ID used in legacy formats.
///
/// Source: `crates/agent/src/legacy_thread.rs`
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MessageId(pub usize);

/// A legacy message segment (text, thinking, or redacted thinking).
///
/// Source: `crates/agent/src/legacy_thread.rs`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SerializedMessageSegment {
    #[serde(rename = "text")]
    Text {
        text: String,
    },

    #[serde(rename = "thinking")]
    Thinking {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    RedactedThinking {
        data: String,
    },
}

/// A legacy tool use (v0.1.0/v0.2.0).
///
/// Source: `crates/agent/src/legacy_thread.rs`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SerializedToolUse {
    pub id: LanguageModelToolUseId,
    pub name: String,
    pub input: serde_json::Value,
}

/// A legacy tool result (v0.1.0/v0.2.0).
///
/// Source: `crates/agent/src/legacy_thread.rs`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SerializedToolResult {
    pub tool_use_id: LanguageModelToolUseId,
    pub is_error: bool,
    pub content: LanguageModelToolResultContent,
    pub output: Option<serde_json::Value>,
}

/// A UI crease (collapsible section) recorded with a legacy message.
///
/// Source: `crates/agent/src/legacy_thread.rs`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SerializedCrease {
    pub start: usize,
    pub end: usize,
    pub icon_path: String,
    pub label: String,
}

/// A single message in the legacy (v0.1.0/v0.2.0) format.
///
/// Source: `crates/agent/src/legacy_thread.rs`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SerializedMessage {
    pub id: MessageId,
    pub role: Role,
    #[serde(default)]
    pub segments: Vec<SerializedMessageSegment>,
    #[serde(default)]
    pub tool_uses: Vec<SerializedToolUse>,
    #[serde(default)]
    pub tool_results: Vec<SerializedToolResult>,
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub creases: Vec<SerializedCrease>,
    #[serde(default)]
    pub is_hidden: bool,
}

/// Complete legacy thread document (v0.2.0 / v0.1.0 after upgrade).
///
/// Reading:
/// - version `"0.1.0"` → deserialize into `SerializedThread`, apply `v0_1_0_upgrade`
/// - version `"0.2.0"` → deserialize directly
/// - no version field → legacy format, apply `legacy_upgrade` first
///
/// Source: `crates/agent/src/legacy_thread.rs`
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct SerializedThread {
    pub version: String,
    pub summary: String,
    pub updated_at: DateTime<Utc>,
    pub messages: Vec<SerializedMessage>,
    #[serde(default)]
    pub initial_project_snapshot: Option<ProjectSnapshot>,
    #[serde(default)]
    pub cumulative_token_usage: TokenUsage,
    #[serde(default)]
    pub request_token_usage: Vec<TokenUsage>,
    #[serde(default)]
    pub detailed_summary_state: DetailedSummaryState,
    #[serde(default)]
    pub model: Option<SerializedLanguageModel>,
    #[serde(default)]
    pub tool_use_limit_reached: bool,
    #[serde(default)]
    pub profile: Option<AgentProfileId>,
}
