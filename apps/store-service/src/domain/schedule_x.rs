//! Phase 1M-D2 — the Schedule X working record.
//!
//! Rule 65(21)(a) of the Drugs and Cosmetics Rules, 1945 requires Schedule X supply to be recorded
//! at the time of supply in a register that is bound, serially page numbered, specially maintained,
//! with separate pages allotted for each drug — and rule 65(21)(b) lists ten particulars covering
//! both what came in from the supplier and what went out to the patient or purchaser.
//!
//! A bound, page-numbered book is not something software can be. So this module builds a WORKING
//! RECORD: the particulars gathered, frozen and printable, so that a person can write the statutory
//! register accurately. It is a compliance aid and a transcription aid. It is not the register, it
//! does not replace the register, and nothing here allocates a page number — rule 65(21) says
//! separate pages per drug and is silent on every other page question, and silence is not a licence
//! to invent a rule.
//!
//! Nothing in this module enables a Schedule X sale. The receipt side is what D2 captures; the
//! supply side has a shape so a later slice has somewhere truthful to land.

use sqlx::{Sqlite, pool::PoolConnection};

use crate::domain::regulatory::{Resolution, resolve_for_product};

/// A posted Purchase's frozen header particulars, exactly as Phase 1M-D1-A wrote them.
///
/// Read from the document, never from the Party: a supplier renamed next year does not change what
/// this receipt says, which is the whole point of freezing it at posting.
#[derive(Debug, Clone, sqlx::FromRow)]
struct PostedPurchaseHeader {
    store_id: String,
    invoice_date: String,
    supplier_invoice_number: String,
    supplier_display_name: Option<String>,
    supplier_address_state: Option<String>,
    supplier_address_line1: Option<String>,
    supplier_drug_licence_state: Option<String>,
    supplier_drug_licence_number: Option<String>,
}

/// One posted line's frozen particulars.
#[derive(Debug, Clone, sqlx::FromRow)]
struct PostedPurchaseLine {
    id: String,
    product_id: String,
    quantity_atoms: i64,
    quantity_packs: i64,
    drug_display_name: Option<String>,
    batch_number: Option<String>,
    manufacturer_state: Option<String>,
    manufacturer_name: Option<String>,
}

/// What preparing the receipt entries for one posted Purchase did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReceiptPreparation {
    /// Working entries written, one per Schedule X line.
    pub prepared: Vec<String>,
    /// Lines already carrying a live entry, so nothing was written again. A replayed posting must
    /// not double-write the register.
    pub already_present: usize,
}

