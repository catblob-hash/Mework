//! The host-authored addendum every child agent's system prompt carries.
//!
//! # Why V1 is frozen
//!
//! V1's exact bytes are inside every conversation fork's persisted
//! `system_prompt_snapshot`. That snapshot is signed by
//! `MemoryStore::sign_fork_system_prompt_receipt` and, through the frozen
//! `ForkModelBindingV1` projection, is also one of the fields inside
//! `canonical_subagent_execution_mode_payload`. Neither receipt has an update
//! path, so editing V1 would not merely invalidate stored forks: for the
//! execution-mode receipt it would permanently burn the
//! `(conversation_id, name)` pair it was reserved under. V1 therefore never
//! changes for any reason — a new shape lands as a new variant.
//!
//! `PromptVersion::V1` deliberately carries no language. V1 renders Chinese
//! unconditionally, and making its bytes a function of anything would silently
//! change frozen output. Because the variant has no fields, that is enforced by
//! the type rather than by a comment. V2 is the first shape whose text is
//! selected: it is `subagent.addendum` of the run's prompt profile
//! (`prompt_profile::PromptKey::SubagentAddendum`), so the built-in English
//! profile, the built-in Chinese one and a user-authored file each render
//! exactly one variant — never two at once, which would double the tokens on
//! every child turn and double the bytes inside a signed fork snapshot.
//!
//! # The version selects only the addendum
//!
//! `render` never rewrites, summarises, truncates or inspects `base`. Every
//! version applies the same `str::trim` to the caller's prompt (Unicode
//! `White_Space`, so U+3000 and U+00A0 count) and the same
//! `\n\n---\n\n` section separator that `lib::append_system_prompt_section`
//! uses crate-wide. A `base` that already contains that separator, or that
//! quotes a memory delimiter literally, is passed through untouched: presence of
//! host-owned context is never inferred from arbitrary system-prompt text.
//!
//! # Deliberate product limitation
//!
//! `api::restore_conversation_fork_binding` overwrites the freshly rendered
//! template prompt with the persisted `system_prompt_snapshot`, which remains
//! authoritative for the rest of that fork's life. An existing conversation fork
//! therefore does not receive V2's instruction-source-boundary paragraph; the
//! product does not re-render or re-sign a fork on resume.
//!
//! Named and ordinary children are the opposite case:
//! `api::build_rehydrated_agent_template` re-renders them from the definition or
//! from the parent prompt on every resume, and the rendered text appears in
//! neither receipt payload (the definition receipt's `systemPrompt` is the raw
//! document prompt, and `AgentDefinitionBindingV1` has no prompt field at all),
//! so they pick a version bump up retroactively with no receipt impact.

use crate::prompt_profile::{PromptKey, PromptProfile};

/// Which subagent-prompt shape to render.
///
/// V1 has no fields on purpose: see the module doc. New shapes are added as new
/// variants; existing ones are never edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PromptVersion {
    /// Frozen forever. Production never selects V1 — every fresh child renders
    /// the current shape — but the golden test does, and so would any future
    /// migration that has to reproduce a historical render byte-for-byte.
    #[cfg_attr(not(test), allow(dead_code))]
    V1,
    /// The instruction-source boundary shape, with its text taken from the
    /// run's prompt profile (`subagent.addendum`).
    V2,
}

impl PromptVersion {
    /// The shape every freshly spawned child renders.
    ///
    /// Changing what this returns is not a prompt-only edit. For a conversation
    /// fork, `api::agent_child_template` renders it into the child's
    /// `system_prompt`, `api::inherit_parent_model_memory_for_fork` snapshots
    /// that verbatim as `system_prompt_snapshot` and signs
    /// `system_prompt_receipt` over it, and BOTH are fields of the frozen
    /// `model::ForkModelBindingV1` projection — so this value reaches
    /// `model::canonical_subagent_execution_mode_payload` and the durable
    /// reservation keyed by `(conversation_id, name)`. Editing V2's own bytes
    /// has the same reach. Named and ordinary children are unaffected: their
    /// rendered prompt appears in no receipt payload (see the module doc).
    /// `tests::the_current_prompt_version_is_pinned_by_the_fork_reservation`
    /// makes a change here fail loudly and states the consequence.
    pub(crate) fn current() -> Self {
        Self::V2
    }
}

