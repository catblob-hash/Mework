//! Built-in model catalog that enriches raw upstream `GET /models` IDs with
//! [`ModelProfile`] capability, context-window, maximum-output, and vendor fields.
//!
//! The catalog is vendored from Cherry Studio's MIT-licensed
//! `@cherrystudio/provider-registry`. Its source, hash, and update procedure are in
//! `resources/model-registry/README.md`.
//!
//! Resolution first uses canonical `modelId` entries, then retries normalized IDs.
//! Size-preserving keys take precedence so `gpt-oss-20b` and `gpt-oss-120b` remain distinct.
//!
//! This read-only layer only enriches discovered models; it never removes models or
//! changes user curation.

use std::{
    collections::{BTreeSet, HashMap},
    sync::OnceLock,
};

use regex::Regex;
use serde::Deserialize;

use crate::model::ModelCapability;

const MODELS_JSON: &str = include_str!("../resources/model-registry/models.json");

// ───────────────────────────── ID normalization ─────────────────────────────
//
// Ported from `packages/provider-registry/src/utils/normalize.ts`.

/// Aggregator and relay routing prefixes. `zai-org-` must precede `zai-` so
/// `zai-org-glm-5` is not stripped to `org-glm-5`. Stop at the first match.
const COMMON_AGGREGATOR_PREFIXES: &[&str] = &[
    // AIHubMix
    "aihubmix-",
    "aihub-",
    "ahm-",
    // Cloud provider routes
    "alicloud-",
    "azure-",
    "baidu-",
    "cbs-",
    "cc-",
    "sf-",
    "s-",
    "bai-",
    // `mm-` expands to the MiniMax abbreviation in `PREFIX_EXPANSIONS`; stripping it
    // as an aggregator prefix would leave the unresolvable ID `m2-1`.
    "web-",
    // Platform aggregators
    "deepinfra-",
    "groq-",
    "nvidia-",
    "sophnet-",
    // Legacy prefixes
    "zai-org-", // Must precede zai-
    "zai-",
    "lucidquery-",
    "lucidnova-",
    "lucid-",
    "siliconflow-",
    "chutes-",
    "huoshan-",
    "meta-",
    "cohere-",
    "coding-",
    "dmxapi-",
    "perplexity-",
    "ai21-",
    "openai-",
    // Underscore prefixes
    "dmxapi_",
    "aistudio_",
];

/// Abbreviation expansion. `mm-m2-1` becomes `minimax-m2-1`.
const PREFIX_EXPANSIONS: &[(&str, &str)] = &[("mm-", "minimax-")];

const COLON_VARIANT_SUFFIXES: &[&str] = &[
    ":free",
    ":nitro",
    ":extended",
    ":beta",
    ":preview",
    ":thinking",
    ":exacto",
    ":latest",
    ":cloud",
];

/// Do not add `-medium`: it is a real tier name in `mistral-medium` and
/// `devstral-medium`, not a reasoning-tier variant.
const HYPHEN_VARIANT_SUFFIXES: &[&str] = &[
    "-free",
    "-search",
    "-online",
    "-think",
    "-reasoning",
    "-classic",
    "-low",
    "-high",
    "-minimal",
    "-nothink",
    "-no-think",
    "-ssvip",
    "-thinking",
    "-nothinking",
    "-aliyun",
    "-huoshan",
    "-tee",
    "-cc",
    "-fw",
    "-di",
    "-t",
    "-reverse",
];

const PAREN_VARIANT_SUFFIXES: &[&str] = &["(free)", "(beta)", "(preview)", "(thinking)"];

/// Quantization suffixes identify precision variants of the same model.
const QUANTIZATION_SUFFIXES: &[&str] = &[
    "-fp8", "-fp16", "-bf16", "-awq", "-int4", "-int8", "-gguf", "-gptq",
];

/// Protect suffixes only when the preceding segment is a complete word, such as
/// `...-no-think`; substring matches such as `volcano-free` still strip.
const PROTECTED_COMPOUND_PREFIXES: &[&str] = &["non", "no", "pre", "anti", "post"];

const BEDROCK_VENDOR: &str =
    "anthropic|amazon|meta|google|mistralai|cohere|openai|ai21|microsoft|nvidia";

