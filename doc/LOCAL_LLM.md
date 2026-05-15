# Local LLM

`orangu` is designed to talk directly to a local llama.cpp server using its OpenAI-compatible API.

## Example configuration

```ini
[orangu]
model = gemma-4-E4B-it-GGUF
timeout = 1800
max_tool_rounds = 10

[gemma-4-E4B-it-GGUF]
provider = llama.cpp
endpoint = http://localhost:8100/v1
model = ggml-org/gemma-4-E4B-it-GGUF
```

## Quick verification

Check the server:

```sh
curl http://localhost:8100/v1/models
```

Run the client:

```sh
orangu --config ./orangu.conf
```

## Notes

- The endpoint may be configured as either the server root or the `/v1` path.
- Tool-calling prompts can be slow on local models, so a larger timeout is recommended.
- The local tools run against the current workspace and can edit files on disk.
