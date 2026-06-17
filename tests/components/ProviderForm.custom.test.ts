import { describe, expect, it } from "vitest";
import {
  buildCustomProviderMeta,
  buildCustomProviderSettingsConfig,
} from "@/components/providers/forms/ProviderForm";
import {
  extractCodexBaseUrl,
  extractCodexModelName,
} from "@/utils/providerConfigUtils";

describe("ProviderForm custom provider config", () => {
  const baseDraft = {
    apiKey: " sk-test ",
    baseUrl: "https://api.example.com/v1/",
    model: "model-main",
    fastModel: "model-fast",
    claudeApiFormat: "anthropic" as const,
    codexApiFormat: "openai_responses" as const,
    authField: "ANTHROPIC_AUTH_TOKEN" as const,
  };

  it("builds Claude custom config from the custom form fields", () => {
    const config = buildCustomProviderSettingsConfig("claude", baseDraft);

    expect(config).toEqual({
      env: {
        ANTHROPIC_BASE_URL: "https://api.example.com/v1",
        ANTHROPIC_AUTH_TOKEN: "sk-test",
        ANTHROPIC_MODEL: "model-main",
        ANTHROPIC_DEFAULT_SONNET_MODEL: "model-main",
        ANTHROPIC_DEFAULT_HAIKU_MODEL: "model-fast",
      },
    });
    expect(buildCustomProviderMeta("claude", baseDraft)).toBeUndefined();
  });

  it("builds Codex custom config and local-routing metadata", () => {
    const draft = {
      ...baseDraft,
      codexApiFormat: "openai_chat" as const,
    };
    const config = buildCustomProviderSettingsConfig("codex", draft);

    expect(config.auth).toEqual({ OPENAI_API_KEY: "sk-test" });
    expect(extractCodexBaseUrl(String(config.config))).toBe(
      "https://api.example.com/v1",
    );
    expect(extractCodexModelName(String(config.config))).toBe("model-main");
    expect(buildCustomProviderMeta("codex", draft)).toEqual({
      apiFormat: "openai_chat",
    });
  });

  it("builds Claude Desktop proxy metadata when a custom route model is provided", () => {
    const meta = buildCustomProviderMeta("claude-desktop", {
      ...baseDraft,
      claudeApiFormat: "openai_chat",
    });

    expect(meta).toMatchObject({
      claudeDesktopMode: "proxy",
      apiFormat: "openai_chat",
      claudeDesktopModelRoutes: {
        "claude-sonnet-4-6": { model: "model-main" },
        "claude-opus-4-8": { model: "model-main" },
        "claude-haiku-4-5": { model: "model-fast" },
      },
    });
  });
});