fn regex_of(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("内置的模型 id 正则必须能编译"))
}

/// Trailing release-date snapshots identify the same model line. Valid month and day
/// ranges prevent parameter sizes and version numbers from matching.
fn date_snapshot_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex_of(
        &CELL,
        r"(?:-20\d{2}-(?:0[1-9]|1[0-2])-(?:[0-2]\d|3[01])$)|(?:-20\d{2}(?:0[1-9]|1[0-2])(?:[0-2]\d|3[01])$)|(?:-2\d(?:0[1-9]|1[0-2])(?:[0-2]\d|3[01])$)|(?:-(?:0[1-9]|1[0-2])(?:[0-2]\d|3[01])$)|(?:-2\d(?:0[1-9]|1[0-2])$)",
    )
}

/// Bedrock cross-vendor ARNs have a dotted region/vendor prefix and an `…-v1:0`
/// revision. The final dotted segment must be a known Bedrock vendor so native dotted
/// IDs such as `flux.2-pro` and `qwen3.7` are preserved.
fn bedrock_dotted_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex_of(
        &CELL,
        &format!(
            r"^(?:[a-z]+\.)*(?:{BEDROCK_VENDOR}|deepseek|minimax|mistral|moonshot|moonshotai|qwen|writer|xai|zai)\."
        ),
    )
}

fn bedrock_dash_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex_of(&CELL, &format!(r"^(?:{BEDROCK_VENDOR})-{{1,2}}"))
}

/// The `v` is optional: `openai.gpt-oss-120b-1:0` needs its full revision removed,
/// while IDs without a colon such as `whisper-v3` must remain unchanged.
fn bedrock_revision_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex_of(&CELL, r"(?i)(?:[-_]v?\d+)?:\d+$")
}

/// Colon-delimited size or quantization tags. Word tags such as `:free` and Bedrock
/// revisions such as `:0` are excluded.
fn colon_variant_tag_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex_of(&CELL, r"(?i)^(?:\d+(?:[.x]\d+)*b(?:$|[-.])|q\d|iq\d|fp16|bf16|f16)")
}

/// Rust `regex` lacks lookahead, so the separator is captured and restored during
/// replacement.
fn parameter_size_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex_of(&CELL, r"(?i)-(\d+(?:\.\d+)?b)(-|$)")
}

fn strip_aggregator_prefixes(model_id: &str) -> String {
    for prefix in COMMON_AGGREGATOR_PREFIXES {
        if let Some(rest) = model_id.strip_prefix(prefix) {
            return rest.to_owned();
        }
    }
    model_id.to_owned()
}

pub fn strip_bedrock_dotted_vendor_prefix(model_id: &str) -> String {
    bedrock_dotted_regex().replacen(model_id, 1, "").into_owned()
}

fn strip_bedrock_vendor_prefix(model_id: &str) -> String {
    let dotted = strip_bedrock_dotted_vendor_prefix(model_id);
    bedrock_dash_regex().replacen(&dotted, 1, "").into_owned()
}

pub fn strip_bedrock_revision(model_id: &str) -> String {
    bedrock_revision_regex().replacen(model_id, 1, "").into_owned()
}

fn expand_known_prefixes(model_id: &str) -> String {
    for (abbrev, canonical) in PREFIX_EXPANSIONS {
        if let Some(rest) = model_id.strip_prefix(abbrev) {
            return format!("{canonical}{rest}");
        }
    }
    model_id.to_owned()
}

fn strip_variant_suffixes(model_id: &str) -> String {
    if let Some(colon_index) = model_id.rfind(':') {
        if colon_index > 0 {
            let suffix = &model_id[colon_index..];
            if COLON_VARIANT_SUFFIXES.contains(&suffix) {
                return model_id[..colon_index].to_owned();
            }
        }
    }

    for suffix in HYPHEN_VARIANT_SUFFIXES {
        if let Some(remaining) = model_id.strip_suffix(suffix) {
            if PROTECTED_COMPOUND_PREFIXES.iter().any(|protected| {
                remaining == *protected || remaining.ends_with(&format!("-{protected}"))
            }) {
                continue;
            }
            return remaining.to_owned();
        }
    }

    for suffix in PAREN_VARIANT_SUFFIXES {
        if let Some(remaining) = model_id.strip_suffix(suffix) {
            return remaining.strip_suffix(' ').unwrap_or(remaining).to_owned();
        }
    }

    model_id.to_owned()
}

