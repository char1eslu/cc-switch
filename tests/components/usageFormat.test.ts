import { describe, expect, it } from "vitest";
import {
  formatOutputTokensPerSecond,
  formatTokensShort,
  getOutputTokensPerSecond,
  getLocaleFromLanguage,
} from "@/components/usage/format";

describe("usage format helpers", () => {
  it("formats Chinese token units with Simplified Chinese characters", () => {
    expect(formatTokensShort(12_345, "zh")).toBe("1.2 万");
    expect(formatTokensShort(123_456_789, "zh-CN", 2)).toBe("1.23 亿");
  });

  it("resolves supported locales", () => {
    expect(getLocaleFromLanguage("zh_CN")).toBe("zh-CN");
    expect(getLocaleFromLanguage("en")).toBe("en-US");
  });

  it("calculates streaming TPS from generation duration after first token", () => {
    expect(
      getOutputTokensPerSecond({
        outputTokens: 120,
        latencyMs: 10_000,
        firstTokenMs: 4_000,
      }),
    ).toBe(20);
  });

  it("prefers explicit durationMs for output TPS", () => {
    expect(
      getOutputTokensPerSecond({
        outputTokens: 120,
        latencyMs: 10_000,
        firstTokenMs: 4_000,
        durationMs: 3_000,
      }),
    ).toBe(40);
  });

  it("falls back to full latency when first token timing is missing", () => {
    expect(
      getOutputTokensPerSecond({
        outputTokens: 120,
        latencyMs: 10_000,
      }),
    ).toBe(12);
  });

  it("does not show TPS without positive tokens or duration", () => {
    expect(
      formatOutputTokensPerSecond({
        outputTokens: 0,
        latencyMs: 10_000,
      }),
    ).toBeNull();
    expect(
      formatOutputTokensPerSecond({
        outputTokens: 120,
        latencyMs: 4_000,
        firstTokenMs: 4_000,
      }),
    ).toBeNull();
  });

  it("formats TPS with integer or single-decimal precision", () => {
    expect(
      formatOutputTokensPerSecond({
        outputTokens: 121,
        latencyMs: 10_000,
      }),
    ).toBe("12");
    expect(
      formatOutputTokensPerSecond({
        outputTokens: 1,
        latencyMs: 4_000,
      }),
    ).toBe("0.3");
  });
});
