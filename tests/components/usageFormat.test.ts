import { describe, expect, it } from "vitest";
import {
  formatTokensShort,
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
});