/// Between digits, `,`, `.`, `p`, and `_` are version separators: 3.5 / 3,5 /
/// 3p5 / 3_5 become 3-5. Iterate by character because lookahead-based replacements
/// do not consume the following digit, allowing adjacent separators to match.
fn normalize_version_separators(model_id: &str) -> String {
    let chars = model_id.chars().collect::<Vec<_>>();
    let mut out = String::with_capacity(model_id.len());
    for (index, current) in chars.iter().copied().enumerate() {
        let separator = matches!(current, ',' | '.' | '_' | 'p');
        let previous_digit = index > 0 && chars[index - 1].is_ascii_digit();
        let next_digit = chars.get(index + 1).is_some_and(char::is_ascii_digit);
        if separator && previous_digit && next_digit {
            out.push('-');
        } else {
            out.push(current);
        }
    }
    out
}

fn strip_quantization(model_id: &str) -> String {
    for suffix in QUANTIZATION_SUFFIXES {
        if let Some(remaining) = model_id.strip_suffix(suffix) {
            return remaining.to_owned();
        }
    }
    model_id.to_owned()
}

pub fn strip_date_snapshot(model_id: &str) -> String {
    let without_tag = match model_id.find('@') {
        Some(index) => &model_id[..index],
        None => model_id,
    };
    date_snapshot_regex().replacen(without_tag, 1, "").into_owned()
}

/// Repeat variant, quantization, and date stripping to a fixed point. A trailing
/// date can conceal an inner variant suffix, so a single pass is not idempotent.
pub fn strip_variant_quant_date_suffixes(model_id: &str) -> String {
    let mut result = model_id.to_owned();
    loop {
        let next = strip_date_snapshot(&strip_quantization(&strip_variant_suffixes(&result)));
        if next == result {
            return result;
        }
        result = next;
    }
}

pub fn extract_parameter_size(model_id: &str) -> Option<String> {
    parameter_size_regex()
        .captures(model_id)
        .map(|captures| captures[1].to_ascii_lowercase())
}

/// Match the upstream non-global replacement: replace only the first occurrence.
fn strip_parameter_size(model_id: &str) -> String {
    parameter_size_regex()
        .replacen(model_id, 1, "${2}")
        .into_owned()
}

/// Normalize colon-delimited size and quantization tags to catalog hyphen syntax.
/// Only size or quantization-leading tags are converted; word tags and Bedrock
/// revisions remain unchanged so size siblings do not collapse to one family key.
pub fn colon_variant_tag_to_hyphen(model_id: &str) -> String {
    if let Some(colon_index) = model_id.rfind(':') {
        if colon_index > 0 && colon_variant_tag_regex().is_match(&model_id[colon_index + 1..]) {
            return format!(
                "{}-{}",
                &model_id[..colon_index],
                &model_id[colon_index + 1..]
            );
        }
    }
    model_id.to_owned()
}

/// Normalize a model ID to its canonical form.
///
/// `keep_parameter_size` preserves size in colon tags, keeping `qwen2.5:7b`,
/// `gpt-oss-20b`, and `gpt-oss-120b` distinct. Both key forms are indexed, with the
/// size-preserving form searched first.
pub fn normalize_model_id(model_id: &str, keep_parameter_size: bool) -> String {
    let base = model_id.rsplit('/').next().unwrap_or(model_id);
    let mut base_name = base.to_lowercase();
    base_name = strip_aggregator_prefixes(&base_name);
    base_name = strip_bedrock_vendor_prefix(&base_name);
    base_name = strip_bedrock_revision(&base_name);
    base_name = expand_known_prefixes(&base_name);
    if keep_parameter_size {
        base_name = colon_variant_tag_to_hyphen(&base_name);
    }
    // Parameter-size stripping can reveal a variant suffix and vice versa.
    loop {
        let stripped = strip_variant_quant_date_suffixes(&base_name);
        let next = if keep_parameter_size {
            stripped
        } else {
            strip_parameter_size(&stripped)
        };
        if next == base_name {
            break;
        }
        base_name = next;
    }
    base_name = normalize_version_separators(&base_name);
    // Treat underscores as interchangeable separators. Catalog base IDs use `-`.
    base_name.replace('_', "-")
}

