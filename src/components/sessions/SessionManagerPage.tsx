import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useSessionSearch } from "@/hooks/useSessionSearch";
import { useTranslation } from "react-i18next";
import { useVirtualizer } from "@tanstack/react-virtual";
import { toast } from "sonner";
import { useQueryClient } from "@tanstack/react-query";
import {
  Copy,
  RefreshCw,
  Search,
  Play,
  Trash2,
  MessageSquare,
  Clock,
  FolderOpen,
  FileText,
  X,
  CheckSquare,
  Wrench,
  Archive,
  DatabaseBackup,
  RotateCcw,
} from "lucide-react";
import {
  useDeleteSessionMutation,
  useSessionMessagesQuery,
  useSessionsQuery,
} from "@/lib/query";
import { sessionsApi } from "@/lib/api";
import type { SessionMessage, SessionMeta } from "@/types";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { extractErrorMessage } from "@/utils/errorUtils";
import { isMac } from "@/lib/platform";
import { ProviderIcon } from "@/components/ProviderIcon";
import { SessionItem } from "./SessionItem";
import { SessionMessageItem } from "./SessionMessageItem";
import { SessionTocDialog, SessionTocSidebar } from "./SessionToc";
import {
  extractCodexPromptPreview,
  formatSessionMessagePreview,
  formatSessionTitle,
  formatTimestamp,
  getBaseName,
  getProviderIconName,
  getProviderLabel,
  getSessionKey,
  shouldHideCodexMessageFromToc,
} from "./utils";

type ProviderFilter =
  | "all"
  | "codex"
  | "claude"
  | "opencode"
  | "openclaw"
  | "gemini"
  | "hermes";

