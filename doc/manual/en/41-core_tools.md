\newpage

# Core tools

The core tools are the local slash commands that drive the client itself — discovering commands, switching server and model, managing sessions, browsing the workspace, building the project, and reviewing changes. They are handled locally and are never sent to the model, so they keep working even when the server or model status in the header is red.

Every command below also accepts the natural-language aliases listed in its **Examples**; the recognized phrases are matched only for these built-in commands, and any other input is sent to the model as an ordinary prompt.

\newpage

## /help

Shows the list of available commands with a one-line description of each, grouped the same way as in this manual.

It is the fastest way to rediscover a command name or its arguments without leaving the prompt, and it works regardless of server or model state.

### Examples

```text
/help
```

Natural-language forms:

```text
help
show help
show commands
show available commands
```

\newpage

## /tools

Lists the model-facing workspace tools — `read_file`, `edit_file`, `list_directory`, `fetch_url`, and `run_shell_command` — that the active model may call. These are the tools described in the Tools chapter and are distinct from the local slash commands documented here.

`/tools` is purely informational: it shows what the model is able to do in the current workspace, not what you can type at the prompt. In particular it is separate from the local `/list_files` convenience command.

### Examples

```text
/tools
```

Natural-language forms:

```text
tools
show tools
list tools
show local tools
```

\newpage

## /model

Selects the model used for requests on the active server.

With no argument it lists the selected server's models, with the active model shown in green and the others in red. With a name it switches to that model. Pressing `Tab` after `/model ` cycles through the models the server reports.

### Examples

List the available models:

```text
/model
```

Switch to a specific model:

```text
/model llama3.1:8b
```

Natural-language forms — list the models:

```text
models
list models
show models
show available models
```

Natural-language forms — show the current model:

```text
model
show model
current model
what model am i using
```

Natural-language forms — switch model:

```text
use model llama3.1:8b
switch model to llama3.1:8b
set model to llama3.1:8b
select model llama3.1:8b
```

\newpage

## /server

Selects the server `orangu` talks to.

With no argument it lists the configured servers — each `[section]` in the configuration file identified as a server — with the active one in green and the others in red. With a name it switches to that server and re-detects an available model on it, so the model status in the header is refreshed automatically. Pressing `Tab` after `/server ` cycles through the configured server names.

### Examples

List the configured servers:

```text
/server
```

Switch to a named server:

```text
/server local-llama
```

Natural-language forms — switch server:

```text
use server local-llama
switch server to local-llama
set server to local-llama
select server local-llama
```

\newpage

## /disconnect

Disconnects from the current server.

After disconnecting, the server status in the header turns red and free-form prompts are blocked, but every local command in this manual continues to work. Use `/server` or `/reload` to reconnect.

### Examples

```text
/disconnect
```

Natural-language form:

```text
disconnect
```

\newpage

## /reload

Restores the configured model and server from the configuration file, undoing any `/model` or `/server` switches made during the session.

`/reload` also clears the in-memory conversation history, so it doubles as a way to start a clean exchange while returning to the configured defaults.

### Examples

```text
/reload
```

Natural-language forms:

```text
reload
reload configuration
reset session
```

\newpage

## /restart

Restarts `orangu` in place, resuming the same workspace and session.

The current session is saved first, then the running process is replaced via `exec` with the same binary, passing `--workspace`, `--config`, and `--resume <session-id>` so the new process picks up exactly where the old one left off. This is the recommended way to pick up a freshly built binary or an edited configuration file without losing conversation context.

When the binary was rebuilt while running, the original on-disk path may be reported as deleted; `/restart` resolves the real path so it relaunches the freshly built binary. Only if that path is gone entirely does it fall back to staging a runnable copy under `~/.orangu/last`, a scratch directory that is cleared on every startup.

### Examples

```text
/restart
```

Natural-language forms:

```text
restart
restart orangu
```

\newpage

## /prune

Deletes session directories from `~/.orangu/sessions/`. The active session is never deleted regardless of strategy.

