# Windows runtime layout

The Rust `directories` resolver maps the AUSHADHARTH application identity to
Windows local application-data locations. Separate descendants are used for:

- `database` — Store Service-owned SQLite files;
- `logs` — structured operational logs;
- `backups` — future local backup containers;
- `attachments` — future customer-owned binary documents;
- `configuration` — non-business configuration and future protected keys.

The database resolver rejects repository descendants and paths visibly under a
OneDrive directory. Production packaging must define Windows service identity,
ACLs, signed installation/update behavior, and portable recovery before release.
