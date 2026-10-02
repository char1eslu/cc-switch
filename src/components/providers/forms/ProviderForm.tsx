import { useCallback, useEffect, useMemo, useState } from "react";
import { useForm } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Form, FormField, FormItem, FormMessage } from "@/components/ui/form";
import { providerSchema, type ProviderFormData } from "@/lib/schemas/provider";
import type { AppId } from "@/lib/api";
import type {
  ClaudeApiFormat,
  ClaudeApiKeyField,
  ClaudeStackModel,
  CodexApiFormat,
  CodexCatalogModel,
  CodexChatReasoning,
  ProviderCategory,
  ProviderMeta,
  ProviderTestConfig,
} from "@/types";
import type { UniversalProviderPreset } from "@/config/universalProviderPresets";
import {
  codexApiFormatFromWireApi,
  extractCodexModelName,
  extractCodexWireApi,
  hasApiKeyField,
  setCodexModelName as setCodexModelNameInConfig,
  setCodexWireApi,
} from "@/utils/providerConfigUtils";
import { mergeProviderMeta } from "@/utils/providerMetaUtils";
import { overlayClaudeProviderFields } from "@/utils/claudeEditorOverlay";
import { getCodexCustomTemplate } from "@/config/codexTemplates";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { BasicFormFields } from "./BasicFormFields";
import { ClaudeDesktopProviderForm } from "./ClaudeDesktopProviderForm";
import { ClaudeFormFields } from "./ClaudeFormFields";
import {
  claudeStackModelsFromEnv,
  createClaudeStackModelRow,
  normalizeClaudeStackModels,
  type ClaudeStackModelRow,
} from "./ClaudeStackModelsField";
import { setClaudeOneMMarker } from "./hooks/useModelState";
import { useSettingsQuery } from "@/lib/query";
import { CodexFormFields } from "./CodexFormFields";
import CodexConfigEditor from "./CodexConfigEditor";
import { CommonConfigEditor } from "./CommonConfigEditor";
import { ProviderAdvancedConfig } from "./ProviderAdvancedConfig";
import {
  useApiKeyLink,
  useApiKeyState,
  useBaseUrlState,
  useCodexConfigState,
  useCodexOauth,
  useCodexTomlValidation,
  useDraftEditorProjection,
  useModelState,
  useSpeedTestEndpoints,
  useTemplateValues,
  type EditorBaseChange,
} from "./hooks";
import type { ProviderEditorInactiveField } from "@/lib/api/providers";
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

const CODEX_API_FORMATS: readonly CodexApiFormat[] = [
  "openai_responses",
  "openai_chat",
  "anthropic",
];