/// Prepares a Schedule X receipt working entry for every line of a freshly posted Purchase whose
/// product is recorded as inside Schedule X on the document's own transaction date.
///
/// Called inside the posting transaction, under the same `BEGIN IMMEDIATE` that freezes the stock
/// and the provenance, so there is no moment in which a Schedule X receipt exists without its
/// working record — and no moment in which a working record exists for a posting that then failed.
///
/// Schedule X status comes only from the owner-recorded, effective-dated finding, resolved through
/// the same `resolve_for_product` the counter uses. A product *named* "Schedule X" gets nothing; a
/// product with no finding at all gets nothing, because `unknown` is not `applies`. An ordinary
/// Purchase of ordinary medicines therefore passes through this function untouched.
pub async fn prepare_receipt_entries(
    connection: &mut PoolConnection<Sqlite>,
    purchase_document_id: &str,
    actor_id: &str,
) -> Result<ReceiptPreparation, sqlx::Error> {
    let header: Option<PostedPurchaseHeader> = sqlx::query_as(
        "SELECT store_id,invoice_date,supplier_invoice_number,supplier_display_name,\
         supplier_address_state,supplier_address_line1,supplier_drug_licence_state,\
         supplier_drug_licence_number FROM purchase_documents WHERE id=? AND status='posted'",
    )
    .bind(purchase_document_id)
    .fetch_optional(&mut **connection)
    .await?;
    let Some(header) = header else {
        // Not posted, or gone. Nothing to record; the caller's own guards own that decision.
        return Ok(ReceiptPreparation::default());
    };

    let lines: Vec<PostedPurchaseLine> = sqlx::query_as(
        "SELECT id,product_id,quantity_atoms,quantity_packs,drug_display_name,batch_number,\
         manufacturer_state,manufacturer_name FROM purchase_lines \
         WHERE purchase_document_id=? ORDER BY line_number",
    )
    .bind(purchase_document_id)
    .fetch_all(&mut **connection)
    .await?;

    let mut outcome = ReceiptPreparation::default();
    for line in lines {
        // The owner's finding, as at the transaction date. Re-resolved here, inside the posting
        // transaction, so a classification recorded a moment ago cannot be missed and one recorded a
        // moment later cannot be applied retroactively to this receipt.
        let resolved =
            resolve_for_product(connection, &line.product_id, &header.invoice_date).await?;
        if resolved.answer("schedule_x") != Resolution::Applies {
            continue;
        }

        // A replayed or retried posting finds the entry already there and leaves it alone. The
        // partial unique index would refuse a second live row anyway; checking first turns a
        // constraint violation into an ordinary, idempotent no-op.
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT id FROM store_schedule_x_register_entries \
             WHERE entry_kind='receipt' AND purchase_line_id=? AND status<>'void'",
        )
        .bind(&line.id)
        .fetch_optional(&mut **connection)
        .await?;
        if existing.is_some() {
            outcome.already_present += 1;
            continue;
        }

        let reference_value: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(reference_value),0)+1 FROM store_schedule_x_register_entries \
             WHERE store_id=?",
        )
        .bind(&header.store_id)
        .fetch_one(&mut **connection)
        .await?;
        let reference = format!("AXR-{reference_value:06}");
        let id = uuid::Uuid::now_v7().to_string();
        let now: String = sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')")
            .fetch_one(&mut **connection)
            .await?;

        // A particular the store never recorded stays absent. `not_recorded` is the truth about the
        // record, and filling it from today's master would be a fabrication dressed as history.
        let batch_state = if line.batch_number.is_some() {
            "recorded"
        } else {
            "not_recorded"
        };
        let manufacturer_state = line.manufacturer_state.as_deref().unwrap_or("not_recorded");
        let manufacturer_name = if manufacturer_state == "recorded" {
            line.manufacturer_name.clone()
        } else {
            None
        };
        let supplier_address_state = header
            .supplier_address_state
            .as_deref()
            .unwrap_or("not_recorded");
        let supplier_address = if supplier_address_state == "recorded" {
            header.supplier_address_line1.clone()
        } else {
            None
        };
        let supplier_licence_state = header
            .supplier_drug_licence_state
            .as_deref()
            .unwrap_or("not_recorded");
        let supplier_licence_number = if supplier_licence_state == "recorded" {
            header.supplier_drug_licence_number.clone()
        } else {
            None
        };

        sqlx::query(
            "INSERT INTO store_schedule_x_register_entries (id,store_id,entry_kind,reference_value,\
             reference,transaction_date,drug_name,product_id,batch_state,batch_number,\
             manufacturer_state,manufacturer_name,quantity_atoms,quantity_packs,bill_number,\
             bill_date,purchase_document_id,purchase_line_id,supplier_name,supplier_address_state,\
             supplier_address,supplier_licence_state,supplier_licence_number,status,\
             prepared_by_user_id,prepared_at_utc,created_at_utc,updated_at_utc) \
             VALUES (?,?,'receipt',?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'prepared',?,?,?,?)",
        )
        .bind(&id)
        .bind(&header.store_id)
        .bind(reference_value)
        .bind(&reference)
        .bind(&header.invoice_date)
        .bind(&line.drug_display_name)
        .bind(&line.product_id)
        .bind(batch_state)
        .bind(&line.batch_number)
        .bind(manufacturer_state)
        .bind(&manufacturer_name)
        .bind(line.quantity_atoms)
        .bind(line.quantity_packs)
        .bind(&header.supplier_invoice_number)
        .bind(&header.invoice_date)
        .bind(purchase_document_id)
        .bind(&line.id)
        .bind(&header.supplier_display_name)
        .bind(supplier_address_state)
        .bind(&supplier_address)
        .bind(supplier_licence_state)
        .bind(&supplier_licence_number)
        .bind(actor_id)
        .bind(&now)
        .bind(&now)
        .bind(&now)
        .execute(&mut **connection)
        .await?;

        // The audit payload names the entry and its source. It carries no patient and no prescriber,
        // because a receipt has neither and a payload is not a place to widen exposure.
        sqlx::query(
            "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,\
             action,occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) \
             VALUES (?,'schedule_x_register_entry',?,1,'created',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),NULL,1,?,?)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(&id)
        .bind(
            serde_json::json!({
                "entryKind": "receipt",
                "reference": reference,
                "purchaseDocumentId": purchase_document_id,
                "purchaseLineId": line.id,
                "productId": line.product_id,
                "transactionDate": header.invoice_date,
                "batchState": batch_state,
                "manufacturerState": manufacturer_state,
                "supplierAddressState": supplier_address_state,
                "supplierLicenceState": supplier_licence_state,
            })
            .to_string(),
        )
        .bind(actor_id)
        .execute(&mut **connection)
        .await?;

        outcome.prepared.push(id);
    }
    Ok(outcome)
}
