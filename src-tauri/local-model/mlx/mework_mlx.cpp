// Qwen3.5 text model on MLX. See mework_mlx.h for the interface and
// src/mlx/mod.rs for how the host drives it.
//
// Numerics: the residual stream, norms, gates and the DeltaNet recurrence are
// float32; projections run in float16 (the weights are float16 and MLX
// accumulates their products in float32). The recurrence is one Metal kernel
// compiled at run time from the source below, so it needs no offline Metal
// toolchain; it loops over the tokens of a call inside the kernel, which
// serves prefill (one sequence, many tokens) and decode (all slots, one
// token) alike.
//
// Per-slot state: KV caches `[slots, kv_heads, context, head_dim]` (float16)
// for the full-attention layers, and for the DeltaNet layers the conv window
// `[slots, kernel - 1, conv_dim]` and the recurrent state
// `[slots, v_heads, v_dim, k_dim]` (float32).

#include "mework_mlx.h"

#include <algorithm>
#include <cmath>
#include <cstring>
#include <optional>
#include <string>
#include <unordered_map>
#include <vector>

#include "mlx/backend/metal/metal.h"
#include "mlx/memory.h"
#include "mlx/mlx.h"

namespace mx = mlx::core;

namespace {

thread_local std::string last_error;

constexpr uint32_t PREFIX_MAGIC = 0x3158574d; // "MWX1"

const char* GATED_DELTA_SOURCE = R"(
  uint n = thread_position_in_grid.z;
  uint b = n / Hv;
  uint hv = n % Hv;
  uint hk = hv / (Hv / Hk);
  uint lane = thread_position_in_threadgroup.x;
  uint dv = thread_position_in_grid.y;
  constexpr int per = Dk / 32;

  const device float* q_ = q + (b * steps * Hk + hk) * Dk + lane * per;
  const device float* k_ = k + (b * steps * Hk + hk) * Dk + lane * per;
  const device float* v_ = v + (b * steps * Hv + hv) * Dv;
  const device float* g_ = g + b * steps * Hv + hv;
  const device float* beta_ = beta + b * steps * Hv + hv;
  device float* y_ = y + (b * steps * Hv + hv) * Dv;

  const device float* s_in = state_in + (n * Dv + dv) * Dk + lane * per;
  device float* s_out = state_out + (n * Dv + dv) * Dk + lane * per;

  float s[per];
  for (int i = 0; i < per; ++i) {
    s[i] = s_in[i];
  }
  for (int t = 0; t < steps; ++t) {
    float decay = g_[0];
    float mem = 0.0f;
    for (int i = 0; i < per; ++i) {
      s[i] = s[i] * decay;
      mem += s[i] * k_[i];
    }
    mem = simd_sum(mem);
    float delta = (v_[dv] - mem) * beta_[0];
    float out = 0.0f;
    for (int i = 0; i < per; ++i) {
      s[i] = s[i] + k_[i] * delta;
      out += s[i] * q_[i];
    }
    out = simd_sum(out);
    if (lane == 0) {
      y_[dv] = out;
    }
    q_ += Hk * Dk;
    k_ += Hk * Dk;
    v_ += Hv * Dv;
    y_ += Hv * Dv;
    g_ += Hv;
    beta_ += Hv;
  }
  for (int i = 0; i < per; ++i) {
    s_out[i] = s[i];
  }
)";

struct Config {
  int hidden, intermediate, vocab, layers;
  std::vector<bool> full;
  int heads, kv_heads, head_dim, rotary_dim;
  int lk, lv, dk, dv, kernel;
  float eps;
  int slots, context;

  int key_dim() const { return lk * dk; }
  int value_dim() const { return lv * dv; }
  int conv_dim() const { return 2 * key_dim() + value_dim(); }
};

// State of the DeltaNet and attention layers for a batch of rows.
struct States {
  std::vector<mx::array> conv, ssm; // per DeltaNet layer
  std::vector<mx::array> k, v;      // per attention layer
};

mx::array silu(const mx::array& x) {
  return x * mx::sigmoid(x);
}

mx::array f16(const mx::array& x) {
  return mx::astype(x, mx::float16);
}

mx::array f32(const mx::array& x) {
  return mx::astype(x, mx::float32);
}