/// FROZEN. Do not edit a single byte; see the module doc. Kept in the original
/// backslash-continued form it was moved out of `api::subagent_system_prompt`
/// in, so the move is provably byte-preserving. The continuation lines start at
/// column 0 because Rust's `\`-newline escape also eats the next line's leading
/// whitespace.
const ADDENDUM_V1_ZH: &str =
    "你是主代理派生的子代理。专注完成给定任务；除任务描述（以及派生时可能附带的对话历史副本）外，\
你看不到主对话的其他内容。主代理可能随时发来新的用户消息补充指令。\
可以使用更新工具向主代理报告重要进展；仍需用最后一条回复输出完整结论。";

fn addendum(version: PromptVersion, profile: &PromptProfile) -> String {
    match version {
        PromptVersion::V1 => ADDENDUM_V1_ZH.to_owned(),
        PromptVersion::V2 => profile.text(PromptKey::SubagentAddendum).to_owned(),
    }
}

/// Appends the host-authored child addendum to `base`.
///
/// Infallible on purpose: unlike a signed payload, a prompt has no persisted
/// version that could name a shape this build cannot produce, so there is no
/// error to report. `base` is the caller's own prompt — a named definition's
/// document prompt or the parent conversation's assembled prompt — and is only
/// trimmed, never rewritten.
pub(crate) fn render(version: PromptVersion, base: &str, profile: &PromptProfile) -> String {
    let addendum = addendum(version, profile);
    // Model-owned and project memory now live only in host-owned ephemeral
    // contexts. Never infer their presence from arbitrary system-prompt text:
    // a user is allowed to quote either delimiter literally.
    let base = base.trim();
    if base.is_empty() {
        addendum
    } else {
        format!("{base}\n\n---\n\n{addendum}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frozen V1 addendum, written here as ONE unbroken literal so the test
    /// is independent of how the production constant is line-wrapped. This is a
    /// golden, not a re-derivation: never regenerate it from `render`.
    const V1_GOLDEN: &str = "你是主代理派生的子代理。专注完成给定任务；除任务描述（以及派生时可能附带的对话历史副本）外，你看不到主对话的其他内容。主代理可能随时发来新的用户消息补充指令。可以使用更新工具向主代理报告重要进展；仍需用最后一条回复输出完整结论。";

    const SEPARATOR: &str = "\n\n---\n\n";

    /// Tripwire, not a property — the sibling of
    /// `api::tests::receipt_payload_dispatchers_support_exactly_their_issued_versions`.
    ///
    /// It lives here rather than folded into that test on purpose: a prompt
    /// version is not a receipt-issuance version (nothing dispatches a payload
    /// on it), its blast radius is fork-only, and this module is where someone
    /// who wants V3 will actually be editing. The two doc comments cross-refer
    /// so neither can be found without the other.
    ///
    /// The pin protects fork-wide receipt re-issuance. `run_agent_spawn` reads
    /// `MemoryStore::reserved_subagent_execution_mode_names` into its dedupe gate,
    /// so an orphaned row cannot be re-selected by `auto_name`, and an explicit
    /// request for one receives the ordinary "pick another name" message.
    /// Changing this value still requires a deliberate decision: an interrupted
    /// fork can retain a reservation under the previous bytes.
    #[test]
    fn the_current_prompt_version_is_pinned_by_the_fork_reservation() {
        const WHY: &str = "\
PromptVersion::current no longer selects V2. That may be right, but it is not a prompt-only edit.\n\
api::agent_child_template renders this version into the child's system_prompt; for a conversation \
fork, api::inherit_parent_model_memory_for_fork snapshots it verbatim as system_prompt_snapshot and \
signs system_prompt_receipt over it, and BOTH are fields of the frozen model::ForkModelBindingV1 \
projection. So this value moves canonical_subagent_execution_mode_payload's bytes, and \
memory::MemoryStore::reserve_subagent_execution_mode_receipt has insert / identical-no-op / \
hard-error branches only — no UPDATE, no production DELETE. Each selected version therefore \
reserves forks under its exact bytes and differs from existing reservations under other bytes.\n\
Editing V2's own bytes has the same effect as changing the selected variant. Named and ordinary \
children are NOT affected — their prompt is in no receipt payload.\n\
api::run_agent_spawn reads memory::MemoryStore::reserved_subagent_execution_mode_names into its \
dedupe gate, so a name whose reservation row outlived its persisted record remains visible: auto_name \
skips it and an explicit request for it gets the ordinary rename advice. Do NOT read that as permission \
to churn this value: a fork that was interrupted mid-turn still loses its name to the change.\n\
WHAT TO DO: add the new variant, confirm the gate change above is still in place \
(api::tests::orphan_reservation_rows_are_visible_to_the_spawn_dedupe_gate), then re-pin here.";
        assert_eq!(PromptVersion::current(), PromptVersion::V2, "{WHY}");
    }

    #[test]
    fn v1_addendum_is_byte_frozen() {
        assert_eq!(ADDENDUM_V1_ZH, V1_GOLDEN);
        assert_eq!(V1_GOLDEN.len(), 342);
        assert_eq!(V1_GOLDEN.chars().count(), 114);
    }

    /// The V2 addenda are not merely product copy: `agent_child_template` renders
    /// them into `system_prompt`, `inherit_parent_model_memory_for_fork`
    /// snapshots that verbatim as `system_prompt_snapshot`, and that field is
    /// projected by `model::ForkModelBindingV1` into the canonical execution-mode
    /// payload. So editing one word here moves the reserved bytes for every fork
    /// issued afterwards, and a conversation holding an orphaned reservation row
    /// under the old bytes can no longer re-reserve that name.
    ///
    /// The other guards do not cover this. `PromptVersion::current` is pinned to
    /// the VARIANT, not to the text it selects, and the render tests assert
    /// substrings. Without these counts a one-word edit passes the whole suite.
    ///
    /// This is a tripwire, not a freeze: unlike `ADDENDUM_V1_ZH` these strings MAY
    /// be edited. Re-run, take the reported lengths, and update them here in the
    /// same commit — the point is that the change is deliberate and reviewed
    /// against the reservation consequence, not that it never happens.
    #[test]
    fn v2_addenda_are_byte_pinned() {
        const WHY: &str = "V2 addendum bytes reach `system_prompt_snapshot`, which \
`ForkModelBindingV1` projects into the canonical execution-mode payload. Changing \
them re-reserves every new fork under different bytes. If this edit is intended, \
update the counts here in the same commit; see the module doc and \
`model::canonical_subagent_execution_mode_payload`.";

        let english = addendum(PromptVersion::V2, &PromptProfile::builtin_english());
        assert_eq!(english.len(), 2038, "{WHY}");
        assert_eq!(english.chars().count(), 2034, "{WHY}");

        // The built-in Chinese addendum opens with V1's exact paragraph:
        // `api::tests::serve_routed_json` routes child turns by that substring.
        let chinese = addendum(PromptVersion::V2, &PromptProfile::builtin_chinese());
        assert_eq!(chinese.len(), 1709, "{WHY}");
        assert_eq!(chinese.chars().count(), 587, "{WHY}");
        assert!(chinese.starts_with(ADDENDUM_V1_ZH), "{WHY}");
    }

    /// Acceptance (c): `render(V1, x)` is byte-identical to the pre-change
    /// `api::subagent_system_prompt(x)` across empty, whitespace-only, short and
    /// multi-KB inputs. Every expectation is spelled out from the golden.
    #[test]
    fn v1_render_matches_the_pre_change_subagent_system_prompt() {
        let long_ascii = "x".repeat(4096);
        let long_multibyte = "阿".repeat(4096);
        let cases: Vec<(String, String)> = vec![
            // Empty and whitespace-only collapse to the bare addendum with no
            // separator on either side.
            (String::new(), V1_GOLDEN.to_owned()),
            ("   ".to_owned(), V1_GOLDEN.to_owned()),
            ("\n\t \r\n".to_owned(), V1_GOLDEN.to_owned()),
            // `str::trim` is Unicode `White_Space`: IDEOGRAPHIC SPACE and
            // NO-BREAK SPACE are trimmed too. Do not "modernize" this to ASCII.
            ("\u{3000}\u{00a0}".to_owned(), V1_GOLDEN.to_owned()),
            (
                "Be useful.".to_owned(),
                format!("Be useful.{SEPARATOR}{V1_GOLDEN}"),
            ),
            // Both ends are trimmed, and the inner text is untouched.
            (
                "  Be useful.  ".to_owned(),
                format!("Be useful.{SEPARATOR}{V1_GOLDEN}"),
            ),
            (
                "中文提示词\n".to_owned(),
                format!("中文提示词{SEPARATOR}{V1_GOLDEN}"),
            ),
            // A base that already contains the separator is passed through
            // verbatim: there is no de-duplication and never was.
            (
                "before\n\n---\n\nafter".to_owned(),
                format!("before\n\n---\n\nafter{SEPARATOR}{V1_GOLDEN}"),
            ),
            (
                long_ascii.clone(),
                format!("{long_ascii}{SEPARATOR}{V1_GOLDEN}"),
            ),
            (
                long_multibyte.clone(),
                format!("{long_multibyte}{SEPARATOR}{V1_GOLDEN}"),
            ),
        ];
        for (base, expected) in cases {
            let rendered = render(PromptVersion::V1, &base, &PromptProfile::builtin_english());
            assert_eq!(
                rendered.as_bytes(),
                expected.as_bytes(),
                "V1 render drifted for a {}-byte base",
                base.len()
            );
        }
        // The multi-KB cases really are multi-KB.
        assert_eq!(long_ascii.len(), 4096);
        assert_eq!(long_multibyte.len(), 12288);
    }

    #[test]
    fn v1_carries_none_of_v2s_additions() {
        let v1 = render(PromptVersion::V1, "Be useful.", &PromptProfile::builtin_english());
        for marker in [
            "指令来源边界",
            "注意事项：",
            "Instruction-source boundary",
            "Notes:",
        ] {
            assert!(
                !v1.contains(marker),
                "V1 must not have grown {marker:?}; V1 is frozen"
            );
        }
        assert_eq!(v1.len(), "Be useful.".len() + SEPARATOR.len() + 342);
    }

    #[test]
    fn v2_chinese_adds_the_boundary_paragraph_and_notes_after_v1s_paragraph() {
        let v2 = render(PromptVersion::V2, "", &PromptProfile::builtin_chinese());
        // `api::tests::serve_routed_json` routes a child turn by this exact
        // substring. Losing it makes that test hang or misroute rather than
        // report a diff.
        assert!(v2.starts_with("你是主代理派生的子代理"));
        assert!(v2.starts_with(V1_GOLDEN));
        assert_eq!(&v2[V1_GOLDEN.len()..V1_GOLDEN.len() + 2], "\n\n");
        assert!(v2.contains("指令来源边界："));
        assert!(v2.contains("\n\n注意事项：\n- "));
        assert!(v2.contains("你没有向用户提问的工具"));
        assert!(v2.contains("不能再派生或指挥子代理"));
        assert!(v2.contains("除非宿主为你单独分配了记忆分区"));
        assert!(v2.contains("浏览器会话与联网授权由整个对话共享"));
        assert!(v2.contains("宿主会限制你的轮数"));
        // No English leaked into the Chinese variant beyond the product name.
        assert!(!v2.contains("Notes:"));
        assert!(!v2.contains("Instruction-source boundary"));
    }

    #[test]
    fn v2_english_is_a_separate_variant_not_a_second_copy() {
        let en = render(PromptVersion::V2, "", &PromptProfile::builtin_english());
        assert!(en.starts_with("You are a child agent spawned by the main agent."));
        assert!(en.contains("Instruction-source boundary:"));
        assert!(en.contains("\n\nNotes:\n- "));
        assert!(en.contains("you have no tool for asking"));
        assert!(en.contains("cannot spawn or direct further child agents"));
        assert!(en.contains("unless the host assigned you a partition of your own"));
        assert!(en.contains("browser session and web authorization are shared"));
        assert!(en.contains("The host caps your rounds"));
        // Bilingual means "one of two", not "both at once": emitting both would
        // double every child turn's prompt tokens.
        assert!(!en.contains("你是主代理派生的子代理"));
        assert!(!en.contains("注意事项"));
        let zh = render(PromptVersion::V2, "", &PromptProfile::builtin_chinese());
        assert_ne!(en, zh);
    }

    /// The version selects the addendum and nothing else: `base` reaches the
    /// output byte-for-byte (after the shared trim) under every version.
    #[test]
    fn no_version_rewrites_the_caller_prompt() {
        let bases = [
            "Be useful.",
            "  padded  ",
            "中文提示词",
            "before\n\n---\n\nafter",
            "指令来源边界：a user is allowed to quote this",
            "<mework-memory-context>\nquoted delimiter\n</mework-memory-context>",
        ];
        for (version, profile) in [
            (PromptVersion::V1, PromptProfile::builtin_english()),
            (PromptVersion::V2, PromptProfile::builtin_chinese()),
            (PromptVersion::V2, PromptProfile::builtin_english()),
        ] {
            let tail = render(version, "", &profile);
            for base in bases {
                let rendered = render(version, base, &profile);
                assert_eq!(rendered, format!("{}{SEPARATOR}{tail}", base.trim()));
                // Nothing was inserted into, removed from, or reordered inside
                // the caller's own text.
                assert!(rendered.starts_with(base.trim()));
                assert!(rendered.ends_with(&tail));
            }
            // Whitespace-only input yields the addendum alone under every
            // version, with no dangling separator.
            assert_eq!(render(version, " \u{3000}\n", &profile), tail);
            assert!(!tail.starts_with('\n'));
            // `api::combined_system_prompt` trims the wire prompt, so a
            // trailing newline here would diverge from the signed snapshot.
            assert!(!tail.ends_with('\n'));
        }
    }
}
