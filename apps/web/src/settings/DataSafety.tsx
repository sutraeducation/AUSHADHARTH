import { useEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { BackupInspection, BackupSummary, PreparedRestore } from "@aushadharth/contracts";
import { useAuth } from "../auth/AuthContext";
import { LocalServiceError } from "../platform/localService";
import {
  commitRestore,
  createBackup,
  fetchBackupStatus,
  formatBytes,
  formatMoment,
  prepareRestore,
  resolveDownload
} from "./backupApi";

/**
 * Data Safety.
 *
 * The pharmacy's entire record is one file on this PC, and this screen is where an operator makes a
 * copy of it and, on the worst day, puts one back. Two things shape how it reads. Taking a backup is
 * ordinary and is presented as such. Restoring one is not: it replaces everything, so it is kept
 * behind a deliberate sequence — choose the file, read what the file actually is, confirm with a
 * password, and only then see it happen.
 */
export function DataSafetyPage() {
  usePageTitle("Data Safety");
  const auth = useAuth();
  const queryClient = useQueryClient();
  const owner = auth.status?.user?.role === "owner_admin";
  const status = useQuery({ queryKey: ["backups", "status"], queryFn: fetchBackupStatus, retry: false, enabled: owner });
  useExpireOnAuthError(status.error);

  const [notice, setNotice] = useState("");
  const [failure, setFailure] = useState("");
  const [restoring, setRestoring] = useState(false);

  const backup = useMutation({
    mutationFn: createBackup,
    onMutate: () => { setNotice(""); setFailure(""); },
    onSuccess: async (job) => {
      setNotice(`Backup created: ${job.filename ?? "ready"}${job.bytes ? ` (${formatBytes(job.bytes)})` : ""}. Save a copy somewhere other than this PC.`);
      await queryClient.invalidateQueries({ queryKey: ["backups", "status"] });
    },
    onError: (error) => setFailure(error instanceof LocalServiceError ? error.message : "The backup could not be created.")
  });

  if (!owner) {
    return <><header className="page-header"><div><p className="eyebrow">CONFIGURATION</p><h1>Data Safety</h1><p>Backups protect the whole pharmacy record, so only the workspace owner can take or restore one.</p></div><span className="read-only-note">Owner access required</span></header></>;
  }

  return <>
    <header className="page-header">
      <div><p className="eyebrow">CONFIGURATION</p><h1>Data Safety</h1><p>Everything your pharmacy has recorded lives in one file on this PC. A backup is a copy of that file you can keep somewhere else.</p></div>
      <button className="button button--primary" type="button" onClick={() => backup.mutate()} disabled={backup.isPending}>{backup.isPending ? "Creating backup…" : "Create Backup Now"}</button>
    </header>

    {notice && <div className="catalog-inline-notice" role="status">{notice}</div>}
    {failure && <div className="catalog-inline-error" role="alert"><span>{failure}</span></div>}

    <section className="master-panel" aria-labelledby="backup-state-title">
      <h2 id="backup-state-title">Backup status</h2>
      {status.isPending ? <div className="table-loading" role="status" aria-live="polite"><span /><span /><span /><b>Loading backup status…</b></div>
        : status.isError ? <div className="empty-state" role="alert"><h3>Backup status could not be loaded</h3><p>The Local Store Service did not complete this request. This is not the same as having no backups.</p><button className="button button--secondary" type="button" onClick={() => void status.refetch()}>Retry</button></div>
        : <>
          {status.data.backupOverdue && <div className="catalog-inline-error" role="alert"><span>{status.data.lastSuccessfulBackupAtUtc ? `The last backup on this PC was ${formatMoment(status.data.lastSuccessfulBackupAtUtc)}. Take one now.` : "This installation has never been backed up. Take a backup now."}</span></div>}
          <dl className="detail-grid">
            <div><dt>Last backup on this PC</dt><dd>{status.data.lastSuccessfulBackupAtUtc ? formatMoment(status.data.lastSuccessfulBackupAtUtc) : "Never"}</dd></div>
            <div><dt>Reminder after</dt><dd>{status.data.reminderThresholdDays} days</dd></div>
            <div><dt>Backups kept here</dt><dd>{status.data.backups.length}</dd></div>
          </dl>
          <p className="privacy-note">A backup file is not encrypted. Keep it somewhere only you can reach — an external drive kept with the cash, not a shared folder.</p>
        </>}
    </section>

    <section className="master-panel" aria-labelledby="backup-list-title">
      <h2 id="backup-list-title">Backups on this PC</h2>
      {status.data && status.data.backups.length === 0 && <div className="empty-state"><h3>No backups yet</h3><p>Create one now, then copy it off this PC. A backup that never leaves the machine does not survive the machine.</p></div>}
      {status.data && status.data.backups.length > 0 && <div className="table-scroll"><table className="data-table"><thead><tr><th scope="col">Taken</th><th scope="col">Kind</th><th scope="col">Size</th><th scope="col">File</th><th scope="col"><span className="sr-only">Download</span></th></tr></thead><tbody>{status.data.backups.map((row) => <BackupRow key={row.backupId} row={row} onFailure={setFailure} />)}</tbody></table></div>}
    </section>

    <section className="master-panel master-panel--danger" aria-labelledby="restore-title">
      <h2 id="restore-title">Restore from a backup</h2>
      <p>Restoring replaces everything recorded on this PC with the contents of the backup file. Anything entered since that backup was taken will be gone. AUSHADHARTH takes a safety copy of the current data first, and will not touch it until the backup file has been checked.</p>
      {restoring ? <RestoreFlow onClose={() => setRestoring(false)} /> : <button className="button button--secondary" type="button" onClick={() => setRestoring(true)}>Restore from a backup…</button>}
    </section>
  </>;
}

function BackupRow({ row, onFailure }: { row: BackupSummary; onFailure: (message: string) => void }) {
  const [working, setWorking] = useState(false);
  const download = async () => {
    setWorking(true);
    try {
      const resolved = await resolveDownload(row.backupId);
      // The service names the file and serves it; the browser only follows where it is told.
      const anchor = document.createElement("a");
      anchor.href = resolved.url;
      anchor.download = resolved.filename;
      document.body.append(anchor);
      anchor.click();
      anchor.remove();
    } catch (error) {
      onFailure(error instanceof LocalServiceError ? error.message : "That backup could not be downloaded.");
    } finally {
      setWorking(false);
    }
  };
  return <tr>
    <td>{formatMoment(row.createdAtUtc)}</td>
    <td>{row.backupKind === "pre_restore_safety" ? "Safety copy" : "Manual"}</td>
    <td className="numeric">{formatBytes(row.bytes)}</td>
    <td><code>{row.filename}</code></td>
    <td><button className="button button--secondary" type="button" onClick={() => void download()} disabled={working}>{working ? "Preparing…" : "Download"}</button></td>
  </tr>;
}

/**
 * The restore sequence, used on this screen and on the first-run screen alike.
 *
 * `firstRun` is the only difference between them: with no account yet there is no password to
 * confirm, and the service decides whether that is allowed by looking at whether the installation is
 * genuinely blank — not by trusting anything the browser says.
 */
export function RestoreFlow({ firstRun = false, onClose }: { firstRun?: boolean; onClose?: () => void }) {
  const [file, setFile] = useState<File | null>(null);
  const [prepared, setPrepared] = useState<PreparedRestore | null>(null);
  const [password, setPassword] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);

  const choose = async (chosen: File) => {
    setError(""); setPrepared(null); setFile(chosen); setBusy(true);
    try {
      setPrepared(await prepareRestore(chosen, { firstRun }));
    } catch (caught) {
      setFile(null);
      setError(caught instanceof LocalServiceError ? caught.message : "That file could not be read as an AUSHADHARTH backup.");
    } finally {
      setBusy(false);
    }
  };

  const confirm = async () => {
    if (!prepared) return;
    setError(""); setBusy(true);
    try {
      const committed = await commitRestore(prepared.candidateToken, { firstRun, password });
      setPassword("");
      setDone(committed.safetyBackup ?? "");
    } catch (caught) {
      setError(caught instanceof LocalServiceError ? caught.message : "The restore could not be completed. Your existing data has been kept.");
    } finally {
      setBusy(false);
    }
  };

  if (done !== null) {
    return <div className="restore-panel" role="status" aria-live="assertive">
      <h3>Restore complete</h3>
      <p>The backup is now this installation's data. Close AUSHADHARTH and start the Local Store Service again to begin using it.</p>
      {done && <p className="privacy-note">A safety copy of the previous data was kept as <code>{done}</code>.</p>}
      <p className="privacy-note">Everyone who was signed in has been signed out, including on other devices that used this workspace.</p>
    </div>;
  }

  return <div className="restore-panel">
    <input ref={input} type="file" accept=".aushbackup" className="sr-only" onChange={(event) => { const chosen = event.target.files?.[0]; if (chosen) void choose(chosen); event.target.value = ""; }} />
    {!prepared && <>
      <button className="button button--secondary" type="button" onClick={() => input.current?.click()} disabled={busy}>{busy ? "Checking the file…" : "Choose backup file…"}</button>
      {onClose && <button className="button button--ghost" type="button" onClick={onClose} disabled={busy}>Cancel</button>}
    </>}
    {error && <div className="catalog-inline-error" role="alert"><span>{error}</span></div>}
    {prepared && <BackupFacts report={prepared.report} filename={file?.name ?? ""} />}
    {prepared && <div className="restore-confirm">
      <p className="restore-warning" role="alert"><strong>This replaces everything.</strong> All records entered on this PC since {formatMoment(prepared.report.createdAtUtc)} will be gone.</p>
      {!firstRun && <div className="field"><label htmlFor="restore-password">Confirm your password</label><input id="restore-password" type="password" value={password} onChange={(event) => setPassword(event.target.value)} autoComplete="current-password" /><small>Your own password, to confirm this is you and not a counter left unattended.</small></div>}
      <div className="restore-actions">
        <button className="button button--danger" type="button" onClick={() => void confirm()} disabled={busy || (!firstRun && password.length === 0)}>{busy ? "Restoring…" : "Replace all data with this backup"}</button>
        <button className="button button--ghost" type="button" onClick={() => { setPrepared(null); setFile(null); setPassword(""); setError(""); }} disabled={busy}>Choose a different file</button>
      </div>
    </div>}
  </div>;
}

