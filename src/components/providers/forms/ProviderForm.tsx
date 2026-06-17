import { useCallback, useEffect, useMemo, useState } from "react";
import { useForm } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Form, FormField, FormItem, FormMessage } from "@/components/ui/form";
import { providerSchema, type ProviderFormData } from "@/lib/schemas/provider";
import { settingsApi, type AppId } from "@/lib/api";
import type {
  ClaudeApiFormat,
  ClaudeApiKeyField,
  CodexApiFormat,
  CodexCatalogModel,
  CodexChatReasoning,
  ProviderCategory,
  ProviderMeta,
  ProviderTestConfig,
} from "@/types";
import type { UniversalProviderPreset } from "@/config/universalProviderPresets";
import {
  extractCodexWireApi,
  hasApiKeyField,
  setCodexModelName as setCodexModelNameInConfig,
  setCodexWireApi,
} from "@/utils/providerConfigUtils";
import { mergeProviderMeta } from "@/utils/providerMetaUtils";
import { isNonNegativeDecimalString } from "@/types/usage";
import { getCodexCustomTemplate } from "@/config/codexTemplates";
import { useSettingsQuery } from "@/lib/query";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { BasicFormFields } from "./BasicFormFields";
import { ClaudeDesktopProviderForm } from "./ClaudeDesktopProviderForm";
import { ClaudeFormFields } from "./ClaudeFormFields";
import { CodexFormFields } from "./CodexFormFields";
import CodexConfigEditor from "./CodexConfigEditor";
import { CommonConfigEditor } from "./CommonConfigEditor";
import {
  ProviderAdvancedConfig,
  type PricingModelSourceOption,
} from "./ProviderAdvancedConfig";
import {
  useApiKeyLink,
  useApiKeyState,
  useBaseUrlState,
  useCodexCommonConfig,
  useCodexConfigState,
  useCodexOauth,
  useCodexTomlValidation,
  useCommonConfigSnippet,
  useCopilotAuth,
  useModelState,
  useSpeedTestEndpoints,
  useTemplateValues,
} from "./hooks";
import { resolveManagedAccountId } from "@/lib/authBinding";

const CLAUDE_DEFAULT_CONFIG = JSON.stringify({ env: {} }, null, 2);

const CODEX_DEFAULT_CONFIG = JSON.stringify(
  {
    auth: {},
    config: "",
  },
  null,
  2,
);

const codexApiFormatFromWireApi = (
  wireApi: string | undefined,
): CodexApiFormat | undefined => {
  switch (wireApi?.trim().toLowerCase()) {
    case "chat":
    case "chat_completions":
    case "chat-completions":
    case "openai_chat":
    case "openai-chat":
      return "openai_chat";
    case "responses":
    case "openai_responses":
    case "openai-responses":
      return "openai_responses";
    default:
      return undefined;
  }
};

const normalizeCodexCatalogModelsForSave = (
  models: CodexCatalogModel[],
): CodexCatalogModel[] => {
  const seen = new Set<string>();
  const normalized: CodexCatalogModel[] = [];

  for (const item of models) {
    const model = item.model.trim();
    if (!model || seen.has(model)) continue;
    seen.add(model);

    const displayName = item.displayName?.trim();
    const rawContextWindow = String(item.contextWindow ?? "").replace(
      /[^\d]/g,
      "",
    );
    const contextWindow = rawContextWindow
      ? Number.parseInt(rawContextWindow, 10)
      : undefined;

    normalized.push({
      model,
      ...(displayName ? { displayName } : {}),
      ...(contextWindow && contextWindow > 0 ? { contextWindow } : {}),
    });
  }

  return normalized;
};

const normalizeCodexChatReasoningForSave = (
  value?: CodexChatReasoning,
): CodexChatReasoning | undefined => {
  const supportsEffort = value?.supportsEffort === true;
  const supportsThinking = value?.supportsThinking === true || supportsEffort;
  const hasExplicitConfig = value && Object.keys(value).length > 0;

  if (!supportsThinking && !supportsEffort) {
    return hasExplicitConfig
      ? {
          supportsThinking: false,
          supportsEffort: false,
          thinkingParam: "none",
          effortParam: "none",
          outputFormat: value?.outputFormat ?? "auto",
        }
      : undefined;
  }

  return {
    supportsThinking,
    supportsEffort,
    thinkingParam: supportsThinking
      ? (value?.thinkingParam ?? "thinking")
      : "none",
    effortParam: supportsEffort
      ? (value?.effortParam ?? "reasoning_effort")
      : "none",
    effortValueMode: supportsEffort
      ? (value?.effortValueMode ?? "passthrough")
      : undefined,
    outputFormat: value?.outputFormat ?? "auto",
  };
};

