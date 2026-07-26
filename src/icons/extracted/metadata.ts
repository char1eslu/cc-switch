// Metadata for the curated provider icons
import { IconMetadata } from "@/types/icon";

export const iconMetadata: Record<string, IconMetadata> = {
  anthropic: {
    name: "anthropic",
    displayName: "Anthropic",
    category: "ai-provider",
    keywords: ["claude"],
    defaultColor: "#D4915D",
  },
  claude: {
    name: "claude",
    displayName: "Claude",
    category: "ai-provider",
    keywords: ["anthropic"],
    defaultColor: "#D4915D",
  },
  openai: {
    name: "openai",
    displayName: "OpenAI",
    category: "ai-provider",
    keywords: ["gpt", "chatgpt"],
    defaultColor: "currentColor",
  },
};

export function getIconMetadata(name: string): IconMetadata | undefined {
  return iconMetadata[name.toLowerCase()];
}

export function searchIcons(query: string): string[] {
  const lowerQuery = query.toLowerCase();
  return Object.values(iconMetadata)
    .filter(
      (meta) =>
        meta.name.includes(lowerQuery) ||
        meta.displayName.toLowerCase().includes(lowerQuery) ||
        meta.keywords.some((k) => k.includes(lowerQuery)),
    )
    .map((meta) => meta.name);
}