// ───────────────────────────── Display names and groups ─────────────────────────────

/// Derive a model-list group from an API model ID.
///
/// Vendor-prefixed IDs use their prefix segment; flat IDs use their family prefix.
/// This must match the renderer fallback in `src/lib/modelCapabilities.ts::modelGroup`.
pub fn derive_model_group_name(model_id: &str) -> Option<String> {
    let normalized = model_id.trim();
    if normalized.contains('/') {
        let head = normalized.split('/').next().unwrap_or_default().trim();
        return (!head.is_empty()).then(|| head.to_owned());
    }
    let family = normalized.split('-').next().unwrap_or_default().trim();
    (!family.is_empty() && family != normalized).then(|| family.to_owned())
}

/// Terms that must remain uppercase when prettified.
const MODEL_NAME_ACRONYMS: &[(&str, &str)] = &[
    ("api", "API"),
    ("asr", "ASR"),
    ("glm", "GLM"),
    ("gpt", "GPT"),
    ("hd", "HD"),
    ("llm", "LLM"),
    ("mt", "MT"),
    ("ocr", "OCR"),
    ("tts", "TTS"),
    ("vl", "VL"),
];

fn title_case_id_token(token: &str) -> String {
    let lower = token.to_lowercase();
    if let Some((_, acronym)) = MODEL_NAME_ACRONYMS.iter().find(|(key, _)| *key == lower) {
        return (*acronym).to_owned();
    }
    let mut chars = token.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {
            first.to_ascii_uppercase().to_string() + chars.as_str()
        }
        _ => token.to_owned(),
    }
}

/// Return the separator-free portion of `id` following `stem`.
fn trailing_remainder(id: &str, stem: &str) -> String {
    id.get(stem.len()..)
        .unwrap_or_default()
        .trim_start_matches(['-', ':', '@', '.', '_'])
        .to_owned()
}

fn prettify_id_segment(segment: &str) -> String {
    let stem = strip_date_snapshot(segment);
    let date = trailing_remainder(segment, &stem);
    let pretty = stem
        .split('-')
        .filter(|token| !token.is_empty())
        .map(title_case_id_token)
        .collect::<Vec<_>>()
        .join(" ");
    if date.is_empty() {
        pretty
    } else {
        format!("{pretty} ({date})")
    }
}

/// Derive the display name for a model discovered through upstream `/models`.
///
/// Raw IDs are per-SKU identities. Exact catalog matches use the curated name;
/// normalized matches append the stripped suffix and render namespaces as prefixes;
/// unmatched IDs are prettified.
pub fn derive_resolved_model_name(
    raw_id: &str,
    curated_name: Option<&str>,
    canonical_api_id: Option<&str>,
) -> String {
    if let (Some(curated), Some(canonical)) = (curated_name, canonical_api_id) {
        if raw_id == canonical {
            return curated.to_owned();
        }
    }

    let slash_index = raw_id.rfind('/');
    let after_slash = match slash_index {
        Some(index) => &raw_id[index + 1..],
        None => raw_id,
    };
    // Restore a dotted vendor namespace removed during normalization.
    // Calculate lengths on a lowercase copy; non-ASCII case folding can change byte
    // length, so fall back to the full segment rather than slicing a code point.
    let tail_length = strip_bedrock_dotted_vendor_prefix(&after_slash.to_lowercase()).len();
    let tail = after_slash
        .len()
        .checked_sub(tail_length)
        .and_then(|start| after_slash.get(start..))
        .unwrap_or(after_slash);

    let name = match curated_name {
        Some(curated) => {
            let stem = strip_bedrock_revision(&strip_variant_quant_date_suffixes(tail));
            let suffix = trailing_remainder(tail, &stem);
            if suffix.is_empty() {
                curated.to_owned()
            } else {
                format!("{curated} ({suffix})")
            }
        }
        None => prettify_id_segment(tail),
    };

    let mut namespaces = Vec::new();
    if let Some(index) = slash_index {
        namespaces.extend(raw_id[..index].split('/').map(title_case_id_token));
    }
    if tail.len() < after_slash.len() {
        namespaces.push(after_slash[..after_slash.len() - tail.len() - 1].to_owned());
    }
    if namespaces.is_empty() {
        name
    } else {
        format!("{}: {name}", namespaces.join(": "))
    }
}