export function SessionManagerPage({ appId }: { appId: string }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { data, isLoading, refetch } = useSessionsQuery();
  const sessions = data ?? [];
  const detailRef = useRef<HTMLDivElement | null>(null);
  const scrollContainerRef = useRef<HTMLDivElement | null>(null);
  const [activeMessageIndex, setActiveMessageIndex] = useState<number | null>(
    null,
  );
  const [tocDialogOpen, setTocDialogOpen] = useState(false);
  const [isSearchOpen, setIsSearchOpen] = useState(false);
  const [deleteTargets, setDeleteTargets] = useState<SessionMeta[] | null>(
    null,
  );
  const [moveTarget, setMoveTarget] = useState<SessionMeta | null>(null);
  const [moveProjectDir, setMoveProjectDir] = useState("");
  const [isMoving, setIsMoving] = useState(false);
  const [isRepairing, setIsRepairing] = useState(false);
  const [isTrashing, setIsTrashing] = useState(false);
  const [backupDialogOpen, setBackupDialogOpen] = useState(false);
  const [codexBackups, setCodexBackups] = useState<
    Awaited<ReturnType<typeof sessionsApi.listCodexBackups>>
  >([]);
  const [codexTrashBackups, setCodexTrashBackups] = useState<
    Awaited<ReturnType<typeof sessionsApi.listCodexBackups>>
  >([]);
  const [codexTrashedThreads, setCodexTrashedThreads] = useState<
    Awaited<ReturnType<typeof sessionsApi.listCodexTrashedThreads>>
  >([]);
  const [isLoadingBackups, setIsLoadingBackups] = useState(false);
  const [selectedSessionKeys, setSelectedSessionKeys] = useState<Set<string>>(
    () => new Set(),
  );
  const [isBatchDeleting, setIsBatchDeleting] = useState(false);
  const [selectionMode, setSelectionMode] = useState(false);
  const searchInputRef = useRef<HTMLInputElement | null>(null);

  const [search, setSearch] = useState("");
  const [providerFilter, setProviderFilter] = useState<ProviderFilter>(
    appId as ProviderFilter,
  );
  const [selectedKey, setSelectedKey] = useState<string | null>(null);

  // 使用 FlexSearch 全文搜索
  const { search: searchSessions } = useSessionSearch({
    sessions,
    providerFilter,
  });

  const filteredSessions = useMemo(() => {
    return searchSessions(search);
  }, [searchSessions, search]);

  useEffect(() => {
    if (filteredSessions.length === 0) {
      setSelectedKey(null);
      return;
    }
    const exists = selectedKey
      ? filteredSessions.some(
          (session) => getSessionKey(session) === selectedKey,
        )
      : false;
    if (!exists) {
      setSelectedKey(getSessionKey(filteredSessions[0]));
    }
  }, [filteredSessions, selectedKey]);

  const selectedSession = useMemo(() => {
    if (!selectedKey) return null;
    return (
      filteredSessions.find(
        (session) => getSessionKey(session) === selectedKey,
      ) || null
    );
  }, [filteredSessions, selectedKey]);

  const { data: messages = [], isLoading: isLoadingMessages } =
    useSessionMessagesQuery(
      selectedSession?.providerId,
      selectedSession?.sourcePath,
    );
  const deleteSessionMutation = useDeleteSessionMutation();
  const isDeleting = deleteSessionMutation.isPending || isBatchDeleting;

  const virtualizer = useVirtualizer({
    count: messages.length,
    getScrollElement: () => scrollContainerRef.current,
    estimateSize: () => 120,
    overscan: 5,
    gap: 12,
  });

  useEffect(() => {
    if (scrollContainerRef.current) {
      scrollContainerRef.current.scrollTop = 0;
    }
  }, [selectedKey]);

  useEffect(() => {
    const validKeys = new Set(
      sessions.map((session) => getSessionKey(session)),
    );
    setSelectedSessionKeys((current) => {
      let changed = false;
      const next = new Set<string>();
      current.forEach((key) => {
        if (validKeys.has(key)) {
          next.add(key);
        } else {
          changed = true;
        }
      });
      return changed ? next : current;
    });
  }, [sessions]);

  const isCodexSession = selectedSession?.providerId === "codex";

  // 提取用户消息用于目录
  const userMessagesToc = useMemo(() => {
    return messages
      .map((msg, index) => ({ msg, index }))
      .filter(({ msg }) => {
        if (msg.role.toLowerCase() !== "user") return false;
        return !(isCodexSession && shouldHideCodexMessageFromToc(msg.content));
      })
      .map(({ msg, index }) => {
        const previewContent = isCodexSession
          ? extractCodexPromptPreview(msg.content)
          : msg.content;

        return {
          index,
          preview: formatSessionMessagePreview(previewContent),
          ts: msg.ts,
        };
      });
  }, [isCodexSession, messages]);

  const scrollToMessage = (index: number) => {
    virtualizer.scrollToIndex(index, { align: "center", behavior: "smooth" });
    setActiveMessageIndex(index);
    setTocDialogOpen(false);
    setTimeout(() => setActiveMessageIndex(null), 2000);
  };

  const handleCopy = useCallback(
    async (text: string, successMessage: string) => {
      try {
        await navigator.clipboard.writeText(text);
        toast.success(successMessage);
      } catch (error) {
        toast.error(
          extractErrorMessage(error) ||
            t("common.error", { defaultValue: "Copy failed" }),
        );
      }
    },
    [t],
  );

  const handleMessageCopy = useCallback(
    (content: string) => {
      void handleCopy(
        content,
        t("sessionManager.messageCopied", { defaultValue: "已复制消息内容" }),
      );
    },
    [handleCopy, t],
  );

  const handleResume = async () => {
    if (!selectedSession?.resumeCommand) return;

    if (!isMac()) {
      await handleCopy(
        selectedSession.resumeCommand,
        t("sessionManager.resumeCommandCopied"),
      );
      return;
    }

    try {
      await sessionsApi.launchTerminal({
        command: selectedSession.resumeCommand,
        cwd: selectedSession.projectDir ?? undefined,
      });
      toast.success(t("sessionManager.terminalLaunched"));
    } catch (error) {
      const fallback = selectedSession.resumeCommand;
      await handleCopy(fallback, t("sessionManager.resumeFallbackCopied"));
      toast.error(extractErrorMessage(error) || t("sessionManager.openFailed"));
    }
  };

  const handleDeleteConfirm = async () => {
    if (!deleteTargets || deleteTargets.length === 0 || isDeleting) {
      return;
    }

    const targets = deleteTargets.filter((session) => session.sourcePath);
    setDeleteTargets(null);

    if (targets.length === 0) {
      return;
    }

    if (targets.length === 1) {
      const [target] = targets;
      await deleteSessionMutation.mutateAsync({
        providerId: target.providerId,
        sessionId: target.sessionId,
        sourcePath: target.sourcePath!,
      });
      setSelectedSessionKeys((current) => {
        const next = new Set(current);
        next.delete(getSessionKey(target));
        return next;
      });
      return;
    }

    setIsBatchDeleting(true);
    try {
      const results = await sessionsApi.deleteMany(
        targets.map((session) => ({
          providerId: session.providerId,
          sessionId: session.sessionId,
          sourcePath: session.sourcePath!,
        })),
      );

      const deletedKeys = results
        .filter((result) => result.success)
        .map(
          (result) =>
            `${result.providerId}:${result.sessionId}:${result.sourcePath ?? ""}`,
        );

      const failedErrors = results
        .filter((result) => !result.success)
        .map((result) => result.error || t("common.unknown"));

      if (deletedKeys.length > 0) {
        const deletedKeySet = new Set(deletedKeys);
        queryClient.setQueryData<SessionMeta[]>(["sessions"], (current) =>
          (current ?? []).filter(
            (session) => !deletedKeySet.has(getSessionKey(session)),
          ),
        );
      }

      results
        .filter((result) => result.success)
        .forEach((result) => {
          queryClient.removeQueries({
            queryKey: ["sessionMessages", result.providerId, result.sourcePath],
          });
        });

      setSelectedSessionKeys((current) => {
        const next = new Set(current);
        deletedKeys.forEach((key) => next.delete(key));
        return next;
      });

      await queryClient.invalidateQueries({ queryKey: ["sessions"] });

      if (deletedKeys.length > 0) {
        toast.success(
          t("sessionManager.batchDeleteSuccess", {
            defaultValue: "已删除 {{count}} 个会话",
            count: deletedKeys.length,
          }),
        );
      }

      if (failedErrors.length > 0) {
        toast.error(
          t("sessionManager.batchDeleteFailed", {
            defaultValue: "{{failed}} 个会话删除失败",
            failed: failedErrors.length,
          }),
          {
            description: failedErrors[0],
          },
        );
      }
    } catch (error) {
      toast.error(
        extractErrorMessage(error) ||
          t("sessionManager.batchDeleteRequestFailed", {
            defaultValue: "批量删除失败，请稍后重试",
          }),
      );
    } finally {
      setIsBatchDeleting(false);
    }
  };

  const deletableFilteredSessions = useMemo(
    () => filteredSessions.filter((session) => Boolean(session.sourcePath)),
    [filteredSessions],
  );

  const selectedSessions = useMemo(
    () =>
      sessions.filter((session) =>
        selectedSessionKeys.has(getSessionKey(session)),
      ),
    [sessions, selectedSessionKeys],
  );

  const selectedDeletableSessions = useMemo(
    () => selectedSessions.filter((session) => Boolean(session.sourcePath)),
    [selectedSessions],
  );

  const codexProjectDirs = useMemo(() => {
    const dirs = new Set<string>();
    sessions.forEach((session) => {
      if (session.providerId !== "codex") return;
      const dir = session.projectDir?.trim();
      if (dir) {
        dirs.add(dir);
      }
    });
    return Array.from(dirs).sort((a, b) => a.localeCompare(b));
  }, [sessions]);

  const moveProjectOptions = useMemo(() => {
    const current = moveTarget?.projectDir?.trim();
    return codexProjectDirs.filter((dir) => dir !== current);
  }, [codexProjectDirs, moveTarget]);

  const trimmedMoveProjectDir = moveProjectDir.trim();
  const canMoveSelectedSession =
    selectedSession?.providerId === "codex" &&
    Boolean(selectedSession.sourcePath);
  const canConfirmMove =
    Boolean(moveTarget?.sourcePath) &&
    trimmedMoveProjectDir.length > 0 &&
    trimmedMoveProjectDir !== moveTarget?.projectDir?.trim() &&
    !isMoving;

  useEffect(() => {
    if (!selectionMode) return;

    const visibleKeys = new Set(
      deletableFilteredSessions.map((session) => getSessionKey(session)),
    );

    setSelectedSessionKeys((current) => {
      let changed = false;
      const next = new Set<string>();

      current.forEach((key) => {
        if (visibleKeys.has(key)) {
          next.add(key);
        } else {
          changed = true;
        }
      });

      return changed ? next : current;
    });
  }, [deletableFilteredSessions, selectionMode]);

  const allFilteredSelected =
    deletableFilteredSessions.length > 0 &&
    deletableFilteredSessions.every((session) =>
      selectedSessionKeys.has(getSessionKey(session)),
    );

  const toggleSessionChecked = (session: SessionMeta, checked: boolean) => {
    if (!session.sourcePath) return;
    const key = getSessionKey(session);
    setSelectedSessionKeys((current) => {
      const next = new Set(current);
      if (checked) {
        next.add(key);
      } else {
        next.delete(key);
      }
      return next;
    });
  };

  const handleToggleSelectAll = () => {
    setSelectedSessionKeys((current) => {
      const next = new Set(current);
      if (allFilteredSelected) {
        deletableFilteredSessions.forEach((session) =>
          next.delete(getSessionKey(session)),
        );
      } else {
        deletableFilteredSessions.forEach((session) =>
          next.add(getSessionKey(session)),
        );
      }
      return next;
    });
  };

  const openBatchDeleteDialog = () => {
    if (selectedDeletableSessions.length === 0) return;
    setDeleteTargets(selectedDeletableSessions);
  };

  const openMoveDialog = (session: SessionMeta) => {
    if (session.providerId !== "codex" || !session.sourcePath) return;
    setMoveTarget(session);
    setMoveProjectDir("");
  };

  const closeMoveDialog = () => {
    if (isMoving) return;
    setMoveTarget(null);
    setMoveProjectDir("");
  };

  const handleMoveConfirm = async () => {
    if (!moveTarget?.sourcePath || !canConfirmMove) return;

    setIsMoving(true);
    try {
      await sessionsApi.move({
        providerId: moveTarget.providerId,
        sessionId: moveTarget.sessionId,
        sourcePath: moveTarget.sourcePath,
        targetProjectDir: trimmedMoveProjectDir,
      });

      queryClient.setQueryData<SessionMeta[]>(["sessions"], (current) =>
        (current ?? []).map((session) =>
          getSessionKey(session) === getSessionKey(moveTarget)
            ? { ...session, projectDir: trimmedMoveProjectDir }
            : session,
        ),
      );
      await queryClient.invalidateQueries({ queryKey: ["sessions"] });

      toast.success(
        t("sessionManager.moveSuccess", {
          defaultValue: "会话已移动",
        }),
      );
      setMoveTarget(null);
      setMoveProjectDir("");
    } catch (error) {
      toast.error(
        extractErrorMessage(error) ||
          t("sessionManager.moveFailed", {
            defaultValue: "移动会话失败",
          }),
      );
    } finally {
      setIsMoving(false);
    }
  };

  const refreshCodexBackups = useCallback(async () => {
    setIsLoadingBackups(true);
    try {
      const [backups, trashBackups, trashedThreads] = await Promise.all([
        sessionsApi.listCodexBackups(false),
        sessionsApi.listCodexBackups(true),
        sessionsApi.listCodexTrashedThreads(),
      ]);
      setCodexBackups(backups);
      setCodexTrashBackups(trashBackups);
      setCodexTrashedThreads(trashedThreads);
    } catch (error) {
      toast.error(
        extractErrorMessage(error) ||
          t("sessionManager.backupLoadFailed", {
            defaultValue: "加载备份失败",
          }),
      );
    } finally {
      setIsLoadingBackups(false);
    }
  }, [t]);

  const openBackupDialog = () => {
    setBackupDialogOpen(true);
    void refreshCodexBackups();
  };

  const handleRepair = async () => {
    if (
      !selectedSession?.sourcePath ||
      selectedSession.providerId !== "codex"
    ) {
      return;
    }
    setIsRepairing(true);
    try {
      await sessionsApi.repair({
        providerId: selectedSession.providerId,
        sessionId: selectedSession.sessionId,
        sourcePath: selectedSession.sourcePath,
      });
      await queryClient.invalidateQueries({ queryKey: ["sessions"] });
      toast.success(
        t("sessionManager.repairSuccess", {
          defaultValue: "索引已修复",
        }),
      );
    } catch (error) {
      toast.error(
        extractErrorMessage(error) ||
          t("sessionManager.repairFailed", {
            defaultValue: "修复索引失败",
          }),
      );
    } finally {
      setIsRepairing(false);
    }
  };

  const handleTrash = async () => {
    if (
      !selectedSession?.sourcePath ||
      selectedSession.providerId !== "codex"
    ) {
      return;
    }
    setIsTrashing(true);
    try {
      await sessionsApi.trash({
        providerId: selectedSession.providerId,
        sessionId: selectedSession.sessionId,
        sourcePath: selectedSession.sourcePath,
      });
      queryClient.removeQueries({
        queryKey: [
          "sessionMessages",
          selectedSession.providerId,
          selectedSession.sourcePath,
        ],
      });
      await queryClient.invalidateQueries({ queryKey: ["sessions"] });
      toast.success(
        t("sessionManager.trashSuccess", {
          defaultValue: "会话已移动到 Codex Keeper Trash",
        }),
      );
    } catch (error) {
      toast.error(
        extractErrorMessage(error) ||
          t("sessionManager.trashFailed", {
            defaultValue: "移动到 Trash 失败",
          }),
      );
    } finally {
      setIsTrashing(false);
    }
  };

  const runCodexMessageOperation = async (
    message: SessionMessage,
    operation: "trim" | "branch",
  ) => {
    if (
      !selectedSession?.sourcePath ||
      selectedSession.providerId !== "codex" ||
      !message.lineNumber
    ) {
      return;
    }

    try {
      const payload = {
        providerId: selectedSession.providerId,
        sessionId: selectedSession.sessionId,
        sourcePath: selectedSession.sourcePath,
        lineNumber:
          operation === "branch"
            ? (message.branchLineNumber ?? message.lineNumber)
            : message.lineNumber,
      };
      if (operation === "trim") {
        await sessionsApi.trim(payload);
      } else {
        await sessionsApi.branch(payload);
      }
      await queryClient.invalidateQueries({ queryKey: ["sessions"] });
      await queryClient.invalidateQueries({
        queryKey: [
          "sessionMessages",
          selectedSession.providerId,
          selectedSession.sourcePath,
        ],
      });
      toast.success(
        operation === "trim"
          ? t("sessionManager.trimSuccess", {
              defaultValue: "会话已裁剪",
            })
          : t("sessionManager.branchSuccess", {
              defaultValue: "分支会话已创建",
            }),
      );
    } catch (error) {
      toast.error(
        extractErrorMessage(error) ||
          t("sessionManager.operationFailed", {
            defaultValue: "操作失败",
          }),
      );
    }
  };

  const exitSelectionMode = () => {
    setSelectionMode(false);
    setSelectedSessionKeys(new Set());
  };

  return (
    <TooltipProvider>
      <div
        className="mx-auto px-4 sm:px-6 flex flex-col h-full min-h-0"
        onWheel={(e) => e.stopPropagation()}
      >
        <div className="flex-1 overflow-hidden flex flex-col gap-4">
          {/* 主内容区域 - 左右分栏 */}
          <div className="flex-1 overflow-hidden grid gap-4 md:grid-cols-[320px_1fr]">
            {/* 左侧会话列表 */}
            <Card className="flex flex-col flex-1 min-h-0 overflow-hidden">
              <CardHeader className="py-2 px-3 border-b">
                {isSearchOpen ? (
                  <div className="flex items-center gap-2">
                    <div className="relative flex-1">
                      <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 size-3.5 text-muted-foreground" />
                      <Input
                        ref={searchInputRef}
                        value={search}
                        onChange={(event) => setSearch(event.target.value)}
                        placeholder={t("sessionManager.searchPlaceholder")}
                        className="h-8 pl-8 pr-8 text-sm"
                        autoFocus
                        onKeyDown={(e) => {
                          if (e.key === "Escape") {
                            setIsSearchOpen(false);
                            setSearch("");
                          }
                        }}
                        onBlur={() => {
                          if (search.trim() === "") {
                            setIsSearchOpen(false);
                          }
                        }}
                      />
                      <Button
                        variant="ghost"
                        size="icon"
                        className="absolute right-1 top-1/2 -translate-y-1/2 size-6"
                        onClick={() => {
                          setIsSearchOpen(false);
                          setSearch("");
                        }}
                      >
                        <X className="size-3" />
                      </Button>
                    </div>
                    {selectionMode && (
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <Button
                            variant="secondary"
                            size="icon"
                            className="size-7 bg-blue-50 text-blue-600 hover:bg-blue-100 dark:bg-blue-950/40 dark:text-blue-300 dark:hover:bg-blue-950/60"
                            aria-label={t(
                              "sessionManager.exitBatchModeTooltip",
                              {
                                defaultValue: "退出批量管理",
                              },
                            )}
                            onClick={exitSelectionMode}
                          >
                            <CheckSquare className="size-3.5" />
                          </Button>
                        </TooltipTrigger>
                        <TooltipContent>
                          {t("sessionManager.exitBatchModeTooltip", {
                            defaultValue: "退出批量管理",
                          })}
                        </TooltipContent>
                      </Tooltip>
                    )}
                  </div>
                ) : (
                  <div className="flex flex-col gap-2">
                    <div className="flex items-center justify-between gap-2">
                      <div className="flex items-center gap-2 min-w-0">
                        <CardTitle className="text-sm font-medium whitespace-nowrap">
                          {t("sessionManager.sessionList")}
                        </CardTitle>
                        <Badge variant="secondary" className="text-xs">
                          {filteredSessions.length}
                        </Badge>
                      </div>
                      <div className="flex items-center gap-1 shrink-0">
                        {(selectionMode ||
                          deletableFilteredSessions.length > 0) && (
                          <Tooltip>
                            <TooltipTrigger asChild>
                              <Button
                                variant={selectionMode ? "secondary" : "ghost"}
                                size="icon"
                                className={
                                  selectionMode
                                    ? "size-7 bg-blue-50 text-blue-600 hover:bg-blue-100 dark:bg-blue-950/40 dark:text-blue-300 dark:hover:bg-blue-950/60"
                                    : "size-7"
                                }
                                aria-label={
                                  selectionMode
                                    ? t("sessionManager.exitBatchModeTooltip", {
                                        defaultValue: "退出批量管理",
                                      })
                                    : t("sessionManager.manageBatchTooltip", {
                                        defaultValue: "批量管理",
                                      })
                                }
                                onClick={() => {
                                  if (selectionMode) {
                                    exitSelectionMode();
                                  } else {
                                    setSelectionMode(true);
                                  }
                                }}
                              >
                                <CheckSquare className="size-3.5" />
                              </Button>
                            </TooltipTrigger>
                            <TooltipContent>
                              {selectionMode
                                ? t("sessionManager.exitBatchModeTooltip", {
                                    defaultValue: "退出批量管理",
                                  })
                                : t("sessionManager.manageBatchTooltip", {
                                    defaultValue: "批量管理",
                                  })}
                            </TooltipContent>
                          </Tooltip>
                        )}
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Button
                              variant="ghost"
                              size="icon"
                              className="size-7"
                              onClick={() => {
                                setIsSearchOpen(true);
                                setTimeout(
                                  () => searchInputRef.current?.focus(),
                                  0,
                                );
                              }}
                            >
                              <Search className="size-3.5" />
                            </Button>
                          </TooltipTrigger>
                          <TooltipContent>
                            {t("sessionManager.searchSessions")}
                          </TooltipContent>
                        </Tooltip>

                        <Select
                          value={providerFilter}
                          onValueChange={(value) =>
                            setProviderFilter(value as ProviderFilter)
                          }
                        >
                          <Tooltip>
                            <TooltipTrigger asChild>
                              <SelectTrigger className="size-7 p-0 justify-center border-0 bg-transparent hover:bg-muted">
                                <ProviderIcon
                                  icon={
                                    providerFilter === "all"
                                      ? "apps"
                                      : getProviderIconName(providerFilter)
                                  }
                                  name={providerFilter}
                                  size={14}
                                />
                              </SelectTrigger>
                            </TooltipTrigger>
                            <TooltipContent>
                              {providerFilter === "all"
                                ? t("sessionManager.providerFilterAll")
                                : providerFilter}
                            </TooltipContent>
                          </Tooltip>
                          <SelectContent>
                            <SelectItem value="all">
                              <div className="flex items-center gap-2">
                                <ProviderIcon
                                  icon="apps"
                                  name="all"
                                  size={14}
                                />
                                <span>
                                  {t("sessionManager.providerFilterAll")}
                                </span>
                              </div>
                            </SelectItem>
                            <SelectItem value="codex">
                              <div className="flex items-center gap-2">
                                <ProviderIcon
                                  icon="openai"
                                  name="codex"
                                  size={14}
                                />
                                <span>Codex</span>
                              </div>
                            </SelectItem>
                            <SelectItem value="claude">
                              <div className="flex items-center gap-2">
                                <ProviderIcon
                                  icon="claude"
                                  name="claude"
                                  size={14}
                                />
                                <span>Claude Code</span>
                              </div>
                            </SelectItem>
                            <SelectItem value="opencode">
                              <div className="flex items-center gap-2">
                                <ProviderIcon
                                  icon="opencode"
                                  name="opencode"
                                  size={14}
                                />
                                <span>OpenCode</span>
                              </div>
                            </SelectItem>
                            <SelectItem value="openclaw">
                              <div className="flex items-center gap-2">
                                <ProviderIcon
                                  icon="openclaw"
                                  name="openclaw"
                                  size={14}
                                />
                                <span>OpenClaw</span>
                              </div>
                            </SelectItem>
                            <SelectItem value="gemini">
                              <div className="flex items-center gap-2">
                                <ProviderIcon
                                  icon="gemini"
                                  name="gemini"
                                  size={14}
                                />
                                <span>Gemini CLI</span>
                              </div>
                            </SelectItem>
                          </SelectContent>
                        </Select>

                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Button
                              variant="ghost"
                              size="icon"
                              className="size-7"
                              onClick={() => void refetch()}
                            >
                              <RefreshCw className="size-3.5" />
                            </Button>
                          </TooltipTrigger>
                          <TooltipContent>{t("common.refresh")}</TooltipContent>
                        </Tooltip>
                      </div>
                    </div>
                    {selectionMode && (
                      <div className="grid gap-3 rounded-md border bg-muted/40 px-3 py-2.5">
                        <div className="flex items-center gap-2 text-xs text-muted-foreground">
                          <Badge variant="outline" className="text-xs">
                            {t("sessionManager.selectedCount", {
                              defaultValue: "已选 {{count}} 项",
                              count: selectedDeletableSessions.length,
                            })}
                          </Badge>
                          <span className="truncate">
                            {t("sessionManager.batchModeHint", {
                              defaultValue: "勾选要删除的会话",
                            })}
                          </span>
                        </div>
                        <div className="grid gap-3 min-[520px]:grid-cols-[minmax(0,1fr)_auto] min-[520px]:items-center">
                          <div className="flex flex-wrap items-center gap-2">
                            {deletableFilteredSessions.length > 0 && (
                              <Button
                                variant="ghost"
                                size="sm"
                                className="h-7 px-2.5 text-xs whitespace-nowrap"
                                onClick={handleToggleSelectAll}
                              >
                                {allFilteredSelected
                                  ? t("sessionManager.clearFilteredSelection", {
                                      defaultValue: "取消全选",
                                    })
                                  : t("sessionManager.selectAllFiltered", {
                                      defaultValue: "全选当前",
                                    })}
                              </Button>
                            )}
                            <Button
                              variant="ghost"
                              size="sm"
                              className="h-7 px-2.5 text-xs whitespace-nowrap"
                              onClick={() => setSelectedSessionKeys(new Set())}
                            >
                              {t("sessionManager.clearSelection", {
                                defaultValue: "清空已选",
                              })}
                            </Button>
                          </div>
                          <Button
                            variant="destructive"
                            size="sm"
                            className="h-7 gap-1.5 px-2.5 whitespace-nowrap justify-self-start min-[520px]:justify-self-end"
                            onClick={openBatchDeleteDialog}
                            disabled={
                              isDeleting ||
                              selectedDeletableSessions.length === 0
                            }
                          >
                            <Trash2 className="size-3.5" />
                            <span className="text-xs">
                              {isBatchDeleting
                                ? t("sessionManager.batchDeleting", {
                                    defaultValue: "删除中...",
                                  })
                                : t("sessionManager.deleteSelected", {
                                    defaultValue: "批量删除",
                                  })}
                            </span>
                          </Button>
                        </div>
                      </div>
                    )}
                  </div>
                )}
              </CardHeader>
              <CardContent className="flex-1 min-h-0 p-0">
                <ScrollArea className="h-full">
                  <div className="p-2">
                    {isLoading ? (
                      <div className="flex items-center justify-center py-12">
                        <RefreshCw className="size-5 animate-spin text-muted-foreground" />
                      </div>
                    ) : filteredSessions.length === 0 ? (
                      <div className="flex flex-col items-center justify-center py-12 text-center">
                        <MessageSquare className="size-8 text-muted-foreground/50 mb-2" />
                        <p className="text-sm text-muted-foreground">
                          {t("sessionManager.noSessions")}
                        </p>
                      </div>
                    ) : (
                      <div className="space-y-1">
                        {filteredSessions.map((session) => {
                          const isSelected =
                            selectedKey !== null &&
                            getSessionKey(session) === selectedKey;

                          return (
                            <SessionItem
                              key={getSessionKey(session)}
                              session={session}
                              isSelected={isSelected}
                              selectionMode={selectionMode}
                              searchQuery={search}
                              isChecked={selectedSessionKeys.has(
                                getSessionKey(session),
                              )}
                              isCheckDisabled={!session.sourcePath}
                              onSelect={setSelectedKey}
                              onToggleChecked={(checked) =>
                                toggleSessionChecked(session, checked)
                              }
                            />
                          );
                        })}
                      </div>
                    )}
                  </div>
                </ScrollArea>
              </CardContent>
            </Card>

            {/* 右侧会话详情 */}
            <Card
              className="flex flex-col overflow-hidden min-h-0"
              ref={detailRef}
            >
              {!selectedSession ? (
                <div className="flex-1 flex flex-col items-center justify-center text-muted-foreground p-8">
                  <MessageSquare className="size-12 mb-3 opacity-30" />
                  <p className="text-sm">{t("sessionManager.selectSession")}</p>
                </div>
              ) : (
                <>
                  {/* 详情头部 */}
                  <CardHeader className="py-3 px-4 border-b shrink-0">
                    <div className="flex items-start justify-between gap-4">
                      {/* 左侧：会话信息 */}
                      <div className="min-w-0 flex-1">
                        <div className="flex items-center gap-2 mb-1">
                          <Tooltip>
                            <TooltipTrigger asChild>
                              <span className="shrink-0">
                                <ProviderIcon
                                  icon={getProviderIconName(
                                    selectedSession.providerId,
                                  )}
                                  name={selectedSession.providerId}
                                  size={20}
                                />
                              </span>
                            </TooltipTrigger>
                            <TooltipContent>
                              {getProviderLabel(selectedSession.providerId, t)}
                            </TooltipContent>
                          </Tooltip>
                          <h2 className="text-base font-semibold truncate">
                            {formatSessionTitle(selectedSession)}
                          </h2>
                          {selectedSession.codexStatus && (
                            <Badge
                              variant={
                                selectedSession.needsRepair
                                  ? "destructive"
                                  : "secondary"
                              }
                              className="text-[10px]"
                            >
                              {selectedSession.codexStatus}
                            </Badge>
                          )}
                        </div>

                        {/* 元信息 */}
                        <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted-foreground">
                          <div className="flex items-center gap-1">
                            <Clock className="size-3" />
                            <span>
                              {formatTimestamp(
                                selectedSession.lastActiveAt ??
                                  selectedSession.createdAt,
                              )}
                            </span>
                          </div>
                          {selectedSession.projectDir && (
                            <Tooltip>
                              <TooltipTrigger asChild>
                                <button
                                  type="button"
                                  onClick={() =>
                                    void handleCopy(
                                      selectedSession.projectDir!,
                                      t("sessionManager.projectDirCopied"),
                                    )
                                  }
                                  className="flex items-center gap-1 hover:text-foreground transition-colors"
                                >
                                  <FolderOpen className="size-3" />
                                  <span className="truncate max-w-[200px]">
                                    {getBaseName(selectedSession.projectDir)}
                                  </span>
                                </button>
                              </TooltipTrigger>
                              <TooltipContent
                                side="bottom"
                                className="max-w-xs"
                              >
                                <p className="font-mono text-xs break-all">
                                  {selectedSession.projectDir}
                                </p>
                                <p className="text-muted-foreground mt-1">
                                  {t("sessionManager.clickToCopyPath")}
                                </p>
                              </TooltipContent>
                            </Tooltip>
                          )}
                          {selectedSession.sourcePath && (
                            <Tooltip>
                              <TooltipTrigger asChild>
                                <button
                                  type="button"
                                  onClick={() =>
                                    void handleCopy(
                                      selectedSession.sourcePath!,
                                      t("sessionManager.sourcePathCopied"),
                                    )
                                  }
                                  className="flex items-center gap-1 hover:text-foreground transition-colors"
                                >
                                  <FileText className="size-3 shrink-0" />
                                  <span className="font-mono truncate max-w-[200px]">
                                    {getBaseName(selectedSession.sourcePath)}
                                  </span>
                                </button>
                              </TooltipTrigger>
                              <TooltipContent
                                side="bottom"
                                className="max-w-xs"
                              >
                                <p className="font-mono text-xs break-all">
                                  {selectedSession.sourcePath}
                                </p>
                                <p className="text-muted-foreground mt-1">
                                  {t("sessionManager.clickToCopyPath")}
                                </p>
                              </TooltipContent>
                            </Tooltip>
                          )}
                        </div>
                      </div>

                      {/* 右侧：操作按钮组 */}
                      <div className="flex items-center gap-2 shrink-0">
                        {isMac() && (
                          <Tooltip>
                            <TooltipTrigger asChild>
                              <Button
                                size="sm"
                                className="gap-1.5"
                                onClick={() => void handleResume()}
                                disabled={!selectedSession.resumeCommand}
                              >
                                <Play className="size-3.5" />
                                <span className="hidden sm:inline">
                                  {t("sessionManager.resume", {
                                    defaultValue: "恢复会话",
                                  })}
                                </span>
                              </Button>
                            </TooltipTrigger>
                            <TooltipContent>
                              {selectedSession.resumeCommand
                                ? t("sessionManager.resumeTooltip", {
                                    defaultValue: "在终端中恢复此会话",
                                  })
                                : t("sessionManager.noResumeCommand", {
                                    defaultValue: "此会话无法恢复",
                                  })}
                            </TooltipContent>
                          </Tooltip>
                        )}
                        {selectedSession.providerId === "codex" && (
                          <>
                            <Tooltip>
                              <TooltipTrigger asChild>
                                <Button
                                  size="sm"
                                  variant="outline"
                                  className="gap-1.5"
                                  onClick={() => void handleRepair()}
                                  disabled={
                                    !selectedSession.sourcePath || isRepairing
                                  }
                                >
                                  <Wrench className="size-3.5" />
                                  <span className="hidden sm:inline">
                                    {selectedSession.needsRepair
                                      ? t("sessionManager.repairIndex", {
                                          defaultValue: "修复索引",
                                        })
                                      : t("sessionManager.repair", {
                                          defaultValue: "Repair",
                                        })}
                                  </span>
                                </Button>
                              </TooltipTrigger>
                              <TooltipContent>
                                {t("sessionManager.repairTooltip", {
                                  defaultValue:
                                    "修复 Codex session_index 和 SQLite 元数据",
                                })}
                              </TooltipContent>
                            </Tooltip>
                            <Tooltip>
                              <TooltipTrigger asChild>
                                <Button
                                  size="sm"
                                  variant="outline"
                                  className="gap-1.5"
                                  onClick={openBackupDialog}
                                >
                                  <DatabaseBackup className="size-3.5" />
                                  <span className="hidden sm:inline">
                                    {t("sessionManager.backups", {
                                      defaultValue: "备份",
                                    })}
                                  </span>
                                </Button>
                              </TooltipTrigger>
                              <TooltipContent>
                                {t("sessionManager.backupsTooltip", {
                                  defaultValue:
                                    "管理 Codex Keeper 备份和 Trash",
                                })}
                              </TooltipContent>
                            </Tooltip>
                          </>
                        )}
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Button
                              size="sm"
                              variant="outline"
                              className="gap-1.5"
                              onClick={() =>
                                selectedSession &&
                                openMoveDialog(selectedSession)
                              }
                              disabled={!canMoveSelectedSession || isMoving}
                            >
                              <FolderOpen className="size-3.5" />
                              <span className="hidden sm:inline">
                                {isMoving
                                  ? t("sessionManager.moving", {
                                      defaultValue: "移动中...",
                                    })
                                  : t("sessionManager.move", {
                                      defaultValue: "移动",
                                    })}
                              </span>
                            </Button>
                          </TooltipTrigger>
                          <TooltipContent>
                            {canMoveSelectedSession
                              ? t("sessionManager.moveTooltip", {
                                  defaultValue: "将此 Codex 会话移动到其他项目",
                                })
                              : t("sessionManager.moveCodexOnlyTooltip", {
                                  defaultValue: "仅支持移动 Codex 会话",
                                })}
                          </TooltipContent>
                        </Tooltip>
                        {selectedSession.providerId === "codex" && (
                          <Tooltip>
                            <TooltipTrigger asChild>
                              <Button
                                size="sm"
                                variant="outline"
                                className="gap-1.5"
                                onClick={() => void handleTrash()}
                                disabled={
                                  !selectedSession.sourcePath || isTrashing
                                }
                              >
                                <Archive className="size-3.5" />
                                <span className="hidden sm:inline">
                                  {t("sessionManager.trash", {
                                    defaultValue: "Trash",
                                  })}
                                </span>
                              </Button>
                            </TooltipTrigger>
                            <TooltipContent>
                              {t("sessionManager.trashTooltip", {
                                defaultValue:
                                  "移动到 Codex Keeper Trash，可从备份面板恢复",
                              })}
                            </TooltipContent>
                          </Tooltip>
                        )}
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Button
                              size="sm"
                              variant="destructive"
                              className="gap-1.5"
                              onClick={() =>
                                setDeleteTargets([selectedSession])
                              }
                              disabled={
                                !selectedSession.sourcePath || isDeleting
                              }
                            >
                              <Trash2 className="size-3.5" />
                              <span className="hidden sm:inline">
                                {isDeleting
                                  ? t("sessionManager.deleting", {
                                      defaultValue: "删除中...",
                                    })
                                  : t("sessionManager.delete", {
                                      defaultValue: "删除会话",
                                    })}
                              </span>
                            </Button>
                          </TooltipTrigger>
                          <TooltipContent>
                            {t("sessionManager.deleteTooltip", {
                              defaultValue: "永久删除此本地会话记录",
                            })}
                          </TooltipContent>
                        </Tooltip>
                      </div>
                    </div>

                    {/* 恢复命令预览 */}
                    {selectedSession.resumeCommand && (
                      <div className="mt-3 flex items-center gap-2">
                        <div className="flex-1 rounded-md bg-muted/60 px-3 py-1.5 font-mono text-xs text-muted-foreground truncate">
                          {selectedSession.resumeCommand}
                        </div>
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Button
                              variant="ghost"
                              size="icon"
                              className="size-7 shrink-0"
                              onClick={() =>
                                void handleCopy(
                                  selectedSession.resumeCommand!,
                                  t("sessionManager.resumeCommandCopied"),
                                )
                              }
                            >
                              <Copy className="size-3.5" />
                            </Button>
                          </TooltipTrigger>
                          <TooltipContent>
                            {t("sessionManager.copyCommand", {
                              defaultValue: "复制命令",
                            })}
                          </TooltipContent>
                        </Tooltip>
                      </div>
                    )}
                  </CardHeader>

                  {/* 消息列表区域 */}
                  <CardContent className="flex-1 min-h-0 p-0">
                    <div className="flex h-full min-w-0">
                      {/* 消息列表 */}
                      <div className="flex-1 min-w-0 flex flex-col">
                        <div className="px-4 pt-4 pb-2 min-w-0">
                          <div className="flex items-center gap-2">
                            <MessageSquare className="size-4 text-muted-foreground" />
                            <span className="text-sm font-medium">
                              {t("sessionManager.conversationHistory", {
                                defaultValue: "对话记录",
                              })}
                            </span>
                            <Badge variant="secondary" className="text-xs">
                              {messages.length}
                            </Badge>
                          </div>
                        </div>
                        <div
                          ref={scrollContainerRef}
                          className="flex-1 overflow-y-auto px-4 pb-4 min-w-0"
                        >
                          {isLoadingMessages ? (
                            <div className="flex items-center justify-center py-12">
                              <RefreshCw className="size-5 animate-spin text-muted-foreground" />
                            </div>
                          ) : messages.length === 0 ? (
                            <div className="flex flex-col items-center justify-center py-12 text-center">
                              <MessageSquare className="size-8 text-muted-foreground/50 mb-2" />
                              <p className="text-sm text-muted-foreground">
                                {t("sessionManager.emptySession")}
                              </p>
                            </div>
                          ) : (
                            <div
                              style={{
                                height: virtualizer.getTotalSize(),
                                position: "relative",
                              }}
                            >
                              {virtualizer
                                .getVirtualItems()
                                .map((virtualRow) => (
                                  <div
                                    key={virtualRow.key}
                                    data-index={virtualRow.index}
                                    ref={virtualizer.measureElement}
                                    style={{
                                      position: "absolute",
                                      top: 0,
                                      left: 0,
                                      width: "100%",
                                      transform: `translateY(${virtualRow.start}px)`,
                                    }}
                                  >
                                    <SessionMessageItem
                                      message={messages[virtualRow.index]}
                                      isActive={
                                        activeMessageIndex === virtualRow.index
                                      }
                                      searchQuery={search}
                                      onCopy={handleMessageCopy}
                                      onTrim={
                                        isCodexSession
                                          ? (message) =>
                                              void runCodexMessageOperation(
                                                message,
                                                "trim",
                                              )
                                          : undefined
                                      }
                                      onBranch={
                                        isCodexSession
                                          ? (message) =>
                                              void runCodexMessageOperation(
                                                message,
                                                "branch",
                                              )
                                          : undefined
                                      }
                                    />
                                  </div>
                                ))}
                            </div>
                          )}
                        </div>
                      </div>

                      {/* 右侧目录 - 类似少数派 (大屏幕) */}
                      <SessionTocSidebar
                        items={userMessagesToc}
                        onItemClick={scrollToMessage}
                      />
                    </div>

                    {/* 浮动目录按钮 (小屏幕) */}
                    <SessionTocDialog
                      items={userMessagesToc}
                      onItemClick={scrollToMessage}
                      open={tocDialogOpen}
                      onOpenChange={setTocDialogOpen}
                    />
                  </CardContent>
                </>
              )}
            </Card>
          </div>
        </div>
      </div>
      <ConfirmDialog
        isOpen={Boolean(deleteTargets)}
        title={
          deleteTargets && deleteTargets.length > 1
            ? t("sessionManager.batchDeleteConfirmTitle", {
                defaultValue: "批量删除会话",
              })
            : t("sessionManager.deleteConfirmTitle", {
                defaultValue: "删除会话",
              })
        }
        message={
          deleteTargets && deleteTargets.length > 1
            ? t("sessionManager.batchDeleteConfirmMessage", {
                defaultValue:
                  "将永久删除已选中的 {{count}} 个本地会话记录。\n\n此操作不可恢复。",
                count: deleteTargets.length,
              })
            : deleteTargets?.[0]
              ? t("sessionManager.deleteConfirmMessage", {
                  defaultValue:
                    "将永久删除本地会话“{{title}}”\nSession ID: {{sessionId}}\n\n此操作不可恢复。",
                  title: formatSessionTitle(deleteTargets[0]),
                  sessionId: deleteTargets[0].sessionId,
                })
              : ""
        }
        confirmText={
          deleteTargets && deleteTargets.length > 1
            ? t("sessionManager.batchDeleteConfirmAction", {
                defaultValue: "删除所选会话",
              })
            : t("sessionManager.deleteConfirmAction", {
                defaultValue: "删除会话",
              })
        }
        cancelText={t("common.cancel", { defaultValue: "取消" })}
        variant="destructive"
        onConfirm={() => void handleDeleteConfirm()}
        onCancel={() => {
          if (!isDeleting) {
            setDeleteTargets(null);
          }
        }}
      />
      <Dialog
        open={Boolean(moveTarget)}
        onOpenChange={(open) => !open && closeMoveDialog()}
      >
        <DialogContent className="max-w-xl">
          <DialogHeader>
            <DialogTitle>
              {t("sessionManager.moveTitle", {
                defaultValue: "移动 Codex 会话",
              })}
            </DialogTitle>
            <DialogDescription>
              {moveTarget
                ? t("sessionManager.moveDescription", {
                    defaultValue:
                      "更新 Codex 本地状态和 JSONL 元数据，把“{{title}}”归到目标项目。",
                    title: formatSessionTitle(moveTarget),
                  })
                : ""}
            </DialogDescription>
          </DialogHeader>

          <div className="grid gap-4 px-6 py-5">
            {moveTarget?.projectDir && (
              <div className="grid gap-1.5">
                <Label>
                  {t("sessionManager.currentProject", {
                    defaultValue: "当前项目",
                  })}
                </Label>
                <div className="rounded-md border bg-muted/50 px-3 py-2 font-mono text-xs break-all">
                  {moveTarget.projectDir}
                </div>
              </div>
            )}

            {moveProjectOptions.length > 0 && (
              <div className="grid gap-1.5">
                <Label>
                  {t("sessionManager.selectTargetProject", {
                    defaultValue: "选择已有项目",
                  })}
                </Label>
                <Select onValueChange={setMoveProjectDir}>
                  <SelectTrigger>
                    <SelectValue
                      placeholder={t(
                        "sessionManager.selectProjectPlaceholder",
                        {
                          defaultValue: "选择一个项目路径",
                        },
                      )}
                    />
                  </SelectTrigger>
                  <SelectContent>
                    {moveProjectOptions.map((dir) => (
                      <SelectItem key={dir} value={dir}>
                        <span className="font-mono text-xs">{dir}</span>
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            )}

            <div className="grid gap-1.5">
              <Label htmlFor="codex-session-target-project">
                {t("sessionManager.targetProject", {
                  defaultValue: "目标项目路径",
                })}
              </Label>
              <Input
                id="codex-session-target-project"
                value={moveProjectDir}
                onChange={(event) => setMoveProjectDir(event.target.value)}
                placeholder="/absolute/path/to/project"
                className="font-mono text-xs"
              />
              <p className="text-xs text-muted-foreground">
                {t("sessionManager.moveSafetyHint", {
                  defaultValue:
                    "移动前会备份 Codex state_5.sqlite 和会话 JSONL 文件。",
                })}
              </p>
            </div>
          </div>

          <DialogFooter>
            <Button
              variant="outline"
              onClick={closeMoveDialog}
              disabled={isMoving}
            >
              {t("common.cancel", { defaultValue: "取消" })}
            </Button>
            <Button
              onClick={() => void handleMoveConfirm()}
              disabled={!canConfirmMove}
            >
              {isMoving
                ? t("sessionManager.moving", {
                    defaultValue: "移动中...",
                  })
                : t("sessionManager.moveConfirm", {
                    defaultValue: "移动会话",
                  })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <Dialog open={backupDialogOpen} onOpenChange={setBackupDialogOpen}>
        <DialogContent className="max-w-4xl">
          <DialogHeader>
            <DialogTitle>
              {t("sessionManager.codexBackupsTitle", {
                defaultValue: "Codex Keeper 备份",
              })}
            </DialogTitle>
            <DialogDescription>
              {t("sessionManager.codexBackupsDescription", {
                defaultValue:
                  "管理 Repair、Move、Trim、Branch、Trash 操作创建的本地备份。",
              })}
            </DialogDescription>
          </DialogHeader>

          <div className="grid gap-4 px-6 py-5 max-h-[65vh] overflow-y-auto">
            <div className="flex items-center justify-between">
              <Badge variant="secondary">
                {t("sessionManager.backupCount", {
                  defaultValue: "{{count}} 个备份",
                  count: codexBackups.length,
                })}
              </Badge>
              <Button
                variant="outline"
                size="sm"
                onClick={() => void refreshCodexBackups()}
                disabled={isLoadingBackups}
              >
                <RefreshCw className="size-3.5 mr-1.5" />
                {t("common.refresh", { defaultValue: "刷新" })}
              </Button>
            </div>

            <div className="grid gap-2">
              {codexBackups.length === 0 ? (
                <div className="rounded-md border bg-muted/40 px-3 py-6 text-center text-sm text-muted-foreground">
                  {isLoadingBackups
                    ? t("common.loading", { defaultValue: "加载中..." })
                    : t("sessionManager.noBackups", {
                        defaultValue: "暂无备份",
                      })}
                </div>
              ) : (
                codexBackups.map((backup) => (
                  <div
                    key={backup.backupPath}
                    className="grid gap-2 rounded-md border px-3 py-2"
                  >
                    <div className="flex items-center justify-between gap-3">
                      <div className="min-w-0">
                        <div className="truncate font-mono text-xs">
                          {backup.originalName}
                        </div>
                        <div className="truncate text-xs text-muted-foreground">
                          {backup.reason}
                        </div>
                      </div>
                      <div className="flex items-center gap-1 shrink-0">
                        {backup.kind === "chatFile" && (
                          <Button
                            variant="outline"
                            size="sm"
                            className="h-7 gap-1.5"
                            onClick={async () => {
                              try {
                                await sessionsApi.restoreCodexBackup({
                                  backupPath: backup.backupPath,
                                  originalPath: backup.originalPath,
                                });
                                await refreshCodexBackups();
                                await queryClient.invalidateQueries({
                                  queryKey: ["sessions"],
                                });
                                toast.success(
                                  t("sessionManager.restoreSuccess", {
                                    defaultValue: "备份已恢复",
                                  }),
                                );
                              } catch (error) {
                                toast.error(
                                  extractErrorMessage(error) ||
                                    t("sessionManager.restoreFailed", {
                                      defaultValue: "恢复失败",
                                    }),
                                );
                              }
                            }}
                          >
                            <RotateCcw className="size-3" />
                            {t("sessionManager.restore", {
                              defaultValue: "恢复",
                            })}
                          </Button>
                        )}
                        <Button
                          variant="ghost"
                          size="sm"
                          className="h-7 text-destructive"
                          onClick={async () => {
                            try {
                              await sessionsApi.moveCodexBackupToTrash(
                                backup.backupPath,
                              );
                              await refreshCodexBackups();
                            } catch (error) {
                              toast.error(
                                extractErrorMessage(error) ||
                                  t("sessionManager.trashBackupFailed", {
                                    defaultValue: "移动备份到 Trash 失败",
                                  }),
                              );
                            }
                          }}
                        >
                          <Trash2 className="size-3" />
                        </Button>
                      </div>
                    </div>
                    <div className="break-all font-mono text-[11px] text-muted-foreground">
                      {backup.backupPath}
                    </div>
                  </div>
                ))
              )}
            </div>

            {(codexTrashedThreads.length > 0 ||
              codexTrashBackups.length > 0) && (
              <div className="grid gap-2 border-t pt-4">
                <div className="flex items-center justify-between">
                  <Badge variant="outline">
                    {t("sessionManager.trashCount", {
                      defaultValue:
                        "Trash: {{threads}} 个会话 / {{backups}} 个备份",
                      threads: codexTrashedThreads.length,
                      backups: codexTrashBackups.length,
                    })}
                  </Badge>
                  <div className="flex items-center gap-2">
                    {codexTrashedThreads.length > 0 && (
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={async () => {
                          try {
                            await sessionsApi.emptyCodexThreadTrash();
                            await refreshCodexBackups();
                            await queryClient.invalidateQueries({
                              queryKey: ["sessions"],
                            });
                          } catch (error) {
                            toast.error(
                              extractErrorMessage(error) ||
                                t("sessionManager.emptyTrashFailed", {
                                  defaultValue: "清空 Trash 失败",
                                }),
                            );
                          }
                        }}
                      >
                        {t("sessionManager.emptyThreadTrash", {
                          defaultValue: "清空会话 Trash",
                        })}
                      </Button>
                    )}
                    {codexTrashBackups.length > 0 && (
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={async () => {
                          try {
                            await sessionsApi.emptyCodexBackupTrash();
                            await refreshCodexBackups();
                          } catch (error) {
                            toast.error(
                              extractErrorMessage(error) ||
                                t("sessionManager.emptyTrashFailed", {
                                  defaultValue: "清空 Trash 失败",
                                }),
                            );
                          }
                        }}
                      >
                        {t("sessionManager.emptyBackupTrash", {
                          defaultValue: "清空备份 Trash",
                        })}
                      </Button>
                    )}
                  </div>
                </div>
                {codexTrashedThreads.map((thread) => (
                  <div
                    key={thread.manifestPath}
                    className="flex items-center justify-between gap-3 rounded-md border px-3 py-2"
                  >
                    <div className="min-w-0">
                      <div className="truncate text-sm">{thread.title}</div>
                      <div className="truncate font-mono text-xs text-muted-foreground">
                        {thread.originalPath}
                      </div>
                    </div>
                    <div className="flex items-center gap-1 shrink-0">
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={async () => {
                          try {
                            await sessionsApi.restoreCodexTrashedThread(
                              thread.manifestPath,
                            );
                            await refreshCodexBackups();
                            await queryClient.invalidateQueries({
                              queryKey: ["sessions"],
                            });
                          } catch (error) {
                            toast.error(
                              extractErrorMessage(error) ||
                                t("sessionManager.restoreFailed", {
                                  defaultValue: "恢复失败",
                                }),
                            );
                          }
                        }}
                      >
                        {t("sessionManager.restore", {
                          defaultValue: "恢复",
                        })}
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        className="text-destructive"
                        onClick={async () => {
                          try {
                            await sessionsApi.deleteCodexTrashedThread(
                              thread.manifestPath,
                            );
                            await refreshCodexBackups();
                          } catch (error) {
                            toast.error(
                              extractErrorMessage(error) ||
                                t("sessionManager.deleteFailed", {
                                  defaultValue: "删除失败",
                                }),
                            );
                          }
                        }}
                      >
                        <Trash2 className="size-3" />
                      </Button>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>

          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setBackupDialogOpen(false)}
            >
              {t("common.close", { defaultValue: "关闭" })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </TooltipProvider>
  );
}
