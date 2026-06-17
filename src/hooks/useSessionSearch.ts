import { useCallback, useMemo } from "react";
import FlexSearch from "flexsearch";
import type { SessionMeta } from "@/types";
import { isSessionInProjectFilter } from "@/components/sessions/utils";

interface UseSessionSearchOptions {
  sessions: SessionMeta[];
  providerFilter: string;
  projectFilter?: string;
}

interface UseSessionSearchResult {
  search: (query: string) => SessionMeta[];
}

/**
 * 使用 FlexSearch 实现会话全文搜索
 * 索引会话元数据（标题、摘要、项目目录等）
 */
export function useSessionSearch({
  sessions,
  providerFilter,
  projectFilter = "all",
}: UseSessionSearchOptions): UseSessionSearchResult {
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

  const index = useMemo(() => {
    const nextIndex = new FlexSearch.Index({
      tokenize: "full",
      resolution: 9,
    });

    scopedSessions.forEach((session, idx) => {
      const metaContent = [
        session.sessionId,
        session.title,
        session.summary,
        session.projectDir,
        session.sourcePath,
      ]
        .filter(Boolean)
        .join(" ");

      nextIndex.add(idx, metaContent);
    });

    return nextIndex;
  }, [scopedSessions]);

  const search = useCallback(
    (query: string): SessionMeta[] => {
      const needle = query.trim();

      if (!needle) {
        return [...scopedSessions].sort((a, b) => {
          const aTs = a.lastActiveAt ?? a.createdAt ?? 0;
          const bTs = b.lastActiveAt ?? b.createdAt ?? 0;
          return bTs - aTs;
        });
      }

      const results = index.search(needle, {
        limit: scopedSessions.length,
      }) as number[];

      return results.map((idx) => scopedSessions[idx]);
    },
    [index, scopedSessions],
  );

  return { search };
}
