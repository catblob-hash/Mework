import { describe, expect, it } from "vitest";
// Vite's ?raw import loads Rust source as plain text; this suite reasons about the
// source, not about a compiled artefact, so it needs neither cargo nor Node types.
import agentsSource from "../src-tauri/src/agents.rs?raw";
import kernelShadowSource from "../src-tauri/src/kernel_shadow.rs?raw";

// `src-tauri/src/lib.rs` opens with `#![cfg_attr(test, allow(dead_code))]`, which only
// applies to the test build. A method that exists solely so a unit test can observe
// internal state therefore compiles into the *non-test* build as well and leaves a
// `dead_code` warning there forever. `#[cfg(test)]` on the method removes it from that
// build without hiding the diagnostic behind an `allow`.
//
// The pin has two halves on purpose. Asserting the attribute alone would be satisfied by
// blindly re-adding it; asserting that no production caller exists is what makes the
// attribute the *correct* answer. If a real caller ever appears, this test fails and says
// to drop the attribute rather than to delete the caller.
const testOnlyMethods = [
  {
    file: "src-tauri/src/agents.rs",
    source: agentsSource,
    method: "lifetime_usage",
    // The production snapshot reads the field directly instead of going through the
    // getter, so `lifetime_usage:` / `.lifetime_usage =` must not count as callers.
    declaration: "pub fn lifetime_usage(&self) -> ModelUsage {"
  },
  {
    file: "src-tauri/src/kernel_shadow.rs",
    source: kernelShadowSource,
    method: "divergences",
    declaration: "pub fn divergences(&self) -> Vec<String> {"
  }
] as const;

describe("Rust methods that only tests use", () => {
  it.each(testOnlyMethods)(
    "$file keeps $method out of the non-test build",
    ({ source, method, declaration }) => {
      const declarationAt = source.indexOf(declaration);
      expect(declarationAt, `找不到 ${method} 的声明，钉子已经失去目标`).toBeGreaterThan(-1);
      expect(source.indexOf(declaration, declarationAt + 1), `${method} 声明不唯一`).toBe(-1);

      // The attribute has to be on the method itself. Doc comments may sit between the
      // attribute and the signature, so scan the lines above rather than requiring
      // adjacency, and stop at the previous item's closing brace.
      const preceding = source.slice(0, declarationAt).split("\n").reverse();
      let guarded = false;
      for (const line of preceding) {
        const text = line.trim();
        if (text === "") continue;
        if (text.startsWith("///") || text.startsWith("//")) continue;
        guarded = text === "#[cfg(test)]";
        break;
      }
      expect(guarded, `${method} 缺少 #[cfg(test)]，非测试构建会留下 dead_code 警告`).toBe(true);
    }
  );

  it.each(testOnlyMethods)(
    "$file has no production caller of $method",
    ({ source, method, file }) => {
      const testModuleAt = source.indexOf("\n#[cfg(test)]\nmod tests {");
      expect(testModuleAt, `${file} 没有顶层测试模块，本钉子的分界线不成立`).toBeGreaterThan(-1);

      // Only a call — `.divergences()` — counts. A field read or a struct literal of the
      // same name is production code and is none of this pin's business.
      const calls = [...source.matchAll(new RegExp(`\\.${method}\\(\\)`, "gu"))];
      expect(calls.length, `${method} 一个调用点都没有，钉子形同虚设`).toBeGreaterThan(0);
      const production = calls
        .filter((call) => call.index < testModuleAt)
        .map((call) => source.slice(0, call.index).split("\n").length);
      expect(
        production,
        `${file} 里 ${method} 出现了生产调用点（行号如上）：应当去掉 #[cfg(test)]，而不是删调用`
      ).toEqual([]);
    }
  );
});
