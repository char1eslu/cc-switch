/**
 * Claude Desktop 预设供应商配置模板
 *
 * 形态与 Claude Code 预设不同：
 * - baseUrl 是顶级字段，而不是 settingsConfig.env.ANTHROPIC_BASE_URL
 * - 模型信息以"Desktop 可见模型 ID → 上游模型"表达，
 *   对应后端 ClaudeDesktopModelRoute 的 routeId / model
 *
 * 翻译来源：src/config/claudeProviderPresets.ts（排除 OAuth 与不兼容预设）
 */
import { ProviderCategory } from "../types";
import type { PresetTheme } from "./claudeProviderPresets";

export type ClaudeDesktopApiFormat =
  | "anthropic"
  | "openai_chat"
  | "openai_responses";

export interface ClaudeDesktopRoutePreset {
  routeId: string;
  upstreamModel: string;
  labelOverride?: string;
  supports1m: boolean;
}

/**
 * Claude Desktop 3P fail-all 校验接受的角色名。Desktop 1.12603.1+ 起白名单
 * 纳入 fable（app.asar 内 ["sonnet","opus","haiku","fable","mythos"]，实测
 * 2026-06-13）；此前 1.6259.1 仅接受 sonnet/opus/haiku。mythos 官方未公开
 * 发布，暂不暴露给用户。所有预设工厂、表单角色下拉、后端
 * `next_catalog_safe_route_id` 都从此映射派生 routeId，避免散落硬编码。
 */
export const CLAUDE_DESKTOP_ROLE_ROUTE_IDS = {
  sonnet: "claude-sonnet-4-6",
  opus: "claude-opus-4-8",
  fable: "claude-fable-5",
  haiku: "claude-haiku-4-5",
} as const;

export type ClaudeDesktopRoleId = keyof typeof CLAUDE_DESKTOP_ROLE_ROUTE_IDS;

export interface ClaudeDesktopProviderPreset {
  name: string;
  nameKey?: string;
  websiteUrl: string;
  apiKeyUrl?: string;
  category?: ProviderCategory;
  isPartner?: boolean;
  partnerPromotionKey?: string;

  baseUrl: string;
  apiKeyField?: "ANTHROPIC_AUTH_TOKEN" | "ANTHROPIC_API_KEY";

  mode: "direct" | "proxy";
  apiFormat?: ClaudeDesktopApiFormat;
  modelRoutes?: ClaudeDesktopRoutePreset[];
  providerType?: "codex_oauth";
  requiresOAuth?: boolean;

  endpointCandidates?: string[];
  theme?: PresetTheme;
  icon?: string;
  iconColor?: string;
}

/**
 * 非 Claude 上游模型用此工厂：route ID 使用 Claude Desktop 能通过校验的
 * Sonnet/Opus/Haiku 路由，真实品牌名只写入 labelOverride 和 upstreamModel。
 */
const brandedRoutes = (
  sonnet: string,
  opus: string,
  haiku: string,
  supports1m = false,
): ClaudeDesktopRoutePreset[] => {
  const seenUpstream = new Set<string>();
  return [
    { routeId: CLAUDE_DESKTOP_ROLE_ROUTE_IDS.sonnet, upstreamModel: sonnet },
    { routeId: CLAUDE_DESKTOP_ROLE_ROUTE_IDS.opus, upstreamModel: opus },
    { routeId: CLAUDE_DESKTOP_ROLE_ROUTE_IDS.haiku, upstreamModel: haiku },
  ]
    .map(({ routeId, upstreamModel }) => ({
      routeId,
      upstreamModel,
      labelOverride: upstreamModel,
      supports1m,
    }))
    .filter((route) => {
      if (seenUpstream.has(route.upstreamModel)) {
        return false;
      }
      seenUpstream.add(route.upstreamModel);
      return true;
    });
};

export const claudeDesktopProviderPresets: ClaudeDesktopProviderPreset[] = [
  {
    name: "Claude Desktop Official",
    websiteUrl: "https://claude.ai/download",
    category: "official",
    baseUrl: "",
    mode: "direct",
    apiFormat: "anthropic",
    theme: {
      icon: "claude",
      backgroundColor: "#D97757",
      textColor: "#FFFFFF",
    },
    icon: "anthropic",
    iconColor: "#D4915D",
  },
  {
    name: "Codex",
    websiteUrl: "https://openai.com/chatgpt/pricing",
    category: "official",
    baseUrl: "https://chatgpt.com/backend-api/codex",
    mode: "proxy",
    apiFormat: "openai_responses",
    providerType: "codex_oauth",
    requiresOAuth: true,
    modelRoutes: brandedRoutes("gpt-5.5", "gpt-5.5", "gpt-5.4-mini"),
    icon: "openai",
    iconColor: "#000000",
  },
];
