import { configDefaults, defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    css: true,
    exclude: [
      ...configDefaults.exclude,
      "**/.codex-tmp/**",
      "**/.claude/worktrees/**",
      // Workflow steps with `isolation: "worktree"` check a full copy of the
      // repository out under `.mework/worktrees/`. Vitest's default include is
      // rooted at the project, not at `src`, so every one of those copies would
      // otherwise contribute a second set of frontend tests — the same
      // pollution `.claude/worktrees` caused before it was excluded.
      "**/.mework/worktrees/**",
      "**/scripts/tests/**",
      "**/target*/**"
    ]
  }
});
