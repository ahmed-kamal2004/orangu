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

for example for an AMD based platform.

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