// ───────────────────────────── Catalog data ─────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistryModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    context_window: Option<u64>,
    #[serde(default)]
    max_output_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct ModelListFile {
    version: String,
    models: Vec<RegistryModel>,
}

/// Load once, index once, and retain for the process lifetime.
///
/// The catalog is a static `include_str!` payload, so retaining parsed 0.5 MB JSON
/// avoids reparsing it on subsequent lookups.
struct Registry {
    models: Vec<RegistryModel>,
    /// Catalog content hash used by the catalog invariant test to ensure the vendored
    /// snapshot matches `resources/model-registry/README.md`.
    #[cfg_attr(not(test), allow(dead_code))]
    models_version: String,

    model_by_id: HashMap<String, usize>,
    model_by_norm_id: HashMap<String, usize>,
    model_by_sized_norm: HashMap<String, usize>,
}

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::load)
}

impl Registry {
    fn load() -> Self {
        let model_file: ModelListFile = serde_json::from_str(MODELS_JSON)
            .expect("resources/model-registry/models.json 必须是合法的模型目录");

        let mut registry = Self {
            models: model_file.models,
            models_version: model_file.version,
            model_by_id: HashMap::new(),
            model_by_norm_id: HashMap::new(),
            model_by_sized_norm: HashMap::new(),
        };
        registry.build_model_index();
        registry
    }

    fn build_model_index(&mut self) {
        for (index, model) in self.models.iter().enumerate() {
            self.model_by_id.insert(model.id.clone(), index);
            self.model_by_norm_id
                .entry(normalize_model_id(&model.id, false))
                .or_insert(index);
            // Size-preserving keys keep `gpt-oss-20b` and `gpt-oss-120b` separate so
            // IDs with `:20b` and `:120b` tags resolve to their respective entries.
            self.model_by_sized_norm
                .entry(normalize_model_id(&model.id, true))
                .or_insert(index);
        }
    }

    fn find_model(&self, model_id: &str) -> Option<&RegistryModel> {
        if let Some(index) = self.model_by_id.get(model_id) {
            return Some(&self.models[*index]);
        }
        // Prioritize colon-delimited size tags. If the catalog lacks the matching size,
        // return None rather than borrowing capability or limit metadata from a sibling.
        if colon_variant_tag_to_hyphen(model_id) != model_id {
            return self
                .model_by_sized_norm
                .get(&normalize_model_id(model_id, true))
                .map(|index| &self.models[*index]);
        }
        let sized_model_id = normalize_model_id(model_id, true);
        if let Some(index) = self.model_by_sized_norm.get(&sized_model_id) {
            return Some(&self.models[*index]);
        }
        if extract_parameter_size(&sized_model_id).is_some() {
            return None;
        }
        self.model_by_norm_id
            .get(&normalize_model_id(model_id, false))
            .map(|index| &self.models[*index])
    }

}

// ───────────────────────────── Vocabulary mapping ─────────────────────────────

/// Map the catalog's capabilities to those Mework can store.
fn capability_from_slug(slug: &str) -> Option<ModelCapability> {
    Some(match slug {
        "image-recognition" => ModelCapability::ImageRecognition,
        _ => return None,
    })
}

// ───────────────────────────── Public interface ─────────────────────────────

/// Facts from the catalog that Mework can store. `None` or an empty set means the
/// catalog does not provide the fact, allowing callers to fall back to ID inference.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegistryResolution {
    /// Display name decorated by [`derive_resolved_model_name`].
    pub name: String,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub capabilities: BTreeSet<ModelCapability>,
}

