import { describe, expect, it } from "vitest";

import { browserDevFixtureAddress } from "./browserDevFixture";

describe("browser-dev fixture address", () => {
  it("derives the exact HTTP fixture origin from a custom high loopback port", () => {
    expect(browserDevFixtureAddress("ws://127.0.0.1:15430/ws")).toEqual({
      origin: "http://127.0.0.1:15430",
      port: "15430"
    });
  });

  it.each([
    [undefined, "missing"],
    ["", "empty"],
    ["http://127.0.0.1:15430/ws", "http"],
    ["wss://127.0.0.1:15430/ws", "wss"],
    ["ws://localhost:15430/ws", "localhost"],
    ["ws://127.0.0.2:15430/ws", "other IPv4"],
    ["ws://[::1]:15430/ws", "IPv6"],
    ["ws://127.0.0.1/ws", "missing port"],
    ["ws://127.0.0.1:1/ws", "privileged port"],
    ["ws://127.0.0.1:1023/ws", "port below boundary"],
    ["ws://127.0.0.1:65536/ws", "port above boundary"],
    ["ws://127.0.0.1:15430/", "wrong path"],
    ["ws://127.0.0.1:15430/ws/", "trailing slash"],
    ["ws://user@127.0.0.1:15430/ws", "userinfo"],
    ["ws://127.0.0.1:15430/ws?fixture=1", "query"],
    ["ws://127.0.0.1:15430/ws#fixture", "hash"]
  ])("rejects %s (%s)", (value, _label) => {
    expect(() => browserDevFixtureAddress(value)).toThrow(
      /浏览器 E2E (?:缺少隔离后端地址|后端必须是精确的高位)/
    );
  });
});
