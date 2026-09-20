import assert from "node:assert/strict";
import test from "node:test";
import {
  extractCopyright,
  licenseFamilies,
  normalDependencyIds,
  packageLicense,
  textMatchesFamily
} from "../third-party-licenses.mjs";

test("classifies SPDX expressions, legacy Cargo slashes, and exceptions", () => {
  assert.deepEqual(licenseFamilies("(MIT OR Apache-2.0) AND Unicode-3.0"), ["Apache-2.0", "MIT", "Unicode-3.0"]);
  assert.deepEqual(licenseFamilies("MIT/Apache-2.0"), ["Apache-2.0", "MIT"]);
  assert.deepEqual(licenseFamilies("Apache-2.0 WITH LLVM-exception OR MIT"), ["Apache-2.0", "LLVM-exception", "MIT"]);
  assert.deepEqual(licenseFamilies("MIT OR MIT"), ["MIT"]);
});

test("commercial license pointers are never reclassified as MIT", () => {
  assert.deepEqual(licenseFamilies("SEE LICENSE IN README.md"), []);
  assert.equal(packageLicense({ license: "SEE LICENSE IN README.md" }, "sdk"), "SEE LICENSE IN README.md");
});

test("supports legacy npm license arrays and Cargo license_file", () => {
  assert.equal(packageLicense({ licenses: [{ type: "MIT" }, { type: "BSD-3-Clause" }] }, "legacy"), "MIT OR BSD-3-Clause");
  assert.equal(packageLicense({ license_file: "COPYING" }, "crate"), "SEE LICENSE IN COPYING");
  assert.throws(() => packageLicense({}, "missing@1.0.0"), /No determinable license for missing@1\.0\.0/);
  assert.throws(() => packageLicense({ licenses: [{}] }, "broken"), /No determinable license/);
});

test("extracts all attribution lines without license prose or template placeholders", () => {
  const text = "The above copyright notice must be retained.\r\nCopyright (c) 2024 Alice\r\nCopyright 2025 Bob\r\nCopyright (c) 2024 Alice\r\nCopyright [yyyy] [name of copyright owner]";
  assert.equal(extractCopyright(text), "Copyright (c) 2024 Alice; Copyright 2025 Bob");
  assert.equal(extractCopyright("Copyright (c) 2020\nExample Foundation\n"), "Copyright (c) 2020 Example Foundation");
});

test("falls back to authors when license prose contains no copyright owner", () => {
  assert.equal(extractCopyright("1. Copyright and Related Rights", { name: "Alice", email: "alice@example.test" }), "Alice <alice@example.test>");
  assert.equal(extractCopyright("", ["Alice", "Bob"], "crate"), "Alice; Bob");
  assert.equal(extractCopyright(""), "no copyright line in package");
  assert.equal(extractCopyright("", [], "crate"), "no copyright line in crate");
});

test("requires actual grant text rather than an identifier mention", () => {
  assert.equal(textMatchesFamily("See the Apache License Version 2.0 online", "Apache-2.0"), false);
  assert.equal(textMatchesFamily("This SDK is governed by Commercial Terms, not MIT.", "MIT"), false);
  const mit = "Permission is hereby granted, free of charge\nThe above copyright notice and this permission notice\nTHE SOFTWARE IS PROVIDED";
  assert.equal(textMatchesFamily(mit, "MIT"), true);
  assert.equal(textMatchesFamily(mit, "MIT-0"), false);
  const isc = "Permission to use, copy, modify, and/or distribute\nprovided the above copyright notice and this permission notice appear\nTHE SOFTWARE IS PROVIDED";
  assert.equal(textMatchesFamily(isc, "ISC"), true);
  assert.equal(textMatchesFamily(isc, "0BSD"), false);
});

test("walks only normal edges, including proc-macros, without build/dev descendants", () => {
  const edge = (pkg, ...kinds) => ({ pkg, dep_kinds: kinds.map((kind) => ({ kind })) });
  const metadata = { workspace_members: ["app", "core"], resolve: { nodes: [
    { id: "app", deps: [edge("normal", null), edge("build", "build"), edge("dev", "dev"), edge("mixed", "build", null), edge("macro", null)] },
    { id: "core", deps: [edge("normal", null)] },
    { id: "normal", deps: [edge("transitive", null), edge("build", "build")] },
    { id: "mixed", deps: [] },
    { id: "macro", deps: [] },
    { id: "transitive", deps: [edge("normal", null)] }
  ] } };
  assert.deepEqual([...normalDependencyIds(metadata)].sort(), ["macro", "mixed", "normal", "transitive"]);
});
