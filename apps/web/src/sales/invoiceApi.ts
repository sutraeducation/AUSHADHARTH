import { InvoiceSchema, type Invoice } from "@aushadharth/contracts";
import { localServiceRequest } from "../platform/localService";

/**
 * The canonical invoice, read from the Store Service.
 *
 * There is one call and no cache of assembled documents: a printed document must come from the
 * posted facts the service holds, not from whatever the browser happened to keep from the counter.
 * The schema parse is the boundary — a response missing a statutory field fails here rather than
 * producing a page with a blank where a GSTIN should be.
 */
export async function getInvoice(saleId: string): Promise<Invoice> {
  return InvoiceSchema.parse(await localServiceRequest(`/api/v1/sales/${saleId}/invoice`));
}
