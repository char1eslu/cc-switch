import { useEffect, useMemo, useState } from "react";
import { useForm } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { ChevronDown, ChevronRight, Settings2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { providerSchema, type ProviderFormData } from "@/lib/schemas/provider";
import type {
  ClaudeApiFormat,
  CodexApiFormat,
  ProviderCategory,
  ProviderMeta,
  ProviderTestConfig,
} from "@/types";
import type { AppId } from "@/lib/api";
import JsonEditor from "@/components/JsonEditor";
import { ProviderIcon } from "@/components/ProviderIcon";
import { getCodexCustomTemplate } from "@/config/codexTemplates";
import {
  extractCodexBaseUrl,
  extractCodexModelName,
  setCodexBaseUrl,
  setCodexModelName,
} from "@/utils/providerConfigUtils";
import { BasicFormFields } from "./BasicFormFields";

type ClaudeAuthField = "ANTHROPIC_AUTH_TOKEN" | "ANTHROPIC_API_KEY";

type CustomProviderDraft = {
  apiKey: string;
  baseUrl: string;
  model: string;
  fastModel: string;
  claudeApiFormat: ClaudeApiFormat;
  codexApiFormat: CodexApiFormat;
  authField: ClaudeAuthField;
};

const DEFAULT_MODEL_BY_APP: Record<AppId, string> = {
  claude: "",
  "claude-desktop": "",
  codex: "gpt-5.5",
};

const trimUrl = (value: string) => value.trim().replace(/\/+$/, "");

const createDefaultCustomDraft = (appId: AppId): CustomProviderDraft => ({
  apiKey: "",
  baseUrl: "",
  model: DEFAULT_MODEL_BY_APP[appId],
  fastModel: "",
  claudeApiFormat: "anthropic",
  codexApiFormat: "openai_responses",
  authField: "ANTHROPIC_AUTH_TOKEN",
});

const buildClaudeSettingsConfig = (draft: CustomProviderDraft) => {
  const env: Record<string, string> = {
    ANTHROPIC_BASE_URL: trimUrl(draft.baseUrl),
    [draft.authField]: draft.apiKey.trim(),
  };
  const model = draft.model.trim();
  const fastModel = draft.fastModel.trim();

  if (model) {
    env.ANTHROPIC_MODEL = model;
    env.ANTHROPIC_DEFAULT_SONNET_MODEL = model;
  }
  if (fastModel) {
    env.ANTHROPIC_DEFAULT_HAIKU_MODEL = fastModel;
  }

  return { env };
};

const buildCodexSettingsConfig = (draft: CustomProviderDraft) => {
  const template = getCodexCustomTemplate();
  let config = setCodexBaseUrl(template.config, trimUrl(draft.baseUrl));
  config = setCodexModelName(config, draft.model.trim() || "gpt-5.5");

  return {
    auth: {
      OPENAI_API_KEY: draft.apiKey.trim(),
    },
    config,
  };
};

const buildClaudeDesktopSettingsConfig = (draft: CustomProviderDraft) => ({
  env: {
    ANTHROPIC_BASE_URL: trimUrl(draft.baseUrl),
    [draft.authField]: draft.apiKey.trim(),
  },
});

export const buildCustomProviderSettingsConfig = (
  appId: AppId,
  draft: CustomProviderDraft,
): Record<string, unknown> => {
  if (appId === "codex") {
    return buildCodexSettingsConfig(draft);
  }
  if (appId === "claude-desktop") {
    return buildClaudeDesktopSettingsConfig(draft);
  }
  return buildClaudeSettingsConfig(draft);
};

export const buildCustomProviderMeta = (
  appId: AppId,
  draft: CustomProviderDraft,
): ProviderMeta | undefined => {
  if (appId === "codex") {
    return draft.codexApiFormat === "openai_chat"
      ? { apiFormat: "openai_chat" }
      : undefined;
  }

  if (appId === "claude") {
    const meta: ProviderMeta = {};
    if (draft.claudeApiFormat !== "anthropic") {
      meta.apiFormat = draft.claudeApiFormat;
    }
    if (draft.authField !== "ANTHROPIC_AUTH_TOKEN") {
      meta.apiKeyField = draft.authField;
    }
    return Object.keys(meta).length > 0 ? meta : undefined;
  }

  const model = draft.model.trim();
  const fastModel = draft.fastModel.trim();
  const needsProxy = draft.claudeApiFormat !== "anthropic" || Boolean(model);
  if (!needsProxy && draft.authField === "ANTHROPIC_AUTH_TOKEN") {
    return undefined;
  }

  const meta: ProviderMeta = {
    claudeDesktopMode: needsProxy ? "proxy" : "direct",
    apiFormat: needsProxy ? draft.claudeApiFormat : undefined,
    apiKeyField:
      draft.authField !== "ANTHROPIC_AUTH_TOKEN" ? draft.authField : undefined,
  };

  if (model) {
    meta.claudeDesktopModelRoutes = {
      "claude-sonnet-4-6": {
        model,
        labelOverride: model,
      },
      "claude-opus-4-8": {
        model,
        labelOverride: model,
      },
      "claude-haiku-4-5": {
        model: fastModel || model,
        labelOverride: fastModel || model,
      },
    };
  }

  return Object.fromEntries(
    Object.entries(meta).filter(([, value]) => value !== undefined),
  ) as ProviderMeta;
};

const getDefaultSettingsConfig = (appId: AppId) =>
  JSON.stringify(
    buildCustomProviderSettingsConfig(appId, createDefaultCustomDraft(appId)),
    null,
    2,
  );

export interface ProviderFormProps {
  appId: AppId;
  providerId?: string;
  submitLabel: string;
  onSubmit: (values: ProviderFormValues) => Promise<void> | void;
  onCancel: () => void;
  onUniversalPresetSelect?: never;
  onManageUniversalProviders?: never;
  initialData?: {
    name?: string;
    notes?: string;
    websiteUrl?: string;
    settingsConfig?: Record<string, unknown>;
    category?: ProviderCategory;
    meta?: ProviderMeta;
    icon?: string;
    iconColor?: string;
  } | null;
  onSubmittingChange?: (submitting: boolean) => void;
  showButtons?: boolean;
  isProxyTakeover?: boolean;
}

export type ProviderFormValues = ProviderFormData & {
  providerKey?: string;
  presetId?: string;
  presetCategory?: ProviderCategory;
  meta?: ProviderMeta;
  testConfig?: ProviderTestConfig;
};

export function ProviderForm({
  appId,
  submitLabel,
  onSubmit,
  onCancel,
  initialData,
  onSubmittingChange,
  showButtons = true,
}: ProviderFormProps) {
  const { t } = useTranslation();
  const isCreateMode = !initialData;
  const [customDraft, setCustomDraft] = useState<CustomProviderDraft>(() =>
    createDefaultCustomDraft(appId),
  );
  const [advancedOpen, setAdvancedOpen] = useState(false);

  const getInitialValues = (): ProviderFormData => ({
    name: initialData?.name ?? "",
    notes: initialData?.notes ?? "",
    websiteUrl: initialData?.websiteUrl ?? "",
    settingsConfig: initialData?.settingsConfig
      ? JSON.stringify(initialData.settingsConfig, null, 2)
      : getDefaultSettingsConfig(appId),
    icon: initialData?.icon ?? "",
    iconColor: initialData?.iconColor ?? "",
  });

  const form = useForm<ProviderFormData>({
    resolver: zodResolver(providerSchema),
    defaultValues: getInitialValues(),
  });

  useEffect(() => {
    form.reset(getInitialValues());
    setCustomDraft(createDefaultCustomDraft(appId));
    setAdvancedOpen(false);
  }, [appId, form, initialData]);

  const generatedSettingsConfig = useMemo(
    () =>
      buildCustomProviderSettingsConfig(appId, {
        ...customDraft,
        model: customDraft.model || DEFAULT_MODEL_BY_APP[appId],
      }),
    [appId, customDraft],
  );

  useEffect(() => {
    if (!isCreateMode) return;
    form.setValue(
      "settingsConfig",
      JSON.stringify(generatedSettingsConfig, null, 2),
      {
        shouldDirty: true,
        shouldValidate: false,
      },
    );
  }, [form, generatedSettingsConfig, isCreateMode]);

  const updateCustomDraft = (patch: Partial<CustomProviderDraft>) => {
    setCustomDraft((current) => ({ ...current, ...patch }));
  };

  const handleSubmit = async (values: ProviderFormData) => {
    if (!values.name.trim()) {
      toast.error(t("provider.nameRequired", { defaultValue: "请输入名称" }));
      return;
    }

    onSubmittingChange?.(true);
    try {
      await onSubmit({
        ...values,
        presetCategory:
          initialData?.category ?? (isCreateMode ? "custom" : undefined),
        meta:
          initialData?.meta ??
          (isCreateMode
            ? buildCustomProviderMeta(appId, customDraft)
            : undefined),
      });
    } finally {
      onSubmittingChange?.(false);
    }
  };

  const renderApiFormatSelect = () => {
    if (appId === "codex") {
      return (
        <div className="space-y-2">
          <Label>{t("providerForm.apiFormat")}</Label>
          <Select
            value={customDraft.codexApiFormat}
            onValueChange={(value) =>
              updateCustomDraft({ codexApiFormat: value as CodexApiFormat })
            }
          >
            <SelectTrigger>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="openai_responses">
                {t("providerForm.codexApiFormatResponses")}
              </SelectItem>
              <SelectItem value="openai_chat">
                {t("providerForm.codexApiFormatOpenAIChat")}
              </SelectItem>
            </SelectContent>
          </Select>
          <p className="text-xs text-muted-foreground">
            {t("providerForm.codexApiFormatHint")}
          </p>
        </div>
      );
    }

    return (
      <div className="space-y-2">
        <Label>{t("providerForm.apiFormat")}</Label>
        <Select
          value={customDraft.claudeApiFormat}
          onValueChange={(value) =>
            updateCustomDraft({ claudeApiFormat: value as ClaudeApiFormat })
          }
        >
          <SelectTrigger>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="anthropic">
              {t("providerForm.apiFormatAnthropic")}
            </SelectItem>
            <SelectItem value="openai_chat">
              {t("providerForm.apiFormatOpenAIChat")}
            </SelectItem>
            <SelectItem value="openai_responses">
              {t("providerForm.apiFormatOpenAIResponses")}
            </SelectItem>
          </SelectContent>
        </Select>
        <p className="text-xs text-muted-foreground">
          {customDraft.claudeApiFormat === "anthropic"
            ? t("providerForm.apiHint")
            : customDraft.claudeApiFormat === "openai_chat"
              ? t("providerForm.apiHintOAI")
              : t("providerForm.apiHintResponses")}
        </p>
      </div>
    );
  };

  const customConfigSummary = (() => {
    if (appId === "codex") {
      const config = generatedSettingsConfig.config;
      return {
        baseUrl:
          typeof config === "string" ? extractCodexBaseUrl(config) : undefined,
        model:
          typeof config === "string"
            ? extractCodexModelName(config)
            : undefined,
      };
    }

    const env = generatedSettingsConfig.env as
      | Record<string, string>
      | undefined;
    return {
      baseUrl: env?.ANTHROPIC_BASE_URL,
      model: env?.ANTHROPIC_MODEL,
    };
  })();

  return (
    <Form {...form}>
      <form
        id="provider-form"
        onSubmit={form.handleSubmit(handleSubmit)}
        className="space-y-6"
      >
        {isCreateMode && (
          <div className="space-y-3 rounded-lg border border-border-default bg-muted/20 p-4">
            <div className="flex flex-wrap items-center justify-between gap-3">
              <div className="flex min-w-0 items-center gap-3">
                <div className="flex size-10 items-center justify-center rounded-lg bg-background border border-border-default">
                  <ProviderIcon
                    icon={appId === "codex" ? "openai" : "anthropic"}
                    name={t("providerPreset.custom")}
                    size={22}
                  />
                </div>
                <div className="min-w-0">
                  <div className="flex items-center gap-2">
                    <p className="truncate text-sm font-semibold">
                      {t("providerPreset.custom")}
                    </p>
                    <Badge variant="secondary" className="rounded-md">
                      {t("providerForm.categoryCustom", {
                        defaultValue: "Custom",
                      })}
                    </Badge>
                  </div>
                  <p className="text-xs text-muted-foreground">
                    {t("providerForm.customApiKeyHint")}
                  </p>
                </div>
              </div>
              <Settings2 className="h-4 w-4 text-muted-foreground" />
            </div>
          </div>
        )}

        <BasicFormFields form={form} />

        {isCreateMode && (
          <div className="space-y-5 rounded-lg border border-border-default p-4">
            <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="custom-api-key">API Key</Label>
                <Input
                  id="custom-api-key"
                  type="password"
                  value={customDraft.apiKey}
                  onChange={(event) =>
                    updateCustomDraft({ apiKey: event.target.value })
                  }
                  placeholder={
                    appId === "codex"
                      ? t("providerForm.codexApiKeyAutoFill")
                      : t("providerForm.apiKeyAutoFill")
                  }
                />
              </div>

              <div className="space-y-2">
                <Label htmlFor="custom-base-url">
                  {appId === "codex"
                    ? t("codexConfig.apiUrlLabel", {
                        defaultValue: "API Endpoint",
                      })
                    : t("providerForm.apiEndpoint")}
                </Label>
                <Input
                  id="custom-base-url"
                  value={customDraft.baseUrl}
                  onChange={(event) =>
                    updateCustomDraft({ baseUrl: event.target.value })
                  }
                  placeholder={
                    appId === "codex"
                      ? t("providerForm.codexApiEndpointPlaceholder")
                      : t("providerForm.apiEndpointPlaceholder")
                  }
                />
              </div>
            </div>

            <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="custom-model">
                  {appId === "claude-desktop"
                    ? t("providerForm.requestModelLabel")
                    : t("providerForm.mainModel")}
                </Label>
                <Input
                  id="custom-model"
                  value={customDraft.model}
                  onChange={(event) =>
                    updateCustomDraft({ model: event.target.value })
                  }
                  placeholder={
                    appId === "codex"
                      ? "gpt-5.5"
                      : t("providerForm.mainModelPlaceholder")
                  }
                />
                <p className="text-xs text-muted-foreground">
                  {appId === "claude-desktop"
                    ? t("claudeDesktop.routeMapHint", {
                        defaultValue:
                          "Fill the upstream model used by the custom route mapping.",
                      })
                    : t("providerForm.modelHint")}
                </p>
              </div>

              {appId !== "codex" && (
                <div className="space-y-2">
                  <Label htmlFor="custom-fast-model">
                    {t("providerForm.fastModel")}
                  </Label>
                  <Input
                    id="custom-fast-model"
                    value={customDraft.fastModel}
                    onChange={(event) =>
                      updateCustomDraft({ fastModel: event.target.value })
                    }
                    placeholder={t("providerForm.fastModelPlaceholder")}
                  />
                  <p className="text-xs text-muted-foreground">
                    {t("providerForm.modelHint")}
                  </p>
                </div>
              )}
            </div>

            <Collapsible open={advancedOpen} onOpenChange={setAdvancedOpen}>
              <CollapsibleTrigger asChild>
                <Button
                  type="button"
                  variant={null}
                  size="sm"
                  className="h-8 justify-start gap-1.5 px-0 text-sm font-medium text-foreground hover:opacity-70"
                >
                  {advancedOpen ? (
                    <ChevronDown className="h-4 w-4" />
                  ) : (
                    <ChevronRight className="h-4 w-4" />
                  )}
                  {t("providerForm.advancedOptionsToggle")}
                </Button>
              </CollapsibleTrigger>
              <CollapsibleContent className="space-y-4 pt-3">
                <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
                  {renderApiFormatSelect()}

                  {appId !== "codex" && (
                    <div className="space-y-2">
                      <Label>{t("providerForm.authField")}</Label>
                      <Select
                        value={customDraft.authField}
                        onValueChange={(value) =>
                          updateCustomDraft({
                            authField: value as ClaudeAuthField,
                          })
                        }
                      >
                        <SelectTrigger>
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          <SelectItem value="ANTHROPIC_AUTH_TOKEN">
                            {t("providerForm.authFieldAuthToken")}
                          </SelectItem>
                          <SelectItem value="ANTHROPIC_API_KEY">
                            {t("providerForm.authFieldApiKey")}
                          </SelectItem>
                        </SelectContent>
                      </Select>
                      <p className="text-xs text-muted-foreground">
                        {t("providerForm.authFieldHint")}
                      </p>
                    </div>
                  )}
                </div>

                <div className="rounded-md border border-border-default bg-muted/20 px-3 py-2 text-xs text-muted-foreground">
                  {customConfigSummary.baseUrl || customConfigSummary.model
                    ? [customConfigSummary.baseUrl, customConfigSummary.model]
                        .filter(Boolean)
                        .join(" · ")
                    : t("providerPreset.hint")}
                </div>
              </CollapsibleContent>
            </Collapsible>
          </div>
        )}

        <FormField
          control={form.control}
          name="settingsConfig"
          render={({ field }) => (
            <FormItem>
              <FormLabel>
                {t("provider.settingsConfig", {
                  defaultValue: "配置 JSON",
                })}
              </FormLabel>
              <FormControl>
                <JsonEditor
                  value={field.value}
                  onChange={field.onChange}
                  rows={isCreateMode ? 10 : 16}
                  placeholder={getDefaultSettingsConfig(appId)}
                  showValidation
                />
              </FormControl>
              <FormMessage />
            </FormItem>
          )}
        />

        {showButtons && (
          <div className="flex justify-end gap-2 pt-4">
            <Button type="button" variant="outline" onClick={onCancel}>
              {t("common.cancel")}
            </Button>
            <Button type="submit">{submitLabel}</Button>
          </div>
        )}
      </form>
    </Form>
  );
}
