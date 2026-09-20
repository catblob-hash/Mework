# Built-in skills

The Agent Skills a fresh installation starts with. `capability_seed.rs` compiles
them in with `include_str!` and writes each one to
`~/.mework/skills/<directory>/SKILL.md` the first time the application data
directory is initialized. From that moment they are ordinary user files: the
catalog page lists them, conversations select them, and deleting one deletes it
for good — the marker file in the data directory is what stops the next launch
from writing it back.

**Do not hand-edit these files.** They are third-party text under MIT, quoted
with its provenance in
[`THIRD-PARTY-NOTICES.md`](../../../THIRD-PARTY-NOTICES.md). Editing a body here
silently detaches it from the upstream it claims to be. To change one, either
resynchronize it from upstream or move the text into a manifest of our own.

## Provenance

Each body is upstream-verbatim apart from the removals listed below; only the
frontmatter is rewritten, because this host parses flat `key: value` lines
between `---` fences rather than YAML, and the model-visible skill name is the
*directory* name, so `name:` has to equal it.

| Directory | Upstream | Path | Commit | SHA-256 (16) |
| --- | --- | --- | --- | --- |
| `systematic-debugging` | [obra/superpowers](https://github.com/obra/superpowers) | `skills/systematic-debugging/SKILL.md` | `b36e0829c6d0140e93cfef2ca599b1b07d4a7797` | `5bab04e516e293be` |
| `verification-before-completion` | [obra/superpowers](https://github.com/obra/superpowers) | `skills/verification-before-completion/SKILL.md` | `b36e0829c6d0140e93cfef2ca599b1b07d4a7797` | `dcae266569e41128` |
| `code-review-and-quality` | [addyosmani/agent-skills](https://github.com/addyosmani/agent-skills) | `skills/code-review-and-quality/SKILL.md` | `be4e44a9` | `dc15b88b73d5b29c` |
| `git-workflow-and-versioning` | [addyosmani/agent-skills](https://github.com/addyosmani/agent-skills) | `skills/git-workflow-and-versioning/SKILL.md` | `be4e44a9` | `9d61c9b4e1d61c12` |
| `resolving-merge-conflicts` | [mattpocock/skills](https://github.com/mattpocock/skills) | `skills/engineering/resolving-merge-conflicts/SKILL.md` | `959a8e9f` | `1611f65cfbbb80eb` |

All five were fetched on 2026-09-17. Hashes are of the adapted file as committed
here, not of the upstream original.

## Removals

Upstream skills cross-reference sibling skills and sibling files that this
product does not ship. A pointer to something absent is worse than no pointer,
so each was removed and the surrounding sentence left readable:

- `systematic-debugging` — the closing "Supporting Techniques" section and its
  pointers to `root-cause-tracing.md`, `defense-in-depth.md` and
  `condition-based-waiting.md`; the `superpowers:test-driven-development`
  reference; `superpowers:verification-before-completion` rebound to the plain
  name, which we do ship.
- `code-review-and-quality` — the "See Also" section pointing at
  `../../references/security-checklist.md` and
  `../../references/performance-checklist.md`, plus three sentences referring to
  the `security-and-hardening` and `performance-optimization` skills.
- `git-workflow-and-versioning` — two sibling-skill cross-references
  (`api-and-interface-design`, and the changelog tail naming
  `deprecation-and-migration` / `shipping-and-launch`). The upstream description
  was 443 characters, above the 240-character metadata limit, so the detail it
  carried moved into `when_to_use`.
- `verification-before-completion`, `resolving-merge-conflicts` — nothing
  removed.

## Resynchronizing

1. Fetch the upstream file at the commit you intend to pin:
   `gh api repos/<owner>/<repo>/contents/<path>?ref=<sha> --jq .content | base64 -d`
2. Re-apply the frontmatter rewrite and the removals above.
3. Write with LF endings and UTF-8, no BOM. `name:` must equal the directory.
4. Update the table, the removal list, and the commit in
   `THIRD-PARTY-NOTICES.md`.
5. Bump `SEED_VERSION` in `src-tauri/src/capability_seed.rs` only if existing
   installations should receive the new text. Re-seeding never overwrites a
   manifest already on disk, so a bump reaches new installations and anyone who
   deleted the file — not people who kept it.
6. Run `cargo test --lib capability_seed`, which checks every manifest still
   parses and still names its directory.

Files here are also scanned by `scripts/tests/e2e-fixture-surface.test.mjs`,
which fails on a short list of retired-surface tokens anywhere under
`src-tauri/resources`. Check any newly vendored text against it.
