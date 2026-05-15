\newpage

# Tools

`orangu` exposes local workspace tools to the active model.

## Available tools

| Tool | Purpose |
| :-- | :-- |
| `read_file` | Read a text file, optionally with a line range |
| `edit_file` | Replace text in a workspace file |
| `list_directory` | List files and directories below the workspace |
| `fetch_url` | Fetch an external URL and convert HTML into readable text |
| `run_shell_command` | Run a shell command inside the workspace |

## Workspace restrictions

The tools are rooted in the active workspace. By default this is the current directory.

Paths that attempt to escape the workspace are rejected.

## File editing

The `edit_file` tool is designed for precise replacements:

```json
{
  "path": "src/main.rs",
  "old_text": "fn old_name()",
  "new_text": "fn new_name()"
}
```

Optional flags:

- `replace_all`
- `create_if_missing`

## URL fetching

`fetch_url` lets the model read external documentation or reference material without leaving the client workflow.

## Shell commands

`run_shell_command` executes within the workspace and can be used for inspection, build, or validation steps.
