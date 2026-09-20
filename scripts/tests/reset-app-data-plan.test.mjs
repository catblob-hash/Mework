import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";

import {
  INTERACTIVE_DEV_IDENTIFIER,
  KEYRING_SERVICES,
  parseResetArguments,
  planCredentialTargets,
  planDataDirectories,
  resolveDataDirectory,
  selectIdentifiers,
  summarizeCredentials
} from "../reset-app-data-plan.mjs";

test("defaults to the production scope and requires an explicit key opt-in", () => {
  assert.deepEqual(parseResetArguments([]), {
    scope: "prod",
    keys: false,
    dryRun: false,
    assumeYes: false
  });
  assert.equal(parseResetArguments(["--dev"]).scope, "dev");
  assert.equal(parseResetArguments(["--all"]).scope, "all");
  assert.equal(parseResetArguments(["--keys"]).keys, true);
  assert.equal(parseResetArguments(["--dry-run"]).dryRun, true);
  assert.equal(parseResetArguments(["--yes"]).assumeYes, true);
});

test("rejects unknown, duplicated and mutually exclusive arguments", () => {
  assert.throws(() => parseResetArguments(["--force"]), /不支持的 reset:data 参数/u);
  assert.throws(() => parseResetArguments(["--dev", "--dev"]), /不能重复/u);
  assert.throws(() => parseResetArguments(["--dev", "--prod"]), /不能同时使用/u);
  assert.throws(() => parseResetArguments(["--dev", "--all"]), /不能同时使用/u);
});

const ENTRIES = [
  "com.mework.app",
  "com.naiword.agentstudio",
  INTERACTIVE_DEV_IDENTIFIER,
  "com.mework.app.e2e.image-input-8492dd92fcf37ee71288626a",
  "com.naiword.agentstudio.e2e.0123456789abcdef01234567",
  // Neither an exact production identifier nor a valid dev identifier.
  "com.mework.appmimic",
  "com.mework.app.e2e.",
  "com.mework.other",
  "Microsoft",
  "npm"
];

test("selects only exact production identifiers in the production scope", () => {
  assert.deepEqual(selectIdentifiers(ENTRIES, "prod"), [
    "com.mework.app",
    "com.naiword.agentstudio"
  ]);
});

test("selects only prefixed dev identifiers with a non-empty suffix", () => {
  assert.deepEqual(selectIdentifiers(ENTRIES, "dev"), [
    "com.mework.app.e2e.image-input-8492dd92fcf37ee71288626a",
    INTERACTIVE_DEV_IDENTIFIER,
    "com.naiword.agentstudio.e2e.0123456789abcdef01234567"
  ]);
});

test("never selects a look-alike directory belonging to another vendor", () => {
  for (const scope of ["prod", "dev", "all"]) {
    const selected = selectIdentifiers(ENTRIES, scope);
    for (const stranger of [
      "com.mework.appmimic",
      "com.mework.app.e2e.",
      "com.mework.other",
      "Microsoft",
      "npm"
    ]) {
      assert.ok(!selected.includes(stranger), `${scope} 不应选中 ${stranger}`);
    }
  }
});

test("the all scope is exactly the union of production and dev", () => {
  assert.deepEqual(
    selectIdentifiers(ENTRIES, "all"),
    [...selectIdentifiers(ENTRIES, "prod"), ...selectIdentifiers(ENTRIES, "dev")].sort()
  );
});

test("rejects an unknown scope rather than silently selecting nothing", () => {
  assert.throws(() => selectIdentifiers(ENTRIES, "everything"), /未知的清理范围/u);
});

test("refuses an identifier that escapes its AppData root", () => {
  const parent = path.resolve("C:/Users/tester/AppData/Roaming");
  assert.equal(
    resolveDataDirectory(parent, "com.mework.app"),
    path.join(parent, "com.mework.app")
  );
  for (const escape of ["..", "../elsewhere", "nested/child", "..\\elsewhere"]) {
    assert.throws(() => resolveDataDirectory(parent, escape), /拒绝清理越出/u);
  }
});

test("plans one absolute path per root and skips roots with no directory", () => {
  const plan = planDataDirectories(
    [
      { label: "APPDATA", directory: "C:/Users/tester/AppData/Roaming", entries: ENTRIES },
      { label: "LOCALAPPDATA", directory: "C:/Users/tester/AppData/Local", entries: ENTRIES },
      { label: "MISSING", directory: undefined, entries: ENTRIES }
    ],
    "prod"
  );
  assert.deepEqual(plan.map((entry) => entry.label), [
    "APPDATA",
    "APPDATA",
    "LOCALAPPDATA",
    "LOCALAPPDATA"
  ]);
  for (const entry of plan) assert.ok(path.isAbsolute(entry.path));
  assert.equal(
    plan[0].path,
    path.resolve("C:/Users/tester/AppData/Roaming/com.mework.app")
  );
});