With a UUID it deletes that single session. With `all` it deletes every session except the active one. With `--workspace <path>` (or `-w <path>`) it deletes all sessions whose recorded workspace path contains `<path>`. With `--older-than <days>` (or `-o <days>`) it deletes all sessions whose `last_updated_at` timestamp is more than `<days>` days in the past.

Tab completion after `/prune ` cycles through session UUIDs newest-first.

### Examples

Delete a single session by UUID:

```text
/prune 550e8400-e29b-41d4-a716-446655440000
```

Delete all sessions except the active one:

```text
/prune all
```

Delete all sessions for a workspace:

```text
/prune -w myproject
```

Delete sessions not used in the last 30 days:

```text
/prune -o 30
```

Natural-language forms:

```text
prune session 550e8400-e29b-41d4-a716-446655440000
prune all
prune sessions in myproject
prune sessions older than 30
```

\newpage

## /session

Lists, switches, and opens sessions. See the Sessions section of the Terminal interface chapter for how sessions are stored, auto-resumed, and cleaned up.

With no argument it lists all sessions found under `~/.orangu/sessions/`, one line per session with aligned columns: UUID, start date, last-updated date, command count, branch, and workspace path. Sessions are sorted by creation time, most-recent first, and the branch column shows `-` for sessions with no recorded branch:

```text
UUID                                  STARTED       LAST          CMDS  BRANCH                WORKSPACE
550e8400-e29b-41d4-a716-446655440000  202605220910  202605221143    42  feature/my-pr         /home/user/myproject
a1b2c3d4-e5f6-7890-abcd-ef1234567890  202605210830  202605210831     3  -                     /home/user/other
```

The argument is resolved in order:

- When it names an existing session directory (a UUID), it switches to that session in place; the current session is saved first.
- Otherwise it is treated as a workspace match: if exactly one session's workspace path contains the string it switches to that session; if several do it lists only those sessions.
- If none match but the argument resolves to a real directory on disk (a leading `~`/`~/` is expanded), it opens that directory as a new workspace, auto-resuming any existing session for it or starting a fresh one.

Tab completion after `/session ` (with a trailing space) cycles through all session UUIDs newest first, then the distinct workspace paths recorded across sessions; when the typed text matches neither, it falls back to filesystem directory completion so a new workspace can be navigated to one segment at a time (`/session ~/co<Tab>/pr<Tab>`).

### Examples

List every session:

```text
/session
```

Narrow to sessions whose workspace path contains a string (switches straight to it when exactly one matches):

```text
/session myproject
```

Switch to a specific session by UUID:

```text
/session 550e8400-e29b-41d4-a716-446655440000
```

Open a directory that no session uses yet as a new workspace:

```text
/session ~/PostgreSQL/pgagroal/official
```

Natural-language forms:

```text
session
sessions
switch session
list sessions
show sessions
```

\newpage

## /list_files

Lists the workspace files as a tree.

This is a local convenience command for quickly orienting yourself in the workspace; it is separate from the model-facing `list_directory` tool that the model uses to explore files on its own.

### Examples

```text
/list_files
```

Natural-language forms:

```text
list files
show files
list workspace files
show workspace files
```

\newpage

## /open_file

Opens a workspace file in your `$EDITOR`.

The command is workspace-scoped — paths outside the workspace are rejected. It launches `$EDITOR` on the file in a separate window so `orangu` stays usable, and it never waits for the editor to close.

Tab completion after `/open_file ` searches the workspace recursively for file paths, and quoted paths such as `/open_file "..."` are supported.

### Examples

```text
/open_file README.md
/open_file src/main.rs
```

Natural-language forms:

```text
open README.md
open file README.md
edit src/main.rs
edit file src/main.rs
```

\newpage

## /show_file

Shows the contents of a workspace file, optionally at a specific Git ref and optionally with per-line blame columns.

```text
/show_file [--hash] [--author] <path> [<ref>]
```

