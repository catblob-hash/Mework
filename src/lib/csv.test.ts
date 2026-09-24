import { describe, expect, it } from "vitest";
import { guessDelimiter, numericColumns, parseDelimited } from "./csv";

describe("parseDelimited", () => {
  it("honours quoting: delimiters, line breaks and doubled quotes inside a field", () => {
    const table = parseDelimited("a,b\n\"x, y\",\"line\none\"\n\"say \"\"hi\"\"\",3\n", ",");
    expect(table.rows).toEqual([["a", "b"], ["x, y", "line\none"], ["say \"hi\"", "3"]]);
    expect(table.truncated).toBe(false);
  });

  it("reads CRLF, skips blank lines and a byte-order mark, and keeps empty fields", () => {
    expect(parseDelimited("﻿a;b\r\n\r\n1;\r\n", ";").rows).toEqual([["a", "b"], ["1", ""]]);
  });

  it("stops at the row limit and says so", () => {
    const table = parseDelimited("1\n2\n3\n4\n", ",", 2);
    expect(table.rows).toEqual([["1"], ["2"]]);
    expect(table.truncated).toBe(true);
  });
});

describe("guessDelimiter", () => {
  it("picks the separator that splits every line the same way", () => {
    expect(guessDelimiter("a;b;c\n1;2;3\n")).toBe(";");
    expect(guessDelimiter("a\tb\n1\t2\n")).toBe("\t");
    expect(guessDelimiter("name,note\nx,\"a;b\"\n")).toBe(",");
    expect(guessDelimiter("just one column\n")).toBe(",");
  });
});

describe("numericColumns", () => {
  it("right-aligns a column only when every value in it is a number", () => {
    expect(numericColumns([["n", "x", "p"], ["1,200", "a", "5%"], ["-3.5e2", "", "7%"]], 1)).toEqual([true, false, true]);
  });
});
