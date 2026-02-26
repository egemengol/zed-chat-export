## Documentation Guidance

You don't need a novel, but you need a map. Since you are targeting developers (Zed users) and potentially LLM agents, your docs need to be structural.

**The README Structure**

1.  **The Hook (What & Why):**
    - _One sentence:_ "Export your Zed conversations to standard Markdown for archiving and LLM context injection."
    - _The Problem:_ "Zed searches are ephemeral. Your knowledge shouldn't be."

2.  **Installation:**
    - Show the `cargo install` command.
    - (Ideally) Show the `brew install` or binary download link (see Section 3).

3.  **Usage (The "Happy Path"):**
    - Don't dump the help text immediately. Show the most common command:
      `zed-export export --target-dir ~/Obsidian/Zed`

4.  **The "How it Works" (Crucial for Trust):**
    - Explain that you read the SQLite DB.
    - Explain that you handle the Zstd decompression.
    - **Privacy Declaration:** Explicitly state that this runs locally. It does not send data to the cloud. This is mandatory for a tool that reads private chat history.

5.  **Output Format:**
    - Show a snippet of the Frontmatter you generate. Users need to know how to query it (e.g., "It includes `git` metadata!").

6.  **Limitations (The "v1" honesty):**
    - List what it _doesn't_ do yet (e.g., "Does not currently support live watching"). This manages expectations and prevents "It's broken" issues.
    - I'm leaving some features to v2, like redaction, ghost pruning, watch