The command is workspace-scoped. Without a ref, the current workspace file is shown — when `bat` is installed it is used for the plain view, otherwise the built-in syntax-highlighted renderer is used. When a ref (commit hash, branch, or tag) is given, the file content at that ref is retrieved via `git show <ref>:<path>` and rendered with the built-in renderer.

`--hash` and `--author` add per-line blame columns sourced from `git blame`, using the same ref when one is provided.

Tab completion for the first positional argument offers workspace file paths recursively; once a file path is present, the next `Tab` press cycles through that file's commit history (abbreviated hashes from `git log --follow`). `--hash` and `--author` are completed as well.

Because source lines may be wider than the terminal, `/show_file` output is laid out on the full virtual canvas and can be panned horizontally with `Alt+Left`/`Alt+Right` without reflowing.

### Examples

Show the current file:

```text
/show_file src/main.rs
```

Show the file with blame columns:

```text
/show_file --hash --author src/main.rs
```

Show the file as it was at an earlier commit:

```text
/show_file src/main.rs HEAD~3
```

Natural-language forms:

```text
show README.md
show file README.md
```

\newpage

## /build

Builds the workspace project, detecting the toolchain from the workspace root.

Each step is reported individually and the pipeline stops on the first failure:

- **Rust** (`Cargo.toml`) — runs `cargo fmt`, `cargo clippy`, `cargo build`, and `cargo test`.
- **C** (`CMakeLists.txt`) — runs `clang-format.sh` (if present), creates a `build/` directory if needed, runs `cmake ..` on the first build, then `make`.
- **Java** (`pom.xml`) — installs frontend dependencies with `npm ci` when outdated, runs `npm run fix` and `npm run check` for the frontend (if `src/frontend/` exists), then `mvn package`.

### Examples

```text
/build
```

Natural-language forms:

```text
build
build project
run build
```

\newpage

## /review

`/review` opens a full-screen, two-pane view for reviewing the changes on the current branch before sharing them, with the status bar and input window kept at the bottom so you can ask the model for help. It is available inside a Git repository and has no `gh` dependency.

Enter it with the `/review` command, or the natural-language forms `review`, `review changes`, `code review`, or `review branch`.

### What is reviewed

The review shows everything the current branch adds on top of the default branch:

- Committed changes on the branch, and
- Local uncommitted changes (staged and unstaged) in the working tree.

The comparison is made against the merge base with the default branch. The default branch is detected in the usual order: `origin/main`, `origin/master`, `main`, then `master`. If the working tree has no changes against that base, `/review` reports that there is nothing to review and does not open the view.

### Layout

Above the bottom prompt frame (the status bar and input window, exactly as on the normal screen), the view is split into two panes separated by a single straight vertical line.

- **Left pane** — the diff of the **selected file only**, rendered through the same pipeline as the `/diff` command, including the configured non-interactive git pager (such as `delta`) when one is set. It is the larger pane and scrolls independently.
- **Right pane** — the checklist of changed files, one per row, shown with their full repository-relative paths. The right pane is kept as narrow as possible: just wide enough for the longest path (capped on very narrow terminals so the diff stays usable).

Each file row begins with a review-status box:

- `[ ]` — not yet reviewed
- a green dot in the box — approved
- a red dot in the box — rejected

The currently selected file is highlighted in the right pane. Selecting a different file replaces the left pane with that file's diff, shown from the top.

```
 diff --git a/src/main.rs b/src/main.rs |Files (3)
 @@ -1,4 +1,5 @@                        |[ ] README.md
 +fn new() {}                           |[*] src/main.rs  <- selected
  (only the selected file's diff,       |[*] src/git.rs
   scrollable)                          |
                                        |
```

### Asking the model to review a file

The input window at the bottom takes a request for the model about the selected file — for example `focus on error handling` or `is this thread-safe?`. Press `Alt+o` (or `Enter`) to send that request together with the selected file's diff to the LLM. While the model works, the status bar shows the usual thinking indicator over the panes.

