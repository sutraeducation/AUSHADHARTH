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

/// The calendar day(s) the current instant can be in the store's own time.
///
/// Phase 1M-B established this policy on the Sale path and Phase 1M-D3-A needs the same answer for
/// the Schedule X compliance facts, so it lives here and both callers share it rather than each
/// keeping its own idea of what day it is.
///
/// Every store this software creates records `Asia/Kolkata`, which observes no daylight saving and
/// sits at a fixed UTC+05:30, so its day is exact. Any other recorded zone cannot be resolved here
/// without a time-zone database, so the answer widens to every day the instant could fall on
/// anywhere — yesterday, today and tomorrow in UTC — and the caller takes the strictest of them.
/// That can refuse something up to a day early around a boundary; it can never let one through late.
pub async fn store_days(
    connection: &mut sqlx::SqliteConnection,
) -> Result<Vec<String>, sqlx::Error> {
    let zone: Option<String> =
        sqlx::query_scalar("SELECT business_time_zone FROM store_identity LIMIT 1")
            .fetch_optional(&mut *connection)
            .await?;
    if zone.as_deref() == Some("Asia/Kolkata") {
        let today: String =
            sqlx::query_scalar("SELECT strftime('%Y-%m-%d','now','+5 hours','+30 minutes')")
                .fetch_one(&mut *connection)
                .await?;
        return Ok(vec![today]);
    }
    let window: (String, String, String) = sqlx::query_as(
        "SELECT strftime('%Y-%m-%d','now','-1 day'),strftime('%Y-%m-%d','now'),\
         strftime('%Y-%m-%d','now','+1 day')",
    )
    .fetch_one(&mut *connection)
    .await?;
    Ok(vec![window.0, window.1, window.2])
}

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

// ---------------------------------------------------------------------------------------------
// Phase 1M-D3-B — lot provenance
// ---------------------------------------------------------------------------------------------

/// Why a lot cannot support a Schedule X supply.
///
/// Named reasons, not a boolean, because "you may not sell this" is a thing an operator has to act
/// on, and "some stock in this lot did not come from a recorded purchase" tells them what to look
/// at. No reason carries a patient, a prescriber or a price.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "reason")]
pub enum LotProvenance {
    /// Every sellable unit in this lot arrived on a posted purchase whose Schedule X receipt entry
    /// has been written into the physical register and authenticated.
    Qualified,
    /// Nothing qualifying ever brought this lot in. Legacy stock, or stock whose receipt entry is
    /// still only prepared, or was withdrawn.
    NoQualifyingReceipt,
    /// Something other than a qualifying purchase added sellable stock to this lot — opening stock,
    /// an adjustment, a counted surplus, or goods released back from quarantine after a return.
    /// Stock inside a lot is fungible, so this disqualifies the whole lot, not part of it.
    UnresolvedInwardMovement,
}

impl LotProvenance {
    pub fn is_qualified(&self) -> bool {
        matches!(self, Self::Qualified)
    }
}

/// The one qualifying-purchase test, written once.
///
/// This is the exact predicate the 0028 trigger enforces. It lives here as a string so the service
/// and the database cannot drift into two different ideas of what a qualifying receipt is: if this
/// changes, the trigger has to change with it, and the migration test compares them.
const QUALIFYING_RECEIPT_SQL: &str = "inward.movement_type = 'purchase' AND EXISTS (\
     SELECT 1 FROM store_schedule_x_register_entries receipt \
     JOIN purchase_documents source ON source.id = receipt.purchase_document_id \
     WHERE receipt.entry_kind = 'receipt' \
       AND receipt.purchase_line_id = inward.purchase_line_id \
       AND receipt.store_id = ?1 \
       AND receipt.status IN ('confirmed', 'finalized') \
       AND receipt.particulars_entered_in_physical_register = 1 \
       AND receipt.physical_entry_authenticated = 1 \
       AND source.store_id = ?1 \
       AND source.status = 'posted')";

