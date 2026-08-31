# Foundation data rules

- SQLite is exclusively owned by the Store Service.
- Durable identities are UUIDv7 strings generated locally.
- Money must never use binary floating point. Each future monetary field must
  define currency, scale, rounding, and serialization.
- Quantities require a documented unit and fixed scale.
- Technical timestamps are UTC; business documents retain local business date
  and IANA time-zone context.
- Future accounting records are append-only and corrections use reversals.
- Retryable commands use idempotency keys.
- Finalizing a future sale must atomically post invoice identity, payment, stock,
  and accounting effects in one explicit transaction.

Phase 0 includes only schema/application metadata and identity table shapes. It
does not create pharmacy business entities.
