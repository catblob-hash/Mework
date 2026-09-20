import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import type { SelectedElement } from "../lib/browser";
import { SelectedElementChips } from "./SelectedElementChips";

function element(overrides: Partial<SelectedElement> = {}): SelectedElement {
  return {
    sequence: 1,
    tagName: "button",
    classes: ["btn", "btn-primary", "btn-lg"],
    attributes: {},
    computedStyles: {},
    boundingBox: { x: 0, y: 0, width: 10, height: 10 },
    screenshotBase64: "",
    innerText: "Place order",
    ...overrides
  };
}

afterEach(() => configureI18n("zh-CN"));

describe("SelectedElementChips", () => {
  it("draws nothing when no element is staged", () => {
    const { container } = render(<SelectedElementChips elements={[]} onRemove={vi.fn()} />);
    expect(container.firstChild).toBeNull();
  });

  it("names each pick by what the user pointed at, not by what the model will read", () => {
    render(
      <SelectedElementChips
        elements={[
          element({ sequence: 1 }),
          element({ sequence: 2, reactComponent: "SubmitButton", innerText: null })
        ]}
        onRemove={vi.fn()}
      />
    );
    expect(screen.getAllByRole("listitem")).toHaveLength(2);
    // Two classes at most: a chip is a label, not the element's whole class attribute.
    expect(screen.getByText('<button class="btn btn-primary" />')).toBeInTheDocument();
    expect(screen.getByText("Place order")).toBeInTheDocument();
    expect(screen.getByText("<SubmitButton />")).toBeInTheDocument();
  });

  it("removes by sequence, so two picks of the same element stay distinct", async () => {
    const user = userEvent.setup();
    const onRemove = vi.fn();
    render(
      <SelectedElementChips
        elements={[element({ sequence: 4 }), element({ sequence: 9 })]}
        onRemove={onRemove}
      />
    );
    const removals = screen.getAllByRole("button", { name: /移除/ });
    expect(removals).toHaveLength(2);
    await user.click(removals[1]!);
    expect(onRemove).toHaveBeenCalledWith(9);
  });
});
