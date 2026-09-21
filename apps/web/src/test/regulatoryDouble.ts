/**
 * Phase 1M-A — what the Store Service answers for a product with no recorded finding.
 *
 * Every product page reads its Drugs Rules position now. A test double that is about something
 * else — tax, price control, packs — answers that read with the truth for an untouched product:
 * every scheme unknown, and a gate that follows from the product's kind. It never answers with a
 * finding nobody recorded.
 */
export function unclassifiedRegulatory(productId: string, productKind: string) {
  return {
    productId,
    productKind,
    classifications: [],
    resolved: ["schedule_h", "schedule_h1", "schedule_x", "schedule_c", "schedule_c1", "ndps_purview"]
      .map((scheme) => ({ scheme, answer: "unknown" })),
    resolvedOn: "2026-09-21",
    saleGate: productKind === "medicine" ? "unresolved" : "clear",
    saleGateScheme: null,
    alcoholPercentVvHundredths: null,
    attributesRevision: null
  };
}
