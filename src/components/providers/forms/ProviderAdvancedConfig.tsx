import { useTranslation } from "react-i18next";
import { useState, useEffect, type KeyboardEvent } from "react";
import { ChevronDown, ChevronRight, FlaskConical } from "lucide-react";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import type { ProviderTestConfig } from "@/types";

// 计费配置段（供应商级成本倍率 / 计费模式）已随上游 46fa2e0e 移除：
// 中继本身就在「官方价 × 自己的倍率」计价，CC Switch 再存一份只会漂移；
// 且它只对经本地路由的请求生效，直连模式下设了也没有效果。
// 模型测试配置段是 fork 自有能力，保留。
interface ProviderAdvancedConfigProps {
  testConfig: ProviderTestConfig;
  onTestConfigChange: (config: ProviderTestConfig) => void;
}

export function ProviderAdvancedConfig({
  testConfig,
  onTestConfigChange,
}: ProviderAdvancedConfigProps) {
  const { t } = useTranslation();
  const [isTestConfigOpen, setIsTestConfigOpen] = useState(testConfig.enabled);

  useEffect(() => {
    setIsTestConfigOpen(testConfig.enabled);
  }, [testConfig.enabled]);

  const handleDisclosureKeyDown = (
    event: KeyboardEvent<HTMLDivElement>,
    toggle: () => void,
  ) => {
    if (event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    toggle();
  };

  return (
    <div className="space-y-4">
      <div className="rounded-lg border border-border/50 bg-muted/20">
        <div
          role="button"
          tabIndex={0}
          className="flex w-full items-center justify-between p-4 hover:bg-muted/30 transition-colors"
          onClick={() => setIsTestConfigOpen(!isTestConfigOpen)}
          onKeyDown={(event) =>
            handleDisclosureKeyDown(event, () =>
              setIsTestConfigOpen((current) => !current),
            )
          }
        >
          <div className="flex items-center gap-3">
            <FlaskConical className="h-4 w-4 text-muted-foreground" />
            <span className="font-medium">
              {t("providerAdvanced.testConfig", {
                defaultValue: "连通检测配置",
              })}
            </span>
          </div>
          <div className="flex items-center gap-3">
            <div
              className="flex items-center gap-2"
              onClick={(e) => e.stopPropagation()}
            >
              <Label
                htmlFor="test-config-enabled"
                className="text-sm text-muted-foreground"
              >
                {t("providerAdvanced.useCustomConfig", {
                  defaultValue: "使用单独配置",
                })}
              </Label>
              <Switch
                id="test-config-enabled"
                checked={testConfig.enabled}
                onCheckedChange={(checked) => {
                  onTestConfigChange({ ...testConfig, enabled: checked });
                  if (checked) setIsTestConfigOpen(true);
                }}
              />
            </div>
            {isTestConfigOpen ? (
              <ChevronDown className="h-4 w-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="h-4 w-4 text-muted-foreground" />
            )}
          </div>
        </div>
        <div
          className={cn(
            "overflow-hidden transition-all duration-200",
            isTestConfigOpen
              ? "max-h-[500px] opacity-100"
              : "max-h-0 opacity-0",
          )}
        >
          <div className="border-t border-border/50 p-4 space-y-4">
            <p className="text-sm text-muted-foreground">
              {t("providerAdvanced.testConfigDesc", {
                defaultValue:
                  "为此供应商配置单独的连通检测参数（超时/阈值/重试），不启用时使用全局配置。",
              })}
            </p>
            <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
              <div className="space-y-2">
                <Label htmlFor="test-timeout">
                  {t("providerAdvanced.timeoutSecs", {
                    defaultValue: "超时时间（秒）",
                  })}
                </Label>
                <Input
                  id="test-timeout"
                  type="number"
                  min={1}
                  max={60}
                  value={testConfig.timeoutSecs || ""}
                  onChange={(e) =>
                    onTestConfigChange({
                      ...testConfig,
                      timeoutSecs: e.target.value
                        ? parseInt(e.target.value, 10)
                        : undefined,
                    })
                  }
                  placeholder="8"
                  disabled={!testConfig.enabled}
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="degraded-threshold">
                  {t("providerAdvanced.degradedThreshold", {
                    defaultValue: "降级阈值（毫秒）",
                  })}
                </Label>
                <Input
                  id="degraded-threshold"
                  type="number"
                  min={100}
                  max={60000}
                  value={testConfig.degradedThresholdMs || ""}
                  onChange={(e) =>
                    onTestConfigChange({
                      ...testConfig,
                      degradedThresholdMs: e.target.value
                        ? parseInt(e.target.value, 10)
                        : undefined,
                    })
                  }
                  placeholder="6000"
                  disabled={!testConfig.enabled}
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="max-retries">
                  {t("providerAdvanced.maxRetries", {
                    defaultValue: "最大重试次数",
                  })}
                </Label>
                <Input
                  id="max-retries"
                  type="number"
                  min={0}
                  max={5}
                  value={testConfig.maxRetries ?? ""}
                  onChange={(e) =>
                    onTestConfigChange({
                      ...testConfig,
                      maxRetries: e.target.value
                        ? parseInt(e.target.value, 10)
                        : undefined,
                    })
                  }
                  placeholder="1"
                  disabled={!testConfig.enabled}
                />
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
