import { describe, expect, it } from "vitest";
import { formatUsageScript } from "@/utils/formatUsageScript";

describe("formatUsageScript", () => {
  it("loads the Prettier plugins on demand and formats JavaScript", async () => {
    await expect(formatUsageScript("const x={a:1}")).resolves.toBe(
      "const x = { a: 1 };\n",
    );
  });
});