/// Whether a specific inventory lot may support a Schedule X supply.
///
/// THE INVARIANT: every movement that ADDS to the SELLABLE balance of this lot, in this store, must
/// be a posted purchase whose Schedule X receipt working entry has been written into the physical
/// register and authenticated.
///
/// It is written as "nothing disqualifying entered", not "something qualifying entered", because
/// stock inside one lot is fungible. A lot holding fifty qualifying units and fifty units of
/// opening stock cannot say which fifty are being handed over, so the whole lot fails. One good
/// delivery never launders a bad one.
///
/// Quantity is deliberately NOT re-derived here. A purchase return lowers the lot's sellable
/// balance without being an inward movement, and the Phase 1H posting path already refuses a sale
/// line that exceeds the sellable balance of its lot. A second opinion about the arithmetic would
/// eventually disagree with the first.
pub async fn resolve_lot_provenance(
    connection: &mut sqlx::SqliteConnection,
    store_id: &str,
    batch_id: &str,
) -> Result<LotProvenance, sqlx::Error> {
    // Stock that entered from outside the purchase path entirely: opening stock, a correction, a
    // counted surplus, or goods released back from quarantine. Reported first because it is the
    // harder problem — there is no receipt to go and record.
    let foreign: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM inventory_movements inward \
         WHERE inward.store_id = ?1 AND inward.batch_id = ?2 \
           AND inward.stock_status = 'sellable' AND inward.quantity_delta_atoms > 0 \
           AND inward.movement_type <> 'purchase' LIMIT 1",
    )
    .bind(store_id)
    .bind(batch_id)
    .fetch_optional(&mut *connection)
    .await?;
    if foreign.is_some() {
        return Ok(LotProvenance::UnresolvedInwardMovement);
    }

    // Everything came in on a purchase, but at least one of those purchases has no Schedule X
    // receipt entry written into the physical register. That is a receipt somebody can still go and
    // record, so it is worth saying so rather than lumping it in above.
    let unrecorded: Option<i64> = sqlx::query_scalar(&format!(
        "SELECT 1 FROM inventory_movements inward \
         WHERE inward.store_id = ?1 AND inward.batch_id = ?2 \
           AND inward.stock_status = 'sellable' AND inward.quantity_delta_atoms > 0 \
           AND NOT ({QUALIFYING_RECEIPT_SQL}) LIMIT 1"
    ))
    .bind(store_id)
    .bind(batch_id)
    .fetch_optional(&mut *connection)
    .await?;
    if unrecorded.is_some() {
        return Ok(LotProvenance::NoQualifyingReceipt);
    }

    // And at least one qualifying purchase actually brought it in, so a lot with no inward history
    // cannot pass by having nothing to disqualify it.
    let qualifying: Option<i64> = sqlx::query_scalar(&format!(
        "SELECT 1 FROM inventory_movements inward \
         WHERE inward.store_id = ?1 AND inward.batch_id = ?2 \
           AND inward.stock_status = 'sellable' AND inward.quantity_delta_atoms > 0 \
           AND ({QUALIFYING_RECEIPT_SQL}) LIMIT 1"
    ))
    .bind(store_id)
    .bind(batch_id)
    .fetch_optional(&mut *connection)
    .await?;
    Ok(match qualifying {
        Some(_) => LotProvenance::Qualified,
        None => LotProvenance::NoQualifyingReceipt,
    })
}

// ---------------------------------------------------------------------------------------------
// Phase 1M-D3-C1 — the supplier's Schedule X purchase-source authority
// ---------------------------------------------------------------------------------------------

/// The legal basis a Schedule X purchase source can hold, as the Rules name it.
///
/// Rule 61(3): a licence to sell, stock, exhibit or offer for sale or distribute drugs specified in
/// Schedule X by retail or by wholesale is issued in Form 20-F or Form 20-G. A DEALER supplying by
/// wholesale therefore holds Form 20-G. Rule 70: licences to manufacture drugs included in
/// Schedule X are granted in Form 25-F. Those are the two sources this software can represent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupplierAuthorityKind {
    /// Rule 61(3) — the Schedule X wholesale licence a dealer supplies under.
    Form20g,
    /// Rule 70 — the licence to manufacture drugs included in Schedule X.
    Form25f,
}

impl SupplierAuthorityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Form20g => "form_20g",
            Self::Form25f => "form_25f",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "form_20g" => Some(Self::Form20g),
            "form_25f" => Some(Self::Form25f),
            _ => None,
        }
    }
}

/// Whether the operator has recorded documentary evidence that a purchase source held Schedule X
/// authority covering a given drug on a given date.
///
/// THIS IS NOT VERIFICATION. `Established` means "the operator recorded evidence that says so", not
/// "the government confirms this licence is genuine and current". No column, variant or message in
/// this module claims otherwise.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    tag = "state"
)]
pub enum SupplierAuthority {
    /// Evidence on file: an authority recorded as in force across the purchase date, with coverage
    /// naming this product on that date.
    Established {
        authority_id: String,
        authority_kind: String,
        authority_number: String,
        coverage_id: String,
    },
    /// Everything needed was recorded and it does not add up to authority on that date.
    NotEstablished { reason: SupplierAuthorityGap },
    /// Nobody has recorded enough to answer at all.
    Unresolved { reason: SupplierAuthorityGap },
    /// Two or more active records could answer and they disagree. Never settled by picking one.
    Conflicting { reason: SupplierAuthorityGap },
}

