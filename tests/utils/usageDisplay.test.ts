import { describe, expect, it } from "vitest";
import {
  formatUsageDataSummary,
  formatUsageRelativeTime,
} from "@/utils/usageDisplay";

const labels = {
  invalid: "Invalid",
  remaining: "Remaining:",
  used: "Used:",
};

describe("formatUsageDataSummary", () => {
  it("formats used percentage when remaining is omitted", () => {
    expect(
      formatUsageDataSummary(
        {
          planName: "Coco OpenRouter",
          used: 55,
          total: 100,
          unit: "%",
        },
        labels,
      ),
    ).toBe("[Coco OpenRouter] Used: 55%");
  });

  it("formats remaining when present", () => {
    expect(
      formatUsageDataSummary(
        {
          planName: "Balance",
          remaining: 12.5,
          unit: "USD",
        },
        labels,
      ),
    ).toBe("[Balance] Remaining: 12.50 USD");
  });

  it("formats invalid results without requiring quota fields", () => {
    expect(
      formatUsageDataSummary(
        {
          isValid: false,
          invalidMessage: "Unauthorized",
        },
        labels,
      ),
    ).toBe("Unauthorized");
  });
});

describe("formatUsageRelativeTime", () => {
  const t = (key: string, options?: { count?: number }) =>
    options?.count === undefined ? key : `${key}:${options.count}`;
  const now = 1_000_000_000;

  it.each([
    [now + 1_000, "usage.justNow"],
    [now - 59_000, "usage.justNow"],
    [now - 60_000, "usage.minutesAgo:1"],
    [now - 3_600_000, "usage.hoursAgo:1"],
    [now - 86_400_000, "usage.daysAgo:1"],
  ])("formats %s relative to now", (timestamp, expected) => {
    expect(formatUsageRelativeTime(timestamp, now, t)).toBe(expected);
  });
});
