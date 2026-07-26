import { describe, expect, it } from "vitest";
import { codexProviderPresets } from "@/config/codexProviderPresets";

describe("Codex provider presets", () => {
  it("only exposes the official provider", () => {
    expect(codexProviderPresets.map((preset) => preset.name)).toEqual([
      "OpenAI Official",
    ]);
  });
});
