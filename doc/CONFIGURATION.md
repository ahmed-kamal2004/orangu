# Configuration

`orangu` uses an INI configuration file.

Default lookup order:

1. `./orangu.conf`
2. `~/.orangu/orangu.conf`

## Main section

The client section is named `[orangu]`.

```ini
[orangu]
model = gemma-4-E4B-it-GGUF
timeout = 1800
max_tool_rounds = 10
```

| Key | Required | Description |
| :-- | :-- | :-- |
| `model` | Yes, if multiple profiles exist | Default profile name |
| `timeout` | No | Request timeout in seconds. Defaults to `1800` |
| `max_tool_rounds` | No | Maximum tool-calling turns per prompt. Defaults to `10` |
| `quotes` | No | Quote set shown while the model is thinking. Defaults to `none`. Options: `none`, `star_trek`, `star_wars`, `marco_pierre_white`, `gordon_ramsay`, `calvin_and_hobbes`, `all` |
| `width` | No | Virtual terminal width for the output canvas. Source lines from `/show_file` are laid out at this width and can be panned horizontally. Defaults to `512` |

## Model sections

Each model profile is declared in its own section.

```ini
[gemma-4-E4B-it-GGUF]
provider = llama.cpp
endpoint = http://localhost:8100/v1
model = ggml-org/gemma-4-E4B-it-GGUF
```

| Key | Required | Description |
| :-- | :-- | :-- |
| `provider` | Yes | `llama.cpp` or `openai` |
| `endpoint` | Yes | OpenAI-compatible server URL |
| `model` | Yes | Model identifier sent to the server |
| `api_key` | No | Bearer token for OpenAI-compatible servers |
| `api_key_env` | No | Environment variable that contains the bearer token |

The canonical example file is `doc/etc/orangu.conf`.
