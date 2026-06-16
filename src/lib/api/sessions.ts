import { invoke } from "@tauri-apps/api/core";
import type { SessionMessage, SessionMeta } from "@/types";

export interface DeleteSessionOptions {
  providerId: string;
  sessionId: string;
  sourcePath: string;
}

export interface DeleteSessionResult extends DeleteSessionOptions {
  success: boolean;
  error?: string;
}

export interface MoveSessionOptions {
  providerId: string;
  sessionId: string;
  sourcePath: string;
  targetProjectDir: string;
}

export interface CodexOperationReport {
  success: boolean;
  sessionId: string;
  timestamp: string;
  backups: string[];
  changedFiles: string[];
  newSessionId?: string;
  newSourcePath?: string;
  removedLineCount?: number;
}

export interface CodexBackupFile {
  backupPath: string;
  originalPath: string;
  originalName: string;
  directory: string;
  stamp: string;
  kind: string;
  size: number;
  originalExists: boolean;
  reason: string;
}

export interface CodexTrashedThread {
  threadId: string;
  title: string;
  originalPath: string;
  trashPath?: string;
  manifestPath: string;
  cwd: string;
  trashedAt: string;
  size: number;
  originalExists: boolean;
}

export const sessionsApi = {
  async list(): Promise<SessionMeta[]> {
    return await invoke("list_sessions");
  },

  async getMessages(
    providerId: string,
    sourcePath: string,
  ): Promise<SessionMessage[]> {
    return await invoke("get_session_messages", { providerId, sourcePath });
  },

  async delete(options: DeleteSessionOptions): Promise<boolean> {
    const { providerId, sessionId, sourcePath } = options;
    return await invoke("delete_session", {
      providerId,
      sessionId,
      sourcePath,
    });
  },

  async deleteMany(
    items: DeleteSessionOptions[],
  ): Promise<DeleteSessionResult[]> {
    return await invoke("delete_sessions", { items });
  },

  async move(options: MoveSessionOptions): Promise<boolean> {
    const { providerId, sessionId, sourcePath, targetProjectDir } = options;
    return await invoke("move_session", {
      providerId,
      sessionId,
      sourcePath,
      targetProjectDir,
    });
  },

  async repair(options: DeleteSessionOptions): Promise<CodexOperationReport> {
    const { providerId, sessionId, sourcePath } = options;
    return await invoke("repair_session", {
      providerId,
      sessionId,
      sourcePath,
    });
  },

  async trim(
    options: DeleteSessionOptions & {
      lineNumber: number;
    },
  ): Promise<CodexOperationReport> {
    const { providerId, sessionId, sourcePath, lineNumber } = options;
    return await invoke("trim_session", {
      providerId,
      sessionId,
      sourcePath,
      lineNumber,
    });
  },

  async branch(
    options: DeleteSessionOptions & {
      lineNumber: number;
    },
  ): Promise<CodexOperationReport> {
    const { providerId, sessionId, sourcePath, lineNumber } = options;
    return await invoke("branch_session", {
      providerId,
      sessionId,
      sourcePath,
      lineNumber,
    });
  },

  async trash(options: DeleteSessionOptions): Promise<CodexOperationReport> {
    const { providerId, sessionId, sourcePath } = options;
    return await invoke("trash_session", { providerId, sessionId, sourcePath });
  },

  async searchCodexRaw(options: {
    query: string;
    projectDir?: string | null;
  }): Promise<string[]> {
    const { query, projectDir } = options;
    return await invoke("search_codex_sessions_raw", { query, projectDir });
  },

  async revealPath(path: string): Promise<boolean> {
    return await invoke("reveal_session_path", { path });
  },

  async listCodexBackups(includeTrash = false): Promise<CodexBackupFile[]> {
    return await invoke("list_codex_backups", { includeTrash });
  },

  async restoreCodexBackup(options: {
    backupPath: string;
    originalPath: string;
  }): Promise<boolean> {
    return await invoke("restore_codex_backup", options);
  },

  async moveCodexBackupToTrash(backupPath: string): Promise<boolean> {
    return await invoke("move_codex_backup_to_trash", { backupPath });
  },

  async listCodexTrashedThreads(): Promise<CodexTrashedThread[]> {
    return await invoke("list_codex_trashed_threads");
  },

  async restoreCodexTrashedThread(manifestPath: string): Promise<boolean> {
    return await invoke("restore_codex_trashed_thread", { manifestPath });
  },

  async deleteCodexTrashedThread(manifestPath: string): Promise<boolean> {
    return await invoke("delete_codex_trashed_thread", { manifestPath });
  },

  async emptyCodexThreadTrash(): Promise<number> {
    return await invoke("empty_codex_thread_trash");
  },

  async emptyCodexBackupTrash(): Promise<number> {
    return await invoke("empty_codex_backup_trash");
  },

  async launchTerminal(options: {
    command: string;
    cwd?: string | null;
    customConfig?: string | null;
  }): Promise<boolean> {
    const { command, cwd, customConfig } = options;
    return await invoke("launch_session_terminal", {
      command,
      cwd,
      customConfig,
    });
  },
};