/// The specific thing missing or in the way. Named, because "no" is not an instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SupplierAuthorityGap {
    /// No Schedule X authority has been recorded for this supplier at all.
    NoAuthorityRecorded,
    /// An authority exists but nobody recorded whether it was in force.
    AuthorityStatusUnknown,
    /// The authority is recorded as suspended for the period covering this purchase.
    AuthoritySuspended,
    /// The authority is recorded as cancelled for the period covering this purchase.
    AuthorityCancelled,
    /// Nobody recorded whether the authority runs perpetually or to a date.
    ValidityBasisUnknown,
    /// Authority was recorded, but no period covers the purchase date.
    OutsideEffectivePeriod,
    /// The authority covers the date, but no coverage names this drug on it.
    DrugNotCovered,
    /// Overlapping active records could both answer. Fail closed.
    ConflictingAuthorities,
}

impl SupplierAuthorityGap {
    /// Phase 1M-D3-C2 — the stable reason a structured refusal carries, so the counter is told which
    /// purchase to look at rather than being handed one undifferentiated failure.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoAuthorityRecorded => "no_authority_recorded",
            Self::AuthorityStatusUnknown => "authority_status_unknown",
            Self::AuthoritySuspended => "authority_suspended",
            Self::AuthorityCancelled => "authority_cancelled",
            Self::ValidityBasisUnknown => "validity_basis_unknown",
            Self::OutsideEffectivePeriod => "outside_effective_period",
            Self::DrugNotCovered => "drug_not_covered",
            Self::ConflictingAuthorities => "conflicting_authorities",
        }
    }
}

impl SupplierAuthority {
    pub fn is_established(&self) -> bool {
        matches!(self, Self::Established { .. })
    }

    /// The gap, for a caller that only needs to say why not.
    pub fn gap(&self) -> Option<SupplierAuthorityGap> {
        match self {
            Self::Established { .. } => None,
            Self::NotEstablished { reason }
            | Self::Unresolved { reason }
            | Self::Conflicting { reason } => Some(*reason),
        }
    }
}

/// One recorded authority, as the resolver reads it.
#[derive(Debug, Clone, sqlx::FromRow)]
struct AuthorityRow {
    id: String,
    authority_kind: String,
    authority_number: String,
    legal_status: String,
    validity_basis: String,
}

