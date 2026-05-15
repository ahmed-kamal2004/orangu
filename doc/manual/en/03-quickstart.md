\newpage

# Quick start

This chapter gets **orangu** running against a local OpenAI-compatible server - like llama.cpp - with the sample configuration in `doc/etc/orangu.conf`.

## Start llama.cpp

Run `llama-server` with your preferred model - for example

```sh
llama-server -hf ggml-org/gemma-4-E4B-it-GGUF \
             --port 8100 \
             --ctx-size 65536 \
             -sm layer \
             -t 4 \
             --webui-mcp-proxy \
             --fit on
```

**orangu** expects an OpenAI-compatible endpoint, such as:

```text
http://localhost:8100/v1
```

## Create a configuration

Copy the sample:

```sh
cp doc/etc/orangu.conf ./orangu.conf
```

Default configuration lookup order is:

1. `./orangu.conf`
2. `~/orangu/orangu.conf`

## Run the client

```sh
./orangu
```

And, use

```sh
/help
```

to get a help for the application.

Note, that by default the tools operate on the current directory. Use `--workspace /path/to/project` to point **orangu** at another tree.
