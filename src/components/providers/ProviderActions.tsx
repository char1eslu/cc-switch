import {
  Activity,
  BarChart3,
  Check,
  Copy,
  Edit,
  Loader2,
  Minus,
  Play,
  Plus,
  Terminal,
  Trash2,
  Zap,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import type { AppId } from "@/lib/api";

interface ProviderActionsProps {
  appId?: AppId;
  isCurrent: boolean;
  isInConfig?: boolean;
  isTesting?: boolean;
  isProxyTakeover?: boolean;
  onSwitch: () => void;
  onEdit: () => void;
  onDuplicate: () => void;
  onTest?: () => void;
  onConfigureUsage?: () => void;
  onDelete: () => void;
  onOpenTerminal?: () => void;
  isAutoFailoverEnabled?: boolean;
  isInFailoverQueue?: boolean;
  onToggleFailover?: (enabled: boolean) => void;
  // Stack 模式：主按钮是添加 / 移除，已添加的另有「设为默认」（onSwitch）。onToggleStack
  // 为空的（官方账号）不能添加，只能设为默认。
  isStackMode?: boolean;
  isStackMember?: boolean;
  onToggleStack?: (enabled: boolean) => void;
  isOfficialBlockedByProxy?: boolean;
}

// 主按钮的呈现状态。title 用于 disabled 态向用户解释为何不可点击；
// 因 Button 基类带 disabled:pointer-events-none，title 必须挂在外层非禁用
// 的 wrapper 上才会在 hover 时显示（见下方 <span> 包裹）。
interface MainButtonState {
  disabled: boolean;
  variant: "default" | "secondary";
  className: string;
  icon: JSX.Element;
  text: string;
  title?: string;
}

export function ProviderActions({
  isCurrent,
  isTesting,
  isProxyTakeover = false,
  onSwitch,
  onEdit,
  onDuplicate,
  onTest,
  onConfigureUsage,
  onDelete,
  onOpenTerminal,
  isAutoFailoverEnabled = false,
  isInFailoverQueue = false,
  onToggleFailover,
  isStackMode = false,
  isStackMember = false,
  onToggleStack,
  isOfficialBlockedByProxy = false,
}: ProviderActionsProps) {
  const { t } = useTranslation();
  const iconButtonClass = "h-8 w-8 p-1";

  const isFailoverMode = isAutoFailoverEnabled && onToggleFailover;
  const canStack = isStackMode && onToggleStack !== undefined;

  // 「设为默认 / 当前默认」按钮（Stack 模式的默认供应商）
  const defaultButtonClassName = (isDefault: boolean) =>
    cn(
      "w-fit px-2.5",
      isDefault
        ? "bg-gray-200 text-muted-foreground dark:bg-gray-700 opacity-60 cursor-not-allowed"
        : "bg-blue-500 hover:bg-blue-600 dark:bg-blue-600 dark:hover:bg-blue-700",
    );

  const handleMainButtonClick = () => {
    if (canStack) {
      // Stack 模式：添加 / 移除（默认那家的移除按钮是禁用的）
      onToggleStack?.(!isStackMember);
    } else if (isFailoverMode) {
      onToggleFailover(!isInFailoverQueue);
    } else {
      onSwitch();
    }
  };

  const getMainButtonState = (): MainButtonState => {
    // Stack 模式：已添加的可以移除（默认那家除外），没添加的可以添加
    if (canStack) {
      if (isStackMember) {
        return {
          disabled: isCurrent,
          variant: "secondary" as const,
          className: cn(
            "bg-orange-100 text-orange-600 hover:bg-orange-200 dark:bg-orange-900/50 dark:text-orange-400 dark:hover:bg-orange-900/70",
            isCurrent && "opacity-40 cursor-not-allowed",
          ),
          icon: <Minus className="h-4 w-4" />,
          text: t("provider.removeFromConfig", { defaultValue: "移除" }),
          title: isCurrent ? t("provider.stackDefaultCannotRemove") : undefined,
        };
      }
      return {
        disabled: false,
        variant: "default" as const,
        className:
          "bg-emerald-500 hover:bg-emerald-600 dark:bg-emerald-600 dark:hover:bg-emerald-700",
        icon: <Plus className="h-4 w-4" />,
        text: t("provider.addToConfig", { defaultValue: "添加" }),
      };
    }

    // Stack 模式下不能添加的（官方账号）只能设为默认
    if (isStackMode && !isOfficialBlockedByProxy) {
      return {
        disabled: isCurrent,
        variant: isCurrent ? ("secondary" as const) : ("default" as const),
        className: defaultButtonClassName(isCurrent),
        icon: <Zap className="h-4 w-4" />,
        text: isCurrent
          ? t("provider.isDefault", { defaultValue: "当前默认" })
          : t("provider.setAsDefault", { defaultValue: "设为默认" }),
      };
    }

    if (isFailoverMode) {
      if (isInFailoverQueue) {
        return {
          disabled: false,
          variant: "secondary" as const,
          className:
            "bg-blue-100 text-blue-600 hover:bg-blue-200 dark:bg-blue-900/50 dark:text-blue-400 dark:hover:bg-blue-900/70",
          icon: <Check className="h-4 w-4" />,
          text: t("failover.inQueue", { defaultValue: "已加入" }),
        };
      }
      return {
        disabled: false,
        variant: "default" as const,
        className:
          "bg-blue-500 hover:bg-blue-600 dark:bg-blue-600 dark:hover:bg-blue-700",
        icon: <Plus className="h-4 w-4" />,
        text: t("failover.addQueue", { defaultValue: "加入" }),
      };
    }

    if (isCurrent) {
      return {
        disabled: true,
        variant: "secondary" as const,
        className:
          "bg-gray-200 text-muted-foreground hover:bg-gray-200 hover:text-muted-foreground dark:bg-gray-700 dark:hover:bg-gray-700",
        icon: <Check className="h-4 w-4" />,
        text: t("provider.inUse"),
      };
    }

    if (isOfficialBlockedByProxy) {
      return {
        disabled: true,
        variant: "default" as const,
        className: "",
        icon: <Play className="h-4 w-4" />,
        text: t("provider.enable"),
        title: t("provider.blockedByProxyHint"),
      };
    }

    return {
      disabled: false,
      variant: "default" as const,
      className: isProxyTakeover
        ? "bg-emerald-500 hover:bg-emerald-600 dark:bg-emerald-600 dark:hover:bg-emerald-700"
        : "",
      icon: <Play className="h-4 w-4" />,
      text: t("provider.enable"),
    };
  };

  const buttonState = getMainButtonState();

  const canDelete = !isCurrent;

  return (
    <div className="flex items-center gap-1.5">
      {canStack && isStackMember && (
        <Button
          size="sm"
          variant={isCurrent ? "secondary" : "default"}
          onClick={isCurrent ? undefined : onSwitch}
          disabled={isCurrent}
          className={defaultButtonClassName(isCurrent)}
        >
          <Zap className="h-4 w-4" />
          {isCurrent
            ? t("provider.isDefault", { defaultValue: "当前默认" })
            : t("provider.setAsDefault", { defaultValue: "设为默认" })}
        </Button>
      )}

      {/* wrapper span 承接 hover：disabled 按钮自身 pointer-events:none，
          原生 title 与 cursor 都必须挂在未禁用的外层元素上才会生效 */}
      <span
        title={buttonState.title}
        className={cn(
          "inline-flex",
          buttonState.disabled && "cursor-not-allowed",
        )}
      >
        <Button
          size="sm"
          variant={buttonState.variant}
          onClick={handleMainButtonClick}
          disabled={buttonState.disabled}
          className={cn("w-[4.5rem] px-2.5", buttonState.className)}
        >
          {buttonState.icon}
          {buttonState.text}
        </Button>
      </span>

      <div className="flex items-center gap-1">
        <Button
          size="icon"
          variant="ghost"
          onClick={onEdit}
          title={t("common.edit")}
          className={iconButtonClass}
        >
          <Edit className="h-4 w-4" />
        </Button>

        <Button
          size="icon"
          variant="ghost"
          onClick={onDuplicate}
          title={t("provider.duplicate")}
          className={iconButtonClass}
        >
          <Copy className="h-4 w-4" />
        </Button>

        <Button
          size="icon"
          variant="ghost"
          onClick={onTest || undefined}
          disabled={isTesting}
          title={t("provider.connectivityCheck", "检测连通")}
          className={cn(
            iconButtonClass,
            !onTest && "opacity-40 cursor-not-allowed text-muted-foreground",
          )}
        >
          {isTesting ? (
            <Loader2 className="h-4 w-4 animate-spin" />
          ) : (
            <Activity className="h-4 w-4" />
          )}
        </Button>

        <Button
          size="icon"
          variant="ghost"
          onClick={onConfigureUsage || undefined}
          title={t("provider.configureUsage")}
          className={cn(
            iconButtonClass,
            !onConfigureUsage &&
              "opacity-40 cursor-not-allowed text-muted-foreground",
          )}
        >
          <BarChart3 className="h-4 w-4" />
        </Button>

        {onOpenTerminal && (
          <Button
            size="icon"
            variant="ghost"
            onClick={onOpenTerminal}
            title={t("provider.openTerminal", "打开终端")}
            className={cn(
              iconButtonClass,
              "hover:text-emerald-600 dark:hover:text-emerald-400",
            )}
          >
            <Terminal className="h-4 w-4" />
          </Button>
        )}

        <Button
          size="icon"
          variant="ghost"
          onClick={canDelete ? onDelete : undefined}
          title={t("common.delete")}
          className={cn(
            iconButtonClass,
            canDelete && "hover:text-red-500 dark:hover:text-red-400",
            !canDelete && "opacity-40 cursor-not-allowed text-muted-foreground",
          )}
        >
          <Trash2 className="h-4 w-4" />
        </Button>
      </div>
    </div>
  );
}