// x (float16) times W^T for a float16 `[out, in]` weight.
mx::array project(const mx::array& x16, const mx::array& w) {
  return mx::matmul(x16, mx::transpose(w));
}

} // namespace

struct mwx_model {
  Config c;
  std::unordered_map<std::string, mx::array> weights;
  mx::array cos = mx::array(0.0f);
  mx::array sin = mx::array(0.0f);
  mx::fast::CustomKernelFunction delta_kernel;
  std::optional<States> slots;
  std::vector<int> lens;
  std::vector<bool> live;

  const mx::array& w(const std::string& name) const {
    auto found = weights.find(name);
    if (found == weights.end()) {
      throw std::runtime_error("权重缺少 " + name);
    }
    return found->second;
  }

  const mx::array& lw(int layer, const char* name) const {
    return w(std::to_string(layer) + "." + name);
  }

  mx::array norm(const mx::array& x, const mx::array& weight) const {
    return mx::fast::rms_norm(x, weight, c.eps);
  }

  // RoPE on the first `rotary_dim` features of `x` `[B, T, heads, head_dim]`
  // at positions `pos` `[B, T]`.
  mx::array rope(const mx::array& x, const mx::array& pos) const {
    int rot = c.rotary_dim, half = rot / 2;
    auto cs = mx::expand_dims(mx::take(cos, pos, 0), 2); // [B, T, 1, rot]
    auto sn = mx::expand_dims(mx::take(sin, pos, 0), 2);
    auto shape = x.shape();
    auto head = mx::slice(x, {0, 0, 0, 0}, {shape[0], shape[1], shape[2], rot});
    auto rest = mx::slice(x, {0, 0, 0, rot}, {shape[0], shape[1], shape[2], shape[3]});
    auto first = mx::slice(head, {0, 0, 0, 0}, {shape[0], shape[1], shape[2], half});
    auto second = mx::slice(head, {0, 0, 0, half}, {shape[0], shape[1], shape[2], rot});
    auto rotated = mx::concatenate({mx::negative(second), first}, 3);
    return mx::concatenate({head * cs + rotated * sn, rest}, 3);
  }

  mx::array mlp(int layer, const mx::array& x) const {
    auto h = f16(norm(x, lw(layer, "post_norm")));
    auto gate = f32(project(h, lw(layer, "gate")));
    auto up = f32(project(h, lw(layer, "up")));
    return f32(project(f16(silu(gate) * up), lw(layer, "down")));
  }

  // Gated DeltaNet over `x` `[B, T, H]`; `conv` and `ssm` are this layer's
  // states for the same B rows, replaced by the states after the T tokens.
  mx::array delta_net(int layer, const mx::array& x, mx::array& conv, mx::array& ssm) const {
    int B = x.shape(0), T = x.shape(1);
    int K = c.kernel, C = c.conv_dim();
    auto h = f16(norm(x, lw(layer, "input_norm")));
    auto qkv = f32(project(h, lw(layer, "qkv")));
    auto z = f32(project(h, lw(layer, "z")));
    auto b = f32(project(h, lw(layer, "b")));
    auto a = f32(project(h, lw(layer, "a")));

    // Causal depthwise conv over [previous K-1 inputs, these T].
    auto padded = mx::concatenate({conv, qkv}, 1); // [B, T+K-1, C]
    const auto& taps = lw(layer, "conv");           // [K, C]
    auto mixed = mx::zeros({B, T, C}, mx::float32);
    for (int j = 0; j < K; ++j) {
      auto window = mx::slice(padded, {0, j, 0}, {B, j + T, C});
      auto tap = mx::slice(taps, {j, 0}, {j + 1, C});
      mixed = mixed + window * tap;
    }
    conv = mx::slice(padded, {0, T, 0}, {B, T + K - 1, C});
    mixed = silu(mixed);

    int kd = c.key_dim();
    auto q = mx::reshape(mx::slice(mixed, {0, 0, 0}, {B, T, kd}), {B, T, c.lk, c.dk});
    auto k = mx::reshape(mx::slice(mixed, {0, 0, kd}, {B, T, 2 * kd}), {B, T, c.lk, c.dk});
    auto v = mx::reshape(mx::slice(mixed, {0, 0, 2 * kd}, {B, T, C}), {B, T, c.lv, c.dv});
    auto l2 = [](const mx::array& t) {
      return t * mx::rsqrt(mx::sum(mx::square(t), -1, true) + 1e-6f);
    };
    q = l2(q) * (1.0f / std::sqrt(static_cast<float>(c.dk)));
    k = l2(k);

    auto beta = mx::sigmoid(b);
    auto softplus = mx::logaddexp(a + lw(layer, "dt_bias"), mx::array(0.0f));
    auto decay = mx::exp(lw(layer, "A") * softplus); // [B, T, lv]

    auto outputs = delta_kernel(
        {q, k, v, decay, beta, ssm, mx::array(T, mx::int32)},
        {{B, T, c.lv, c.dv}, ssm.shape()},
        {mx::float32, mx::float32},
        {32, c.dv, B * c.lv},
        {32, 4, 1},
        {{"Hk", c.lk}, {"Hv", c.lv}, {"Dk", c.dk}, {"Dv", c.dv}},
        std::nullopt,
        false,
        {});
    ssm = outputs[1];
    auto y = norm(outputs[0], lw(layer, "linear_norm"));
    y = y * silu(mx::reshape(z, {B, T, c.lv, c.dv}));
    return f32(project(f16(mx::reshape(y, {B, T, c.value_dim()})), lw(layer, "out")));
  }

