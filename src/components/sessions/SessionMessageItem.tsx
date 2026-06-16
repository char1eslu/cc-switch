import { memo, useState } from "react";
import {
  ChevronDown,
  ChevronUp,
  Copy,
  GitBranch,
  Scissors,
} from "lucide-react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/ui/button";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import type { SessionMessage } from "@/types";
import {
  formatTimestamp,
  getRoleLabel,
  getRoleTone,
  highlightText,
} from "./utils";
import { SessionMarkdown } from "./SessionMarkdown";

const COLLAPSE_THRESHOLD = 3000;
const COLLAPSED_LENGTH = 1500;

interface SessionMessageItemProps {
  message: SessionMessage;
  isActive: boolean;
  searchQuery?: string;
  onCopy: (content: string) => void;
  onTrim?: (message: SessionMessage) => void;
  onBranch?: (message: SessionMessage) => void;
}

export const SessionMessageItem = memo(function SessionMessageItem({
  message,
  isActive,
  searchQuery,
  onCopy,
  onTrim,
  onBranch,
}: SessionMessageItemProps) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);

  const isLong = message.content.length > COLLAPSE_THRESHOLD;
  const hasSearchMatch =
    isLong &&
    !expanded &&
    !!searchQuery &&
    message.content.toLowerCase().includes(searchQuery.toLowerCase());
  const collapsed = isLong && !expanded && !hasSearchMatch;
  const displayContent = collapsed
    ? message.content.slice(0, COLLAPSED_LENGTH) + "…"
    : message.content;
  const normalizedRole = message.role.toLowerCase();
  const shouldRenderMarkdown =
    !searchQuery?.trim() &&
    (normalizedRole === "assistant" || normalizedRole === "user");

  return (
    <div
      className={cn(
        "rounded-lg border px-3 py-2.5 relative group transition-shadow min-w-0",
        normalizedRole === "user"
          ? "bg-primary/5 border-primary/20 ml-8"
          : normalizedRole === "assistant"
            ? "bg-blue-500/5 border-blue-500/20 mr-8"
            : "bg-muted/40 border-border/60",
        isActive && "ring-2 ring-primary ring-offset-2",
      )}
    >
      <div className="absolute top-2 right-2 flex items-center gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
        {message.canTrim && onTrim && (
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className="size-6"
                onClick={() => onTrim(message)}
              >
                <Scissors className="size-3" />
              </Button>
            </TooltipTrigger>
            <TooltipContent>
              {t("sessionManager.trimFromHere", {
                defaultValue: "从这里裁剪",
              })}
            </TooltipContent>
          </Tooltip>
        )}
        {message.canBranch && onBranch && (
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className="size-6"
                onClick={() => onBranch(message)}
              >
                <GitBranch className="size-3" />
              </Button>
            </TooltipTrigger>
            <TooltipContent>
              {t("sessionManager.branchFromHere", {
                defaultValue: "从这里分支",
              })}
            </TooltipContent>
          </Tooltip>
        )}
        <Tooltip>
          <TooltipTrigger asChild>
            <Button
              variant="ghost"
              size="icon"
              className="size-6"
              onClick={() => onCopy(message.content)}
            >
              <Copy className="size-3" />
            </Button>
          </TooltipTrigger>
          <TooltipContent>
            {t("sessionManager.copyMessage", {
              defaultValue: "复制内容",
            })}
          </TooltipContent>
        </Tooltip>
      </div>
      <div className="flex items-center justify-between text-xs mb-1.5 pr-6">
        <span className={cn("font-semibold", getRoleTone(message.role))}>
          {getRoleLabel(message.role, t)}
        </span>
        {message.ts && (
          <span className="text-muted-foreground">
            {formatTimestamp(message.ts)}
          </span>
        )}
      </div>
      <div className="min-w-0">
        {shouldRenderMarkdown ? (
          <SessionMarkdown content={displayContent} />
        ) : searchQuery ? (
          <div className="whitespace-pre-wrap break-words text-sm leading-relaxed [overflow-wrap:anywhere]">
            {highlightText(displayContent, searchQuery)}
          </div>
        ) : (
          <div className="whitespace-pre-wrap break-words text-sm leading-relaxed [overflow-wrap:anywhere]">
            {displayContent}
          </div>
        )}
      </div>
      {isLong && !hasSearchMatch && (
        <button
          type="button"
          aria-expanded={expanded}
          onClick={() => setExpanded((v) => !v)}
          className="flex items-center gap-1 mt-1.5 text-xs text-muted-foreground hover:text-foreground transition-colors"
        >
          {expanded ? (
            <>
              <ChevronUp className="size-3" />
              {t("sessionManager.collapseContent", {
                defaultValue: "收起",
              })}
            </>
          ) : (
            <>
              <ChevronDown className="size-3" />
              {t("sessionManager.expandContent", {
                defaultValue: "展开完整内容",
              })}
              <span className="text-muted-foreground/60">
                ({Math.round(message.content.length / 1000)}k)
              </span>
            </>
          )}
        </button>
      )}
    </div>
  );
});
