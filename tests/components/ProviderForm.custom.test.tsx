import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ProviderForm } from "@/components/providers/forms/ProviderForm";
import type { AppId } from "@/lib/api";
import type { ProviderCategory, ProviderMeta } from "@/types";

function renderProviderForm({
  appId = "claude",
  initialData,
}: {
  appId?: AppId;
  initialData?: {
    name?: string;
    websiteUrl?: string;
    notes?: string;
    settingsConfig?: Record<string, unknown>;
    category?: ProviderCategory;
    meta?: ProviderMeta;
    icon?: string;
    iconColor?: string;
  } | null;
} = {}) {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
      mutations: { retry: false },
    },
  });

  return render(
    <QueryClientProvider client={queryClient}>
      <ProviderForm
        appId={appId}
        submitLabel="Save"
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
        initialData={initialData}
      />
    </QueryClientProvider>,
  );
}

describe("ProviderForm custom provider UI", () => {
  it("renders the custom Claude form directly without a preset selector", () => {
    renderProviderForm();

    expect(
      screen.queryByText("providerForm.selectPreset"),
    ).not.toBeInTheDocument();
    expect(screen.getByLabelText("API Key")).toBeInTheDocument();
    expect(screen.getByText("providerForm.apiEndpoint")).toBeInTheDocument();
    expect(screen.getByText("provider.configJson")).toBeInTheDocument();
  });

  it("keeps existing Claude providers editable through the custom form", () => {
    renderProviderForm({
      initialData: {
        name: "Existing Claude",
        websiteUrl: "https://example.com",
        category: "aggregator",
        settingsConfig: {
          env: {
            ANTHROPIC_BASE_URL: "https://api.example.com/v1",
            ANTHROPIC_AUTH_TOKEN: "sk-existing",
          },
        },
      },
    });

    expect(screen.getByDisplayValue("Existing Claude")).toBeInTheDocument();
    expect(screen.getByDisplayValue("https://example.com")).toBeInTheDocument();
    expect(
      screen.getByDisplayValue("https://api.example.com/v1"),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("API Key")).toHaveValue("sk-existing");
    expect(screen.getByText("provider.configJson")).toBeInTheDocument();
  });

  it("renders custom Codex fields for existing providers", () => {
    renderProviderForm({
      appId: "codex",
      initialData: {
        name: "Existing Codex",
        websiteUrl: "https://codex.example.com",
        category: "custom",
        settingsConfig: {
          auth: { OPENAI_API_KEY: "sk-codex" },
          config:
            'model = "gpt-5"\nmodel_provider = "openai"\n[model_providers.openai]\nbase_url = "https://codex.example.com/v1"\nwire_api = "responses"\n',
        },
      },
    });

    expect(screen.getByDisplayValue("Existing Codex")).toBeInTheDocument();
    expect(screen.getByLabelText("API Key")).toHaveValue("sk-codex");
    expect(
      screen.getByDisplayValue("https://codex.example.com/v1"),
    ).toBeInTheDocument();
    expect(screen.getByText("codexConfig.configToml")).toBeInTheDocument();
  });
});
