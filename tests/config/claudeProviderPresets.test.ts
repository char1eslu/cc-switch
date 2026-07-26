import { describe, expect, it } from "vitest";
import { providerPresets } from "@/config/claudeProviderPresets";
import { claudeDesktopProviderPresets } from "@/config/claudeDesktopProviderPresets";

describe("Claude provider presets", () => {
  it("only exposes official and Codex OAuth presets", () => {
    expect(providerPresets.map((preset) => preset.name)).toEqual([
      "Claude Official",
      "Codex",
    ]);
    expect(claudeDesktopProviderPresets.map((preset) => preset.name)).toEqual([
      "Claude Desktop Official",
      "Codex",
    ]);
  });
});