  struct Attention {
    mx::array q, k, v, gate; // q/k/v `[B, heads, T, head_dim]`, gate `[B, T, heads*head_dim]`
  };

  // Projections, norms and RoPE of an attention layer for `x` `[B, T, H]`.
  Attention attention_inputs(int layer, const mx::array& x, const mx::array& pos) const {
    int B = x.shape(0), T = x.shape(1), hd = c.head_dim;
    auto h = f16(norm(x, lw(layer, "input_norm")));
    auto qg = mx::reshape(f32(project(h, lw(layer, "q"))), {B, T, c.heads, 2 * hd});
    auto q = mx::slice(qg, {0, 0, 0, 0}, {B, T, c.heads, hd});
    auto gate = mx::reshape(mx::slice(qg, {0, 0, 0, hd}, {B, T, c.heads, 2 * hd}), {B, T, c.heads * hd});
    auto k = mx::reshape(f32(project(h, lw(layer, "k"))), {B, T, c.kv_heads, hd});
    auto v = mx::reshape(project(h, lw(layer, "v")), {B, T, c.kv_heads, hd});
    q = rope(norm(q, lw(layer, "q_norm")), pos);
    k = rope(norm(k, lw(layer, "k_norm")), pos);
    auto heads_first = [](const mx::array& t) { return mx::transpose(t, {0, 2, 1, 3}); };
    return {heads_first(f16(q)), heads_first(f16(k)), heads_first(v), gate};
  }

  mx::array attention_output(int layer, const mx::array& o, const mx::array& gate) const {
    int B = o.shape(0), T = o.shape(2);
    auto merged = mx::reshape(mx::transpose(o, {0, 2, 1, 3}), {B, T, c.heads * c.head_dim});
    auto gated = f32(merged) * mx::sigmoid(gate);
    return f32(project(f16(gated), lw(layer, "o")));
  }

  mx::array embed(const std::vector<uint32_t>& tokens, int B, int T) const {
    std::vector<int32_t> ids(tokens.begin(), tokens.end());
    for (auto id : ids) {
      if (id < 0 || id >= c.vocab) {
        throw std::runtime_error("词元超出词表");
      }
    }
    auto index = mx::array(ids.data(), {B, T}, mx::int32);
    return f32(mx::take(w("embed"), index, 0));
  }

  // Final norm and output projection of `h` `[N, H]`.
  mx::array logits(const mx::array& h) const {
    return f32(project(f16(norm(h, w("norm"))), w("embed")));
  }

  States empty_states(int rows) const {
    States s;
    for (int layer = 0; layer < c.layers; ++layer) {
      if (c.full[layer]) {
        s.k.push_back(mx::zeros({rows, c.kv_heads, c.context, c.head_dim}, mx::float16));
        s.v.push_back(mx::zeros({rows, c.kv_heads, c.context, c.head_dim}, mx::float16));
      } else {
        s.conv.push_back(mx::zeros({rows, c.kernel - 1, c.conv_dim()}, mx::float32));
        s.ssm.push_back(mx::zeros({rows, c.lv, c.dv, c.dk}, mx::float32));
      }
    }
    return s;
  }

