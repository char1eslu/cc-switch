import React from "react";
import type { AppId } from "@/lib/api/types";
import { ClaudeIcon, CodexIcon } from "@/components/BrandIcons";

export interface AppConfig {
  label: string;
  icon: React.ReactNode;
  activeClass: string;
  badgeClass: string;
}

export const APP_IDS: AppId[] = ["claude", "claude-desktop", "codex"];

/**
 * 切换只替换关键字段的应用：供应商编辑器显示「切到这个供应商之后配置文件的样子」，由后端
 * `ProviderService::editor_view` 投影。
 */
export const EDITOR_VIEW_APP_IDS: AppId[] = ["claude", "codex"];

export function usesEditorView(appId: AppId): boolean {
  return EDITOR_VIEW_APP_IDS.includes(appId);
}

export type ProxyAppId = Extract<AppId, "claude" | "codex">;

/** Apps with a complete local gateway + failover data plane. */
export const PROXY_APP_IDS: ProxyAppId[] = ["claude", "codex"];

export function isProxyAppId(appId: string): appId is ProxyAppId {
  return (PROXY_APP_IDS as string[]).includes(appId);
}

/** App IDs shown in Skills panels. */
export const SKILLS_APP_IDS: AppId[] = ["claude", "codex"];

/** App IDs shown in MCP panels. Claude Desktop 走 gateway 模式，本地 mcpServers
 * 被忽略，不支持 MCP 同步，故不在此列出。 */
export const MCP_APP_IDS: AppId[] = ["claude", "codex"];

export const APP_ICON_MAP: Record<AppId, AppConfig> = {
  claude: {
    label: "Claude",
    icon: <ClaudeIcon size={14} />,
    activeClass:
      "bg-orange-500/10 ring-1 ring-orange-500/20 hover:bg-orange-500/20 text-orange-600 dark:text-orange-400",
    badgeClass:
      "bg-orange-500/10 text-orange-700 dark:text-orange-300 hover:bg-orange-500/20 border-0 gap-1.5",
  },
  "claude-desktop": {
    label: "Claude Desktop",
    icon: <ClaudeIcon size={14} />,
    activeClass:
      "bg-amber-500/10 ring-1 ring-amber-500/20 hover:bg-amber-500/20 text-amber-700 dark:text-amber-300",
    badgeClass:
      "bg-amber-500/10 text-amber-700 dark:text-amber-300 hover:bg-amber-500/20 border-0 gap-1.5",
  },
  codex: {
    label: "Codex",
    icon: <CodexIcon size={14} />,
    activeClass:
      "bg-green-500/10 ring-1 ring-green-500/20 hover:bg-green-500/20 text-green-600 dark:text-green-400",
    badgeClass:
      "bg-green-500/10 text-green-700 dark:text-green-300 hover:bg-green-500/20 border-0 gap-1.5",
  },
};
