# Migration tests

Every release must test fresh installation, supported upgrades, interrupted
migration recovery, and restart idempotence. The Phase 0 Rust database test runs
the first migration twice against the same temporary database.
