# ADR-019: Backup and restore implementation

## Status

Accepted. Prerequisite for pilot. **Supersedes the encryption requirement in
[ADR-007](ADR-007-backup-restore.md) for local backups only**, and settles four questions ADR-007
listed as open: free-space checks, atomic file replacement, restore verification, and drills.
ADR-007 is retained unchanged as the record of the original direction.

## Context

Every business fact AUSHADHARTH holds lives in one SQLite file on one PC. There is no server, no
replica, and no synchronisation — those are deliberate consequences of [ADR-001](ADR-001-local-first-topology.md)
and [ADR-002](ADR-002-store-service-owns-sqlite.md), and they mean that a failed disk, a stolen
machine, or a ransomware afternoon ends the business's records unless a copy exists somewhere else.

ADR-007 set the direction in Phase 0 and deferred everything hard about it. Four of those deferrals
have to be answered before a real pharmacy is asked to depend on this, and a fifth question — what a
restore does to identity — was never asked at all.

The open questions, and what actually forced each answer:

1. **The snapshot mechanism.** ADR-007 named SQLite's online backup API. `sqlx` 0.8 does not expose
   it, and `rusqlite` is excluded by ADR-002's single-owner rule.
2. **The container.** A ZIP would be conventional and would mean a new dependency.
3. **Encryption.** ADR-007 required authenticated encryption for portable backups. An encrypted
   backup whose key is lost is not a backup.
4. **Free space.** No dependency in the tree exposes a free-space API.
5. **Replacing the live file.** Windows will not rename over an open file, and there is no
   single call that swaps two files.
6. **Identity after a restore.** A restored database claims to be an installation that may still
   exist on another machine.

## Decision

### 1. `VACUUM INTO` produces the snapshot

`VACUUM INTO 'path'` runs inside a read transaction, so the snapshot is the committed state at its
start and the counter keeps billing throughout. It writes one self-contained file with no `-wal` or
`-shm` sidecar, which is exactly what a backup must be and exactly what copying the live file would
not give. Measured at roughly 31 ms per megabyte. It is plain SQL through the pool the Store Service
already owns, so **no new dependency and no second SQLite handle**.

Copying `database.sqlite3` while WAL mode is active remains what it always was: not a backup.

### 2. `.aushbackup` is a dependency-free container

```
magic "AUSHBAK\x1A"  8 bytes
format_version       u16 little-endian
reserved             u16, must be zero
manifest_len         u32   (≤ 64 KiB)
payload_len          u64   (≤ 2 GiB)
manifest             UTF-8 JSON
payload              the snapshot, byte for byte
```

The header is fixed at 24 bytes and the declared lengths must account for the file exactly: a
truncated file and a file with bytes appended are each refused, with different reasons. The manifest
carries product, format version, backup id, creation time, application version, schema version, the
**full applied-migration chain with checksums**, installation id, store id and display name, the
SHA-256 of the payload, its length, the backup kind, and the source platform. Unknown manifest fields
are tolerated so a later version can add one; missing required fields are refused.

No compression and no archive library. The payload is already a vacuumed SQLite file, the
implementation is a hundred lines of framing, and the alternative was a dependency.

### 3. Local backups are not encrypted, for the pilot

ADR-007 required authenticated encryption. For a **local** backup taken by an owner onto their own
media, encryption without a recovery path converts a recoverable disk failure into an unrecoverable
one, and there is no key-recovery design and no second machine to escrow to. A pharmacy that cannot
open its own backup is worse off than one whose backup was readable by anyone holding the disk.

So: the pilot ships unencrypted local backups, the interface says so in plain words on the screen
where the file is produced, and the owner is told to keep it somewhere only they can reach.

**This supersedes ADR-007 for local backups only.** Cloud or otherwise off-premises backup is
deferred, and ADR-007's encryption requirement stands unchanged for it — a file leaving the owner's
own custody is a different decision with a different threat model, and it must not inherit this one.

### 4. There is no free-space check, and the ordering makes one unnecessary

No dependency in the tree exposes `GetDiskFreeSpaceEx`; `windows-sys` and `libc` are transitive only,
and adding either to ask one question is not a trade this phase makes. A free-space check would also
be advisory rather than authoritative — the answer can be stale by the time it is used.

The operation is therefore **ordered so that the destructive step needs no additional space**:

```
1. write the candidate to disk        (space needed: the backup)
2. write the safety backup to disk    (space needed: the database)
3. rename the live database aside     (no space)
4. rename the candidate into place    (no space)
```

If either write fails the restore is refused before anything has moved. Every write path maps
`StorageFull` and Windows error 112 to `insufficient_disk_space`, which the interface states as a
condition the operator can act on rather than as a fault.

### 5. The swap is two renames, journalled outside the database it replaces

`std::fs::rename` is atomic within a volume, but nothing in the standard library swaps two files and
`ReplaceFileW` would be a new dependency. The window between the two renames is real, so a journal
records what is happening — as a **file**, at `backups/restore.journal`, written with
`write → flush → sync_all → rename → sync directory`.

The journal cannot live in a database table. It has to survive being read when the database it
describes may be half-replaced, and a table inside that file could not be read at that moment.

Five stages, each with one recovery:

