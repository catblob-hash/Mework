import { describe, expect, it } from "vitest";
import { elidePath } from "./pathDisplay";

/** Fits `limit` characters, the way a monospaced box of that width would. */
const within = (limit: number) => (text: string) => Array.from(text).length <= limit;

describe("elidePath", () => {
  const path = "/Users/holycat/Desktop/projects/mework";

  it("leaves a path that fits alone", () => {
    expect(elidePath(path, within(100))).toBe(path);
  });

  it("drops whole directories from the middle, keeping the first and last names", () => {
    expect(elidePath(path, within(15))).toBe("/Users/…/mework");
  });

  it("brings names back from the tail end first", () => {
    expect(elidePath(path, within(24))).toBe("/Users/…/projects/mework");
    expect(elidePath(path, within(31))).toBe("/Users/…/projects/mework");
    expect(elidePath(path, within(32))).toBe("/Users/holycat/…/projects/mework");
    expect(elidePath(path, within(37))).toBe("/Users/holycat/…/projects/mework");
  });

  it("shortens the first name, then the last, when first/…/last does not fit", () => {
    expect(elidePath(path, within(13))).toBe("/U…s/…/mework");
    expect(elidePath(path, within(10))).toBe("/U…/…/me…k");
  });

  it("returns the shortest form when nothing fits", () => {
    expect(elidePath(path, within(2))).toBe("/U…/…/m…");
  });

  it("keeps separators, roots and trailing separators as written", () => {
    expect(elidePath("C:\\Users\\me\\Documents\\app", within(10))).toBe("C:\\…\\app");
    expect(elidePath("\\\\server\\share\\dir\\sub", within(16))).toBe("\\\\server\\…\\sub");
    expect(elidePath("~/src/app/web", within(8))).toBe("~/…/web");
    expect(elidePath("/srv/www/app/", within(11))).toBe("/srv/…/app/");
  });

  it("shortens a path with nothing in its middle by its names alone", () => {
    expect(elidePath("/Users/mework", within(10))).toBe("/U…/mework");
    expect(elidePath("/mework", within(4))).toBe("/m…k");
  });

  it("counts characters, not UTF-16 units", () => {
    expect(elidePath("/文档/项目/工作/猫猫项目", within(10))).toBe("/文档/…/猫猫项目");
  });
});
