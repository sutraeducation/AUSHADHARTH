# Backup and restore

Implemented. The design and the reasoning behind each choice are in
[ADR-019](../adr/ADR-019-backup-and-restore-implementation.md), which supersedes
[ADR-007](../adr/ADR-007-backup-restore.md)'s encryption requirement **for local backups only**.

## What a backup is

A single `.aushbackup` file containing a consistent snapshot of the database, taken with
`VACUUM INTO` while the pharmacy keeps trading, plus a manifest recording what it is: product,
format version, creation time, application and schema versions, the full applied-migration chain
with checksums, store and installation identity, and the SHA-256 of the snapshot.

It is **not encrypted**. An owner is told so on the screen that produces it, and told to keep it
somewhere only they can reach.

## What is still not a backup

Copying `database.sqlite3` while the service is running. Uploading the data directory to OneDrive.
Copying the main file while WAL mode is active and leaving the `-wal` behind. Each of these produces
a file that looks like a database and is missing committed transactions.

## Taking one

**Data Safety** under CONFIGURATION, owner only. The backup is written into the installation's own
`backups` directory and can then be downloaded through the browser to wherever the owner keeps it.
Backups are **manual**; a reminder appears when the last backup taken on this installation is more
than seven days old.

Backup freshness is a fact about the installation, not about the business, so it is stored outside
the database and is never restored from one.

## Restoring one

Restoring replaces everything. The sequence is deliberate:

1. The file is uploaded and **proved on a copy** — framing, checksum, product identity, migration
   chain, `integrity_check`, `foreign_key_check`, and a trial forward migration if it is older than
   this build. The live database is not touched by any of this.
2. The owner is shown what the file actually turned out to be, and re-enters their own password.
3. A **safety backup of the current database is taken**, always.
4. The live database is renamed aside and the candidate renamed into place, with an external journal
   written before each move.
5. The service closes the file it had open and answers `service_restoring` until it is restarted.
6. On restart the installation identity is re-issued, the lineage is recorded in
   `restore_provenance`, and every session in the restored database is revoked.

The store identity is restored exactly as the backup had it: it is the same pharmacy.

## After an interruption

The restore journal at `backups/restore.journal` is read before the database is opened, because only
it can explain what the authoritative path holds. Each stage has exactly one recovery, an unreadable
journal is treated as an interrupted swap and rolled back, and a missing database with nothing
recoverable makes the service **refuse to start** rather than create an empty pharmacy.

## First run

A new installation with nothing in it — no users, no store, no movements, no documents — may restore
without signing in, because there is no account yet and nothing to lose. The moment anything exists
that door is shut and the authenticated route with its password confirmation is the only way in.

## Drill

Restoring is verified by a disaster-recovery drill, not only by tests. The drill and its result are
recorded in [drill.md](drill.md).

## Deferred

Encryption of local backups, cloud and network destinations, scheduled and automatic backups,
incremental backups, restoring individual records, and attachment payloads inside the container. The
manifest already carries an attachments field, so adding them will not need a new format version.
