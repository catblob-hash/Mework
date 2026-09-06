import "@testing-library/jest-dom/vitest";
import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";

// jsdom does not implement `scrollIntoView`. These calls only affect presentation,
// but errors from rAF callbacks become unhandled Vitest failures.
if (typeof Element !== "undefined" && !Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = () => {};
}

// jsdom implements no `AnimationEvent`, so tests cannot build one — and Testing
// Library's `fireEvent.animationEnd` quietly degrades to a plain `Event`, losing
// `animationName` with it. Anything asserting on which animation finished needs
// the constructor to exist.
if (typeof window !== "undefined" && !("AnimationEvent" in window)) {
  class AnimationEventStandIn extends Event {
    readonly animationName: string;
    readonly elapsedTime: number;
    readonly pseudoElement: string;

    constructor(type: string, init: AnimationEventInit = {}) {
      super(type, init);
      this.animationName = init.animationName ?? "";
      this.elapsedTime = init.elapsedTime ?? 0;
      this.pseudoElement = init.pseudoElement ?? "";
    }
  }
  Object.defineProperty(window, "AnimationEvent", {
    configurable: true,
    writable: true,
    value: AnimationEventStandIn
  });
}

afterEach(() => cleanup());
