import type { ReactNode } from "react";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { CommonConfigEditor } from "@/components/providers/forms/CommonConfigEditor";

vi.mock("@/components/common/FullScreenPanel", () => ({
  FullScreenPanel: ({ isOpen, children }: { isOpen: boolean; children: ReactNode }) =>
    isOpen ? <div>{children}</div> : null,
}));

vi.mock("@/components/JsonEditor", () => ({
  default: () => <textarea aria-label="settings-json-editor" />,
}));

function renderEditor(value: string, onChange = vi.fn()) {
  render(
    <CommonConfigEditor
      value={value}
      onChange={onChange}
      useCommonConfig={false}
      onCommonConfigToggle={() => {}}
      commonConfigSnippet="{}"
      onCommonConfigSnippetChange={() => {}}
      commonConfigError=""
      onEditClick={() => {}}
      isModalOpen={false}
      onModalClose={() => {}}
    />,
  );
  return onChange;
}

const hideAttributionCheckbox = () =>
  screen.getByRole("checkbox", { name: "claudeConfig.hideAttribution" });

describe("CommonConfigEditor hide attribution toggle", () => {
  it("requires sessionUrl=false to treat attribution as hidden", () => {
    renderEditor(JSON.stringify({ attribution: { commit: "", pr: "" } }));
    expect(hideAttributionCheckbox()).not.toBeChecked();
  });

  it("disables commit, PR, and session URL attribution", () => {
    const onChange = renderEditor("{}");
    fireEvent.click(hideAttributionCheckbox());
    expect(JSON.parse(onChange.mock.calls[0][0])).toEqual({
      attribution: { commit: "", pr: "", sessionUrl: false },
    });
  });
});