const CMDKEY_OUTPUT = [
  "当前保存的凭据:",
  "",
  "    Target: LegacyGeneric:target=api-key:v4:12e6f8f2:e1b4adc3.com.mework.api",
  "    Target: LegacyGeneric:target=binding:v4:6e8e5f34.com.mework.api",
  "    Target: LegacyGeneric:target=database-key:5ee4ea36.com.mework.memory.v1",
  "    Target: LegacyGeneric:target=source:9f12.Mework Marketplace",
  "    Target: LegacyGeneric:target=trust:aa01.com.mework.app.project-import-trust.v1",
  "    Target: LegacyGeneric:target=key:v1:bb02.com.mework.web-search",
  "    Target: LegacyGeneric:target=legacy:cc03.com.naiword.agent-studio.api",
  "    Target: LegacyGeneric:target=git:https://github.com",
  "    Target: LegacyGeneric:target=some.other.com.mework.api.vendor",
  "    Target: LegacyGeneric:target=com.mework.api",
  "    User: example-user"
].join("\r\n");

test("matches a credential only when the service is the target's trailing segment", () => {
  const planned = planCredentialTargets(CMDKEY_OUTPUT);
  const targets = planned.map((entry) => entry.target);
  assert.equal(planned.length, 7);
  assert.ok(targets.includes("api-key:v4:12e6f8f2:e1b4adc3.com.mework.api"));
  assert.ok(targets.includes("source:9f12.Mework Marketplace"));
  assert.ok(targets.includes("legacy:cc03.com.naiword.agent-studio.api"));
  // Unrelated credentials, and near-misses that merely contain a service name
  // or equal it without the `<identity>.` prefix the keyring crate always writes.
  assert.ok(!targets.includes("git:https://github.com"));
  assert.ok(!targets.includes("some.other.com.mework.api.vendor"));
  assert.ok(!targets.includes("com.mework.api"));
});

// `cmdkey` prints the label in the console's OEM code page and in the system
// language, so on a Chinese Windows the entries read `目标:` and arrive
// mojibaked. Only the target name after `LegacyGeneric:target=` is ASCII, and
// planning that ignored it deleted nothing while reporting success.
const LOCALIZED_CMDKEY_OUTPUT = [
  "��ǰ�����ƾ��:",
  "",
  "    Ŀ��: LegacyGeneric:target=api-key:v4:12e6f8f2:e1b4adc3.com.mework.api",
  "    ����: ��ͨ ",
  "    ����: example-user",
  "",
  "    Ŀ��: LegacyGeneric:target=source:9f12.Mework Marketplace",
  "    Ŀ��: LegacyGeneric:target=git:https://github.com"
].join("\r\n");

test("plans credentials on a Windows whose cmdkey labels are not English", () => {
  const planned = planCredentialTargets(LOCALIZED_CMDKEY_OUTPUT);
  assert.deepEqual(planned, [
    { target: "api-key:v4:12e6f8f2:e1b4adc3.com.mework.api", service: "com.mework.api" },
    { target: "source:9f12.Mework Marketplace", service: "Mework Marketplace" }
  ]);
});

test("a target listed twice is deleted once", () => {
  const duplicated = [
    "    Target: LegacyGeneric:target=binding:v4:6e8e5f34.com.mework.api",
    "    Target: LegacyGeneric:target=binding:v4:6e8e5f34.com.mework.api"
  ].join("\r\n");
  assert.equal(planCredentialTargets(duplicated).length, 1);
});

test("covers every declared keyring service", () => {
  const output = KEYRING_SERVICES
    .map((service, index) => `    Target: LegacyGeneric:target=entry${index}.${service}`)
    .join("\r\n");
  assert.deepEqual(
    planCredentialTargets(output).map((entry) => entry.service),
    KEYRING_SERVICES
  );
});

test("summarizes credentials per service in a stable order", () => {
  assert.deepEqual(summarizeCredentials(planCredentialTargets(CMDKEY_OUTPUT)), [
    { service: "Mework Marketplace", count: 1 },
    { service: "com.mework.api", count: 2 },
    { service: "com.mework.app.project-import-trust.v1", count: 1 },
    { service: "com.mework.memory.v1", count: 1 },
    { service: "com.mework.web-search", count: 1 },
    { service: "com.naiword.agent-studio.api", count: 1 }
  ]);
});
