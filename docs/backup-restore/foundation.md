# Backup and restore foundation

The planned `.aushbackup` format is a versioned container with a consistent
SQLite online-backup snapshot, manifest, application/schema versions, attachment
inventory, and checksums. Portable archives will use authenticated encryption;
the key-recovery and passphrase policy must be frozen before implementation.

Copying a live SQLite file, uploading its working directory to OneDrive, or
copying only the main file while WAL mode is active is not a supported backup.
No backup or restore operation is implemented in Phase 0.
