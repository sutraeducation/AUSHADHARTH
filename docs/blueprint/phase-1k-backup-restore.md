# Phase 1K — Backup, Restore and Disaster Recovery

**Status: implementation authority.** Written against the repository at `371682b` (Phase 1J stock
operations). Every claim below was checked against the repository, not against earlier blueprints.

The design and the reasoning behind §§4–9 are recorded once in **ADR-019**, which supersedes
ADR-007's encryption requirement for local backups only, and are not restated here.

---

## 1. Objective

Make a pharmacy's records survive the loss of the PC they live on. Everything AUSHADHARTH holds is
one SQLite file on one machine: no server, no replica, no synchronisation. Until this phase there
was no supported way to copy that file out, and no way at all to put one back.

## 2. Scope

A `.aushbackup` container produced with `VACUUM INTO` while the pharmacy keeps trading; a Data
Safety screen for the owner to take one, download it, and restore one; a restore that proves the
candidate before touching the live database, takes a mandatory safety copy, and replaces the file
through a journalled two-rename swap; startup recovery for an interrupted restore; first-run restore
on a blank installation; and a disaster-recovery drill that is a hard gate rather than a test.

## 3. What this phase deliberately does not do

No encryption of local backups, no cloud or network destination, no scheduled or automatic backup,
no incremental backup, no restoring a single document, and no schema downgrade. ADR-019 records why
each is deferred rather than forgotten; the manifest already carries an attachments field so adding
payloads later will not need a new format version.

## 4. The container

Fixed 24-byte header — magic, format version, reserved, manifest length, payload length, all
big-endian — then a UTF-8 JSON manifest of at most 64 KiB, then the snapshot itself. The declared
lengths must account for the file exactly, so a truncated file and a file with bytes appended are
each refused with their own reason. No compression and no archive dependency.

## 5. Proving a candidate before anything moves

In order, all on a copy: framing, SHA-256, product identity, the applied migration chain against
this build's own embedded migrations, `integrity_check`, `foreign_key_check`, and — for an older
backup — a trial forward migration followed by the structural checks again.

`PRAGMA application_id = 0x41555348` identifies the product. **`0` is accepted**, because databases
older than migration 0016 are legitimately ours and carry no marker; those prove themselves on the
chain, whose first entry must be migration 0001. The chain rather than a version number is what
separates *newer build* (refuse) from *fork or tampered file* (refuse) — a version number alone
reports both identically.

## 6. The swap

```
1. write the candidate          space: the backup
2. write the safety backup      space: the database
3. rename the live file aside   no space
4. rename the candidate in      no space
```

Both large writes happen before either rename, so the destructive step needs no additional disk.
That ordering is the answer to the free-space question, because no dependency in the tree exposes a
free-space API (§11).

A file journal at `backups/restore.journal` — not a table, which could not be read while the
database it describes is half-replaced — records the stage before each move. Five stages, one
recovery each; an unreadable journal is treated as an interrupted swap and rolled back; a missing
database with nothing recoverable makes the service refuse to start.

## 7. Identity after a restore

The store identity is restored exactly as the backup had it: it is the same business. The
installation identity is re-issued, because the machine the backup came from may still exist.
Lineage goes to `restore_provenance` (append-only by trigger) and to `master_change_events` as a
`restore_operation`. **Every session in the restored database is revoked.**

Backup freshness lives in `configuration/backup-state.json`, outside the database, so restoring a
six-month-old backup cannot also restore "last backup: yesterday".

## 8. Authority

`owner_admin` **and** the owner's own password re-entered — a session can be hours old and left open
at a counter. No password-reset bypass; the existing Argon2 verification is reused.

The first-run route takes no session, and is gated on the installation being blank: no users, no
store identity, no movements, no documents, no stock operations, and no prior restore. A database
with history but no users is damaged, not blank.

## 9. The browser's part

Backups are addressed by id, candidates by opaque token, and nothing a caller sends is ever joined
onto a path. Uploads are `application/octet-stream`, which is not cosmetic: requiring JSON on
mutations is the CSRF defence, and octet-stream is equally impossible for a cross-site form to send.
The 2 GiB limit applies to the backup routes only.

After a restore the service answers `service_restoring` on every route until it is restarted, and
the web application shows that as a restart instruction rather than as a failure.

## 10. Migration 0016

Stamps `application_id`, adds `restore_provenance`, and rebuilds `master_change_events` to admit
`backup` and `restore_operation`. The rebuild reproduces the frozen DDL verbatim — every column,
index and trigger — and `tests/migration_0016.rs` builds a populated database at 0015 from the files
on disk, applies 0016, and asserts that no object, column or row was lost.

## 11. Dependencies: none added

| Wanted | Conventional answer | What was done instead |
|---|---|---|
| Consistent snapshot | SQLite online backup API | `VACUUM INTO` (sqlx does not expose the API) |
| Archive container | a ZIP crate | 24-byte header plus JSON manifest |
| Streaming request body | `futures-core` | `axum::body::HttpBody` is re-exported |
| Streaming download | a hand-rolled stream | `tower-http`'s `ServeDir`, already a dependency |
| Free space | `windows-sys` / `libc` | ordering that needs no additional space (§6) |

## 12. Verification

| Evidence | Where |
|---|---|
| Container framing, adversarial inputs | `domain::backup` unit tests |
| Journal stages and recovery mapping | `platform::restore_journal` unit tests |
| Routes, authority, refusals, interruption | `api::backups` tests |
| Real HTTP, real sockets, real files | `tests/real_service_gate.rs` |
| Migration against a populated database | `tests/migration_0016.rs` |
| 100 MB / 500 MB / 1 GB | `tests/backup_scale.rs` (`--ignored`) |
| **Lost machine, restored onto a new one** | `tests/disaster_recovery_drill.rs` |
| Screen behaviour | `DataSafety.test.tsx`, `data-safety.spec.ts` |