const normalizePricingSource = (
  value?: string,
): PricingModelSourceOption =>
  value === "request" || value === "response" ? value : "inherit";

export interface ProviderFormProps {
  appId: AppId;
  providerId?: string;
  submitLabel: string;
  onSubmit: (values: ProviderFormValues) => Promise<void> | void;
  onCancel: () => void;
  onUniversalPresetSelect?: (preset: UniversalProviderPreset) => void;
  onManageUniversalProviders?: () => void;
  onSubmittingChange?: (isSubmitting: boolean) => void;
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
  showButtons?: boolean;
  isProxyTakeover?: boolean;
}

export type ProviderFormValues = ProviderFormData & {
  presetId?: string;
  presetCategory?: ProviderCategory;
  isPartner?: boolean;
  meta?: ProviderMeta;
  providerKey?: string;
  suggestedDefaults?: unknown;
};

export function ProviderForm(props: ProviderFormProps) {
  if (props.appId === "claude-desktop") {
    return (
      <ClaudeDesktopProviderForm
        {...props}
        initialData={props.initialData ?? undefined}
      />
    );
  }

  return <ProviderFormCustom {...props} />;
}

function ProviderFormCustom({
  appId,
  providerId,
  submitLabel,
  onSubmit,
  onCancel,
  onSubmittingChange,
  initialData,
  showButtons = true,
  isProxyTakeover = false,
}: ProviderFormProps) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const isEditMode = Boolean(initialData);
  const selectedPresetId = "custom";
  const category: ProviderCategory = initialData?.category ?? "custom";
  const nonOfficialCategory =
    category === "official" ? "custom" : category;
  const { data: settingsData } = useSettingsQuery();
  const showCommonConfigNotice =
    settingsData != null && settingsData.commonConfigConfirmed !== true;

  const [draftCustomEndpoints, setDraftCustomEndpoints] = useState<string[]>(
    [],
  );
  const [endpointAutoSelect, setEndpointAutoSelect] = useState<boolean>(
    () => initialData?.meta?.endpointAutoSelect ?? true,
  );
  const [localIsFullUrl, setLocalIsFullUrl] = useState<boolean>(
    () => initialData?.meta?.isFullUrl ?? false,
  );
  const [testConfig, setTestConfig] = useState<ProviderTestConfig>(
    () => initialData?.meta?.testConfig ?? { enabled: false },
  );
  const [pricingConfig, setPricingConfig] = useState<{
    enabled: boolean;
    costMultiplier?: string;
    pricingModelSource: PricingModelSourceOption;
  }>(() => ({
    enabled:
      initialData?.meta?.costMultiplier !== undefined ||
      initialData?.meta?.pricingModelSource !== undefined,
    costMultiplier: initialData?.meta?.costMultiplier,
    pricingModelSource: normalizePricingSource(
      initialData?.meta?.pricingModelSource,
    ),
  }));
  const [isEndpointModalOpen, setIsEndpointModalOpen] = useState(false);
  const [isCodexEndpointModalOpen, setIsCodexEndpointModalOpen] =
    useState(false);
  const [isCommonConfigModalOpen, setIsCommonConfigModalOpen] =
    useState(false);
  const [softIssues, setSoftIssues] = useState<string[] | null>(null);
  const [pendingFormValues, setPendingFormValues] =
    useState<ProviderFormData | null>(null);
  const [isConfirmSubmitting, setIsConfirmSubmitting] = useState(false);
  const [selectedGitHubAccountId, setSelectedGitHubAccountId] = useState<
    string | null
  >(() => resolveManagedAccountId(initialData?.meta, "github_copilot"));
  const [selectedCodexAccountId, setSelectedCodexAccountId] = useState<
    string | null
  >(() => resolveManagedAccountId(initialData?.meta, "codex_oauth"));
  const [codexFastMode, setCodexFastMode] = useState<boolean>(
    () => initialData?.meta?.codexFastMode ?? false,
  );
  const [codexChatReasoning, setCodexChatReasoning] =
    useState<CodexChatReasoning>(
      () => initialData?.meta?.codexChatReasoning ?? {},
    );
  const [customUserAgent, setCustomUserAgent] = useState<string>(
    () => initialData?.meta?.customUserAgent ?? "",
  );
  const [localApiKeyField, setLocalApiKeyField] = useState<ClaudeApiKeyField>(
    () => {
      if (appId !== "claude") return "ANTHROPIC_AUTH_TOKEN";
      if (initialData?.meta?.apiKeyField) return initialData.meta.apiKeyField;
      const env = initialData?.settingsConfig?.env as
        | Record<string, unknown>
        | undefined;
      return env?.ANTHROPIC_API_KEY !== undefined
        ? "ANTHROPIC_API_KEY"
        : "ANTHROPIC_AUTH_TOKEN";
    },
  );
  const [localApiFormat, setLocalApiFormat] = useState<ClaudeApiFormat>(() => {
    if (appId !== "claude") return "anthropic";
    return initialData?.meta?.apiFormat ?? "anthropic";
  });

  const defaultValues: ProviderFormData = useMemo(
    () => ({
      name: initialData?.name ?? "",
      websiteUrl: initialData?.websiteUrl ?? "",
      notes: initialData?.notes ?? "",
      settingsConfig: initialData?.settingsConfig
        ? JSON.stringify(initialData.settingsConfig, null, 2)
        : appId === "codex"
          ? CODEX_DEFAULT_CONFIG
          : CLAUDE_DEFAULT_CONFIG,
      icon: initialData?.icon ?? "",
      iconColor: initialData?.iconColor ?? "",
    }),
    [appId, initialData],
  );

  const form = useForm<ProviderFormData>({
    resolver: zodResolver(providerSchema),
    defaultValues,
    mode: "onSubmit",
  });
  const { isSubmitting } = form.formState;
  const settingsConfig = form.watch("settingsConfig");
  const formWebsiteUrl = form.watch("websiteUrl") || "";
  const formName = form.watch("name");

  useEffect(() => {
    form.reset(defaultValues);
    setDraftCustomEndpoints([]);
    setEndpointAutoSelect(initialData?.meta?.endpointAutoSelect ?? true);
    setLocalIsFullUrl(initialData?.meta?.isFullUrl ?? false);
    setTestConfig(initialData?.meta?.testConfig ?? { enabled: false });
    setPricingConfig({
      enabled:
        initialData?.meta?.costMultiplier !== undefined ||
        initialData?.meta?.pricingModelSource !== undefined,
      costMultiplier: initialData?.meta?.costMultiplier,
      pricingModelSource: normalizePricingSource(
        initialData?.meta?.pricingModelSource,
      ),
    });
    setCodexChatReasoning(initialData?.meta?.codexChatReasoning ?? {});
    setCustomUserAgent(initialData?.meta?.customUserAgent ?? "");
    setSelectedGitHubAccountId(
      resolveManagedAccountId(initialData?.meta, "github_copilot"),
    );
    setSelectedCodexAccountId(
      resolveManagedAccountId(initialData?.meta, "codex_oauth"),
    );
    setCodexFastMode(initialData?.meta?.codexFastMode ?? false);
    if (appId === "claude") {
      setLocalApiFormat(initialData?.meta?.apiFormat ?? "anthropic");
      const env = initialData?.settingsConfig?.env as
        | Record<string, unknown>
        | undefined;
      setLocalApiKeyField(
        initialData?.meta?.apiKeyField ??
          (env?.ANTHROPIC_API_KEY !== undefined
            ? "ANTHROPIC_API_KEY"
            : "ANTHROPIC_AUTH_TOKEN"),
      );
    }
  }, [appId, defaultValues, form, initialData]);

  useEffect(() => {
    onSubmittingChange?.(isSubmitting || isConfirmSubmitting);
  }, [isSubmitting, isConfirmSubmitting, onSubmittingChange]);

  const handleSettingsConfigChange = useCallback(
    (config: string) => {
      form.setValue("settingsConfig", config, {
        shouldDirty: true,
        shouldValidate: false,
      });
    },
    [form],
  );

  const handleCommonConfigConfirm = async () => {
    try {
      if (settingsData) {
        const { webdavSync: _, ...rest } = settingsData;
        await settingsApi.save({ ...rest, commonConfigConfirmed: true });
        await queryClient.invalidateQueries({ queryKey: ["settings"] });
      }
    } catch (error) {
      console.error("Failed to save commonConfigConfirmed:", error);
    }
  };

  const {
    apiKey,
    handleApiKeyChange,
    showApiKey: shouldShowApiKey,
  } = useApiKeyState({
    initialConfig: settingsConfig,
    onConfigChange: handleSettingsConfigChange,
    selectedPresetId,
    category: nonOfficialCategory,
    appType: appId,
    apiKeyField: appId === "claude" ? localApiKeyField : undefined,
  });

  const { baseUrl, handleClaudeBaseUrlChange } = useBaseUrlState({
    appType: appId,
    category: nonOfficialCategory,
    settingsConfig,
    codexConfig: "",
    onSettingsConfigChange: handleSettingsConfigChange,
    onCodexConfigChange: () => {},
  });

  const {
    claudeModel,
    defaultHaikuModel,
    defaultHaikuModelName,
    defaultSonnetModel,
    defaultSonnetModelName,
    defaultOpusModel,
    defaultOpusModelName,
    defaultFableModel,
    defaultFableModelName,
    handleModelChange,
  } = useModelState({
    settingsConfig,
    onConfigChange: handleSettingsConfigChange,
  });

  const {
    codexAuth,
    codexConfig,
    codexApiKey,
    codexBaseUrl,
    codexCatalogModels,
    codexAuthError,
    setCodexAuth,
    setCodexConfig,
    setCodexCatalogModels,
    handleCodexApiKeyChange,
    handleCodexBaseUrlChange,
    handleCodexConfigChange: originalHandleCodexConfigChange,
    resetCodexConfig,
  } = useCodexConfigState({
    initialData: initialData ?? undefined,
  });

  const [localCodexApiFormat, setLocalCodexApiFormat] =
    useState<CodexApiFormat>(() => {
      if (initialData?.meta?.apiFormat === "openai_chat") {
        return "openai_chat";
      }
      if (initialData?.meta?.apiFormat === "openai_responses") {
        return "openai_responses";
      }
      return (
        codexApiFormatFromWireApi(
          extractCodexWireApi(
            typeof initialData?.settingsConfig?.config === "string"
              ? initialData.settingsConfig.config
              : "",
          ),
        ) ?? "openai_responses"
      );
    });

  useEffect(() => {
    if (appId !== "codex") return;
    const nextFormat =
      initialData?.meta?.apiFormat === "openai_chat"
        ? "openai_chat"
        : initialData?.meta?.apiFormat === "openai_responses"
          ? "openai_responses"
          : (codexApiFormatFromWireApi(
              extractCodexWireApi(
                typeof initialData?.settingsConfig?.config === "string"
                  ? initialData.settingsConfig.config
                  : "",
              ),
            ) ?? "openai_responses");
    setLocalCodexApiFormat(nextFormat);
  }, [appId, initialData]);

  const { configError: codexConfigError, debouncedValidate } =
    useCodexTomlValidation();

  const handleCodexConfigChange = useCallback(
    (value: string) => {
      originalHandleCodexConfigChange(value);
      debouncedValidate(value);
    },
    [debouncedValidate, originalHandleCodexConfigChange],
  );

  const handleCodexApiFormatChange = useCallback(
    (format: CodexApiFormat) => {
      setLocalCodexApiFormat(format);
      setCodexConfig((prev) => {
        const updated = setCodexWireApi(prev, "responses");
        debouncedValidate(updated);
        return updated;
      });
    },
    [debouncedValidate, setCodexConfig],
  );

  useEffect(() => {
    if (appId === "codex" && !initialData) {
      const template = getCodexCustomTemplate();
      resetCodexConfig(template.auth, template.config);
      setCodexChatReasoning({});
    }
  }, [appId, initialData, resetCodexConfig]);

  const {
    templateValues,
    templateValueEntries,
    handleTemplateValueChange,
    validateTemplateValues,
  } = useTemplateValues({
    selectedPresetId: null,
    presetEntries: [],
    settingsConfig,
    onConfigChange: handleSettingsConfigChange,
  });

  const {
    useCommonConfig,
    commonConfigSnippet,
    commonConfigError,
    handleCommonConfigToggle,
    handleCommonConfigSnippetChange,
    isExtracting: isClaudeExtracting,
    handleExtract: handleClaudeExtract,
  } = useCommonConfigSnippet({
    settingsConfig,
    onConfigChange: handleSettingsConfigChange,
    initialData: appId === "claude" ? (initialData ?? undefined) : undefined,
    initialEnabled:
      appId === "claude" ? initialData?.meta?.commonConfigEnabled : undefined,
    selectedPresetId,
    enabled: appId === "claude",
  });

  const {
    useCommonConfig: useCodexCommonConfigFlag,
    commonConfigSnippet: codexCommonConfigSnippet,
    commonConfigError: codexCommonConfigError,
    handleCommonConfigToggle: handleCodexCommonConfigToggle,
    handleCommonConfigSnippetChange: handleCodexCommonConfigSnippetChange,
    isExtracting: isCodexExtracting,
    handleExtract: handleCodexExtract,
    clearCommonConfigError: clearCodexCommonConfigError,
  } = useCodexCommonConfig({
    codexConfig,
    onConfigChange: handleCodexConfigChange,
    initialData: appId === "codex" ? (initialData ?? undefined) : undefined,
    initialEnabled:
      appId === "codex" ? initialData?.meta?.commonConfigEnabled : undefined,
    selectedPresetId,
  });

  const { isAuthenticated: isCopilotAuthenticated } = useCopilotAuth();
  const { isAuthenticated: isCodexOauthAuthenticated } = useCodexOauth();

  const isCopilotProvider =
    initialData?.meta?.providerType === "github_copilot" ||
    baseUrl.includes("githubcopilot.com");
  const isCodexOauthProvider =
    initialData?.meta?.providerType === "codex_oauth";

  const {
    shouldShowApiKeyLink: shouldShowClaudeApiKeyLink,
    websiteUrl: claudeWebsiteUrl,
    isPartner: isClaudePartner,
    partnerPromotionKey: claudePartnerPromotionKey,
  } = useApiKeyLink({
    appId: "claude",
    category: nonOfficialCategory,
    selectedPresetId: null,
    presetEntries: [],
    formWebsiteUrl,
  });

  const {
    shouldShowApiKeyLink: shouldShowCodexApiKeyLink,
    websiteUrl: codexWebsiteUrl,
    isPartner: isCodexPartner,
    partnerPromotionKey: codexPartnerPromotionKey,
  } = useApiKeyLink({
    appId: "codex",
    category: nonOfficialCategory,
    selectedPresetId: null,
    presetEntries: [],
    formWebsiteUrl,
  });

  const speedTestEndpoints = useSpeedTestEndpoints({
    appId,
    selectedPresetId: null,
    presetEntries: [],
    baseUrl,
    codexBaseUrl,
    initialData: initialData ?? undefined,
  });

  const handleApiFormatChange = useCallback((format: ClaudeApiFormat) => {
    setLocalApiFormat(format);
  }, []);

  const handleApiKeyFieldChange = useCallback(
    (field: ClaudeApiKeyField) => {
      const prev = localApiKeyField;
      setLocalApiKeyField(field);

      try {
        const config = JSON.parse(settingsConfig || "{}");
        if (config?.env && prev in config.env) {
          const value = config.env[prev];
          delete config.env[prev];
          config.env[field] = value;
          handleSettingsConfigChange(JSON.stringify(config, null, 2));
        }
      } catch {
        // ignore parse errors during editing
      }
    },
    [handleSettingsConfigChange, localApiKeyField, settingsConfig],
  );

  const handleSubmit = async (values: ProviderFormData) => {
    const issues: string[] = [];

    if (appId === "claude" && templateValueEntries.length > 0) {
      const validation = validateTemplateValues();
      if (!validation.isValid && validation.missingField) {
        issues.push(
          t("providerForm.fillParameter", {
            label: validation.missingField.label,
            defaultValue: `请填写 ${validation.missingField.label}`,
          }),
        );
      }
    }

    if (!values.name.trim()) {
      issues.push(
        t("providerForm.fillSupplierName", {
          defaultValue: "请填写供应商名称",
        }),
      );
    }

    const costMultiplier = pricingConfig.costMultiplier?.trim();
    if (
      pricingConfig.enabled &&
      costMultiplier &&
      !isNonNegativeDecimalString(costMultiplier)
    ) {
      toast.error(
        t("settings.globalProxy.defaultCostMultiplierInvalid", {
          defaultValue: "成本倍率必须为非负数",
        }),
      );
      return;
    }

    if (isCopilotProvider && !isCopilotAuthenticated) {
      toast.error(
        t("copilot.loginRequired", {
          defaultValue: "请先登录 GitHub Copilot",
        }),
      );
      return;
    }
    if (isCodexOauthProvider && !isCodexOauthAuthenticated) {
      toast.error(
        t("codexOauth.loginRequired", {
          defaultValue: "请先登录 ChatGPT 账号",
        }),
      );
      return;
    }

    if (appId === "claude") {
      if (!isCodexOauthProvider && !baseUrl.trim()) {
        issues.push(
          t("providerForm.endpointRequired", {
            defaultValue: "非官方供应商请填写 API 端点",
          }),
        );
      }
      if (!isCopilotProvider && !isCodexOauthProvider && !apiKey.trim()) {
        issues.push(
          t("providerForm.apiKeyRequired", {
            defaultValue: "非官方供应商请填写 API Key",
          }),
        );
      }
    } else if (appId === "codex") {
      if (!codexBaseUrl.trim()) {
        issues.push(
          t("providerForm.endpointRequired", {
            defaultValue: "非官方供应商请填写 API 端点",
          }),
        );
      }
      if (!codexApiKey.trim()) {
        issues.push(
          t("providerForm.apiKeyRequired", {
            defaultValue: "非官方供应商请填写 API Key",
          }),
        );
      }
    }

    if (issues.length > 0) {
      setSoftIssues(issues);
      setPendingFormValues(values);
      return;
    }

    await performSubmit(values);
  };

  const performSubmit = async (values: ProviderFormData) => {
    let settingsConfigToSave: string;

    if (appId === "codex") {
      try {
        const authJson = JSON.parse(codexAuth);
        let normalizedCodexConfig = (codexConfig ?? "").trim()
          ? setCodexWireApi(codexConfig ?? "", "responses")
          : (codexConfig ?? "");
        const normalizedCatalogModels =
          localCodexApiFormat === "openai_chat"
            ? normalizeCodexCatalogModelsForSave(codexCatalogModels)
            : [];
        if (normalizedCatalogModels.length > 0) {
          normalizedCodexConfig = setCodexModelNameInConfig(
            normalizedCodexConfig,
            normalizedCatalogModels[0].model,
          );
        }
        const configObj = {
          auth: authJson,
          config: normalizedCodexConfig,
        } as {
          auth: unknown;
          config: string;
          modelCatalog?: { models: CodexCatalogModel[] };
        };
        if (normalizedCatalogModels.length > 0) {
          configObj.modelCatalog = { models: normalizedCatalogModels };
        }
        settingsConfigToSave = JSON.stringify(configObj);
      } catch {
        settingsConfigToSave = values.settingsConfig.trim();
      }
    } else {
      settingsConfigToSave = values.settingsConfig.trim();
    }

    const payload: ProviderFormValues = {
      ...values,
      name: values.name.trim(),
      websiteUrl: values.websiteUrl?.trim() ?? "",
      settingsConfig: settingsConfigToSave,
      presetCategory: initialData?.category ?? "custom",
    };

    if (!isEditMode && draftCustomEndpoints.length > 0) {
      const customEndpointsToSave: Record<
        string,
        import("@/types").CustomEndpoint
      > = draftCustomEndpoints.reduce(
        (acc, url) => {
          const now = Date.now();
          acc[url] = { url, addedAt: now, lastUsed: undefined };
          return acc;
        },
        {} as Record<string, import("@/types").CustomEndpoint>,
      );

      const mergedMeta = mergeProviderMeta(
        initialData?.meta,
        customEndpointsToSave,
      );
      if (mergedMeta !== undefined) {
        payload.meta = mergedMeta;
      }
    }

    const baseMeta: ProviderMeta | undefined =
      payload.meta ?? (initialData?.meta ? { ...initialData.meta } : undefined);
    const providerType = initialData?.meta?.providerType;
    const nextMeta: ProviderMeta = {
      ...(baseMeta ?? {}),
      commonConfigEnabled:
        appId === "claude" ? useCommonConfig : useCodexCommonConfigFlag,
      endpointAutoSelect,
      claudeDesktopMode: undefined,
      providerType,
      authBinding: isCopilotProvider
        ? {
            source: "managed_account",
            authProvider: "github_copilot",
            accountId: selectedGitHubAccountId ?? undefined,
          }
        : isCodexOauthProvider
          ? {
              source: "managed_account",
              authProvider: "codex_oauth",
              accountId: selectedCodexAccountId ?? undefined,
            }
          : undefined,
      githubAccountId:
        isCopilotProvider && selectedGitHubAccountId
          ? selectedGitHubAccountId
          : undefined,
      codexFastMode: isCodexOauthProvider ? codexFastMode : undefined,
      codexChatReasoning:
        appId === "codex" && localCodexApiFormat === "openai_chat"
          ? normalizeCodexChatReasoningForSave(codexChatReasoning)
          : undefined,
      customUserAgent:
        appId === "claude" || appId === "codex"
          ? customUserAgent.trim() || undefined
          : undefined,
      testConfig: testConfig.enabled ? testConfig : undefined,
      costMultiplier: pricingConfig.enabled
        ? pricingConfig.costMultiplier
        : undefined,
      pricingModelSource:
        pricingConfig.enabled && pricingConfig.pricingModelSource !== "inherit"
          ? pricingConfig.pricingModelSource
          : undefined,
      apiFormat:
        appId === "claude" ? localApiFormat : localCodexApiFormat,
      apiKeyField:
        appId === "claude" && localApiKeyField !== "ANTHROPIC_AUTH_TOKEN"
          ? localApiKeyField
          : undefined,
      isFullUrl: localIsFullUrl ? true : undefined,
    };

    if (!isCodexOauthProvider && "codexFastMode" in nextMeta) {
      delete nextMeta.codexFastMode;
    }

    payload.meta = nextMeta;

    await onSubmit(payload);
  };

  const settingsConfigErrorField = (
    <FormField
      control={form.control}
      name="settingsConfig"
      render={() => (
        <FormItem className="space-y-0">
          <FormMessage />
        </FormItem>
      )}
    />
  );

  return (
    <>
      <Form {...form}>
        <form
          id="provider-form"
          onSubmit={form.handleSubmit(handleSubmit)}
          className="space-y-6 glass rounded-xl p-6 border border-white/10"
        >
          <BasicFormFields form={form} />

          {appId === "claude" && (
            <ClaudeFormFields
              providerId={providerId}
              shouldShowApiKey={
                hasApiKeyField(settingsConfig, "claude") ||
                shouldShowApiKey(settingsConfig, isEditMode)
              }
              apiKey={apiKey}
              onApiKeyChange={handleApiKeyChange}
              category={nonOfficialCategory}
              shouldShowApiKeyLink={shouldShowClaudeApiKeyLink}
              websiteUrl={claudeWebsiteUrl}
              isPartner={isClaudePartner}
              partnerPromotionKey={claudePartnerPromotionKey}
              isCopilotPreset={isCopilotProvider}
              isCodexOauthPreset={isCodexOauthProvider}
              usesOAuth={isCopilotProvider || isCodexOauthProvider}
              isCopilotAuthenticated={isCopilotAuthenticated}
              selectedGitHubAccountId={selectedGitHubAccountId}
              onGitHubAccountSelect={setSelectedGitHubAccountId}
              isCodexOauthAuthenticated={isCodexOauthAuthenticated}
              selectedCodexAccountId={selectedCodexAccountId}
              onCodexAccountSelect={setSelectedCodexAccountId}
              codexFastMode={codexFastMode}
              onCodexFastModeChange={setCodexFastMode}
              templateValueEntries={templateValueEntries}
              templateValues={templateValues}
              templatePresetName=""
              onTemplateValueChange={handleTemplateValueChange}
              shouldShowSpeedTest
              baseUrl={baseUrl}
              onBaseUrlChange={handleClaudeBaseUrlChange}
              isEndpointModalOpen={isEndpointModalOpen}
              onEndpointModalToggle={setIsEndpointModalOpen}
              onCustomEndpointsChange={
                isEditMode ? undefined : setDraftCustomEndpoints
              }
              autoSelect={endpointAutoSelect}
              onAutoSelectChange={setEndpointAutoSelect}
              showEndpointTools
              shouldShowModelSelector
              claudeModel={claudeModel}
              defaultHaikuModel={defaultHaikuModel}
              defaultHaikuModelName={defaultHaikuModelName}
              defaultSonnetModel={defaultSonnetModel}
              defaultSonnetModelName={defaultSonnetModelName}
              defaultOpusModel={defaultOpusModel}
              defaultOpusModelName={defaultOpusModelName}
              defaultFableModel={defaultFableModel}
              defaultFableModelName={defaultFableModelName}
              onModelChange={handleModelChange}
              speedTestEndpoints={speedTestEndpoints}
              apiFormat={localApiFormat}
              onApiFormatChange={handleApiFormatChange}
              apiKeyField={localApiKeyField}
              onApiKeyFieldChange={handleApiKeyFieldChange}
              isFullUrl={localIsFullUrl}
              onFullUrlChange={setLocalIsFullUrl}
              customUserAgent={customUserAgent}
              onCustomUserAgentChange={setCustomUserAgent}
            />
          )}

          {appId === "codex" && (
            <CodexFormFields
              providerId={providerId}
              codexApiKey={codexApiKey}
              onApiKeyChange={handleCodexApiKeyChange}
              category={nonOfficialCategory}
              shouldShowApiKeyLink={shouldShowCodexApiKeyLink}
              websiteUrl={codexWebsiteUrl}
              isPartner={isCodexPartner}
              partnerPromotionKey={codexPartnerPromotionKey}
              shouldShowSpeedTest
              codexBaseUrl={codexBaseUrl}
              onBaseUrlChange={handleCodexBaseUrlChange}
              isFullUrl={localIsFullUrl}
              onFullUrlChange={setLocalIsFullUrl}
              isEndpointModalOpen={isCodexEndpointModalOpen}
              onEndpointModalToggle={setIsCodexEndpointModalOpen}
              onCustomEndpointsChange={
                isEditMode ? undefined : setDraftCustomEndpoints
              }
              autoSelect={endpointAutoSelect}
              onAutoSelectChange={setEndpointAutoSelect}
              apiFormat={localCodexApiFormat}
              onApiFormatChange={handleCodexApiFormatChange}
              codexChatReasoning={codexChatReasoning}
              onCodexChatReasoningChange={setCodexChatReasoning}
              catalogModels={codexCatalogModels}
              onCatalogModelsChange={setCodexCatalogModels}
              speedTestEndpoints={speedTestEndpoints}
              customUserAgent={customUserAgent}
              onCustomUserAgentChange={setCustomUserAgent}
            />
          )}

          {appId === "codex" ? (
            <>
              <CodexConfigEditor
                authValue={codexAuth}
                configValue={codexConfig}
                providerName={formName}
                showRemoteCompaction
                isProxyTakeover={isProxyTakeover}
                onAuthChange={setCodexAuth}
                onConfigChange={handleCodexConfigChange}
                useCommonConfig={useCodexCommonConfigFlag}
                onCommonConfigToggle={handleCodexCommonConfigToggle}
                commonConfigSnippet={codexCommonConfigSnippet}
                onCommonConfigSnippetChange={
                  handleCodexCommonConfigSnippetChange
                }
                onCommonConfigErrorClear={clearCodexCommonConfigError}
                commonConfigError={codexCommonConfigError}
                authError={codexAuthError}
                configError={codexConfigError}
                onExtract={handleCodexExtract}
                isExtracting={isCodexExtracting}
              />
              {settingsConfigErrorField}
            </>
          ) : (
            <>
              <CommonConfigEditor
                value={settingsConfig}
                onChange={handleSettingsConfigChange}
                useCommonConfig={useCommonConfig}
                onCommonConfigToggle={handleCommonConfigToggle}
                commonConfigSnippet={commonConfigSnippet}
                onCommonConfigSnippetChange={handleCommonConfigSnippetChange}
                commonConfigError={commonConfigError}
                onEditClick={() => setIsCommonConfigModalOpen(true)}
                isModalOpen={isCommonConfigModalOpen}
                onModalClose={() => setIsCommonConfigModalOpen(false)}
                onExtract={handleClaudeExtract}
                isExtracting={isClaudeExtracting}
              />
              {settingsConfigErrorField}
            </>
          )}

          <ProviderAdvancedConfig
            testConfig={testConfig}
            pricingConfig={pricingConfig}
            onTestConfigChange={setTestConfig}
            onPricingConfigChange={setPricingConfig}
          />

          {showButtons && (
            <div className="flex justify-end gap-2">
              <Button variant="outline" type="button" onClick={onCancel}>
                {t("common.cancel")}
              </Button>
              <Button
                type="submit"
                disabled={isSubmitting || isConfirmSubmitting}
              >
                {submitLabel}
              </Button>
            </div>
          )}
        </form>
      </Form>

      <ConfirmDialog
        isOpen={showCommonConfigNotice}
        variant="info"
        title={t("confirm.commonConfig.title")}
        message={t("confirm.commonConfig.message")}
        confirmText={t("confirm.commonConfig.confirm")}
        onConfirm={() => void handleCommonConfigConfirm()}
        onCancel={() => void handleCommonConfigConfirm()}
      />

      <ConfirmDialog
        isOpen={softIssues !== null && softIssues.length > 0}
        variant="info"
        title={t("providerForm.softValidation.title", {
          defaultValue: "配置存在以下问题",
        })}
        message={
          (softIssues ?? []).map((issue) => `- ${issue}`).join("\n") +
          "\n\n" +
          t("providerForm.softValidation.hint", {
            defaultValue:
              "仍要保存吗？保存后切换此供应商时可能失败，可以之后再补全。",
          })
        }
        confirmText={t("providerForm.softValidation.saveAnyway", {
          defaultValue: "仍要保存",
        })}
        cancelText={t("common.cancel")}
        onConfirm={async () => {
          if (isConfirmSubmitting) return;
          const values = pendingFormValues;
          if (!values) {
            setSoftIssues(null);
            return;
          }
          setIsConfirmSubmitting(true);
          try {
            await performSubmit(values);
            setSoftIssues(null);
            setPendingFormValues(null);
          } catch (error) {
            console.error("[ProviderForm] soft-confirm submit failed:", error);
          } finally {
            setIsConfirmSubmitting(false);
          }
        }}
        onCancel={() => {
          if (isConfirmSubmitting) return;
          setSoftIssues(null);
          setPendingFormValues(null);
        }}
      />
    </>
  );
}
