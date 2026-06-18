\newpage

## OpenAI platform

### llama.cpp

[**llama.cpp**][llama] is an OpenAI compatible platform.

Please, look at their documentation for further information.

**Building**

Clone the repository

```sh
git clone https://github.com/ggml-org/llama.cpp.git
```

```sh
cmake -DCMAKE_C_COMPILER=clang -DCMAKE_CXX_COMPILER=clang++ \
      -DCMAKE_INSTALL_PREFIX=/usr/local/ -DGGML_VULKAN=1 -B build
cmake --build build --config Debug -j 12

cd build
su
make install
```

for example for an AMD/Vulkan based platform.

**Running**

Small model

```sh
llama-server -hf ggml-org/gemma-4-E4B-it-GGUF \
             --port 8100 \
             --ctx-size 65536 \
             -sm layer \
             -t 4 \
             --webui-mcp-proxy \
             --fit on
```

Coding model

```sh
llama-server -hf yuxinlu1/gemma-4-12B-coder-fable5-composer2.5-v1-GGUF \
             --port 8100 \
             --ctx-size 131072 \
             -t 4 \
             --webui-mcp-proxy \
             --fit on \
             --image-min-tokens 1024 \
             --tools all
```

or

```sh
llama-server -hf unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF \
             --port 8100 \
             --ctx-size 262144 \
             -sm layer \
             -t 4 \
             --webui-mcp-proxy \
             --fit on
```

Big model

```sh
llama-server -hf bartowski/Qwen_Qwen3.6-27B-GGUF \
             --port 8100 \
             --ctx-size 65536 \
             -t 4 \
             --webui-mcp-proxy \
             --fit on
```

Review model

[Gemma4](https://huggingface.co/collections/ggml-org/gemma-4) models will work locally.

For example

* gemma-4-E4B-it-GGUF
* gemma-4-12B-it-GGUF
* gemma-4-31B-it-GGUF

depending on your machine size, and then

```sh
llama-server -hf ggml-org/gemma-4-12B-it-GGUF \
             --port 8100 \
             --ctx-size 262144 \
             -np 1 \
             -fa on \
             -ctk q8_0 \
             -ctv q8_0 \
             -b 2048 \
             -ub 2048 \
             --cache-reuse 256 \
             --reasoning-budget 0 \
             --reasoning off \
             --temp 0.7 \
             --top-p 0.8 \
             --top-k 20 \
             --min-p 0 \
             --jinja \
             --fit on
```

Or, you can use a Qwen model which might give more feedback, but many more false positives

```sh
llama-server -hf bartowski/Qwen_Qwen3.6-35B-A3B-GGUF \
             --port 8100 \
             --ctx-size 32768 \
             -np 1 \
             -fa on \
             -ctk q8_0 -ctv q8_0 \
             -b 2048 -ub 2048 \
             --cache-reuse 256 \
             --reasoning-budget 0 \
             --chat-template-kwargs '{"enable_thinking": false}' \
             --temp 0.7 --top-p 0.8 --top-k 20 --min-p 0 \
             --jinja \
             --fit on
```