/// Whether a purchase source held recorded Schedule X authority covering `product_id` on
/// `purchase_date`.
///
/// Evaluated ENTIRELY on the historical purchase date against the recorded effective periods. The
/// current Party master's licence text is never consulted, and neither is the D1-A snapshot frozen
/// on the Purchase: that snapshot discharges the record-keeping duty of rule 65(4)(4)(i) and says
/// nothing about whether the source was duly licensed.
///
/// Fail-closed throughout. Two overlapping active authorities are a `Conflicting` answer, not a
/// choice between them — the 0029 triggers refuse to create that state, and this refuses to resolve
/// it if one ever exists.
pub async fn resolve_supplier_schedule_x_authority(
    connection: &mut sqlx::SqliteConnection,
    store_id: &str,
    supplier_party_id: &str,
    purchase_date: &str,
    product_id: &str,
) -> Result<SupplierAuthority, sqlx::Error> {
    // Every ACTIVE authority of this store for this supplier whose recorded period covers the day.
    // Half-open, exactly as every other effective-dated model here: `effective_to` is exclusive.
    let candidates: Vec<AuthorityRow> = sqlx::query_as(
        "SELECT id,authority_kind,authority_number,legal_status,validity_basis \
         FROM supplier_schedule_x_authorities \
         WHERE store_id = ?1 AND supplier_party_id = ?2 AND status = 'active' \
           AND effective_from <= ?3 \
           AND (effective_to IS NULL OR effective_to > ?3) \
         ORDER BY effective_from, id",
    )
    .bind(store_id)
    .bind(supplier_party_id)
    .bind(purchase_date)
    .fetch_all(&mut *connection)
    .await?;

    if candidates.len() > 1 {
        return Ok(SupplierAuthority::Conflicting {
            reason: SupplierAuthorityGap::ConflictingAuthorities,
        });
    }

    let Some(authority) = candidates.into_iter().next() else {
        // Nothing covers the day. Distinguish "nobody recorded anything" from "something was
        // recorded but not for this date", because the two need different actions.
        let any: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM supplier_schedule_x_authorities \
             WHERE store_id = ?1 AND supplier_party_id = ?2 AND status = 'active' LIMIT 1",
        )
        .bind(store_id)
        .bind(supplier_party_id)
        .fetch_optional(&mut *connection)
        .await?;
        return Ok(match any {
            Some(_) => SupplierAuthority::NotEstablished {
                reason: SupplierAuthorityGap::OutsideEffectivePeriod,
            },
            None => SupplierAuthority::Unresolved {
                reason: SupplierAuthorityGap::NoAuthorityRecorded,
            },
        });
    };

    // What the document says about the period it covers. `unknown` never establishes: a nullable
    // "probably fine" is exactly the ambiguity a fail-closed gate must not inherit.
    match authority.legal_status.as_str() {
        "in_force" => {}
        "suspended" => {
            return Ok(SupplierAuthority::NotEstablished {
                reason: SupplierAuthorityGap::AuthoritySuspended,
            });
        }
        "cancelled" => {
            return Ok(SupplierAuthority::NotEstablished {
                reason: SupplierAuthorityGap::AuthorityCancelled,
            });
        }
        _ => {
            return Ok(SupplierAuthority::Unresolved {
                reason: SupplierAuthorityGap::AuthorityStatusUnknown,
            });
        }
    }
    if authority.validity_basis == "unknown" {
        return Ok(SupplierAuthority::Unresolved {
            reason: SupplierAuthorityGap::ValidityBasisUnknown,
        });
    }

    // And the drug. A Schedule X authority is not a blanket permission, so coverage is named per
    // product by stable identity — never by drug name.
    let coverage: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM supplier_schedule_x_authority_coverage \
         WHERE authority_id = ?1 AND store_id = ?2 AND product_id = ?3 AND status = 'active' \
           AND effective_from <= ?4 \
           AND (effective_to IS NULL OR effective_to > ?4) \
         ORDER BY effective_from, id",
    )
    .bind(&authority.id)
    .bind(store_id)
    .bind(product_id)
    .bind(purchase_date)
    .fetch_all(&mut *connection)
    .await?;

    match coverage.len() {
        0 => Ok(SupplierAuthority::NotEstablished {
            reason: SupplierAuthorityGap::DrugNotCovered,
        }),
        1 => Ok(SupplierAuthority::Established {
            authority_id: authority.id,
            authority_kind: authority.authority_kind,
            authority_number: authority.authority_number,
            coverage_id: coverage.into_iter().next().expect("one coverage row"),
        }),
        _ => Ok(SupplierAuthority::Conflicting {
            reason: SupplierAuthorityGap::ConflictingAuthorities,
        }),
    }
}

/// Every purchase source that contributed sellable stock to a lot, with its authority verdict.
///
/// Phase 1M-D3-B established the chain from a sale line to its batch to every sellable inward
/// movement to the purchase line behind it. This walks the same chain and asks the authority
/// question of EACH contributing source, because one authorised supplier must never legalise
/// another whose authority is unresolved.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LotSourceAuthority {
    pub purchase_document_id: String,
    pub purchase_line_id: String,
    pub supplier_party_id: String,
    pub invoice_date: String,
    pub authority: SupplierAuthority,
}

