import { useEffect } from "react";
import { useForm } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from "@/components/ui/form";
import { providerSchema, type ProviderFormData } from "@/lib/schemas/provider";
import type {
  ProviderCategory,
  ProviderMeta,
  ProviderTestConfig,
} from "@/types";
import type { AppId } from "@/lib/api";
import JsonEditor from "@/components/JsonEditor";
import { BasicFormFields } from "./BasicFormFields";

const DEFAULT_CONFIG_BY_APP: Record<AppId, string> = {
  claude: JSON.stringify({ env: {} }, null, 2),
  "claude-desktop": JSON.stringify(
    {
      env: {
        ANTHROPIC_BASE_URL: "",
        ANTHROPIC_AUTH_TOKEN: "",
      },
    },
    null,
    2,
  ),
  codex: JSON.stringify({ auth: {}, config: "" }, null, 2),
};

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

  const getInitialValues = (): ProviderFormData => ({
    name: initialData?.name ?? "",
    notes: initialData?.notes ?? "",
    websiteUrl: initialData?.websiteUrl ?? "",
    settingsConfig: initialData?.settingsConfig
      ? JSON.stringify(initialData.settingsConfig, null, 2)
      : DEFAULT_CONFIG_BY_APP[appId],
    icon: initialData?.icon ?? "",
    iconColor: initialData?.iconColor ?? "",
  });

  const form = useForm<ProviderFormData>({
    resolver: zodResolver(providerSchema),
    defaultValues: getInitialValues(),
  });

  useEffect(() => {
    form.reset(getInitialValues());
  }, [appId, form, initialData]);

  const handleSubmit = async (values: ProviderFormData) => {
    if (!values.name.trim()) {
      toast.error(t("provider.nameRequired", { defaultValue: "请输入名称" }));
      return;
    }

    onSubmittingChange?.(true);
    try {
      await onSubmit({
        ...values,
        presetCategory: initialData?.category,
        meta: initialData?.meta,
      });
    } finally {
      onSubmittingChange?.(false);
    }
  };

  return (
    <Form {...form}>
      <form
        id="provider-form"
        onSubmit={form.handleSubmit(handleSubmit)}
        className="space-y-6"
      >
        <BasicFormFields form={form} />

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
                  rows={16}
                  placeholder={DEFAULT_CONFIG_BY_APP[appId]}
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