| Stage | Meaning | Recovery on next start |
|---|---|---|
| `Prepared` | nothing has moved | discard the candidate |
| `SafetyTaken` | safety backup exists | discard the candidate |
| `OldMoved` | live database renamed aside | rename it back |
| `CandidateInstalled` | candidate is at the live path | verify it; keep it, or roll back |
| `Validated` | verified and recorded | delete the superseded copy |

A journal that cannot be parsed is treated as `OldMoved`, not as absent: unreadable means a restore
**may** have been in flight, and rolling back is the answer that cannot lose data. If the database is
missing and nothing recoverable can be found, the service **refuses to start**. Starting would create
an empty pharmacy where a real one used to be, and the operator would discover it by selling from it.

### 6. A candidate proves itself before the live database is touched

In order, all on a copy: container framing, SHA-256 of the payload, product identity, the applied
migration chain against this build's own embedded migrations, `PRAGMA integrity_check`,
`PRAGMA foreign_key_check`, and — when the backup is older than this build — a **trial forward
migration**, followed by the structural checks again. A backup that cannot be upgraded is discovered
here, as a refusal, rather than after the swap as a disaster.

Product identity is `PRAGMA application_id = 0x41555348` ("AUSH"), fixed for the life of the product.
**`application_id = 0` is accepted**, because every database created before this migration is
legitimately ours and carries no marker; those prove themselves on the migration chain alone, whose
first entry must be the migration this product was born with. Any other non-zero value is somebody
else's SQLite database wearing our file extension.

The chain, rather than a version number, is what distinguishes a backup from a newer build (refuse,
because schemas are not downgraded) from a backup from a fork or a tampered file (refuse, because
the numbers agree and the checksums do not). A version number alone reports those two identically.

### 7. A restored database is the same pharmacy on a new installation

The store identity inside the backup is restored **exactly as it was** — it is the business, and the
business did not change. The installation identity is **re-issued**, because the backup may have come
from a machine that still exists, and two live installations sharing an id would corrupt any future
synchronisation built on [ADR-008](ADR-008-identifiers-and-sync.md).

Lineage is recorded in `restore_provenance`: which backup, from which store and installation, the
digest of the source, the digest of what was replaced, the schema version it arrived at and the one
it was brought to, and who did it. The table is append-only by trigger. `master_change_events` gains
`backup` and `restore_operation` entity types so both appear in business history.

**Every session in the restored database is revoked.** Those sessions were issued against a database
that no longer exists, on a machine the file may have left on a USB stick since.

Backup freshness is deliberately **not** restored. It is stored in a file,
`configuration/backup-state.json`, not in the database, so that restoring a six-month-old backup onto
a new machine cannot also restore "last backup: yesterday" — which would silence the reminder on the
one day it matters most.

### 8. Authority: an owner with a recent password, or a genuinely blank installation

A restore requires `owner_admin` **and** the owner's own password re-entered. A session can be hours
old and left open at a counter; this operation replaces the pharmacy's entire record. There is no
password-reset bypass and no new password mechanism — the existing Argon2 verification is reused.

The first-run route accepts no session at all, because on a new machine there is no account yet. It
is gated on the installation being **blank**, which means more than "no users": no store identity, no
movements, no documents, no stock operations, and no prior restore. A database with history but no
users is not blank, it is damaged, and restoring over it would destroy the evidence.

A prepared candidate belongs to whoever prepared it, expires in thirty minutes, and can be committed
exactly once.

### 9. The browser never names a path

Backups are addressed by id, candidates by opaque token, and nothing a caller sends is ever joined
onto a filesystem path. Downloads are served by the Store Service from its own directory behind the
same owner check; uploads are streamed as `application/octet-stream`.

That content type is not cosmetic. Requiring `application/json` on mutations is part of the CSRF
defence — a cross-site HTML form can only send form encodings. A binary upload cannot be JSON, so it
requires octet-stream, which a form equally cannot produce. **Choosing multipart here would have given
that protection away.** The 2 GiB limit is applied to the backup routes only; the global body limit
is untouched.

### 10. Backups are manual, and the service must restart after a restore

No scheduler and no automatic backup in this phase. A reminder appears when the last backup on this
installation is more than seven days old, and it is honest about never having seen one.

After the swap the Store Service has deliberately closed the file it had open, so every route answers
`service_restoring` until it is restarted. Nothing is wrong and nothing is lost; the interface says
exactly that, because "restart the service" and "your service has failed" are very different
instructions.

## Consequences

- `sqlx::migrate!` embeds migrations at compile time, so the migration chain a build validates
  against is the chain it can actually apply.
- `SqlitePool::close` returns **before** the operating system has released the file handle. Measured
  on Windows: the first rename after a close fails with a sharing violation and succeeds roughly
  thirty milliseconds later. Every move of a database file therefore folds the write-ahead log in
  with `PRAGMA wal_checkpoint(TRUNCATE)` first, and retries the move itself until the handle is free
  or ten seconds have passed. The retry is keyed on the operation, not on a fixed pause: the rename
  is its own proof, and no interval could be assumed correct on somebody else's machine.
- A disaster-recovery drill is a **hard gate** for this phase, not a test that may be skipped.
- Cloud backup, scheduled backup, encrypted portable backup, and attachment payloads inside the
  container remain deferred; the manifest carries an attachments field so adding them needs no new
  format version.

## Non-goals

Encryption of local backups; cloud or network destinations; automatic or scheduled backups;
incremental or differential backups; restoring a single document, party or product from a backup;
downgrading a schema.
