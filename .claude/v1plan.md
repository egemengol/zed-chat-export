Here is the comprehensive, code-free architectural plan for **Zed Export v1.0**.

### 1. The Core Directive

**Goal:** Create a robust, "stateless" Command Line Interface (CLI) tool that performs a one-way synchronization of Zed conversation history into a flat, grep-friendly Markdown directory.

**Philosophy:**

- **Atomic:** Never interfere with the running Zed instance.
- **Flat:** No complex tree structures. Everything sits in one folder for easy navigation and linking.
- **Predictable:** Filenames are deterministic based on ID, not mutable titles.

---

### 2. Interface & Inputs

The CLI will be minimal and focused. It relies on `std::env::args` to avoid heavy dependency bloat.

**Usage:**
`zed-export <TARGET_DIRECTORY> [--db <PATH>] [--tags <TAG_LIST>]`

**Arguments:**

1.  **Target Directory (Required):** Where the files will go. If it doesn't exist, we create it.
2.  **`--db` (Optional):**
    - **Default:** We check `~/Library/Application Support/Zed/threads/threads.db` (macOS).
    - **Fallback:** If not found, we error and request this flag.
3.  **`--tags` (Optional):** A comma-separated list (e.g., `zed,export,llm`). These are injected into the frontmatter of every exported file.

_Decision Note:_ Redaction is **omitted** for v1. We will hand-roll a specific solution in v2 rather than importing a heavy regex engine now.

---

### 3. The "Snapshot" Pipeline (Safety Mechanism)

To guarantee we never lock the database or hit `SQLITE_BUSY` errors while Zed is running:

1.  **Connect:** Open a Read-Only connection to the live `threads.db`.
2.  **Initialize Backup:** Use the SQLite Online Backup API (`sqlite3_backup_init`) to target a temporary file path (e.g., in `/tmp`).
3.  **Execute:** Run the backup steps. This copies the pages efficiently and atomically.
4.  **Disconnect:** Close the connection to the live DB immediately.
5.  **Process:** Open the _temporary snapshot_ for the actual heavy lifting (extraction).
6.  **Cleanup:** Delete the snapshot file when done.

---

### 4. The Extraction Logic

We iterate through every row in the `threads` table of the snapshot.

**Data Handling:**

1.  **Decompression:** Detect `zstd` blobs and decode them. Pass through `json` blobs as-is.
2.  **Deserialization:**
    - Attempt to parse as `DbThread` (Schema v0.3 - current).
    - Fallback to `SerializedThread` (Schema v0.1/v0.2 - legacy).
    - Discard rows that fail both (logging the error).

---

### 5. Filename & Collision Strategy

This is the critical "Identity" logic. We prioritize short, readable filenames (8 chars) but handle the mathematical possibility of collisions by extending the UUID length, not by appending arbitrary numbers.

**The Algorithm:**
We maintain a runtime Map: `Map<ShortFilename, OriginalFullUUID>`.

For each thread:

1.  **Draft Name:** Take the first 8 characters of the Thread UUID.
2.  **Check Availability:** Look up the draft name in the Map.
    - **Case A (Empty):** The name is free. Register `DraftName -> FullUUID`.
    - **Case B (Collision):** The name is taken, and the `FullUUID` in the map does **not** match the current thread.
      - _Action:_ Extend the draft name to 12 characters.
      - _Retry:_ Check availability again. If 12 collides (astronomically unlikely), go to full UUID.
3.  **Finalize:** Use the determined unique string as the base filename.

**Result:**

- Most files: `a1b2c3d4.md`
- Rare collision: `a1b2c3d4e5f6.md`

---

### 6. The Output Format

All files live in the root of `<TARGET_DIRECTORY>`.

**Markdown Files (`.md`):**

- **Frontmatter:** YAML block containing:
  - `title`: The conversation title (from DB).
  - `updated_at`: ISO 8601 timestamp.
  - `model`: Provider/Model string (e.g., `anthropic/claude-3-opus`).
  - `tags`: The list provided via CLI args (if any).
- **Body:** The conversation history formatted as Markdown headers (`## User`, `## Assistant`) and text blocks.

**Assets (Images):**

- **Extraction:** Base64 image strings are decoded to bytes.
- **Naming:** `[Thread_Short_ID].[Image_Content_Hash_6_Chars].[Ext]`
  - _Example:_ `a1b2c3d4.f89a2b.png`
- **Linking:** The Markdown file references the image using a relative link: `![image](./a1b2c3d4.f89a2b.png)`.

---

### 7. Dependencies Checklist

- `rusqlite` (with `backup` feature)
- `serde` / `serde_json` / `serde_yaml`
- `zstd`
- `chrono`
- `sha2` (for image hashing)
- `base64`
- `infer` (for file extension detection)

This plan is self-contained and ready for implementation.
