// Qwen3.5 text model on MLX (Metal GPU) behind a small C interface for
// `src/mlx/mod.rs`. Every function returns 0 (or a non-null pointer) on
// success; on failure `mwx_last_error` describes what went wrong on this
// thread.
#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct mwx_config {
  int32_t hidden;
  int32_t intermediate;
  int32_t vocab;
  int32_t layers;
  // One byte per layer: 1 = full attention, 0 = Gated DeltaNet.
  const uint8_t* full;
  int32_t heads;
  int32_t kv_heads;
  int32_t head_dim;
  int32_t rotary_dim;
  int32_t linear_key_heads;
  int32_t linear_value_heads;
  int32_t linear_key_dim;
  int32_t linear_value_dim;
  int32_t conv_kernel;
  float eps;
  int32_t slots;
  int32_t context;
  // RoPE tables `[context, rotary_dim]`, both halves filled (NeoX layout).
  const float* cos;
  const float* sin;
} mwx_config;

enum { MWX_F16 = 0, MWX_F32 = 1 };

// One weight, borrowed: `data` must stay valid until `mwx_free`.
typedef struct mwx_tensor {
  const char* name;
  const void* data;
  int32_t dtype;
  int32_t ndim;
  int64_t shape[4];
} mwx_tensor;

typedef struct mwx_model mwx_model;

const char* mwx_last_error(void);

// Once per process, before anything else. `cache_limit` caps the bytes of
// freed GPU buffers MLX keeps for reuse.
int mwx_init(const char* metallib_path, size_t cache_limit);

// The GPU's name, e.g. "Apple M4 Pro".
const char* mwx_device_name(void);

mwx_model* mwx_load(const mwx_config* config, const mwx_tensor* tensors, size_t count);
void mwx_free(mwx_model* model);

// Runs `tokens` from an empty sequence; `*out` (free with `mwx_free_bytes`)
// receives the state after them.
int mwx_prefix(mwx_model* model, const uint32_t* tokens, size_t n, uint8_t** out, size_t* out_len);
void mwx_free_bytes(uint8_t* bytes);

// Starts `slot` from a state `mwx_prefix` wrote for `prefix_tokens` tokens,
// followed by `tokens`; writes the next-token logits (`vocab` floats).
int mwx_admit(
    mwx_model* model,
    int32_t slot,
    const uint8_t* prefix,
    size_t prefix_len,
    int32_t prefix_tokens,
    const uint32_t* tokens,
    size_t n,
    float* logits);

// Appends `tokens[i]` to `slots[i]`; writes `n * vocab` logits.
int mwx_step(mwx_model* model, const int32_t* slots, const uint32_t* tokens, size_t n, float* logits);

void mwx_release(mwx_model* model, int32_t slot);

// With no live slot, drops every per-sequence buffer; always empties MLX's
// buffer cache. The weights stay.
void mwx_trim(mwx_model* model);

#ifdef __cplusplus
}
#endif