/// The authority verdict for every source behind a lot.
///
/// Reported BESIDE the Phase 1M-D3-B lot verdict, never folded into it: D3-B decides whether the
/// stock is accounted for, and this decides whether its sources were recorded as authorised. A
/// later slice combines them into one sale predicate; this phase only answers the question.
pub async fn resolve_lot_source_authorities(
    connection: &mut sqlx::SqliteConnection,
    store_id: &str,
    batch_id: &str,
    product_id: &str,
) -> Result<Vec<LotSourceAuthority>, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct SourceRow {
        purchase_document_id: String,
        purchase_line_id: String,
        supplier_party_id: String,
        invoice_date: String,
    }
    // One row per distinct purchase line that added sellable stock to this lot, with the supplier
    // and the date as the POSTED document froze them.
    let sources: Vec<SourceRow> = sqlx::query_as(
        "SELECT DISTINCT document.id AS purchase_document_id,line.id AS purchase_line_id,\
         document.supplier_party_id AS supplier_party_id,document.invoice_date AS invoice_date \
         FROM inventory_movements inward \
         JOIN purchase_lines line ON line.id = inward.purchase_line_id \
         JOIN purchase_documents document ON document.id = line.purchase_document_id \
         WHERE inward.store_id = ?1 AND inward.batch_id = ?2 \
           AND inward.stock_status = 'sellable' AND inward.quantity_delta_atoms > 0 \
           AND inward.movement_type = 'purchase' AND document.status = 'posted' \
         ORDER BY document.invoice_date,line.id",
    )
    .bind(store_id)
    .bind(batch_id)
    .fetch_all(&mut *connection)
    .await?;

    let mut out = Vec::with_capacity(sources.len());
    for source in sources {
        let authority = resolve_supplier_schedule_x_authority(
            &mut *connection,
            store_id,
            &source.supplier_party_id,
            &source.invoice_date,
            product_id,
        )
        .await?;
        out.push(LotSourceAuthority {
            purchase_document_id: source.purchase_document_id,
            purchase_line_id: source.purchase_line_id,
            supplier_party_id: source.supplier_party_id,
            invoice_date: source.invoice_date,
            authority,
        });
    }
    Ok(out)
}

/// Whether EVERY contributing source of a lot has recorded authority evidence.
///
/// `None` when there are no contributing sources at all, which is not an authorised lot — it is a
/// lot Phase 1M-D3-B already refuses, and this does not pretend otherwise.
pub fn every_source_authorised(sources: &[LotSourceAuthority]) -> bool {
    !sources.is_empty()
        && sources
            .iter()
            .all(|source| source.authority.is_established())
}

// -------------------------------------------------------------------------------------------------
// Phase 1M-D3-C2 — the Schedule X retail-supply predicates.
//
// Each requirement of a supported Schedule X retail supply is resolved on its own and reported on
// its own. They are deliberately NOT reduced to one "compliant" boolean: the counter has to know
// which requirement is outstanding, and an operator told only "refused" cannot act.
//
// What is NOT here matters as much as what is. The prescription's own authority — exact product
// under rule 65(11A), the total under rule 65(10)(c), the repeat under rule 65(11)(a), the interval
// under rule 65(11)(b), and what is left of the authority — is resolved by
// `domain::prescriptions::check_dispense`, which the posting planner already runs for every
// supervised line. Schedule X joins that path rather than growing a second copy of it, so one set
// of rules answers the prescription question for Schedule H, H1 and X alike, and every database
// protection behind it keeps applying.
//
// Likewise the NDPS axis, the Punjab boundary and the Schedule C/C(1) intersection are answered by
// `domain::regulatory::combined_gate` before any of this is reached, and the Schedule H1 working
// entry by the posting planner's own H1 layer. They appear in the preflight report because the
// operator needs to see them, but their verdicts come from those owners, not from here.
// -------------------------------------------------------------------------------------------------

/// One independent predicate's verdict.
///
/// `NotEstablished` and `Unresolved` are kept apart for the reason Phase 1M-D1-B first kept them
/// apart: "something is recorded and it does not support this supply" and "nobody has recorded
/// anything" need different actions from different people. `Unsupported` is AUSHADHARTH's own
/// boundary rather than a statutory refusal, and `OverlayBlocked` names an independent axis that
/// stops the supply however complete the Schedule X evidence is.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    tag = "state"
)]
pub enum Predicate {
    Established,
    NotEstablished {
        reason: &'static str,
    },
    Unresolved {
        reason: &'static str,
    },
    Unsupported {
        reason: &'static str,
    },
    OverlayBlocked {
        axis: &'static str,
        reason: &'static str,
    },
}

impl Predicate {
    pub fn is_established(&self) -> bool {
        matches!(self, Self::Established)
    }

    /// The stable reason a refusal carries, or `None` when the predicate is established.
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Self::Established => None,
            Self::NotEstablished { reason }
            | Self::Unresolved { reason }
            | Self::Unsupported { reason }
            | Self::OverlayBlocked { reason, .. } => Some(reason),
        }
    }
}

/// A named predicate, the stable code its refusal carries, and its verdict.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedPredicate {
    /// The predicate's stable name, as the preflight contract reports it.
    pub predicate: &'static str,
    /// The structured error code raised when this predicate is not established.
    pub code: &'static str,
    pub verdict: Predicate,
}

impl NamedPredicate {
    pub fn new(predicate: &'static str, code: &'static str, verdict: Predicate) -> Self {
        Self {
            predicate,
            code,
            verdict,
        }
    }
}

