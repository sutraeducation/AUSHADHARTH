# Logging and privacy policy

The Store Service writes newline-delimited structured JSON to its resolved local
log directory, outside source control. Logging defaults to `info` and is rolled
daily. Production packaging must enforce a retention limit of 14 daily files and
a configurable total-size ceiling; retention cleanup is not implemented in
Phase 0.

Normal logs must never contain patient/customer data, prescriptions, medicine
sale lines, database contents, credentials, encryption material, tokens, or full
filesystem paths. Use opaque request/correlation identifiers and coarse outcome
codes. Diagnostics requiring sensitive data must be explicit, time-bounded, and
customer-authorized in a later design.