/// Resolve an upstream model ID in the catalog.
///
/// `None` means the catalog does not recognize the ID and callers should infer from
/// response fields and the ID.
pub fn resolve(api_model_id: &str) -> Option<RegistryResolution> {
    let model = registry().find_model(api_model_id)?;

    let mut capabilities = model
        .capabilities
        .iter()
        .filter_map(|slug| capability_from_slug(slug))
        .collect::<BTreeSet<_>>();

    // Derive capabilities from modalities when a catalog row omits them.
    let has = |list: &[String], modality: &str| list.iter().any(|value| value == modality);
    if has(&model.input_modalities, "image") {
        capabilities.insert(ModelCapability::ImageRecognition);
    }

    // A matched catalog row always has a curated name (falling back to its ID), so
    // normalized matches use the curated-name decoration path.
    let curated_name = model
        .name
        .as_deref()
        .or(Some(model.id.as_str()))
        .filter(|value| !value.trim().is_empty());
    let name = derive_resolved_model_name(api_model_id, curated_name, Some(model.id.as_str()));

    Some(RegistryResolution {
        name,
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        capabilities,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ID normalization ──
    //
    // Cases cover fixed-point behavior, ordering dependencies, and entries that
    // must not be stripped.

    #[test]
    fn normalize_model_id_folds_the_spellings_one_model_arrives_under() {
        let cases: &[(&str, &str)] = &[
            // Only the final vendor-prefixed segment is retained.
            ("anthropic/claude-sonnet-4-5", "claude-sonnet-4-5"),
            // Accepted trailing release-date forms.
            ("claude-sonnet-4-5-20250929", "claude-sonnet-4-5"),
            ("gpt-4o-2024-08-06", "gpt-4o"),
            ("kimi-k2-250905", "kimi-k2"),
            ("qwen-plus-1201", "qwen-plus"),
            // Every digit-delimited version separator normalizes to `-`.
            ("qwen3.5", "qwen3-5"),
            ("claude-3.5.7-sonnet", "claude-3-5-7-sonnet"),
            ("accounts/fireworks/models/llama-v3p1-8b", "llama-v3-1"),
            // Aggregator prefixes.
            ("aihubmix-gpt-4o", "gpt-4o"),
            ("dmxapi_gpt-4o", "gpt-4o"),
            // `zai-org-` must match before `zai-`.
            ("zai-org-glm-5", "glm-5"),
            // Bedrock cross-vendor ARN prefixes and revisions.
            ("us.anthropic.claude-sonnet-4-5-v1:0", "claude-sonnet-4-5"),
            ("meta-llama-4-scout", "llama-4-scout"),
            ("openai.gpt-oss-120b-1:0", "gpt-oss"),
            // Variant suffixes.
            ("deepseek-v3-2:free", "deepseek-v3-2"),
            ("glm-4-5-fp8", "glm-4-5"),
            ("qwen3-max-thinking", "qwen3-max"),
            // A date can conceal an inner variant suffix until the next pass.
            ("qwen3-30b-thinking-2507", "qwen3"),
            // Underscores normalize to hyphens.
            ("bce-embedding-base_v1", "bce-embedding-base-v1"),
        ];
        for (raw, expected) in cases {
            assert_eq!(&normalize_model_id(raw, false), expected, "输入 {raw}");
        }
    }

    #[test]
    fn normalize_model_id_leaves_the_entries_that_look_like_variants_but_are_not() {
        // `-medium` is a real tier name, not a reasoning variant.
        assert_eq!(normalize_model_id("mistral-medium", false), "mistral-medium");
        assert_eq!(
            normalize_model_id("devstral-medium", false),
            "devstral-medium"
        );
        // A complete preceding `no` token protects the compound suffix.
        assert_eq!(normalize_model_id("glm-4-5-no-think", false), "glm-4-5");
        // `pre` is a protected token, but `-pre-think` is not a recognized suffix.
        assert_eq!(normalize_model_id("pre-think", false), "pre-think");
        // A suffix still strips when its prefix is merely part of the final token.
        assert_eq!(normalize_model_id("volcano-free", false), "volcano");
        // Parameter sizes are not dates.
        assert_eq!(normalize_model_id("glm-4-9b", true), "glm-4-9b");
        assert_eq!(normalize_model_id("qwen3-235b", true), "qwen3-235b");
        // `-v3` without a colon is not a Bedrock revision.
        assert_eq!(normalize_model_id("whisper-v3", false), "whisper-v3");
        // Native dotted IDs are not Bedrock vendor prefixes; dots require digits on
        // both sides to act as version separators.
        assert_eq!(normalize_model_id("flux.2-pro", false), "flux.2-pro");
        assert_eq!(normalize_model_id("qwen3.7", false), "qwen3-7");
    }

    #[test]
    fn the_size_preserving_key_keeps_siblings_apart() {
        // Size-free keys collapse siblings, requiring a second index.
        assert_eq!(normalize_model_id("gpt-oss-20b", false), "gpt-oss");
        assert_eq!(normalize_model_id("gpt-oss-120b", false), "gpt-oss");
        assert_eq!(normalize_model_id("gpt-oss-20b", true), "gpt-oss-20b");
        assert_eq!(normalize_model_id("gpt-oss-120b", true), "gpt-oss-120b");
        // Colon tags normalize to hyphens to preserve the size.
        assert_eq!(normalize_model_id("gpt-oss:20b", true), "gpt-oss-20b");
        assert_eq!(colon_variant_tag_to_hyphen("qwen2.5:7b"), "qwen2.5-7b");
        // Word tags and Bedrock revisions are not size tags.
        assert_eq!(colon_variant_tag_to_hyphen("deepseek-v3:free"), "deepseek-v3:free");
        assert_eq!(colon_variant_tag_to_hyphen("claude-v1:0"), "claude-v1:0");
    }

    #[test]
    fn parameter_size_is_extracted_and_stripped_once() {
        assert_eq!(extract_parameter_size("qwen3-30b-a3b").as_deref(), Some("30b"));
        assert_eq!(extract_parameter_size("gpt-4o"), None);
        // Match upstream's non-global replacement.
        assert_eq!(strip_parameter_size("qwen3-30b-a3b"), "qwen3-a3b");
        assert_eq!(strip_parameter_size("gpt-oss-20b"), "gpt-oss");
    }

    // ── Groups and display names ──

    #[test]
    fn model_group_comes_from_the_vendor_segment_or_the_family_prefix() {
        assert_eq!(derive_model_group_name("openai/gpt-4o").as_deref(), Some("openai"));
        assert_eq!(derive_model_group_name("Qwen/Qwen3-8B").as_deref(), Some("Qwen"));
        assert_eq!(derive_model_group_name("deepseek-v4-pro").as_deref(), Some("deepseek"));
        // An ID with no separate leading segment has no group.
        assert_eq!(derive_model_group_name("grok"), None);
        assert_eq!(derive_model_group_name(""), None);
    }

    #[test]
    fn a_display_name_stays_distinguishable_between_siblings() {
        // Exact API model ID match uses the curated name unchanged.
        assert_eq!(
            derive_resolved_model_name("gpt-4o", Some("GPT-4o"), Some("gpt-4o")),
            "GPT-4o"
        );
        // Normalized matches append the stripped suffix in parentheses.
        assert_eq!(
            derive_resolved_model_name(
                "claude-sonnet-4-5-20250929",
                Some("Claude Sonnet 4.5"),
                Some("claude-sonnet-4-5"),
            ),
            "Claude Sonnet 4.5 (20250929)"
        );
        // Slash namespaces render as prefixes.
        assert_eq!(
            derive_resolved_model_name("openai/gpt-4o", Some("GPT-4o"), Some("gpt-4o")),
            "Openai: GPT-4o"
        );
        // Unmatched IDs are prettified while preserving uppercase acronyms.
        assert_eq!(derive_resolved_model_name("gpt-4o-mini", None, None), "GPT 4o Mini");
        assert_eq!(derive_resolved_model_name("glm-4-6", None, None), "GLM 4 6");
    }

    // ── Vocabulary mapping ──

    #[test]
    fn only_the_capabilities_and_endpoints_mework_can_hold_survive_the_mapping() {
        assert_eq!(
            capability_from_slug("image-recognition"),
            Some(ModelCapability::ImageRecognition)
        );
        // Unsupported capabilities are excluded. The retired slugs sit alongside
        // the never-supported ones: a catalog row that still carries them must
        // not resurrect a variant Mework no longer holds.
        for slug in [
            "function-call",
            "reasoning",
            "image-generation",
            "audio-generation",
            "audio-transcript",
            "embedding",
            "rerank",
            "video-recognition",
            "video-generation",
            "structured-output",
            "file-input",
            "code-execution",
            "file-search",
            "computer-use",
            "audio-recognition",
        ] {
            assert_eq!(capability_from_slug(slug), None, "{slug} 不该被映射");
        }
    }

    // ── Catalog data ──

    #[test]
    fn the_vendored_catalog_is_the_snapshot_its_readme_records() {
        let registry = registry();
        // The README records the expected row count and content hash.
        assert_eq!(registry.models.len(), 911);
        assert_eq!(registry.models_version, "90f2173cfa6525a2");
    }

    #[test]
    fn every_catalog_row_is_reachable_through_its_own_exact_id() {
        // Verify each row survives exact-ID indexing; collision errors may affect only
        // a few entries.
        let registry = registry();
        for model in &registry.models {
            let found = registry.find_model(&model.id);
            assert_eq!(
                found.map(|entry| entry.id.as_str()),
                Some(model.id.as_str()),
                "目录行 {} 查不回自己",
                model.id
            );
        }
    }

    #[test]
    fn a_known_model_gets_its_capabilities_and_limits_from_the_catalog() {
        let resolved = resolve("gpt-4o").expect("gpt-4o 必须在目录里");
        assert_eq!(resolved.name, "GPT-4o");
        assert_eq!(resolved.context_window, Some(128_000));
        assert_eq!(resolved.max_output_tokens, Some(16_384));
        assert_eq!(
            resolved.capabilities,
            BTreeSet::from([ModelCapability::ImageRecognition]),
            "function-call / structured-output / file-input 落选，image 模态补出视觉"
        );
    }

    #[test]
    fn a_dated_snapshot_resolves_to_its_model_line_and_says_so_in_the_name() {
        let resolved = resolve("claude-sonnet-4-5-20250929")
            .expect("带日期的 Claude id 必须归一化到目录行");
        assert_eq!(resolved.name, "Claude Sonnet 4.5 (20250929)");
        assert_eq!(resolved.context_window, Some(1_000_000));
        assert!(resolved
            .capabilities
            .contains(&ModelCapability::ImageRecognition));
    }

    #[test]
    fn size_siblings_do_not_borrow_each_others_limits() {
        let small = resolve("gpt-oss-20b").expect("gpt-oss-20b");
        let large = resolve("gpt-oss-120b").expect("gpt-oss-120b");
        assert_eq!(small.max_output_tokens, Some(32_768));
        assert_eq!(large.max_output_tokens, Some(131_072));
    }

    /// A text-only catalog row carries no storable capability at all: the
    /// retired `embedding` slug must not survive as some other variant.
    #[test]
    fn a_text_only_catalog_row_resolves_to_no_capabilities() {
        let resolved = resolve("text-embedding-3-small").expect("目录行");
        assert_eq!(resolved.capabilities, BTreeSet::new());
    }

    #[test]
    fn an_unknown_id_is_not_invented() {
        assert_eq!(resolve("no-such-model-anywhere-2099"), None);
    }

    /// Vendor-specific models absent from the canonical catalog must resolve to
    /// `None`; a canonical entry verifies that lookup remains functional.
    #[test]
    fn a_row_that_only_lived_in_the_provider_override_table_is_gone() {
        assert_eq!(resolve("black-forest-labs/flux.2-flex"), None);
        assert!(resolve("claude-sonnet-4-5").is_some());
    }

    #[test]
    fn a_relayed_id_still_reaches_the_canonical_catalog() {
        // Relayed vendor IDs must still resolve through the canonical catalog.
        let resolved = resolve("claude-sonnet-4-5").expect("规范目录行");
        assert_eq!(resolved.name, "Claude Sonnet 4.5");
        assert_eq!(resolved.context_window, Some(1_000_000));
    }
}
