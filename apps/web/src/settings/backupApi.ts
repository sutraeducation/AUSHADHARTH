import {
  BackupInspectionSchema,
  BackupJobSchema,
  BackupStatusSchema,
  PreparedRestoreSchema,
  RestoreCommittedSchema,
  type BackupInspection,
  type BackupJob,
  type BackupStatus,
  type PreparedRestore,
  type RestoreCommitted
} from "@aushadharth/contracts";
import { localServiceRequest, localServiceUpload } from "../platform/localService";

/**
 * The browser's half of backup and restore.
 *
 * Nothing here names a path on disk. A backup is addressed by the id the service issued, a prepared
 * restore by an opaque token, and the file to upload comes from the operating system's own picker —
 * the web application never learns, and never sends, where anything lives.
 */

/** Taking a backup snapshots the whole database, so it is given room rather than the usual seconds. */
const BACKUP_TIMEOUT_MS = 10 * 60_000;

export async function fetchBackupStatus(): Promise<BackupStatus> {
  return BackupStatusSchema.parse(await localServiceRequest("/api/v1/backups/status"));
}

export async function createBackup(): Promise<BackupJob> {
  return BackupJobSchema.parse(
    await localServiceRequest("/api/v1/backups/create", {
      method: "POST",
      body: "{}",
      signal: AbortSignal.timeout(BACKUP_TIMEOUT_MS)
    })
  );
}

/**
 * Asks the service where a finished backup can be fetched from.
 *
 * The service answers with a URL it serves itself rather than a filesystem path, which is what keeps
 * the download inside the same authenticated origin as everything else.
 */
export async function resolveDownload(backupId: string): Promise<{ filename: string; url: string }> {
  const body = (await localServiceRequest(`/api/v1/backups/${encodeURIComponent(backupId)}/download`)) as {
    filename: string;
    url: string;
  };
  return { filename: body.filename, url: body.url };
}

export async function inspectBackup(file: File, signal?: AbortSignal): Promise<BackupInspection> {
  return BackupInspectionSchema.parse(await localServiceUpload("/api/v1/backups/inspect", file, signal));
}

/**
 * Uploads a backup and has the service prove it before anybody is offered the choice to restore it.
 *
 * `firstRun` selects the unauthenticated route, which the service accepts only while the
 * installation is genuinely blank.
 */
export async function prepareRestore(
  file: File,
  options: { firstRun: boolean; signal?: AbortSignal }
): Promise<PreparedRestore> {
  const path = options.firstRun ? "/api/v1/setup/restore/prepare" : "/api/v1/backups/restore/prepare";
  return PreparedRestoreSchema.parse(await localServiceUpload(path, file, options.signal));
}

export async function commitRestore(
  candidateToken: string,
  options: { firstRun: boolean; password?: string }
): Promise<RestoreCommitted> {
  const path = options.firstRun ? "/api/v1/setup/restore/commit" : "/api/v1/backups/restore/commit";
  return RestoreCommittedSchema.parse(
    await localServiceRequest(path, {
      method: "POST",
      body: JSON.stringify(
        options.firstRun ? { candidateToken } : { candidateToken, password: options.password ?? "" }
      ),
      // The commit takes a mandatory safety backup of the existing database before it replaces it.
      signal: AbortSignal.timeout(BACKUP_TIMEOUT_MS)
    })
  );
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} bytes`;
  const units = ["KB", "MB", "GB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 10 ? 0 : 1)} ${units[unit]}`;
}

/** Local wall-clock, because a backup's age is something an operator judges against their own day. */
export function formatMoment(isoUtc: string): string {
  const parsed = new Date(isoUtc);
  if (Number.isNaN(parsed.getTime())) return isoUtc;
  return new Intl.DateTimeFormat("en-IN", { dateStyle: "medium", timeStyle: "short" }).format(parsed);
}