/** What the file turned out to be, read from the file itself rather than from its name. */
function BackupFacts({ report, filename }: { report: BackupInspection; filename: string }) {
  return <dl className="detail-grid">
    <div><dt>File</dt><dd><code>{filename}</code></dd></div>
    <div><dt>Pharmacy</dt><dd>{report.storeDisplayName || "Not recorded"}</dd></div>
    <div><dt>Taken</dt><dd>{formatMoment(report.createdAtUtc)}</dd></div>
    <div><dt>Size</dt><dd>{formatBytes(report.bytes)}</dd></div>
    <div><dt>Contents verified</dt><dd>{report.checksumVerified ? "Yes — the file matches its own record" : "No"}</dd></div>
    <div><dt>Made by</dt><dd>AUSHADHARTH {report.applicationVersion}</dd></div>
    <div><dt>Upgrade needed</dt><dd>{report.migrationRequired ? `Yes — brought forward from version ${report.schemaVersion} to ${report.currentSchemaVersion}` : "No"}</dd></div>
  </dl>;
}

function usePageTitle(title: string) { useEffect(() => { const previous = document.title; document.title = `${title} | AUSHADHARTH`; return () => { document.title = previous; }; }, [title]); }
function useExpireOnAuthError(error: Error | null) { const auth = useAuth(); useEffect(() => { if (error instanceof LocalServiceError && ["authentication_required", "session_expired"].includes(error.code)) void auth.expireSession(); }, [error]); }
