import { afterEach, describe, expect, it } from "vitest";
import {
  CSP_STYLE_NONCE_CARRIER_ID,
  installCspStyleNonce,
  requireCspStyleNonce
} from "./cspStyleNonce";

const TEST_NONCE = "test-style-nonce";

function installCarrier(nonce = TEST_NONCE): HTMLStyleElement {
  const carrier = document.createElement("style");
  carrier.id = CSP_STYLE_NONCE_CARRIER_ID;
  carrier.nonce = nonce;
  document.head.append(carrier);
  return carrier;
}

afterEach(() => {
  document.getElementById(CSP_STYLE_NONCE_CARRIER_ID)?.remove();
});

describe("CSP style nonce scope", () => {
  it("fails closed when the host did not nonce the carrier", () => {
    expect(() => requireCspStyleNonce(document)).toThrow(/缺少前端 CSP 样式 nonce 载体/u);
    installCarrier("");
    expect(() => requireCspStyleNonce(document)).toThrow(/未被宿主 nonce 化/u);
  });

  it("nonces styles for the whole installation lifetime", () => {
    installCarrier();
    const outside = document.createElement("style");
    const release = installCspStyleNonce(document);
    try {
      const style = document.createElement("style");
      const div = document.createElement("div");
      expect(style.nonce).toBe(TEST_NONCE);
      expect(div.getAttribute("nonce")).toBeNull();
    } finally {
      release();
    }
    const after = document.createElement("style");

    expect(outside.nonce).toBe("");
    expect(after.nonce).toBe("");
  });

  it("reference-counts terminals and restores the exact createElement binding", () => {
    installCarrier();
    const original = document.createElement;
    const ownDescriptor = Object.getOwnPropertyDescriptor(document, "createElement");

    const releaseFirst = installCspStyleNonce(document);
    const protectedCreateElement = document.createElement;
    const releaseSecond = installCspStyleNonce(document);
    expect(document.createElement).toBe(protectedCreateElement);

    releaseFirst();
    expect(document.createElement).toBe(protectedCreateElement);
    expect(document.createElement("style").nonce).toBe(TEST_NONCE);

    releaseSecond();
    releaseSecond();
    expect(document.createElement).toBe(original);
    expect(Object.getOwnPropertyDescriptor(document, "createElement")).toEqual(ownDescriptor);
  });
});