// meta.apiFormat 优先；缺失时回落到 TOML wire_api 推断，最后默认原生 Responses。
// 不要在此处逐个 case 列举格式：漏掉一个（例如 anthropic）会让已保存的供应商
// 静默退回 Responses，表单显示的协议与后端实际路由不一致。
const resolveCodexApiFormat = (
  initialData?: { meta?: ProviderMeta; settingsConfig?: any } | null,
): CodexApiFormat => {
  const metaFormat = initialData?.meta?.apiFormat;
  if (metaFormat && CODEX_API_FORMATS.includes(metaFormat as CodexApiFormat)) {
    return metaFormat as CodexApiFormat;
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
    const reasoningLevels = Array.from(
      new Set(
        item.reasoningLevels?.map((level) => level.trim()).filter(Boolean) ??
          [],
      ),
    );
    const defaultReasoningLevel = item.defaultReasoningLevel?.trim();
    const validDefaultReasoningLevel = reasoningLevels.includes(
      defaultReasoningLevel ?? "",
    )
      ? defaultReasoningLevel
      : undefined;

    normalized.push({
      model,
      ...(displayName ? { displayName } : {}),
      ...(contextWindow && contextWindow > 0 ? { contextWindow } : {}),
      ...(reasoningLevels.length > 0 ? { reasoningLevels } : {}),
      ...(validDefaultReasoningLevel
        ? { defaultReasoningLevel: validDefaultReasoningLevel }
        : {}),
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

/**
 * 表单里的 Stack 模型列表：行里配了就用它（空列表是用户清空了），没配是 `null`（跟着模型
 * 映射）。
 */
const initialClaudeStackRows = (
  models: ClaudeStackModel[] | undefined,
): ClaudeStackModelRow[] | null =>
  models ? models.map((model) => createClaudeStackModelRow(model)) : null;

/** 列表的第一个模型（默认模型）写进 `ANTHROPIC_MODEL` 的样子：1M 模型带标记。 */
const claudeStackDefaultModel = (
  rows: ClaudeStackModel[],
): string | undefined => {
  const first = normalizeClaudeStackModels(rows)[0];
  return first && setClaudeOneMMarker(first.model, first.oneM === true);
};

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
  /** 编辑器里行保存着、但不随切换生效的字段（Claude Code、Codex、Gemini CLI、Grok Build）。 */
  inactiveFields?: ProviderEditorInactiveField[];
  /**
   * Claude 新增：当前 live 去掉当前供应商的关键字段后的样子。预设的关键字段套在它上面
   * 显示，保存时其余部分的改动写进 live。
   */
  claudeLiveBase?: Record<string, unknown>;
  /**
   * Codex、Gemini CLI、Grok Build 新增：预设或模板投影到当前配置文件上之后的内容，保存时
   * 作为三方比较的底；投影进行中或失败时为 `null`。
   */
  onEditorBaseChange?: EditorBaseChange;
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
  inactiveFields,
  claudeLiveBase,
  onEditorBaseChange,
}: ProviderFormProps) {
  const { t } = useTranslation();
  const isEditMode = Boolean(initialData);
  const selectedPresetId = "custom";
  const category: ProviderCategory = initialData?.category ?? "custom";
  const nonOfficialCategory = category === "official" ? "custom" : category;

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
  const [isEndpointModalOpen, setIsEndpointModalOpen] = useState(false);
  const [isCodexEndpointModalOpen, setIsCodexEndpointModalOpen] =
    useState(false);
  const [softIssues, setSoftIssues] = useState<string[] | null>(null);
  const [pendingFormValues, setPendingFormValues] =
    useState<ProviderFormData | null>(null);
  const [isConfirmSubmitting, setIsConfirmSubmitting] = useState(false);
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
        : appId === "claude" && claudeLiveBase
          ? JSON.stringify(
              overlayClaudeProviderFields(
                claudeLiveBase,
                JSON.parse(CLAUDE_DEFAULT_CONFIG) as Record<string, unknown>,
              ),
              null,
              2,
            )
          : appId === "codex"
            ? CODEX_DEFAULT_CONFIG
            : CLAUDE_DEFAULT_CONFIG,
      icon: initialData?.icon ?? "",
      iconColor: initialData?.iconColor ?? "",
    }),
    [appId, initialData, claudeLiveBase],
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
    setCodexChatReasoning(initialData?.meta?.codexChatReasoning ?? {});
    setCustomUserAgent(initialData?.meta?.customUserAgent ?? "");
    setSelectedCodexAccountId(
      resolveManagedAccountId(initialData?.meta, "codex_oauth"),
    );
    setCodexFastMode(initialData?.meta?.codexFastMode ?? false);
    setClaudeStackRows(initialClaudeStackRows(initialData?.meta?.stackModels));
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

  // Stack 模式：Claude Code 的模型列表存在 meta.stackModels；Codex 复用模型目录。`null` 表示
  // 没配列表，显示（后端也按它发布）模型映射里的模型，跟着映射变；动过列表才存。
  const [claudeStackRows, setClaudeStackRows] = useState<
    ClaudeStackModelRow[] | null
  >(() => initialClaudeStackRows(initialData?.meta?.stackModels));
  // 按行里实际写的映射算（和后端一样），不用 useModelState 回填过的值。
  const claudeSettingsConfig =
    appId === "claude" ? form.watch("settingsConfig") : "";
  const mappedClaudeStackRows = useMemo(() => {
    let env: Record<string, unknown> | undefined;
    try {
      const parsed = JSON.parse(claudeSettingsConfig || "{}") as {
        env?: Record<string, unknown>;
      } | null;
      env = parsed?.env;
    } catch {
      env = undefined;
    }
    return claudeStackModelsFromEnv(env).map((model) =>
      createClaudeStackModelRow(model),
    );
  }, [claudeSettingsConfig]);
  const shownClaudeStackRows = claudeStackRows ?? mappedClaudeStackRows;
  // 列表的第一个就是默认模型：它一变（设为默认、删掉、改名），`ANTHROPIC_MODEL` 当场跟着变，
  // 两种布局共用这份状态，切到完整表单也看得到；没动第一个就不碰。删光了也不碰。
  const handleClaudeStackRowsChange = (rows: ClaudeStackModelRow[]) => {
    setClaudeStackRows(rows);
    const next = claudeStackDefaultModel(rows);
    if (next && next !== claudeStackDefaultModel(shownClaudeStackRows)) {
      handleModelChange("ANTHROPIC_MODEL", next);
    }
  };

  // 设置里开了 Stack 模式时，Claude Code / Codex 的第三方供应商默认用简化面板（连接 + 模型
  // 列表 + 高级）；可以切到完整表单，两种布局共用同一份表单状态。
  const { data: settingsData } = useSettingsQuery();
  const [preferFullForm, setPreferFullForm] = useState(false);

  const {
    codexAuth,
    codexConfig,
    codexApiKey,
    codexBaseUrl,
    codexModel,
    codexCatalogModels,
    codexAuthError,
    setCodexAuth,
    setCodexConfig,
    setCodexCatalogModels,
    handleCodexApiKeyChange,
    handleCodexBaseUrlChange,
    handleCodexModelChange,
    handleCodexConfigChange: originalHandleCodexConfigChange,
    resetCodexConfig,
  } = useCodexConfigState({
    initialData: initialData ?? undefined,
  });

  const [localCodexApiFormat, setLocalCodexApiFormat] =
    useState<CodexApiFormat>(() => resolveCodexApiFormat(initialData));

  useEffect(() => {
    if (appId !== "codex") return;
    setLocalCodexApiFormat(resolveCodexApiFormat(initialData));
  }, [appId, initialData]);

  // Anthropic 上游的鉴权字段：默认 ANTHROPIC_AUTH_TOKEN（Authorization: Bearer），
  // 可切到 ANTHROPIC_API_KEY（x-api-key）。二者互斥，只发其一。
  const [localCodexAnthropicAuthField, setLocalCodexAnthropicAuthField] =
    useState<ClaudeApiKeyField>(
      () => initialData?.meta?.apiKeyField ?? "ANTHROPIC_AUTH_TOKEN",
    );
  const [localImpersonateClaudeCode, setLocalImpersonateClaudeCode] =
    useState<boolean>(() => initialData?.meta?.impersonateClaudeCode === true);
  const [localCodexMaxOutputTokens, setLocalCodexMaxOutputTokens] =
    useState<string>(() => {
      const value = initialData?.meta?.maxOutputTokens;
      return typeof value === "number" && value > 0 ? String(value) : "";
    });

  useEffect(() => {
    if (appId !== "codex") return;
    setLocalCodexAnthropicAuthField(
      initialData?.meta?.apiKeyField ?? "ANTHROPIC_AUTH_TOKEN",
    );
    setLocalImpersonateClaudeCode(
      initialData?.meta?.impersonateClaudeCode === true,
    );
    const maxOut = initialData?.meta?.maxOutputTokens;
    setLocalCodexMaxOutputTokens(
      typeof maxOut === "number" && maxOut > 0 ? String(maxOut) : "",
    );
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

  // 新增：预设或模板投影到当前配置文件上显示。每次重置显示内容都要重新投影，否则保存时
  // 三方比较的底和显示内容对不上。
  const { projectDraft } = useDraftEditorProjection(appId, onEditorBaseChange);
  const projectCodexDraft = useCallback(
    (auth: Record<string, unknown>, config: string, category?: string) =>
      projectDraft({ auth, config }, category, (shown) =>
        setCodexConfig(typeof shown.config === "string" ? shown.config : ""),
      ),
    [projectDraft, setCodexConfig],
  );

  useEffect(() => {
    if (appId === "codex" && !initialData) {
      const template = getCodexCustomTemplate();
      resetCodexConfig(template.auth, template.config);
      setCodexChatReasoning({});
      projectCodexDraft(template.auth, template.config);
    }
  }, [appId, initialData, resetCodexConfig, projectCodexDraft]);

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

  const { isAuthenticated: isCodexOauthAuthenticated } = useCodexOauth();

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

  // 官方判定只认显式 category === "official"（SSOT，见 ProviderCard 的同名说明），
  // 所以这里不再像上游那样额外排除 Codex 官方账号。
  const stackLayoutAvailable =
    settingsData?.enableStackMode === true &&
    (appId === "claude" || appId === "codex") &&
    category !== "official";
  const useStackLayout = stackLayoutAvailable && !preferFullForm;

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
      if (!isCodexOauthProvider && !apiKey.trim()) {
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

    // Stack 布局：一个模型都没有的供应商加进 Stack 后不会出现在模型选择器里。
    if (useStackLayout) {
      const hasNoModels =
        appId === "claude"
          ? normalizeClaudeStackModels(shownClaudeStackRows).length === 0
          : normalizeCodexCatalogModelsForSave(codexCatalogModels).length ===
              0 && !extractCodexModelName(codexConfig ?? "");
      if (hasNoModels) {
        issues.push(
          t("providerForm.stackLayout.noModels", {
            defaultValue:
              "模型列表为空：叠加这家后，模型选择器里不会多出它的模型",
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
        // 模型映射与「路由接管」解耦：对所有非官方供应商，填了就持久化
        //（Chat 生成兼容路由、原生 Responses 生成 model-catalogs.json），
        // 留空归一化为 [] 即不写。后端只看 modelCatalog.models 是否非空。
        const normalizedCatalogModels =
          category !== "official"
            ? normalizeCodexCatalogModelsForSave(codexCatalogModels)
            : [];
        // 默认模型字段会随输入把顶层 `model` 写进 TOML；只有它空着才回落到目录第一行，
        // 这样「只补映射」保持原行为。Stack 布局的 ★ 也是当场写 `model`，这里一样只补空的。
        if (
          normalizedCatalogModels.length > 0 &&
          !extractCodexModelName(normalizedCodexConfig)
        ) {
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
    // 动过列表才存；清空了存空列表（什么都不发布），和没配（跟着映射）区分开。
    const stackModels =
      appId === "claude" && category !== "official" && claudeStackRows
        ? normalizeClaudeStackModels(claudeStackRows)
        : undefined;
    const nextMeta: ProviderMeta = {
      ...(baseMeta ?? {}),
      // Claude Code、Codex 的通用配置片段已冻结：沿用行里原有的标记，新增时由后端写
      // true（兼容旧版）。前端的片段编辑已移除，片段由后端统一维护。
      commonConfigEnabled:
        appId === "claude" || appId === "codex"
          ? initialData?.meta?.commonConfigEnabled
          : undefined,
      endpointAutoSelect,
      claudeDesktopMode: undefined,
      providerType,
      authBinding: isCodexOauthProvider
        ? {
            source: "managed_account",
            authProvider: "codex_oauth",
            accountId: selectedCodexAccountId ?? undefined,
          }
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
      apiFormat: appId === "claude" ? localApiFormat : localCodexApiFormat,
      // 两条路径共用 apiKeyField：Claude 直连，以及 Codex→Anthropic 桥。
      // 都只在非默认值时落库，保持默认 ANTHROPIC_AUTH_TOKEN 不写入。
      apiKeyField:
        appId === "claude"
          ? localApiKeyField !== "ANTHROPIC_AUTH_TOKEN"
            ? localApiKeyField
            : undefined
          : appId === "codex" &&
              localCodexApiFormat === "anthropic" &&
              localCodexAnthropicAuthField !== "ANTHROPIC_AUTH_TOKEN"
            ? localCodexAnthropicAuthField
            : undefined,
      impersonateClaudeCode:
        appId === "codex" &&
        localCodexApiFormat === "anthropic" &&
        localImpersonateClaudeCode
          ? true
          : undefined,
      maxOutputTokens:
        appId === "codex" && localCodexApiFormat === "anthropic"
          ? (() => {
              const parsed = Number.parseInt(localCodexMaxOutputTokens, 10);
              return Number.isFinite(parsed) && parsed > 0 ? parsed : undefined;
            })()
          : undefined,
      isFullUrl: localIsFullUrl ? true : undefined,
      stackModels,
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

          {stackLayoutAvailable && (
            <div className="-mt-2 flex justify-end">
              <Button
                type="button"
                variant="link"
                size="sm"
                className="h-auto p-0 text-xs text-muted-foreground"
                onClick={() => setPreferFullForm((value) => !value)}
              >
                {useStackLayout
                  ? t("providerForm.stackLayout.fullForm", {
                      defaultValue: "显示完整表单",
                    })
                  : t("providerForm.stackLayout.simpleForm", {
                      defaultValue: "返回叠加模式的简化表单",
                    })}
              </Button>
            </div>
          )}

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
              isCodexOauthPreset={isCodexOauthProvider}
              usesOAuth={isCodexOauthProvider}
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
              variant={useStackLayout ? "stack" : "classic"}
              stackModelRows={shownClaudeStackRows}
              onStackModelRowsChange={handleClaudeStackRowsChange}
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
              codexModel={codexModel}
              onModelChange={handleCodexModelChange}
              apiFormat={localCodexApiFormat}
              onApiFormatChange={handleCodexApiFormatChange}
              anthropicAuthField={localCodexAnthropicAuthField}
              onAnthropicAuthFieldChange={setLocalCodexAnthropicAuthField}
              impersonateClaudeCode={localImpersonateClaudeCode}
              onImpersonateClaudeCodeChange={setLocalImpersonateClaudeCode}
              maxOutputTokens={localCodexMaxOutputTokens}
              onMaxOutputTokensChange={setLocalCodexMaxOutputTokens}
              codexChatReasoning={codexChatReasoning}
              onCodexChatReasoningChange={setCodexChatReasoning}
              catalogModels={codexCatalogModels}
              onCatalogModelsChange={setCodexCatalogModels}
              speedTestEndpoints={speedTestEndpoints}
              customUserAgent={customUserAgent}
              onCustomUserAgentChange={setCustomUserAgent}
              variant={useStackLayout ? "stack" : "classic"}
            />
          )}

          {/* 配置编辑器：Stack 简化布局下不显示，Codex / 其他分别使用不同的编辑器 */}
          {useStackLayout ? (
            settingsConfigErrorField
          ) : appId === "codex" ? (
            <>
              <CodexConfigEditor
                authValue={codexAuth}
                configValue={codexConfig}
                providerName={formName}
                showRemoteCompaction
                isProxyTakeover={isProxyTakeover}
                onAuthChange={setCodexAuth}
                onConfigChange={handleCodexConfigChange}
                authError={codexAuthError}
                configError={codexConfigError}
                inactiveFields={inactiveFields}
              />
              {settingsConfigErrorField}
            </>
          ) : (
            <>
              <CommonConfigEditor
                value={settingsConfig}
                onChange={handleSettingsConfigChange}
                inactiveFields={inactiveFields}
              />
              {settingsConfigErrorField}
            </>
          )}

          <ProviderAdvancedConfig
            testConfig={testConfig}
            onTestConfigChange={setTestConfig}
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
