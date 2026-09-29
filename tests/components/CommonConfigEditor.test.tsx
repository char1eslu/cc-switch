import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { CommonConfigEditor } from "@/components/providers/forms/CommonConfigEditor";
import type { ProviderEditorInactiveField } from "@/lib/api/providers";

vi.mock("@/components/JsonEditor", () => ({
  default: ({
    value,
    onChange,
  }: {
    value: string;
    onChange: (value: string) => void;
  }) => (
    <textarea
      aria-label="settings-json-editor"
      value={value}
      onChange={(event) => onChange(event.target.value)}
    />
  ),
}));

function renderEditor(
  value: string,
  onChange = vi.fn(),
  inactiveFields: ProviderEditorInactiveField[] = [],
) {
  render(
    <CommonConfigEditor
      value={value}
      onChange={onChange}
      inactiveFields={inactiveFields}
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

describe("CommonConfigEditor inactive row fields", () => {
  const timeout: ProviderEditorInactiveField = {
    path: ["env", "API_TIMEOUT_MS"],
    value: "3000000",
  };

  it("offers row fields that never reach live and adds one on click", () => {
    const onChange = renderEditor(
      JSON.stringify({ env: { ANTHROPIC_BASE_URL: "https://a.example" } }),
      vi.fn(),
      [timeout],
    );

    fireEvent.click(
      screen.getByRole("button", { name: /env\.API_TIMEOUT_MS/ }),
    );

    expect(JSON.parse(onChange.mock.calls[0][0])).toEqual({
      env: {
        ANTHROPIC_BASE_URL: "https://a.example",
        API_TIMEOUT_MS: "3000000",
      },
    });
  });

  it("hides a field once the JSON already carries its value", () => {
    renderEditor(
      JSON.stringify({ env: { API_TIMEOUT_MS: "3000000" } }),
      vi.fn(),
      [timeout],
    );

    expect(
      screen.queryByRole("button", { name: /env\.API_TIMEOUT_MS/ }),
    ).not.toBeInTheDocument();
  });
});
