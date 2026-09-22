import { describe, expect, it } from "vitest";

import { detectCodingPlanProvider } from "@/config/codingPlanProviders";

describe("detectCodingPlanProvider (MiniMax)", () => {
  it.each([
    "https://api.minimax.cn/v1",
    "https://api.minimaxi.com/v1",
    "https://api.minimax.io/v1",
    "https://API.MINIMAX.CN/anthropic",
  ])("recognizes the MiniMax usage provider for %s", (baseUrl) => {
    expect(detectCodingPlanProvider(baseUrl)).toBe("minimax");
  });

  it.each([
    "https://api.minimax.cn.example.com/v1",
    "https://proxy.example.com/api.minimax.io/v1",
  ])("ignores look-alike MiniMax hosts such as %s", (baseUrl) => {
    expect(detectCodingPlanProvider(baseUrl)).toBeNull();
  });
});
