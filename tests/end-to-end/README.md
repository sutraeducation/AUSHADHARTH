# End-to-end tests

The Phase 0 Playwright test loads the production-built shell, waits for its
service worker, reloads under Chromium offline emulation, and verifies both the
brand shell and local-service-offline state. It tests static shell availability
only and does not store or simulate business transactions in the service worker.
