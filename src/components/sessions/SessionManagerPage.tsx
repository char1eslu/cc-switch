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
  FileSearch,
  Play,
  Trash2,
  MessageSquare,
  Clock,
  FolderOpen,
  FolderSearch,
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
  CODEX_CHATS_PROJECT_LABEL,
  CODEX_CHATS_PROJECT_ID,
  extractCodexPromptPreview,
  formatCodexProjectName,
  formatSessionMessagePreview,
  formatSessionTitle,
  formatTimestamp,
  getBaseName,
  getCodexStatusLabel,
  getCodexProjectFilterKey,
  getProviderIconName,
  getProviderLabel,
  getSessionKey,
  isSessionInProjectFilter,
  shouldHideCodexMessageFromToc,
} from "./utils";

type ProviderFilter = "all" | "codex" | "claude";

interface ProjectSummary {
  path: string;
  name: string;
  displayPath: string;
  totalCount: number;
  repairCount: number;
  availableCount: number;
  latestAt: number;
}

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
  const [moveTargets, setMoveTargets] = useState<SessionMeta[] | null>(null);
  const [moveProjectDir, setMoveProjectDir] = useState("");
  const [isMoving, setIsMoving] = useState(false);
  const [isRepairing, setIsRepairing] = useState(false);
  const [isTrashing, setIsTrashing] = useState(false);
  const [isDeepSearching, setIsDeepSearching] = useState(false);
  const [deepSearchIds, setDeepSearchIds] = useState<Set<string> | null>(null);
  const [deepSearchQuery, setDeepSearchQuery] = useState("");
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
  const [projectFilter, setProjectFilter] = useState("all");
  const [selectedKey, setSelectedKey] = useState<string | null>(null);

  const codexProjectSummaries = useMemo<ProjectSummary[]>(() => {
    const grouped = new Map<string, ProjectSummary>();
    sessions.forEach((session) => {
      if (session.providerId !== "codex") return;
      const rawPath = session.projectDir?.trim();
      const path = getCodexProjectFilterKey(rawPath);
      if (!path) return;
      const current =
        grouped.get(path) ??
        ({
          path,
          name: formatCodexProjectName(rawPath) || path,
          displayPath: path === CODEX_CHATS_PROJECT_ID ? "" : path,
          totalCount: 0,
          repairCount: 0,
          availableCount: 0,
          latestAt: 0,
        } satisfies ProjectSummary);
      current.totalCount += 1;
      if (session.needsRepair) {
        current.repairCount += 1;
      }
      if (
        session.archived !== true &&
        session.fileExists !== false &&
        session.isInSessionIndex !== false
      ) {
        current.availableCount += 1;
      }
      current.latestAt = Math.max(
        current.latestAt,
        session.lastActiveAt ?? session.createdAt ?? 0,
      );
      grouped.set(path, current);
    });

    return Array.from(grouped.values()).sort((a, b) => {
      if (a.path === CODEX_CHATS_PROJECT_ID) return -1;
      if (b.path === CODEX_CHATS_PROJECT_ID) return 1;
      if (b.latestAt !== a.latestAt) return b.latestAt - a.latestAt;
      return a.name.localeCompare(b.name);
    });
  }, [sessions]);

  const scopedSessions = useMemo(() => {
    return sessions.filter((session) => {
      if (providerFilter !== "all" && session.providerId !== providerFilter) {
        return false;
      }
      if (!isSessionInProjectFilter(session, projectFilter)) {
        return false;
      }
      return true;
    });
  }, [sessions, providerFilter, projectFilter]);

  // 使用 FlexSearch 全文搜索
  const { search: searchSessions } = useSessionSearch({
    sessions,
    providerFilter,
    projectFilter,
  });

  const metadataFilteredSessions = useMemo(() => {
    return searchSessions(search);
  }, [searchSessions, search]);

  const filteredSessions = useMemo(() => {
    const query = search.trim();
    if (
      !deepSearchIds ||
      query.length < 3 ||
      deepSearchQuery !== query ||
      providerFilter === "claude"
    ) {
      return metadataFilteredSessions;
    }

    const byKey = new Map(
      metadataFilteredSessions.map((session) => [
        getSessionKey(session),
        session,
      ]),
    );
    scopedSessions
      .filter(
        (session) =>
          session.providerId === "codex" &&
          deepSearchIds.has(session.sessionId),
      )
      .forEach((session) => byKey.set(getSessionKey(session), session));

    return Array.from(byKey.values()).sort((a, b) => {
      const aTs = a.lastActiveAt ?? a.createdAt ?? 0;
      const bTs = b.lastActiveAt ?? b.createdAt ?? 0;
      return bTs - aTs;
    });
  }, [
    deepSearchIds,
    deepSearchQuery,
    metadataFilteredSessions,
    providerFilter,
    scopedSessions,
    search,
  ]);

  useEffect(() => {
    if (
      projectFilter !== "all" &&
      !codexProjectSummaries.some((project) => project.path === projectFilter)
    ) {
      setProjectFilter("all");
    }
  }, [codexProjectSummaries, projectFilter]);

  useEffect(() => {
    if (providerFilter === "claude" && projectFilter !== "all") {
      setProjectFilter("all");
    }
  }, [projectFilter, providerFilter]);

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
  const isCodexSession = selectedSession?.providerId === "codex";
  const visibleMessages = useMemo(() => {
    if (!isCodexSession) return messages;
    return messages.filter(
      (message) => !shouldHideCodexMessageFromToc(message.content),
    );
  }, [isCodexSession, messages]);

  const virtualizer = useVirtualizer({
    count: visibleMessages.length,
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

  // 提取用户消息用于目录
  const userMessagesToc = useMemo(() => {
    return visibleMessages
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
  }, [isCodexSession, visibleMessages]);

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
        t("sessionManager.messageCopied", { defaultValue: "Message copied" }),
      );
    },
    [handleCopy, t],
  );

  const handleRevealPath = useCallback(
    async (path?: string | null) => {
      if (!path) return;
      try {
        await sessionsApi.revealPath(path);
      } catch (error) {
        toast.error(
          extractErrorMessage(error) ||
            t("sessionManager.revealFailed", {
              defaultValue: "Could not reveal file",
            }),
        );
      }
    },
    [t],
  );

  const clearSearch = useCallback(() => {
    setSearch("");
    setDeepSearchIds(null);
    setDeepSearchQuery("");
  }, []);

  const handleDeepSearch = useCallback(async () => {
    const query = search.trim();
    if (query.length < 3 || providerFilter === "claude" || isDeepSearching) {
      return;
    }

    setIsDeepSearching(true);
    try {
      const selectedProject = codexProjectSummaries.find(
        (project) => project.path === projectFilter,
      );
      const ids = await sessionsApi.searchCodexRaw({
        query,
        projectDir:
          projectFilter === "all" || projectFilter === CODEX_CHATS_PROJECT_ID
            ? undefined
            : selectedProject?.displayPath || projectFilter,
      });
      setDeepSearchIds(new Set(ids));
      setDeepSearchQuery(query);
      toast.success(
        t("sessionManager.deepSearchSuccess", {
          defaultValue: "Deep search matched {{count}} Codex sessions",
          count: ids.length,
        }),
      );
    } catch (error) {
      toast.error(
        extractErrorMessage(error) ||
          t("sessionManager.deepSearchFailed", {
            defaultValue: "Deep search failed",
          }),
      );
    } finally {
      setIsDeepSearching(false);
    }
  }, [
    codexProjectSummaries,
    isDeepSearching,
    projectFilter,
    providerFilter,
    search,
    t,
  ]);

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
            defaultValue: "Deleted {{count}} sessions",
            count: deletedKeys.length,
          }),
        );
      }

      if (failedErrors.length > 0) {
        toast.error(
          t("sessionManager.batchDeleteFailed", {
            defaultValue: "{{failed}} sessions could not be deleted",
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
            defaultValue: "Batch delete failed. Please try again later.",
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

  const selectedCodexSessions = useMemo(
    () =>
      selectedSessions.filter(
        (session) =>
          session.providerId === "codex" && Boolean(session.sourcePath),
      ),
    [selectedSessions],
  );

  const selectedRepairableCodexSessions = useMemo(
    () => selectedCodexSessions.filter((session) => session.needsRepair),
    [selectedCodexSessions],
  );

  const selectedMovableCodexSessions = useMemo(
    () =>
      selectedCodexSessions.filter(
        (session) => session.archived !== true && session.fileExists !== false,
      ),
    [selectedCodexSessions],
  );

  const codexProjectDirs = useMemo(() => {
    const dirs = new Set<string>();
    sessions.forEach((session) => {
      if (session.providerId !== "codex") return;
      const dir = session.projectDir?.trim();
      if (dir && getCodexProjectFilterKey(dir) !== CODEX_CHATS_PROJECT_ID) {
        dirs.add(dir);
      }
    });
    return Array.from(dirs).sort((a, b) => a.localeCompare(b));
  }, [sessions]);

  const moveTargetsList = moveTargets ?? [];
  const moveTarget = moveTargetsList[0] ?? null;
  const moveProjectOptions = useMemo(() => {
    if (moveTargetsList.length === 0) return [];
    return codexProjectDirs.filter(
      (dir) =>
        !moveTargetsList.every((session) => session.projectDir?.trim() === dir),
    );
  }, [codexProjectDirs, moveTargetsList]);

  const trimmedMoveProjectDir = moveProjectDir.trim();
  const canMoveSelectedSession =
    selectedSession?.providerId === "codex" &&
    Boolean(selectedSession.sourcePath) &&
    selectedSession.archived !== true &&
    selectedSession.fileExists !== false;
  const canConfirmMove =
    moveTargetsList.length > 0 &&
    trimmedMoveProjectDir.length > 0 &&
    moveTargetsList.some(
      (session) => session.projectDir?.trim() !== trimmedMoveProjectDir,
    ) &&
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
    setMoveTargets([session]);
    setMoveProjectDir("");
  };

  const openBatchMoveDialog = () => {
    if (selectedMovableCodexSessions.length === 0) return;
    setMoveTargets(selectedMovableCodexSessions);
    setMoveProjectDir("");
  };

  const closeMoveDialog = () => {
    if (isMoving) return;
    setMoveTargets(null);
    setMoveProjectDir("");
  };

  const handleMoveConfirm = async () => {
    if (!canConfirmMove) return;

    const targets = moveTargetsList.filter(
      (session) =>
        session.sourcePath &&
        session.projectDir?.trim() !== trimmedMoveProjectDir,
    );
    if (targets.length === 0) return;

    setIsMoving(true);
    try {
      const movedKeys = new Set<string>();
      const failures: string[] = [];

      for (const target of targets) {
        try {
          await sessionsApi.move({
            providerId: target.providerId,
            sessionId: target.sessionId,
            sourcePath: target.sourcePath!,
            targetProjectDir: trimmedMoveProjectDir,
          });
          movedKeys.add(getSessionKey(target));
        } catch (error) {
          failures.push(
            `${formatSessionTitle(target)}: ${extractErrorMessage(error)}`,
          );
        }
      }

      queryClient.setQueryData<SessionMeta[]>(["sessions"], (current) =>
        (current ?? []).map((session) =>
          movedKeys.has(getSessionKey(session))
            ? { ...session, projectDir: trimmedMoveProjectDir }
            : session,
        ),
      );
      await queryClient.invalidateQueries({ queryKey: ["sessions"] });

      if (movedKeys.size > 0) {
        setProviderFilter("codex");
        setProjectFilter(getCodexProjectFilterKey(trimmedMoveProjectDir));
        toast.success(
          t("sessionManager.moveSuccess", {
            defaultValue: "Session moved",
            count: movedKeys.size,
          }),
        );
      }
      if (failures.length > 0) {
        toast.error(
          t("sessionManager.movePartialFailed", {
            defaultValue: "{{count}} sessions could not be moved",
            count: failures.length,
          }),
          { description: failures[0] },
        );
      }
      setSelectedSessionKeys((current) => {
        const next = new Set(current);
        movedKeys.forEach((key) => next.delete(key));
        return next;
      });
      setMoveTargets(null);
      setMoveProjectDir("");
    } catch (error) {
      toast.error(
        extractErrorMessage(error) ||
          t("sessionManager.moveFailed", {
            defaultValue: "Failed to move session",
          }),
      );
    } finally {
      setIsMoving(false);
    }
  };

  const handleBatchRepair = async () => {
    const targets = selectedRepairableCodexSessions;
    if (targets.length === 0 || isRepairing) return;

    setIsRepairing(true);
    const repairedKeys = new Set<string>();
    const failures: string[] = [];
    try {
      for (const target of targets) {
        try {
          await sessionsApi.repair({
            providerId: target.providerId,
            sessionId: target.sessionId,
            sourcePath: target.sourcePath!,
          });
          repairedKeys.add(getSessionKey(target));
        } catch (error) {
          failures.push(
            `${formatSessionTitle(target)}: ${extractErrorMessage(error)}`,
          );
        }
      }
      await queryClient.invalidateQueries({ queryKey: ["sessions"] });
      if (repairedKeys.size > 0) {
        toast.success(
          t("sessionManager.batchRepairSuccess", {
            defaultValue: "Repaired {{count}} session indexes",
            count: repairedKeys.size,
          }),
        );
      }
      if (failures.length > 0) {
        toast.error(
          t("sessionManager.batchRepairFailed", {
            defaultValue: "{{count}} sessions could not be repaired",
            count: failures.length,
          }),
          { description: failures[0] },
        );
      }
      setSelectedSessionKeys((current) => {
        const next = new Set(current);
        repairedKeys.forEach((key) => next.delete(key));
        return next;
      });
    } finally {
      setIsRepairing(false);
    }
  };

  const handleBatchTrash = async () => {
    const targets = selectedCodexSessions;
    if (targets.length === 0 || isTrashing) return;

    setIsTrashing(true);
    const trashedKeys = new Set<string>();
    const failures: string[] = [];
    try {
      for (const target of targets) {
        try {
          await sessionsApi.trash({
            providerId: target.providerId,
            sessionId: target.sessionId,
            sourcePath: target.sourcePath!,
          });
          trashedKeys.add(getSessionKey(target));
          queryClient.removeQueries({
            queryKey: ["sessionMessages", target.providerId, target.sourcePath],
          });
        } catch (error) {
          failures.push(
            `${formatSessionTitle(target)}: ${extractErrorMessage(error)}`,
          );
        }
      }

      if (trashedKeys.size > 0) {
        queryClient.setQueryData<SessionMeta[]>(["sessions"], (current) =>
          (current ?? []).filter(
            (session) => !trashedKeys.has(getSessionKey(session)),
          ),
        );
      }
      await queryClient.invalidateQueries({ queryKey: ["sessions"] });
      if (trashedKeys.size > 0) {
        toast.success(
          t("sessionManager.batchTrashSuccess", {
            defaultValue: "Moved {{count}} sessions to Trash",
            count: trashedKeys.size,
          }),
        );
      }
      if (failures.length > 0) {
        toast.error(
          t("sessionManager.batchTrashFailed", {
            defaultValue: "{{count}} sessions could not be moved to Trash",
            count: failures.length,
          }),
          { description: failures[0] },
        );
      }
      setSelectedSessionKeys((current) => {
        const next = new Set(current);
        trashedKeys.forEach((key) => next.delete(key));
        return next;
      });
    } finally {
      setIsTrashing(false);
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
            defaultValue: "Failed to load backups",
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
          defaultValue: "Index repaired",
        }),
      );
    } catch (error) {
      toast.error(
        extractErrorMessage(error) ||
          t("sessionManager.repairFailed", {
            defaultValue: "Failed to repair index",
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
          defaultValue: "Session moved to Codex Wake Trash",
        }),
      );
    } catch (error) {
      toast.error(
        extractErrorMessage(error) ||
          t("sessionManager.trashFailed", {
            defaultValue: "Failed to move to Trash",
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
              defaultValue: "Session trimmed",
            })
          : t("sessionManager.branchSuccess", {
              defaultValue: "Branch session created",
            }),
      );
    } catch (error) {
      toast.error(
        extractErrorMessage(error) ||
          t("sessionManager.operationFailed", {
            defaultValue: "Operation failed",
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
          <div
            className={
              backupDialogOpen || Boolean(moveTargets) || Boolean(deleteTargets)
                ? "flex-1 overflow-hidden grid gap-4 md:grid-cols-[320px_1fr] opacity-40"
                : "flex-1 overflow-hidden grid gap-4 md:grid-cols-[320px_1fr]"
            }
          >
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
                            clearSearch();
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
                        aria-label={t("common.clear", {
                          defaultValue: "Clear",
                        })}
                        onClick={() => {
                          setIsSearchOpen(false);
                          clearSearch();
                        }}
                      >
                        <X className="size-3" />
                      </Button>
                    </div>
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <Button
                          variant="ghost"
                          size="icon"
                          className="size-7"
                          aria-label={t("sessionManager.deepSearch", {
                            defaultValue: "Deep search Codex JSONL",
                          })}
                          onClick={() => void handleDeepSearch()}
                          disabled={
                            search.trim().length < 3 ||
                            providerFilter === "claude" ||
                            isDeepSearching
                          }
                        >
                          {isDeepSearching ? (
                            <RefreshCw className="size-3.5 animate-spin" />
                          ) : (
                            <FileSearch className="size-3.5" />
                          )}
                        </Button>
                      </TooltipTrigger>
                      <TooltipContent>
                        {t("sessionManager.deepSearch", {
                          defaultValue: "Deep search Codex JSONL",
                        })}
                      </TooltipContent>
                    </Tooltip>
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
                                defaultValue: "Exit batch management",
                              },
                            )}
                            onClick={exitSelectionMode}
                          >
                            <CheckSquare className="size-3.5" />
                          </Button>
                        </TooltipTrigger>
                        <TooltipContent>
                          {t("sessionManager.exitBatchModeTooltip", {
                            defaultValue: "Exit batch management",
                          })}
                        </TooltipContent>
                      </Tooltip>
                    )}
                  </div>
                ) : (
                  <div className="flex flex-col gap-2">
                    <div className="flex flex-wrap items-center justify-between gap-2">
                      <div className="flex items-center gap-2 min-w-0">
                        <CardTitle className="text-sm font-medium whitespace-nowrap">
                          {t("sessionManager.sessionList")}
                        </CardTitle>
                        <Badge variant="secondary" className="text-xs">
                          {filteredSessions.length}
                        </Badge>
                      </div>
                      <div className="flex flex-wrap items-center justify-end gap-1 shrink-0">
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
                                        defaultValue: "Exit batch management",
                                      })
                                    : t("sessionManager.manageBatchTooltip", {
                                        defaultValue: "Batch management",
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
                                    defaultValue: "Exit batch management",
                                  })
                                : t("sessionManager.manageBatchTooltip", {
                                    defaultValue: "Batch management",
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
                              aria-label={t("sessionManager.searchSessions", {
                                defaultValue: "Search sessions",
                              })}
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
                          </SelectContent>
                        </Select>

                        {providerFilter !== "claude" &&
                          codexProjectSummaries.length > 0 && (
                            <Select
                              value={projectFilter}
                              onValueChange={(value) => setProjectFilter(value)}
                            >
                              <Tooltip>
                                <TooltipTrigger asChild>
                                  <SelectTrigger className="h-7 w-[104px] max-w-[104px] border-0 bg-transparent px-2 text-xs hover:bg-muted">
                                    <div className="flex min-w-0 items-center gap-1.5">
                                      <FolderOpen className="size-3.5 shrink-0" />
                                      <span className="truncate">
                                        {projectFilter === "all"
                                          ? t("sessionManager.projectFilterAll")
                                          : (codexProjectSummaries.find(
                                              (project) =>
                                                project.path === projectFilter,
                                            )?.name ??
                                            getBaseName(projectFilter))}
                                      </span>
                                    </div>
                                  </SelectTrigger>
                                </TooltipTrigger>
                                <TooltipContent className="max-w-xs">
                                  {projectFilter === "all"
                                    ? t("sessionManager.projectFilterAll", {
                                        defaultValue: "All projects",
                                      })
                                    : codexProjectSummaries.find(
                                        (project) =>
                                          project.path === projectFilter,
                                      )?.displayPath ||
                                      CODEX_CHATS_PROJECT_LABEL}
                                </TooltipContent>
                              </Tooltip>
                              <SelectContent className="w-[min(620px,calc(100vw-3rem))] max-w-[min(620px,calc(100vw-3rem))]">
                                <SelectItem value="all">
                                  <div className="flex items-center gap-2">
                                    <FolderOpen className="size-3.5" />
                                    <span>
                                      {t("sessionManager.projectFilterAll", {
                                        defaultValue: "All projects",
                                      })}
                                    </span>
                                    <Badge variant="secondary" className="ml-1">
                                      {
                                        sessions.filter(
                                          (session) =>
                                            session.providerId === "codex",
                                        ).length
                                      }
                                    </Badge>
                                  </div>
                                </SelectItem>
                                {codexProjectSummaries.map((project) => (
                                  <SelectItem
                                    key={project.path}
                                    value={project.path}
                                  >
                                    <div className="grid w-full min-w-0 grid-cols-[minmax(0,1fr)_auto] items-center gap-3">
                                      <div className="min-w-0">
                                        <div className="truncate text-sm">
                                          {project.name}
                                        </div>
                                        {project.displayPath && (
                                          <div className="truncate font-mono text-xs text-muted-foreground">
                                            {project.displayPath}
                                          </div>
                                        )}
                                      </div>
                                      <div className="flex shrink-0 items-center gap-1">
                                        <Badge variant="secondary">
                                          {project.totalCount}
                                        </Badge>
                                        {project.repairCount > 0 && (
                                          <Badge variant="outline">
                                            {project.repairCount}
                                          </Badge>
                                        )}
                                      </div>
                                    </div>
                                  </SelectItem>
                                ))}
                              </SelectContent>
                            </Select>
                          )}

                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Button
                              variant="ghost"
                              size="icon"
                              className="size-7"
                              aria-label={t("common.refresh", {
                                defaultValue: "Refresh",
                              })}
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
                      <div className="grid gap-2 rounded-md border bg-muted/40 px-3 py-2">
                        <div className="flex min-w-0 items-center justify-between gap-2 text-xs text-muted-foreground">
                          <Badge
                            variant="outline"
                            className="shrink-0 whitespace-nowrap text-xs"
                          >
                            {t("sessionManager.selectedCount", {
                              defaultValue: "{{count}} selected",
                              count: selectedDeletableSessions.length,
                            })}
                          </Badge>
                          <span className="min-w-0 truncate">
                            {t("sessionManager.batchModeHint", {
                              defaultValue: "Select sessions to manage",
                            })}
                          </span>
                        </div>
                        <div className="flex min-w-0 items-center justify-between gap-2">
                          <div className="flex min-w-0 items-center gap-1">
                            {deletableFilteredSessions.length > 0 && (
                              <Tooltip>
                                <TooltipTrigger asChild>
                                  <Button
                                    variant="ghost"
                                    size="icon"
                                    className="size-7"
                                    onClick={handleToggleSelectAll}
                                    aria-label={
                                      allFilteredSelected
                                        ? t(
                                            "sessionManager.clearFilteredSelection",
                                            {
                                              defaultValue: "Clear selection",
                                            },
                                          )
                                        : t(
                                            "sessionManager.selectAllFiltered",
                                            {
                                              defaultValue: "Select all",
                                            },
                                          )
                                    }
                                  >
                                    <CheckSquare className="size-3.5" />
                                  </Button>
                                </TooltipTrigger>
                                <TooltipContent>
                                  {allFilteredSelected
                                    ? t(
                                        "sessionManager.clearFilteredSelection",
                                        {
                                          defaultValue: "Clear selection",
                                        },
                                      )
                                    : t("sessionManager.selectAllFiltered", {
                                        defaultValue: "Select all",
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
                                  onClick={() =>
                                    setSelectedSessionKeys(new Set())
                                  }
                                  aria-label={t(
                                    "sessionManager.clearSelection",
                                    {
                                      defaultValue: "Clear",
                                    },
                                  )}
                                >
                                  <X className="size-3.5" />
                                </Button>
                              </TooltipTrigger>
                              <TooltipContent>
                                {t("sessionManager.clearSelection", {
                                  defaultValue: "Clear",
                                })}
                              </TooltipContent>
                            </Tooltip>
                          </div>
                          <div className="flex shrink-0 items-center gap-1">
                            <Tooltip>
                              <TooltipTrigger asChild>
                                <Button
                                  variant="outline"
                                  size="icon"
                                  className="size-7"
                                  onClick={() => void handleBatchRepair()}
                                  disabled={
                                    isRepairing ||
                                    selectedRepairableCodexSessions.length === 0
                                  }
                                  aria-label={t(
                                    "sessionManager.repairSelected",
                                    {
                                      defaultValue: "Repair",
                                    },
                                  )}
                                >
                                  <Wrench className="size-3.5" />
                                </Button>
                              </TooltipTrigger>
                              <TooltipContent>
                                {t("sessionManager.repairSelected", {
                                  defaultValue: "Repair",
                                })}
                              </TooltipContent>
                            </Tooltip>
                            <Tooltip>
                              <TooltipTrigger asChild>
                                <Button
                                  variant="outline"
                                  size="icon"
                                  className="size-7"
                                  onClick={openBatchMoveDialog}
                                  disabled={
                                    isMoving ||
                                    selectedMovableCodexSessions.length === 0
                                  }
                                  aria-label={t("sessionManager.moveSelected", {
                                    defaultValue: "Move",
                                  })}
                                >
                                  <FolderOpen className="size-3.5" />
                                </Button>
                              </TooltipTrigger>
                              <TooltipContent>
                                {t("sessionManager.moveSelected", {
                                  defaultValue: "Move",
                                })}
                              </TooltipContent>
                            </Tooltip>
                            <Tooltip>
                              <TooltipTrigger asChild>
                                <Button
                                  variant="outline"
                                  size="icon"
                                  className="size-7"
                                  onClick={() => void handleBatchTrash()}
                                  disabled={
                                    isTrashing ||
                                    selectedCodexSessions.length === 0
                                  }
                                  aria-label={t(
                                    "sessionManager.trashSelected",
                                    {
                                      defaultValue: "Trash",
                                    },
                                  )}
                                >
                                  <Archive className="size-3.5" />
                                </Button>
                              </TooltipTrigger>
                              <TooltipContent>
                                {t("sessionManager.trashSelected", {
                                  defaultValue: "Trash",
                                })}
                              </TooltipContent>
                            </Tooltip>
                            <Tooltip>
                              <TooltipTrigger asChild>
                                <Button
                                  variant="destructive"
                                  size="icon"
                                  className="size-7"
                                  onClick={openBatchDeleteDialog}
                                  disabled={
                                    isDeleting ||
                                    selectedDeletableSessions.length === 0
                                  }
                                  aria-label={
                                    isBatchDeleting
                                      ? t("sessionManager.batchDeleting", {
                                          defaultValue: "Deleting...",
                                        })
                                      : t("sessionManager.deleteSelected", {
                                          defaultValue: "Delete",
                                        })
                                  }
                                >
                                  <Trash2 className="size-3.5" />
                                </Button>
                              </TooltipTrigger>
                              <TooltipContent>
                                {isBatchDeleting
                                  ? t("sessionManager.batchDeleting", {
                                      defaultValue: "Deleting...",
                                    })
                                  : t("sessionManager.deleteSelected", {
                                      defaultValue: "Delete",
                                    })}
                              </TooltipContent>
                            </Tooltip>
                          </div>
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
                    <div className="flex flex-col gap-3 2xl:flex-row 2xl:items-start 2xl:justify-between">
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
                              className="shrink-0 whitespace-nowrap text-[10px]"
                            >
                              {getCodexStatusLabel(
                                selectedSession.codexStatus,
                                t,
                              )}
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
                                    {selectedSession.providerId === "codex"
                                      ? formatCodexProjectName(
                                          selectedSession.projectDir,
                                        )
                                      : getBaseName(selectedSession.projectDir)}
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
                      <div className="flex flex-wrap items-center gap-2">
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
                                <span className="hidden 2xl:inline">
                                  {t("sessionManager.resume", {
                                    defaultValue: "Resume session",
                                  })}
                                </span>
                              </Button>
                            </TooltipTrigger>
                            <TooltipContent>
                              {selectedSession.resumeCommand
                                ? t("sessionManager.resumeTooltip", {
                                    defaultValue:
                                      "Resume this session in terminal",
                                  })
                                : t("sessionManager.noResumeCommand", {
                                    defaultValue:
                                      "This session cannot be resumed",
                                  })}
                            </TooltipContent>
                          </Tooltip>
                        )}
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Button
                              size="sm"
                              variant="outline"
                              className="gap-1.5"
                              onClick={() =>
                                void handleRevealPath(
                                  selectedSession.sourcePath ??
                                    selectedSession.projectDir,
                                )
                              }
                              disabled={
                                !selectedSession.sourcePath &&
                                !selectedSession.projectDir
                              }
                            >
                              <FolderSearch className="size-3.5" />
                              <span className="hidden 2xl:inline">
                                {t("sessionManager.reveal", {
                                  defaultValue: "Reveal",
                                })}
                              </span>
                            </Button>
                          </TooltipTrigger>
                          <TooltipContent>
                            {t("sessionManager.revealTooltip", {
                              defaultValue: "Reveal the session file in Finder",
                            })}
                          </TooltipContent>
                        </Tooltip>
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
                                  <span className="hidden 2xl:inline">
                                    {selectedSession.needsRepair
                                      ? t("sessionManager.repairIndex", {
                                          defaultValue: "Repair index",
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
                                    "Repair Codex session_index and SQLite metadata",
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
                                  <span className="hidden 2xl:inline">
                                    {t("sessionManager.backups", {
                                      defaultValue: "Backups",
                                    })}
                                  </span>
                                </Button>
                              </TooltipTrigger>
                              <TooltipContent>
                                {t("sessionManager.backupsTooltip", {
                                  defaultValue: "Manage backups and Trash",
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
                              <span className="hidden 2xl:inline">
                                {isMoving
                                  ? t("sessionManager.moving", {
                                      defaultValue: "Moving...",
                                    })
                                  : t("sessionManager.move", {
                                      defaultValue: "Move",
                                    })}
                              </span>
                            </Button>
                          </TooltipTrigger>
                          <TooltipContent>
                            {canMoveSelectedSession
                              ? t("sessionManager.moveTooltip", {
                                  defaultValue:
                                    "Move this Codex session to another project",
                                })
                              : t("sessionManager.moveCodexOnlyTooltip", {
                                  defaultValue:
                                    "Only Codex sessions can be moved",
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
                                <span className="hidden 2xl:inline">
                                  {t("sessionManager.trash", {
                                    defaultValue: "Trash",
                                  })}
                                </span>
                              </Button>
                            </TooltipTrigger>
                            <TooltipContent>
                              {t("sessionManager.trashTooltip", {
                                defaultValue:
                                  "Move to Codex Wake Trash. You can restore it from backups.",
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
                              <span className="hidden 2xl:inline">
                                {isDeleting
                                  ? t("sessionManager.deleting", {
                                      defaultValue: "Deleting...",
                                    })
                                  : t("sessionManager.delete", {
                                      defaultValue: "Delete session",
                                    })}
                              </span>
                            </Button>
                          </TooltipTrigger>
                          <TooltipContent>
                            {t("sessionManager.deleteTooltip", {
                              defaultValue:
                                "Permanently delete this local session record",
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
                              defaultValue: "Copy command",
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
                                defaultValue: "Conversation history",
                              })}
                            </span>
                            <Badge variant="secondary" className="text-xs">
                              {visibleMessages.length}
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
                          ) : visibleMessages.length === 0 ? (
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
                                      message={
                                        visibleMessages[virtualRow.index]
                                      }
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
                defaultValue: "Delete selected sessions",
              })
            : t("sessionManager.deleteConfirmTitle", {
                defaultValue: "Delete session",
              })
        }
        message={
          deleteTargets && deleteTargets.length > 1
            ? t("sessionManager.batchDeleteConfirmMessage", {
                defaultValue:
                  "This will permanently delete {{count}} selected local session records.\n\nThis action cannot be undone.",
                count: deleteTargets.length,
              })
            : deleteTargets?.[0]
              ? t("sessionManager.deleteConfirmMessage", {
                  defaultValue:
                    'This will permanently delete local session "{{title}}"\nSession ID: {{sessionId}}\n\nThis action cannot be undone.',
                  title: formatSessionTitle(deleteTargets[0]),
                  sessionId: deleteTargets[0].sessionId,
                })
              : ""
        }
        confirmText={
          deleteTargets && deleteTargets.length > 1
            ? t("sessionManager.batchDeleteConfirmAction", {
                defaultValue: "Delete selected",
              })
            : t("sessionManager.deleteConfirmAction", {
                defaultValue: "Delete session",
              })
        }
        cancelText={t("common.cancel", { defaultValue: "Cancel" })}
        variant="destructive"
        onConfirm={() => void handleDeleteConfirm()}
        onCancel={() => {
          if (!isDeleting) {
            setDeleteTargets(null);
          }
        }}
      />
      <Dialog
        open={Boolean(moveTargets)}
        onOpenChange={(open) => !open && closeMoveDialog()}
      >
        <DialogContent
          zIndex="top"
          className="w-[min(680px,calc(100vw-3rem))] max-w-none overflow-hidden"
        >
          <DialogHeader>
            <DialogTitle>
              {t("sessionManager.moveTitle", {
                defaultValue: "Move Codex session",
              })}
            </DialogTitle>
            <DialogDescription>
              {moveTargetsList.length > 1
                ? t("sessionManager.moveBatchDescription", {
                    defaultValue:
                      "Update SQLite and JSONL project paths for {{count}} Codex sessions.",
                    count: moveTargetsList.length,
                  })
                : moveTarget
                  ? t("sessionManager.moveDescription", {
                      defaultValue:
                        'Update Codex local state and JSONL metadata so "{{title}}" belongs to the target project.',
                      title: formatSessionTitle(moveTarget),
                    })
                  : ""}
            </DialogDescription>
          </DialogHeader>

          <div className="grid min-w-0 gap-4 overflow-hidden px-6 py-5">
            {moveTargetsList.length > 1 ? (
              <div className="grid min-w-0 gap-1.5">
                <Label>
                  {t("sessionManager.selectedSessions", {
                    defaultValue: "Selected sessions",
                  })}
                </Label>
                <div className="rounded-md border bg-muted/50 px-3 py-2 text-xs text-muted-foreground">
                  {t("sessionManager.selectedMoveCount", {
                    defaultValue: "{{count}} Codex sessions",
                    count: moveTargetsList.length,
                  })}
                </div>
              </div>
            ) : moveTarget?.projectDir ? (
              <div className="grid min-w-0 gap-1.5">
                <Label>
                  {t("sessionManager.currentProject", {
                    defaultValue: "Current project",
                  })}
                </Label>
                <div className="min-w-0 truncate rounded-md border bg-muted/50 px-3 py-2 font-mono text-xs">
                  {moveTarget.projectDir}
                </div>
              </div>
            ) : null}

            {moveProjectOptions.length > 0 && (
              <div className="grid min-w-0 gap-1.5">
                <Label>
                  {t("sessionManager.selectTargetProject", {
                    defaultValue: "Choose existing project",
                  })}
                </Label>
                <Select onValueChange={setMoveProjectDir}>
                  <SelectTrigger className="min-w-0 max-w-full overflow-hidden font-mono text-xs [&>span]:min-w-0 [&>span]:truncate">
                    <SelectValue
                      placeholder={t(
                        "sessionManager.selectProjectPlaceholder",
                        {
                          defaultValue: "Choose a project path",
                        },
                      )}
                    />
                  </SelectTrigger>
                  <SelectContent className="max-w-[min(640px,calc(100vw-4rem))]">
                    {moveProjectOptions.map((dir) => (
                      <SelectItem key={dir} value={dir}>
                        <span className="block max-w-[min(560px,calc(100vw-6rem))] truncate font-mono text-xs">
                          {dir}
                        </span>
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            )}

            <div className="grid min-w-0 gap-1.5">
              <Label htmlFor="codex-session-target-project">
                {t("sessionManager.targetProject", {
                  defaultValue: "Target project path",
                })}
              </Label>
              <Input
                id="codex-session-target-project"
                value={moveProjectDir}
                onChange={(event) => setMoveProjectDir(event.target.value)}
                placeholder="/absolute/path/to/project"
                className="min-w-0 max-w-full font-mono text-xs"
              />
              <p className="text-xs text-muted-foreground">
                {t("sessionManager.moveSafetyHint", {
                  defaultValue:
                    "Codex state_5.sqlite and session JSONL files are backed up before moving.",
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
              {t("common.cancel", { defaultValue: "Cancel" })}
            </Button>
            <Button
              onClick={() => void handleMoveConfirm()}
              disabled={!canConfirmMove}
            >
              {isMoving
                ? t("sessionManager.moving", {
                    defaultValue: "Moving...",
                  })
                : t("sessionManager.moveConfirm", {
                    defaultValue: "Move session",
                  })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <Dialog open={backupDialogOpen} onOpenChange={setBackupDialogOpen}>
        <DialogContent zIndex="top" className="max-w-4xl">
          <DialogHeader>
            <DialogTitle>
              {t("sessionManager.codexBackupsTitle", {
                defaultValue: "Backups",
              })}
            </DialogTitle>
          </DialogHeader>

          <div className="grid gap-4 px-6 py-5 max-h-[65vh] overflow-y-auto">
            <div className="flex items-center justify-between">
              <Badge variant="secondary">
                {t("sessionManager.backupCount", {
                  defaultValue: "{{count}} backups",
                  count: codexBackups.length,
                })}
              </Badge>
              <div className="flex items-center gap-2">
                {codexBackups.length > 0 && (
                  <Button
                    variant="outline"
                    size="sm"
                    className="text-destructive hover:text-destructive"
                    onClick={async () => {
                      try {
                        await Promise.all(
                          codexBackups.map((backup) =>
                            sessionsApi.moveCodexBackupToTrash(
                              backup.backupPath,
                            ),
                          ),
                        );
                        await refreshCodexBackups();
                        toast.success(
                          t("sessionManager.trashAllBackupsSuccess", {
                            defaultValue: "Moved all backups to Trash",
                          }),
                        );
                      } catch (error) {
                        toast.error(
                          extractErrorMessage(error) ||
                            t("sessionManager.trashBackupFailed", {
                              defaultValue: "Failed to move backup to Trash",
                            }),
                        );
                      }
                    }}
                    disabled={isLoadingBackups}
                  >
                    <Trash2 className="size-3.5 mr-1.5" />
                    {t("sessionManager.trashAllBackups", {
                      defaultValue: "Trash all",
                    })}
                  </Button>
                )}
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => void refreshCodexBackups()}
                  disabled={isLoadingBackups}
                >
                  <RefreshCw className="size-3.5 mr-1.5" />
                  {t("common.refresh", { defaultValue: "Refresh" })}
                </Button>
              </div>
            </div>

            <div className="grid gap-2">
              {codexBackups.length === 0 ? (
                <div className="rounded-md border bg-muted/40 px-3 py-6 text-center text-sm text-muted-foreground">
                  {isLoadingBackups
                    ? t("common.loading", { defaultValue: "Loading..." })
                    : t("sessionManager.noBackups", {
                        defaultValue: "No backups",
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
                                    defaultValue: "Backup restored",
                                  }),
                                );
                              } catch (error) {
                                toast.error(
                                  extractErrorMessage(error) ||
                                    t("sessionManager.restoreFailed", {
                                      defaultValue: "Restore failed",
                                    }),
                                );
                              }
                            }}
                          >
                            <RotateCcw className="size-3" />
                            {t("sessionManager.restore", {
                              defaultValue: "Restore",
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
                                    defaultValue:
                                      "Failed to move backup to Trash",
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
                        "Trash: {{threads}} sessions / {{backups}} backups",
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
                                  defaultValue: "Failed to empty Trash",
                                }),
                            );
                          }
                        }}
                      >
                        {t("sessionManager.emptyThreadTrash", {
                          defaultValue: "Empty session Trash",
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
                                  defaultValue: "Failed to empty Trash",
                                }),
                            );
                          }
                        }}
                      >
                        {t("sessionManager.emptyBackupTrash", {
                          defaultValue: "Empty backup Trash",
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
                                  defaultValue: "Restore failed",
                                }),
                            );
                          }
                        }}
                      >
                        {t("sessionManager.restore", {
                          defaultValue: "Restore",
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
                                  defaultValue: "Delete failed",
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
              {t("common.close", { defaultValue: "Close" })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </TooltipProvider>
  );
}
