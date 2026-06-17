import { describe, expect, it } from "vitest";
import {
  extractIdFromToml,
  inferMcpServerType,
  normalizeMcpServerSpec,
  tomlToMcpServer,
} from "@/utils/tomlUtils";

describe("MCP TOML utils", () => {
  it("parses Codex url-only mcp_servers entries as http", () => {
    const server = tomlToMcpServer(
      [
        "[mcp_servers.pubmed]",
        'url = "https://example.test/mcp"',
        "",
        "[mcp_servers.pubmed.http_headers]",
        'Authorization = "Bearer token"',
        '"X-Client" = "cc-switch"',
      ].join("\n"),
    );

    expect(server).toMatchObject({
      type: "http",
      url: "https://example.test/mcp",
      headers: {
        Authorization: "Bearer token",
        "X-Client": "cc-switch",
      },
    });
    expect(server.http_headers).toBeUndefined();
  });

  it("parses direct url-only server configs as http", () => {
    const server = tomlToMcpServer('url = "https://example.test/mcp"');

    expect(server).toMatchObject({
      type: "http",
      url: "https://example.test/mcp",
    });
  });

  it("extracts ids from Codex mcp_servers TOML", () => {
    expect(
      extractIdFromToml(
        [
          "[mcp_servers.research-gateway]",
          'url = "https://example.test/mcp"',
        ].join("\n"),
      ),
    ).toBe("research-gateway");
  });

  it("normalizes streamable-http specs to internal http", () => {
    expect(
      normalizeMcpServerSpec({
        type: "streamable-http",
        url: "https://example.test/mcp",
      }),
    ).toMatchObject({
      type: "http",
      url: "https://example.test/mcp",
    });
    expect(
      inferMcpServerType({
        type: "streamable-http",
        url: "https://example.test/mcp",
      }),
    ).toBe("http");
  });
});