  // One sequence: `tokens` after `start` positions whose attention keys and
  // values are `history` (`[1, kv_heads, start, head_dim]` each, per
  // attention layer; empty when start is 0) and whose DeltaNet states are in
  // `conv`/`ssm`. Returns the last position's hidden state `[1, H]`; the
  // states are advanced and `history` grows by the tokens.
  mx::array run_sequence(
      const std::vector<uint32_t>& tokens,
      int start,
      std::vector<mx::array>& conv,
      std::vector<mx::array>& ssm,
      std::vector<mx::array>& keys,
      std::vector<mx::array>& values) const {
    int T = static_cast<int>(tokens.size());
    if (start + T > c.context) {
      throw std::runtime_error("超出上下文长度");
    }
    auto x = embed(tokens, 1, T);
    auto pos = mx::reshape(mx::arange(start, start + T, mx::int32), {1, T});
    int li = 0, ai = 0;
    for (int layer = 0; layer < c.layers; ++layer) {
      mx::array mixed = mx::array(0.0f);
      if (c.full[layer]) {
        auto in = attention_inputs(layer, x, pos);
        if (start > 0) {
          keys[ai] = mx::concatenate({keys[ai], in.k}, 2);
          values[ai] = mx::concatenate({values[ai], in.v}, 2);
        } else {
          keys[ai] = in.k;
          values[ai] = in.v;
        }
        auto scale = 1.0f / std::sqrt(static_cast<float>(c.head_dim));
        auto o = mx::fast::scaled_dot_product_attention(in.q, keys[ai], values[ai], scale, T > 1 ? "causal" : "");
        mixed = attention_output(layer, o, in.gate);
        ++ai;
      } else {
        mixed = delta_net(layer, x, conv[li], ssm[li]);
        ++li;
      }
      x = x + mixed;
      x = x + mlp(layer, x);
    }
    return mx::reshape(mx::slice(x, {0, T - 1, 0}, {1, T, c.hidden}), {1, c.hidden});
  }

  void ensure_slots() {
    if (!slots) {
      slots = empty_states(c.slots);
    }
  }
};

namespace {

template <typename F>
int guard(F&& body) {
  try {
    body();
    return 0;
  } catch (const std::exception& error) {
    last_error = error.what();
    return 1;
  }
}

template <typename T>
void put(std::vector<uint8_t>& out, const T* data, size_t count) {
  auto bytes = reinterpret_cast<const uint8_t*>(data);
  out.insert(out.end(), bytes, bytes + count * sizeof(T));
}

// Copies an evaluated array's elements out in row-major order.
template <typename T>
void put_array(std::vector<uint8_t>& out, const mx::array& a) {
  auto dense = mx::contiguous(a);
  mx::eval(dense);
  put(out, dense.data<T>(), dense.size());
}

struct Reader {
  const uint8_t* data;
  size_t len;
  size_t at = 0;

  template <typename T>
  const T* take(size_t count) {
    size_t bytes = count * sizeof(T);
    if (at + bytes > len) {
      throw std::runtime_error("前置状态数据不完整");
    }
    auto p = reinterpret_cast<const T*>(data + at);
    at += bytes;
    return p;
  }
};

} // namespace

