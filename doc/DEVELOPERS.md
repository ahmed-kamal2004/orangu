# Developers

This project is a local coding-environment client built around a direct OpenAI-compatible chat loop.

## Main components

- `src/bin/orangu.rs` - terminal loop, commands, history, prompt rendering, and waiting state
- `src/config.rs` - INI parsing and normalization
- `src/llm/openai.rs` - OpenAI-compatible llama.cpp client
- `src/session.rs` - tool-calling conversation flow
- `src/tools.rs` - local workspace tools for reading, editing, listing, fetching, and shell commands
- `src/tui.rs` - banner and prompt frame rendering

## Development workflow

```sh
cargo fmt
cargo test
```

## Documentation workflow

The manual sources live under `doc/manual/en`.

```sh
./doc/build_manual.sh
```

## Notes

- The client is workspace-scoped by default and uses the current directory unless `--workspace` is supplied.
- Command history is stored in `~/orangu/orangu.history`.
- Local llama.cpp deployments may take significant time to answer tool-calling prompts, so the default timeout is 30 minutes.
