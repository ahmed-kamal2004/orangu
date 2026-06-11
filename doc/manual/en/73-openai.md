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

Tuned for `/auto_review`: the client sends many small, sequential requests —
one per file and category, each carrying one file's diff — with short, bounded
answers (see the *Response-token caps* part of the Configuration chapter).

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

Why these options:

- `--ctx-size 32768` — a review prompt is one file's diff plus a short answer,
  so a modest context frees the memory that `--fit` then spends on keeping
  more layers on the GPU. A pinned huge context (e.g. `262144`) forces layers
  off the GPU instead.
- `-np 1` and `--cache-reuse 256` — the requests are strictly sequential, and
  a file's category requests share their prefix (the diff leads each prompt),
  so a single slot with KV-cache chunk reuse processes each diff once instead
  of once per category.
- `--reasoning-budget 0` with `--chat-template-kwargs
  '{"enable_thinking": false}'` — both are needed to actually disable thinking
  on Qwen3.x hybrids; the review then answers in seconds. To review **with**
  thinking instead, drop these two flags and raise the
  `[orangu].review_max_tokens` key in `orangu.conf` (e.g. `2048`) so the
  thinking tokens do not eat the answer — see *Response-token caps* in the
  Configuration chapter.
- `--temp 0.7 --top-p 0.8 --top-k 20 --min-p 0` — Qwen's non-thinking sampler
  recommendation; the lower temperature also makes APPROVE/REJECT verdicts
  more consistent between runs.
- `-fa on`, `-ctk q8_0 -ctv q8_0`, `-ub 2048` — flash attention, a half-size
  quantized KV cache, and a large physical batch for fast diff prefill. Dial
  `-ub` back first if `--fit` starts dropping GPU layers.

Two optional speed-ups:

- **MTP (multi-token prediction)** — with an MTP-converted GGUF (standard
  GGUF conversions lack the prediction heads; MTP support was merged into
  llama.cpp in May 2026), speculative decoding accelerates generation further:
  add `--spec-draft-n-max 3` and use the MTP build of the model.
- **Tight VRAM** — `-ncmoe N` (or `-cmoe` for all layers) keeps the
  Mixture-of-Experts weights of the first `N` layers on the CPU while
  attention stays on the GPU — the standard recipe for running
  few-active-parameter MoE models (such as this one: 35B total, ~3B active,
  the `A3B` in its name) fast on small GPUs.