/// The store's own Schedule X RETAIL authority, and this drug's coverage under it.
///
/// Two predicates, never one. Form 20-F is headed "LICENCE TO SELL, STOCK OR EXHIBIT FOR SALE OR
/// DISTRIBUTE BY RETAIL DRUGS SPECIFIED IN SCHEDULE X" and its item 2 is "Names of drugs", so the
/// licence is not blanket permission and "no licence" is a different problem from "this drug is not
/// named on it".
///
/// Licence validity is inclusive at both ends, matching how every other licence and registration
/// date in this repository is read. Coverage is a half-open effective period with `effective_to`
/// exclusive, matching every other effective-dated model here.
///
/// Existence is `EXISTS`, not exactly-one, and that is deliberate: it is the same question the
/// database's own `sale_documents_schedule_x_register_required` asks, and two implementations of one
/// predicate must not disagree. It differs from the Phase 1M-D3-C1 supplier authority, where
/// exactly-one matters because that authority's own number is a recorded register particular; here
/// the question is only whether this store is authorised for this drug on this day.
pub async fn resolve_store_retail_authority(
    connection: &mut sqlx::SqliteConnection,
    store_id: &str,
    product_id: &str,
    business_date: &str,
) -> Result<(Predicate, Predicate), sqlx::Error> {
    let licence: Option<String> = sqlx::query_scalar(
        "SELECT id FROM store_compliance_licences \
         WHERE store_id = ?1 AND licence_form = 'form_20f' AND status = 'active' \
           AND (valid_from IS NULL OR valid_from <= ?2) \
           AND (valid_upto IS NULL OR valid_upto >= ?2) \
         ORDER BY id LIMIT 1",
    )
    .bind(store_id)
    .bind(business_date)
    .fetch_optional(&mut *connection)
    .await?;

    if licence.is_none() {
        // Distinguish "a Form 20-F is recorded but not for this day" from "none is recorded".
        let any: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM store_compliance_licences \
             WHERE store_id = ?1 AND licence_form = 'form_20f' AND status = 'active' LIMIT 1",
        )
        .bind(store_id)
        .fetch_optional(&mut *connection)
        .await?;
        let authority = match any {
            Some(_) => Predicate::NotEstablished {
                reason: "authority_outside_validity",
            },
            None => Predicate::Unresolved {
                reason: "no_authority_recorded",
            },
        };
        return Ok((
            authority,
            Predicate::Unresolved {
                reason: "authority_not_established",
            },
        ));
    }

    // The drug, under any Form 20-F of this store that is in force on the day, so a store holding
    // two licences is not refused because the drug is named on the other one.
    let covered: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM store_compliance_licences licence \
         JOIN store_licence_drug_coverage coverage ON coverage.licence_id = licence.id \
         WHERE licence.store_id = ?1 AND licence.licence_form = 'form_20f' \
           AND licence.status = 'active' \
           AND (licence.valid_from IS NULL OR licence.valid_from <= ?2) \
           AND (licence.valid_upto IS NULL OR licence.valid_upto >= ?2) \
           AND coverage.store_id = ?1 AND coverage.product_id = ?3 \
           AND coverage.status = 'active' \
           AND coverage.effective_from <= ?2 \
           AND (coverage.effective_to IS NULL OR coverage.effective_to > ?2) \
         LIMIT 1",
    )
    .bind(store_id)
    .bind(business_date)
    .bind(product_id)
    .fetch_optional(&mut *connection)
    .await?;

    Ok((
        Predicate::Established,
        match covered {
            Some(_) => Predicate::Established,
            None => Predicate::NotEstablished {
                reason: "drug_not_covered",
            },
        },
    ))
}

/// Rule 65(9)(a): the retained duplicate prescription copy.
///
/// The attestation records a person's statement about a sheet of paper. A row saying the copy is NOT
/// held is as meaningful as one saying it is, so a negative attestation is `NotEstablished` while a
/// missing one is `Unresolved`.
pub async fn resolve_duplicate_copy(
    connection: &mut sqlx::SqliteConnection,
    store_id: &str,
    prescription_id: &str,
) -> Result<Predicate, sqlx::Error> {
    let confirmed: Option<i64> = sqlx::query_scalar(
        "SELECT retained_duplicate_prescription_copy_confirmed \
         FROM prescription_duplicate_copy_attestations \
         WHERE store_id = ?1 AND prescription_id = ?2",
    )
    .bind(store_id)
    .bind(prescription_id)
    .fetch_optional(&mut *connection)
    .await?;
    Ok(match confirmed {
        Some(1) => Predicate::Established,
        Some(_) => Predicate::NotEstablished {
            reason: "duplicate_copy_not_held",
        },
        None => Predicate::Unresolved {
            reason: "no_attestation_recorded",
        },
    })
}

