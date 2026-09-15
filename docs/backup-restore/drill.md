# Disaster-recovery drill

A backup nobody has ever restored is a belief, not a backup. This is the drill that turns it into a
fact, and it is a **hard gate**: the phase does not freeze if it does not pass.

It is not a set of unit tests. It is the sequence a pharmacy would actually live through — trade,
back up, lose the machine, restore onto a new one, and then survive being interrupted at each of the
moments where an interruption is dangerous.

```bash
cargo test --manifest-path apps/store-service/Cargo.toml \
  --test disaster_recovery_drill -- --nocapture --test-threads=1
```

Nothing in it touches an installed AUSHADHARTH. Every path is inside a per-run temporary directory,
and step 1 asserts that before anything else happens.

## What it covers

| Steps | What is being proved |
|---|---|
| 1–2 | The drill cannot reach a real pharmacy's data, and its own path satisfies the frozen policy |
| 3–12 | A pharmacy trades, backs up while open, downloads the file byte-identically, and keeps it off the machine |
| 13–15 | The machine is destroyed; the backup on the external drive survives it |
| 16–20 | A replacement PC refuses rubbish and a tampered backup, leaving nothing behind |
| 21–26 | The genuine backup is proved, restored on a blank installation, and the pharmacy comes back — without the fact recorded after the backup |
| 27–33 | The owner signs in, inherited sessions are dead, the store identity is kept, the installation identity is re-issued, lineage is recorded, and the restored database is traded on |
| 34–44 | An established installation restores over itself: the first-run door is shut, a wrong password is refused, a safety copy is taken and is itself readable |
| 45–49 | Interruptions before the swap, between the two renames, with an unreadable journal, and with nothing recoverable at all |
| 50 | The pharmacy is still all there |

## Last run

```text
running 1 test
test the_disaster_recovery_drill ... [ 1] Confirm the drill runs entirely inside a disposable directory
     working inside C:\Users\varun\AppData\Local\Temp\.tmpACBV2v
[ 2] Confirm the frozen path policy accepts the drill's database on its own terms
[ 3] Start a first installation the way the application starts it
[ 4] Complete first-run setup and obtain a real session
[ 5] Record business facts that must survive everything that follows
     recorded ["Aditya Distributors", "Bharat Medical Agency", "Chandra Pharma"]
[ 6] Confirm the installation reports itself as overdue for a backup
[ 7] Take a backup while the service is running and the pharmacy is open
     produced AUSHADHARTH-drill-pharmacy-20260915-180821-3e463acb.aushbackup
[ 8] Confirm the reminder now reports the installation as protected
[ 9] Download the backup through the service, as an owner would to an external drive
     990228 bytes retrieved over HTTP
[10] Confirm the retrieved bytes are identical to the file on disk
[11] Keep the backup somewhere the failing machine cannot take with it
[12] Record one more fact AFTER the backup, which a restore must correctly lose
[13] Shut the installation down
[14] Destroy the machine: the database and everything beside it are gone
[15] Confirm the backup on the external drive survived the machine
[16] Start a brand-new installation, as a replacement PC would
[17] Confirm the replacement is blank and asks for first-run setup
[18] Refuse a file that is not a backup, before any of it is trusted
     refused as "backup_product_mismatch"
[19] Refuse a backup whose contents were altered after it was made
[20] Confirm neither refusal left anything behind or changed the blank installation
[21] Upload the genuine backup from the external drive and have it proved
     the file says: "Drill Pharmacy" taken "2026-09-15T18:08:21.550Z"
[22] Commit the restore on the blank installation
[23] Confirm the service refuses to carry on serving the database it replaced
[24] Restart the replacement installation
[25] Confirm every fact recorded before the backup is present
     recovered ["Aditya Distributors", "Bharat Medical Agency", "Chandra Pharma"]
[26] Confirm the fact recorded after the backup is correctly absent
[27] Confirm the owner can sign in with the password they had before the machine failed
[28] Confirm every session issued before the restore is dead
[29] Confirm the pharmacy's identity was kept and the installation's was re-issued
     lineage recorded: 01a0a641-6b56-7db0-ae66-de6da6170523 → 01a0a641-7657-79c0-92ca-5fa4b265ef31
[30] Confirm the restore appears in business history
[31] Confirm the restored installation is told to take a backup of its own
[32] Confirm the restored database is structurally sound
[33] Trade on the restored installation, proving it is a working pharmacy and not an archive
[34] Take a fresh backup of the now-established installation
[35] Record a fact after that backup, to be deliberately discarded
[36] Refuse the unauthenticated first-run route now that the installation is in use
[37] Prepare the restore as the signed-in owner
[38] Refuse the commit when the password is wrong
[39] Confirm the refusal changed nothing at all
[40] Commit the restore with the owner's own password
[41] Confirm a safety copy of the replaced data was taken and can be read back
     safety copy AUSHADHARTH-drill-pharmacy-20260915-180826-6cdb666a.aushbackup is a readable backup of the replaced data
[42] Restart after the restore
[43] Confirm the deliberately discarded fact is gone and the rest remains
[44] Confirm the lineage now records both restores
[45] Interrupt before anything has moved, and confirm nothing was touched
[46] Interrupt between the two renames, and confirm the original comes back
[47] Interrupt with an unreadable journal, and confirm the service still recovers
[48] Confirm a missing database with nothing recoverable makes the service refuse to start
     refused to start: the database is missing and no recoverable copy was found; refusing to start
[49] Recover the set-aside database and confirm the pharmacy is still all there
[50] Drill complete: the pharmacy survived a lost machine and four interruptions
     final state ["Aditya Distributors", "Bharat Medical Agency", "Chandra Pharma", "Eknath Surgicals"]
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.39s
```

## Scale

The drill runs on a small database, because what it proves is sequence rather than throughput. The
sizes a pilot pharmacy will actually reach are measured separately:

```bash
cargo test --manifest-path apps/store-service/Cargo.toml --release \
  --test backup_scale -- --ignored --nocapture --test-threads=1
```

Measured on the development machine, release build, each figure the whole operation end to end:

| Database | Container | Snapshot | Package | Read framing | Unpack and verify |
|---|---|---|---|---|---|
| 101.7 MB | 101.7 MB | 1.24 s | 1.28 s | 16 ms | 0.98 s |
| 504.9 MB | 504.9 MB | 4.27 s | 8.48 s | 241 ms | 9.62 s |
| 1033.0 MB | 1033.0 MB | 20.31 s | 27.08 s | 17 ms | 33.47 s |

Reading the framing is effectively free at any size because it reads a fixed 24-byte header and a
manifest of at most 64 KiB — which is the point: an owner learns what a file is, and whether it is
theirs, without waiting for a gigabyte to be read.

Memory does not scale with the database. Writing, hashing, reading and unpacking all move in 64 KiB
chunks, so a one-gigabyte backup costs the same working set as a one-megabyte one. That is the
property that matters on a pharmacy counter PC; the seconds are not.