While the model works you are not stuck: press `Esc` twice to cancel the request and return to the diff, or `Alt+x` to leave review mode entirely. A cancelled request is rolled back out of the session.

When the response arrives it opens in a **feedback window** over the panes. If you typed a question, it is echoed at the top of the window — styled like a submitted prompt in the main output — above the model's review. A plain `Alt+o` (empty input) just asks for a plain review of the file, with no question echoed. Press `x` (or `Esc`) to close the window and return to the diff.

The request and the model's reply are added to your chat session, so after leaving review mode you can keep discussing it with full context.

### Opening the file

Press `Alt+e` to open the currently selected file in your `$EDITOR` — the same way as the `/open_file` command. Terminal editors open in a new window (the configured `terminal` command, or an auto-detected emulator) and GUI editors open their own window; either way the editor is detached, leaving review mode on screen so you can keep working through the diff. If the file cannot be opened, the error is shown in a feedback window.

### Commenting on a line

Move the highlighted line to the place you want to comment on and press `Alt+c`. A small comment window opens **inline, just below that line** in the left pane. Type your note (it wraps and the five-line window scrolls if the comment is long), then press `Enter` to save it or `Esc` to discard it.

Each comment is recorded against the file and that diff line; lines with a comment are flagged with an amber dot at the right edge. Pressing `Alt+c` on a line that already has a comment re-opens it for editing, and saving an empty comment removes it.

You can also add a **general note** about the patch: type `# <note>` in the input window and press `Enter` (or `Alt+o`). Instead of being sent to the model, it is recorded as a general note (the `#` is dropped). Anything not starting with `#` is still treated as an LLM request.

When you leave review mode (`Alt+x`), a summary is written to the output window. Each file is listed with its status and a colored dot — `<file>: Approved` with a green dot, `Rejected` with a red dot, or `No review` with a white dot (for unmarked files) — followed by the line comments, one per line, as `<file>:<line>: <comment>` (ordered by file then line, with 1-based line numbers), then the general notes (with the `#` removed). The summary ends with a bold verdict line. If every file is approved and there are no comments or notes the summary is just `Patch approved`; otherwise (any file rejected or unreviewed, or any comment/note) the verdict is `Patch rejected`.

The comments — both the line comments and the general (`#`) notes — are copied to the system clipboard; the per-file statuses and the verdict are not. If the clipboard cannot be reached (for example on a headless machine), a short note is shown instead and the output-window summary is unaffected.

### Key bindings

| Key | Action |
| --- | --- |
| `Alt+j` | Select the next file (shows its diff in the left pane) |
| `Alt+k` | Select the previous file (shows its diff in the left pane) |
| `Alt+a` | Mark the selected file approved (green dot) |
| `Alt+r` | Mark the selected file rejected (red dot) |
| `Alt+c` | Comment on the highlighted line (`Enter` saves, `Esc` discards) |
| `Alt+e` | Open the selected file in your configured editor |
| `Alt+o` / `Enter` | Ask the model to review the selected file using the typed request |
| `Esc` `Esc` | Cancel an in-progress review request (while the model is thinking) |
| `Alt+x` or `Esc` `Esc` | Exit review mode and return to the prompt |

When the feedback window is open it is modal: `x` or `Esc` closes it, and `Up`/`Down`, `PageUp`/`PageDown`, and `Left`/`Right` scroll and pan it.

Otherwise you can type into the input window normally, and move through the selected file's diff:

- `Up` / `Down` move a highlighted line cursor through the diff; the pane scrolls to keep it in view
- `Alt+Up` / `Alt+Down` scroll the diff one line at a time without moving the cursor
- `PageUp` / `PageDown` scroll the diff by a full page
- `Alt+Left` / `Alt+Right` pan horizontally for long lines

The review status marks and line comments are kept for the duration of the review session and are not persisted after exit.

### Examples

```text
/review
```

Natural-language forms:

```text
review
review changes
code review
review branch
```