/// Rule 65(11)(c): the seller's name and address and the date of dispensing, noted on the
/// prescription above the prescriber's signature.
pub async fn resolve_annotation(
    connection: &mut sqlx::SqliteConnection,
    store_id: &str,
    sale_document_id: &str,
    sale_line_id: &str,
    prescription_item_id: &str,
    product_id: &str,
) -> Result<Predicate, sqlx::Error> {
    let present: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM schedule_x_prescription_annotations \
         WHERE store_id = ?1 AND sale_document_id = ?2 AND sale_line_id = ?3 \
           AND prescription_item_id = ?4 AND product_id = ?5 \
           AND seller_particulars_noted_on_prescription = 1 LIMIT 1",
    )
    .bind(store_id)
    .bind(sale_document_id)
    .bind(sale_line_id)
    .bind(prescription_item_id)
    .bind(product_id)
    .fetch_optional(&mut *connection)
    .await?;
    Ok(match present {
        Some(_) => Predicate::Established,
        None => Predicate::Unresolved {
            reason: "no_annotation_recorded",
        },
    })
}

/// A confirmed Schedule X supply working entry of one Sale line, as the finalizer needs it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ConfirmedSupplyEntry {
    pub id: String,
    pub reference: String,
    pub prescription_id: String,
    pub prescription_item_id: String,
    pub supervising_professional_id: String,
}

/// Rule 65(21): the working entry whose particulars were written into the bound, serially page
/// numbered physical register and authenticated by hand.
///
/// Both attestations are required, and the table's own CHECK keeps them equal so one cannot be
/// recorded without the other. What is verified here is the RECORDED ATTESTATION AND ITS LINKAGE —
/// the same drug, the same lot, the same quantity, the same named registered pharmacist valid on
/// the day. The physical act itself is outside any database and this does not pretend otherwise.
///
/// The three posting bindings are required to be NULL, so an entry already bound to a dispensing or
/// a bill number is never offered to the finalizer a second time.
#[allow(clippy::too_many_arguments)]
pub async fn resolve_confirmed_supply_entry(
    connection: &mut sqlx::SqliteConnection,
    store_id: &str,
    sale_document_id: &str,
    sale_line_id: &str,
    product_id: &str,
    batch_id: &str,
    quantity_atoms: i64,
    business_date: &str,
) -> Result<(Predicate, Option<ConfirmedSupplyEntry>), sqlx::Error> {
    let _ = business_date;
    let entry: Option<ConfirmedSupplyEntry> = sqlx::query_as(
        "SELECT entry.id,entry.reference,entry.prescription_id,entry.prescription_item_id,\
         entry.supervising_professional_id \
         FROM store_schedule_x_register_entries entry \
         WHERE entry.store_id = ?1 AND entry.sale_document_id = ?2 AND entry.sale_line_id = ?3 \
           AND entry.entry_kind = 'supply' AND entry.supply_basis = 'prescription' \
           AND entry.status = 'confirmed' \
           AND entry.particulars_entered_in_physical_register = 1 \
           AND entry.physical_entry_authenticated = 1 \
           AND entry.product_id = ?4 AND entry.batch_id = ?5 AND entry.quantity_atoms = ?6 \
           AND entry.dispensing_id IS NULL \
           AND entry.bill_number IS NULL AND entry.bill_date IS NULL \
         LIMIT 1",
    )
    .bind(store_id)
    .bind(sale_document_id)
    .bind(sale_line_id)
    .bind(product_id)
    .bind(batch_id)
    .bind(quantity_atoms)
    .fetch_optional(&mut *connection)
    .await?;

    if let Some(entry) = entry {
        return Ok((Predicate::Established, Some(entry)));
    }

    // Nothing usable. Say which of the ordinary situations this is, because each needs a different
    // next step from a different person.
    let state: Option<String> = sqlx::query_scalar(
        "SELECT status FROM store_schedule_x_register_entries \
         WHERE store_id = ?1 AND sale_document_id = ?2 AND sale_line_id = ?3 \
           AND entry_kind = 'supply' AND status IN ('prepared','confirmed','finalized') \
         ORDER BY CASE status WHEN 'confirmed' THEN 0 WHEN 'prepared' THEN 1 ELSE 2 END LIMIT 1",
    )
    .bind(store_id)
    .bind(sale_document_id)
    .bind(sale_line_id)
    .fetch_optional(&mut *connection)
    .await?;

    Ok((
        match state.as_deref() {
            Some("prepared") => Predicate::NotEstablished {
                reason: "physical_entry_not_confirmed",
            },
            // Confirmed, but it did not match this line's drug, lot, quantity or a date-valid
            // supervising registered pharmacist — or it is already bound.
            Some("confirmed") => Predicate::NotEstablished {
                reason: "confirmed_entry_does_not_match_line",
            },
            Some("finalized") => Predicate::NotEstablished {
                reason: "entry_already_finalized",
            },
            _ => Predicate::Unresolved {
                reason: "no_working_entry_prepared",
            },
        },
        None,
    ))
}