extern "C" {

const char* mwx_last_error(void) {
  return last_error.c_str();
}

int mwx_init(const char* metallib_path, size_t cache_limit) {
  return guard([&] {
    mx::metal::set_metallib_path(metallib_path);
    if (!mx::metal::is_available()) {
      throw std::runtime_error("这台 Mac 的 GPU 不能运行 MLX");
    }
    mx::set_default_device(mx::Device::gpu);
    mx::set_cache_limit(cache_limit);
  });
}

const char* mwx_device_name(void) {
  static std::string name;
  guard([&] {
    const auto& info = mx::device_info(mx::Device(mx::Device::gpu));
    auto found = info.find("device_name");
    name = found != info.end() && std::holds_alternative<std::string>(found->second)
        ? std::get<std::string>(found->second)
        : std::string("Apple GPU");
  });
  return name.c_str();
}

mwx_model* mwx_load(const mwx_config* config, const mwx_tensor* tensors, size_t count) {
  mwx_model* model = nullptr;
  int status = guard([&] {
    auto m = std::make_unique<mwx_model>();
    auto& c = m->c;
    c.hidden = config->hidden;
    c.intermediate = config->intermediate;
    c.vocab = config->vocab;
    c.layers = config->layers;
    for (int i = 0; i < c.layers; ++i) {
      c.full.push_back(config->full[i] != 0);
    }
    c.heads = config->heads;
    c.kv_heads = config->kv_heads;
    c.head_dim = config->head_dim;
    c.rotary_dim = config->rotary_dim;
    c.lk = config->linear_key_heads;
    c.lv = config->linear_value_heads;
    c.dk = config->linear_key_dim;
    c.dv = config->linear_value_dim;
    c.kernel = config->conv_kernel;
    c.eps = config->eps;
    c.slots = config->slots;
    c.context = config->context;
    if (c.dk % 32 != 0 || c.dv % 4 != 0 || c.lv % c.lk != 0) {
      throw std::runtime_error("DeltaNet 维度不受支持");
    }
    m->cos = mx::array(config->cos, {c.context, c.rotary_dim}, mx::float32);
    m->sin = mx::array(config->sin, {c.context, c.rotary_dim}, mx::float32);
    for (size_t i = 0; i < count; ++i) {
      const auto& t = tensors[i];
      mx::Shape shape(t.shape, t.shape + t.ndim);
      auto dtype = t.dtype == MWX_F16 ? mx::float16 : mx::float32;
      // Borrowed: the host keeps the mapped file alive until mwx_free.
      mx::array a(const_cast<void*>(t.data), shape, dtype, [](void*) {});
      m->weights.emplace(t.name, a);
    }
    m->delta_kernel = mx::fast::metal_kernel(
        "mework_gated_delta",
        {"q", "k", "v", "g", "beta", "state_in", "steps"},
        {"y", "state_out"},
        GATED_DELTA_SOURCE);
    m->lens.assign(c.slots, 0);
    m->live.assign(c.slots, false);
    model = m.release();
  });
  return status == 0 ? model : nullptr;
}

void mwx_free(mwx_model* model) {
  delete model;
  mx::clear_cache();
}

int mwx_prefix(mwx_model* model, const uint32_t* tokens, size_t n, uint8_t** out, size_t* out_len) {
  return guard([&] {
    auto& c = model->c;
    auto s = model->empty_states(1);
    std::vector<mx::array> keys(s.k.size(), mx::array(0.0f)), values(s.v.size(), mx::array(0.0f));
    std::vector<uint32_t> ids(tokens, tokens + n);
    model->run_sequence(ids, 0, s.conv, s.ssm, keys, values);
    std::vector<mx::array> all;
    all.insert(all.end(), s.conv.begin(), s.conv.end());
    all.insert(all.end(), s.ssm.begin(), s.ssm.end());
    all.insert(all.end(), keys.begin(), keys.end());
    all.insert(all.end(), values.begin(), values.end());
    mx::eval(all);

    std::vector<uint8_t> bytes;
    uint32_t header[3] = {PREFIX_MAGIC, static_cast<uint32_t>(n), static_cast<uint32_t>(c.layers)};
    put(bytes, header, 3);
    int li = 0, ai = 0;
    for (int layer = 0; layer < c.layers; ++layer) {
      if (c.full[layer]) {
        put_array<mx::float16_t>(bytes, keys[ai]);
        put_array<mx::float16_t>(bytes, values[ai]);
        ++ai;
      } else {
        put_array<float>(bytes, s.conv[li]);
        put_array<float>(bytes, s.ssm[li]);
        ++li;
      }
    }
    *out = static_cast<uint8_t*>(malloc(bytes.size()));
    std::memcpy(*out, bytes.data(), bytes.size());
    *out_len = bytes.size();
  });
}

void mwx_free_bytes(uint8_t* bytes) {
  free(bytes);
}

int mwx_admit(
    mwx_model* model,
    int32_t slot,
    const uint8_t* prefix,
    size_t prefix_len,
    int32_t prefix_tokens,
    const uint32_t* tokens,
    size_t n,
    float* logits_out) {
  return guard([&] {
    auto& c = model->c;
    if (slot < 0 || slot >= c.slots || n == 0) {
      throw std::runtime_error("无效的槽位或空请求");
    }
    Reader r{prefix, prefix_len};
    auto header = r.take<uint32_t>(3);
    if (header[0] != PREFIX_MAGIC || static_cast<int32_t>(header[1]) != prefix_tokens ||
        static_cast<int>(header[2]) != c.layers) {
      throw std::runtime_error("前置状态与模型不符");
    }
    int P = prefix_tokens;
    std::vector<mx::array> conv, ssm, keys, values;
    for (int layer = 0; layer < c.layers; ++layer) {
      if (c.full[layer]) {
        size_t count = static_cast<size_t>(c.kv_heads) * P * c.head_dim;
        keys.emplace_back(r.take<mx::float16_t>(count), mx::Shape{1, c.kv_heads, P, c.head_dim}, mx::float16);
        values.emplace_back(r.take<mx::float16_t>(count), mx::Shape{1, c.kv_heads, P, c.head_dim}, mx::float16);
      } else {
        size_t conv_count = static_cast<size_t>(c.kernel - 1) * c.conv_dim();
        size_t ssm_count = static_cast<size_t>(c.lv) * c.dv * c.dk;
        conv.emplace_back(r.take<float>(conv_count), mx::Shape{1, c.kernel - 1, c.conv_dim()}, mx::float32);
        ssm.emplace_back(r.take<float>(ssm_count), mx::Shape{1, c.lv, c.dv, c.dk}, mx::float32);
      }
    }
    std::vector<uint32_t> ids(tokens, tokens + n);
    auto h = model->run_sequence(ids, P, conv, ssm, keys, values);
    auto logits = model->logits(h);

    model->ensure_slots();
    auto& s = *model->slots;
    int len = P + static_cast<int>(n);
    for (size_t i = 0; i < conv.size(); ++i) {
      s.conv[i] = mx::slice_update(s.conv[i], conv[i], {slot, 0, 0}, {slot + 1, c.kernel - 1, c.conv_dim()});
      s.ssm[i] = mx::slice_update(s.ssm[i], ssm[i], {slot, 0, 0, 0}, {slot + 1, c.lv, c.dv, c.dk});
    }
    for (size_t i = 0; i < keys.size(); ++i) {
      s.k[i] = mx::slice_update(s.k[i], keys[i], {slot, 0, 0, 0}, {slot + 1, c.kv_heads, len, c.head_dim});
      s.v[i] = mx::slice_update(s.v[i], values[i], {slot, 0, 0, 0}, {slot + 1, c.kv_heads, len, c.head_dim});
    }
    std::vector<mx::array> all{logits};
    all.insert(all.end(), s.conv.begin(), s.conv.end());
    all.insert(all.end(), s.ssm.begin(), s.ssm.end());
    all.insert(all.end(), s.k.begin(), s.k.end());
    all.insert(all.end(), s.v.begin(), s.v.end());
    mx::eval(all);
    std::memcpy(logits_out, logits.data<float>(), sizeof(float) * c.vocab);
    model->lens[slot] = len;
    model->live[slot] = true;
  });
}

int mwx_step(mwx_model* model, const int32_t* slots, const uint32_t* tokens, size_t n, float* logits_out) {
  return guard([&] {
    auto& c = model->c;
    if (!model->slots) {
      throw std::runtime_error("没有进行中的序列");
    }
    auto& s = *model->slots;
    int S = c.slots;
    std::vector<uint32_t> row_tokens(S, 0);
    std::vector<int32_t> row_pos(S, 0);
    std::vector<bool> listed(S, false);
    int longest = 1;
    for (size_t i = 0; i < n; ++i) {
      int slot = slots[i];
      if (slot < 0 || slot >= S || !model->live[slot] || listed[slot]) {
        throw std::runtime_error("批次里有无效的槽位");
      }
      if (model->lens[slot] >= c.context) {
        throw std::runtime_error("超出上下文长度");
      }
      listed[slot] = true;
      row_tokens[slot] = tokens[i];
      row_pos[slot] = model->lens[slot];
      longest = std::max(longest, model->lens[slot] + 1);
    }
    // Every row runs (their cost is the same as one row's: the weights are
    // read once). Rows of free slots compute garbage nobody reads; live rows
    // left out of this step must keep their state.
    bool partial = false;
    for (int slot = 0; slot < S; ++slot) {
      partial = partial || (model->live[slot] && !listed[slot]);
    }
    std::vector<uint8_t> keep_bytes(S);
    std::vector<uint8_t> attend(static_cast<size_t>(S) * longest, 0);
    for (int slot = 0; slot < S; ++slot) {
      keep_bytes[slot] = listed[slot] ? 1 : 0;
      int until = listed[slot] ? row_pos[slot] : 0;
      for (int j = 0; j <= until && j < longest; ++j) {
        attend[static_cast<size_t>(slot) * longest + j] = 1;
      }
    }
    auto commit = mx::array(reinterpret_cast<const bool*>(keep_bytes.data()), {S, 1, 1}, mx::bool_);
    auto mask = mx::array(reinterpret_cast<const bool*>(attend.data()), {S, 1, 1, longest}, mx::bool_);
    auto x = model->embed(row_tokens, S, 1);
    auto pos = mx::array(row_pos.data(), {S, 1}, mx::int32);

    int li = 0, ai = 0;
    for (int layer = 0; layer < c.layers; ++layer) {
      mx::array mixed = mx::array(0.0f);
      if (c.full[layer]) {
        auto in = model->attention_inputs(layer, x, pos);
        for (int slot = 0; slot < S; ++slot) {
          if (!listed[slot]) continue;
          int p = row_pos[slot];
          auto k_row = mx::slice(in.k, {slot, 0, 0, 0}, {slot + 1, c.kv_heads, 1, c.head_dim});
          auto v_row = mx::slice(in.v, {slot, 0, 0, 0}, {slot + 1, c.kv_heads, 1, c.head_dim});
          s.k[ai] = mx::slice_update(s.k[ai], k_row, {slot, 0, p, 0}, {slot + 1, c.kv_heads, p + 1, c.head_dim});
          s.v[ai] = mx::slice_update(s.v[ai], v_row, {slot, 0, p, 0}, {slot + 1, c.kv_heads, p + 1, c.head_dim});
        }
        auto keys = mx::slice(s.k[ai], {0, 0, 0, 0}, {S, c.kv_heads, longest, c.head_dim});
        auto values = mx::slice(s.v[ai], {0, 0, 0, 0}, {S, c.kv_heads, longest, c.head_dim});
        auto scale = 1.0f / std::sqrt(static_cast<float>(c.head_dim));
        auto o = mx::fast::scaled_dot_product_attention(in.q, keys, values, scale, "", mask);
        mixed = model->attention_output(layer, o, in.gate);
        ++ai;
      } else {
        auto conv = s.conv[li], ssm = s.ssm[li];
        mixed = model->delta_net(layer, x, conv, ssm);
        if (partial) {
          conv = mx::where(commit, conv, s.conv[li]);
          ssm = mx::where(mx::expand_dims(commit, 3), ssm, s.ssm[li]);
        }
        s.conv[li] = conv;
        s.ssm[li] = ssm;
        ++li;
      }
      x = x + mixed;
      x = x + model->mlp(layer, x);
    }
    std::vector<int32_t> order(slots, slots + n);
    auto rows = mx::take(mx::reshape(x, {S, c.hidden}), mx::array(order.data(), {static_cast<int>(n)}, mx::int32), 0);
    auto logits = model->logits(rows);
    std::vector<mx::array> all{logits};
    all.insert(all.end(), s.conv.begin(), s.conv.end());
    all.insert(all.end(), s.ssm.begin(), s.ssm.end());
    all.insert(all.end(), s.k.begin(), s.k.end());
    all.insert(all.end(), s.v.begin(), s.v.end());
    mx::eval(all);
    std::memcpy(logits_out, logits.data<float>(), sizeof(float) * c.vocab * n);
    for (size_t i = 0; i < n; ++i) {
      model->lens[slots[i]] += 1;
    }
  });
}

void mwx_release(mwx_model* model, int32_t slot) {
  if (slot >= 0 && slot < model->c.slots) {
    model->live[slot] = false;
    model->lens[slot] = 0;
  }
}

void mwx_trim(mwx_model* model) {
  bool any_live = std::find(model->live.begin(), model->live.end(), true) != model->live.end();
  if (!any_live) {
    model->slots.reset();
    std::fill(model->lens.begin(), model->lens.end(), 0);
  }
  mx::clear_cache();
}

} // extern "C"