/// Rule 65(2): the registered pharmacist under whose personal supervision this supply is effected,
/// as the confirmed working entry names them, still valid on the day.
///
/// A predicate of its own, and deliberately not folded into the working-entry predicate above. The
/// 0028 transition trigger already proves the named professional was an active registered pharmacist
/// with a registration number, valid on the entry's transaction date, AT THE MOMENT OF CONFIRMATION.
/// What this answers is whether that is still so when the Sale is posted — a professional can be
/// archived, or their registration can lapse, between the signature and the till.
///
/// Reporting that as "the register entry is missing" would send the operator to rewrite a page of a
/// bound register that is already correctly written and signed. The paper is not the problem; the
/// pharmacist's record is.
pub async fn resolve_entry_supervising_pharmacist(
    connection: &mut sqlx::SqliteConnection,
    store_id: &str,
    sale_document_id: &str,
    sale_line_id: &str,
    business_date: &str,
) -> Result<Predicate, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Named {
        status: String,
        capacity: String,
        registration_number: Option<String>,
        valid_from: Option<String>,
        valid_upto: Option<String>,
        store_id: String,
    }
    let named: Option<Named> = sqlx::query_as(
        "SELECT professional.status,professional.capacity,professional.registration_number,\
         professional.valid_from,professional.valid_upto,professional.store_id \
         FROM store_schedule_x_register_entries entry \
         JOIN store_professionals professional \
           ON professional.id = entry.supervising_professional_id \
         WHERE entry.store_id = ?1 AND entry.sale_document_id = ?2 AND entry.sale_line_id = ?3 \
           AND entry.entry_kind = 'supply' AND entry.status = 'confirmed' \
         LIMIT 1",
    )
    .bind(store_id)
    .bind(sale_document_id)
    .bind(sale_line_id)
    .fetch_optional(&mut *connection)
    .await?;

    // Nothing to judge until the entry names somebody. The working-entry predicate reports that.
    let Some(named) = named else {
        return Ok(Predicate::Unresolved {
            reason: "no_confirmed_entry",
        });
    };
    if named.store_id != store_id {
        return Ok(Predicate::NotEstablished {
            reason: "professional_of_another_store",
        });
    }
    if named.status != "active" {
        return Ok(Predicate::NotEstablished {
            reason: "professional_archived",
        });
    }
    // Rule 65(2) names a registered pharmacist. A competent person is not one, whatever they are
    // called, and the Pharmacy Act registration number is part of being one.
    if named.capacity != "registered_pharmacist" {
        return Ok(Predicate::NotEstablished {
            reason: "not_a_registered_pharmacist",
        });
    }
    if named
        .registration_number
        .as_deref()
        .is_none_or(|number| number.trim().is_empty())
    {
        return Ok(Predicate::NotEstablished {
            reason: "no_registration_number",
        });
    }
    // Valid on the business date AND on the store's actual day, the stricter of the two, exactly as
    // the Sale path judges its supervising pharmacist. A backdated draft cannot reach past a lapse.
    let mut days = store_days(&mut *connection).await?;
    days.push(business_date.to_owned());
    for day in &days {
        if named
            .valid_from
            .as_deref()
            .is_some_and(|from| day.as_str() < from)
        {
            return Ok(Predicate::NotEstablished {
                reason: "registration_not_yet_valid",
            });
        }
        if named
            .valid_upto
            .as_deref()
            .is_some_and(|upto| day.as_str() > upto)
        {
            return Ok(Predicate::NotEstablished {
                reason: "registration_lapsed",
            });
        }
    }
    Ok(Predicate::Established)
}
