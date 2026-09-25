//! Phase 1H sales / POS outward.
//!
//! The store's first outward transaction. A Draft is freely editable and touches nothing; posting is
//! a named command that, inside one `BEGIN IMMEDIATE` transaction, allocates the invoice number,
//! freezes every snapshot, checks stock, computes the tax and takes the stock out. Any failure rolls
//! all of it back, including the number.
//!
//! Scope is one transaction shape: an over-the-counter / store-delivery sale. The goods are handed
//! over at the counter, so under IGST Act s.10(1)(c) the place of supply is the store and the
//! treatment is intra-State. Delivery and inter-State dispatch have different place-of-supply rules
//! and are deliberately not modelled here rather than being forced through this document.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, Sqlite, SqliteConnection, SqlitePool, pool::PoolConnection};
use uuid::Uuid;

use super::auth::{self, AuthError, AuthenticatedActor};
use super::reference_masters::ReferenceState;
use crate::domain::{
    catalog::{CatalogValidationIssue, optional_text, validate_date, validate_uuid_v7},
    invoice_compliance::{
        self, ComplianceIssue, ComplianceRefusal, ComplianceSource, DynamicQrApplicability,
        EinvoiceApplicability, HsnBand, Rule46sDeclaration, SellerGst, TenderFact,
    },
    money::{self, LineAmounts, MoneyError, RateComponents, TaxTreatment},
    prescriptions,
    price_control::{self, Comparability, PriceControlStatus},
    recipient::{
        self, AddressFacts, MissingRecipientFact, PartyBilling, PartyRecipient, RecipientSource,
    },
    regulatory,
    sales::{self, QuantityBasis, SaleMoneyError, SaleQuantity},
    store_profile::{self, MissingSellerFact},
    taxation,
};

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum SaleError {
    Auth(AuthError),
    Validation(Vec<CatalogValidationIssue>),
    NotFound,
    NotDraft,
    Revision {
        expected: i64,
        current: i64,
    },
    CustomerNotEligible,
    StoreTaxIncomplete,
    /// The Store has no name, no address, or no active sale licence, so no lawful memo could be
    /// issued for this sale. Carries the exact missing particulars so the browser can send the
    /// operator to one field instead of to a settings page.
    StoreLegalProfileIncomplete(Vec<MissingSellerFact>),
    /// Rule 46 requires this invoice to show recipient particulars the Sale does not have — a
    /// registered customer, a taxable value of ₹50,000 or more, or a customer who asked. Carries
    /// the exact missing particulars, as the seller refusal does.
    RecipientParticularsIncomplete(Vec<MissingRecipientFact>),
    /// The Store has not recorded whether it is GST-registered, so whether this Sale charges GST
    /// cannot be decided. Refused rather than guessed in either direction.
    StoreGstStatusUnresolved,
    /// A fact a posted document must carry — the Rule 46(s) applicability, the Rule 48(4)
    /// applicability for a registered recipient, the HSN policy and codes, or a designated
    /// retail-memo licence — is missing. Carries each issue.
    SaleComplianceIncomplete(Vec<ComplianceIssue>),
    /// The Store recorded that Rule 48(4) requires an e-invoice for supplies to registered persons,
    /// and this Sale is to one. AUSHADHARTH cannot obtain an IRN, and Rule 48(5) says a document
    /// issued any other way "shall not be treated as an invoice", so it is not issued at all.
    EinvoiceRequired,
    /// Notification No. 14/2020-CT applies to this B2C invoice, which relies on the payment
    /// cross-reference, and these tenders (by position) are card or UPI payments with no transaction
    /// reference. Carries one issue per tender.
    PaymentReferenceRequired(Vec<usize>),
    /// A registered seller's Sale to a registered recipient mixing taxable and untaxed supplies.
    /// No document AUSHADHARTH issues covers it (Rule 46A is for unregistered recipients), so it is
    /// refused before anything is posted rather than left without a document afterwards.
    RegisteredRecipientMixedSupply,
    /// Phase 1M-A: a medicine nobody has placed inside or outside the schedules. Not knowing
    /// whether a drug is Schedule H is not a reason to hand it over.
    RegulatoryClassificationUnresolved {
        line_number: i64,
    },
    /// Phase 1M-A: lawfully sellable, but only with a statutory record this software does not yet
    /// keep. Refused rather than sold without one.
    RegulatedSaleWorkflowNotAvailable {
        line_number: i64,
        scheme: &'static str,
    },
    /// Phase 1M-C: a Schedule H1 line whose NDPS purview applies or is not recorded. AUSHADHARTH
    /// does not support that combined workflow — an unsupported NDPS-intersection workflow, which
    /// is a boundary of this software and not a requirement rule 65 itself states.
    ScheduleH1NdpsWorkflowNotAvailable {
        line_number: i64,
    },
    /// Phase 1M-C: an additional Punjab drug-control workflow applies to this drug at this store,
    /// and AUSHADHARTH does not implement it. Not a statement that the sale is prohibited.
    StateRestrictedDrugWorkflowNotAvailable {
        line_number: i64,
    },
    /// Phase 1M-C: the medicine's position under the Punjab boundary, or the store's premises
    /// State itself, is not recorded, so the boundary cannot be judged.
    StateRegulatoryPositionUnresolved {
        line_number: i64,
    },
    /// Phase 1M-B: Schedule X needs the duplicate prescription and the rule 65(21) register.
    ScheduleXWorkflowNotAvailable {
        line_number: i64,
    },
    /// Phase 1M-B: a Schedule H supply whose prescription, supervision or endorsement is not
    /// complete. Carries every unmet requirement.
    PrescriptionRequirementsIncomplete(Vec<PrescriptionIssue>),
    /// Phase 1M-B: no rule 65(3)(2) election is in force for this supply, so there is no book to
    /// enter it in. Only the owner can record one.
    PrescriptionRecordElectionUnresolved,
    /// Phase 1M-B: the Sale's rule 65(3)(1) entry is prepared or confirmed, so the Sale is held as
    /// the pharmacist signed it for. Void the entry to change the Sale.
    PrescriptionRecordPrepared {
        serial: String,
    },
    ClassificationIncomplete,
    TaxRateNotFound,
    PackMismatch,
    PackNotSellable,
    BatchPackMismatch,
    BatchExpired,
    FractionalNotAllowed,
    QuantityIncrement,
    InsufficientStock {
        available: i64,
    },
    SellingRateAboveMrp,
    SellingRateAboveCeiling,
    PriceControlUnresolved,
    PriceControlIncomparable,
    TenderMismatch,
    ArithmeticOverflow,
    IdempotencyConflict,
    /// A replay of the same key with the same facts: the original document is returned.
    AlreadyPostedReplay,
    ServiceBusy,
    Internal,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
    issues: Vec<ErrorIssue>,
    expected_revision: Option<i64>,
    current_revision: Option<i64>,
    available_atoms: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorIssue {
    field: String,
    message: String,
}

fn simple(code: &'static str, message: &'static str) -> ErrorBody {
    ErrorBody {
        code,
        message,
        issues: Vec::new(),
        expected_revision: None,
        current_revision: None,
        available_atoms: None,
    }
}

impl IntoResponse for SaleError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            Self::Auth(error) => return error.into_response(),
            Self::Validation(issues) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorBody {
                    code: "validation_failed",
                    message: "The request failed validation.",
                    issues: issues
                        .into_iter()
                        .map(|issue| ErrorIssue {
                            field: issue.field,
                            message: issue.message,
                        })
                        .collect(),
                    expected_revision: None,
                    current_revision: None,
                    available_atoms: None,
                },
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                simple("sale_not_found", "The sale was not found."),
            ),
            Self::NotDraft => (
                StatusCode::CONFLICT,
                simple(
                    "sale_not_draft",
                    "A posted sale cannot be changed. Correct it with a later document.",
                ),
            ),
            Self::Revision { expected, current } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "revision_conflict",
                    message: "The sale changed after it was read.",
                    issues: Vec::new(),
                    expected_revision: Some(expected),
                    current_revision: Some(current),
                    available_atoms: None,
                },
            ),
            Self::CustomerNotEligible => (
                StatusCode::CONFLICT,
                simple(
                    "customer_not_eligible",
                    "That party is not an active customer.",
                ),
            ),
            Self::StoreTaxIncomplete => (
                StatusCode::CONFLICT,
                simple(
                    "store_tax_profile_incomplete",
                    "Record this store's place of supply before selling.",
                ),
            ),
            Self::StoreLegalProfileIncomplete(missing) => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "store_legal_profile_incomplete",
                    message: "Complete the pharmacy's details in Store Profile before selling.",
                    issues: missing
                        .into_iter()
                        .map(|fact| ErrorIssue {
                            field: fact.field().to_owned(),
                            message: fact.message().to_owned(),
                        })
                        .collect(),
                    expected_revision: None,
                    current_revision: None,
                    available_atoms: None,
                },
            ),
            Self::RecipientParticularsIncomplete(missing) => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "recipient_particulars_incomplete",
                    message: "This invoice must show the customer's details. Complete them before posting.",
                    issues: missing
                        .into_iter()
                        .map(|fact| ErrorIssue {
                            field: fact.field().to_owned(),
                            message: fact.message().to_owned(),
                        })
                        .collect(),
                    expected_revision: None,
                    current_revision: None,
                    available_atoms: None,
                },
            ),
            Self::StoreGstStatusUnresolved => (
                StatusCode::CONFLICT,
                simple(
                    "store_gst_status_unresolved",
                    "Record in Store Profile whether this pharmacy is GST-registered before selling.",
                ),
            ),
            Self::EinvoiceRequired => (
                StatusCode::CONFLICT,
                simple(
                    "einvoice_required_unsupported",
                    "This pharmacy must e-invoice GST-registered customers, and AUSHADHARTH cannot issue e-invoices.",
                ),
            ),
            Self::PaymentReferenceRequired(tenders) => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "payment_reference_required",
                    message: "Enter the card or UPI transaction reference before posting.",
                    issues: tenders
                        .iter()
                        .map(|index| ErrorIssue {
                            field: format!("tenders[{index}].referenceText"),
                            message: "Enter the transaction reference shown on the card slip or UPI app. This pharmacy records it on the invoice as the payment cross-reference.".to_owned(),
                        })
                        .collect(),
                    expected_revision: None,
                    current_revision: None,
                    available_atoms: None,
                },
            ),
            Self::RegisteredRecipientMixedSupply => (
                StatusCode::CONFLICT,
                simple(
                    "registered_recipient_mixed_supply_unsupported",
                    "A GST-registered customer's bill cannot mix taxable and untaxed items. Bill them separately.",
                ),
            ),
            Self::RegulatoryClassificationUnresolved { line_number } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "regulatory_classification_unresolved",
                    message: "This medicine has no recorded schedule position, so it cannot be sold yet.",
                    issues: vec![ErrorIssue {
                        field: format!("lines.{line_number}"),
                        message:
                            "Record whether this medicine is within Schedule H, H1, X, C or C(1) before selling it."
                                .to_owned(),
                    }],
                    expected_revision: None,
                    current_revision: None,
                    available_atoms: None,
                },
            ),
            Self::RegulatedSaleWorkflowNotAvailable {
                line_number,
                scheme,
            } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "regulated_sale_workflow_not_available",
                    message: "This sale needs a statutory record this version cannot keep yet.",
                    issues: vec![ErrorIssue {
                        field: format!("lines.{line_number}"),
                        message: format!(
                            "This line is classified within {scheme}, whose prescription or register requirements are not implemented yet."
                        ),
                    }],
                    expected_revision: None,
                    current_revision: None,
                    available_atoms: None,
                },
            ),
            Self::ScheduleH1NdpsWorkflowNotAvailable { line_number } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "schedule_h1_ndps_workflow_not_available",
                    message: "This Schedule H1 drug is, or may be, within the NDPS Act, and AUSHADHARTH does not support that combined workflow.",
                    issues: vec![ErrorIssue {
                        field: format!("lines.{line_number}"),
                        message: "Unsupported NDPS-intersection workflow: record the product's NDPS position as not applying if that is the finding, or remove this line to post the rest.".to_owned(),
                    }],
                    expected_revision: None,
                    current_revision: None,
                    available_atoms: None,
                },
            ),
            Self::StateRestrictedDrugWorkflowNotAvailable { line_number } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "state_restricted_drug_workflow_not_available",
                    message: "This drug is subject to an additional Punjab drug-control workflow that AUSHADHARTH does not yet support.",
                    issues: vec![ErrorIssue {
                        field: format!("lines.{line_number}"),
                        message: "Remove this line to post the rest.".to_owned(),
                    }],
                    expected_revision: None,
                    current_revision: None,
                    available_atoms: None,
                },
            ),
            Self::StateRegulatoryPositionUnresolved { line_number } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "state_regulatory_position_unresolved",
                    message: "This medicine's position under the Punjab drug-control boundary is not recorded, or the store's premises State is not recorded.",
                    issues: vec![ErrorIssue {
                        field: format!("lines.{line_number}"),
                        message: "Record the product's Punjab finding in Drug Compliance, or the store's premises address in Store Profile.".to_owned(),
                    }],
                    expected_revision: None,
                    current_revision: None,
                    available_atoms: None,
                },
            ),
            Self::ScheduleXWorkflowNotAvailable { line_number } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "schedule_x_workflow_not_available",
                    message: "Schedule X dispensing requires the Schedule X workflow, which is not yet available.",
                    issues: vec![ErrorIssue {
                        field: format!("lines.{line_number}"),
                        message: "This line is Schedule X. Remove it to post the rest.".to_owned(),
                    }],
                    expected_revision: None,
                    current_revision: None,
                    available_atoms: None,
                },
            ),
            Self::PrescriptionRequirementsIncomplete(issues) => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "prescription_requirements_incomplete",
                    message: "This prescription sale is missing prescription requirements.",
                    issues: issues
                        .iter()
                        .map(|issue| ErrorIssue {
                            field: match issue.line_number {
                                Some(line) => format!("lines.{line}.{}", issue.code),
                                None => format!("supply.{}", issue.code),
                            },
                            message: prescription_issue_message(issue.code).to_owned(),
                        })
                        .collect(),
                    expected_revision: None,
                    current_revision: None,
                    available_atoms: None,
                },
            ),
            Self::PrescriptionRecordElectionUnresolved => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "prescription_record_election_unresolved",
                    message: "The pharmacy's rule 65(3)(2) prescription-supply record election is not recorded.",
                    issues: vec![ErrorIssue {
                        field: "supply.prescription_record_election_unresolved".to_owned(),
                        message: prescription_issue_message("prescription_record_election_unresolved")
                            .to_owned(),
                    }],
                    expected_revision: None,
                    current_revision: None,
                    available_atoms: None,
                },
            ),
            Self::PrescriptionRecordPrepared { serial } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "prescription_record_prepared",
                    message: "This sale's prescription-supply entry is prepared, so the sale cannot be changed.",
                    issues: vec![ErrorIssue {
                        field: "supply.prescription_record_prepared".to_owned(),
                        message: format!(
                            "Entry {serial} was prepared for this sale as it stands. Void the entry to change the sale; its serial is not reused."
                        ),
                    }],
                    expected_revision: None,
                    current_revision: None,
                    available_atoms: None,
                },
            ),
            Self::SaleComplianceIncomplete(issues) => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "sale_compliance_incomplete",
                    message: "This sale needs details recorded before it can be posted.",
                    issues: issues
                        .iter()
                        .map(|issue| ErrorIssue {
                            field: issue.field(),
                            message: issue.message(),
                        })
                        .collect(),
                    expected_revision: None,
                    current_revision: None,
                    available_atoms: None,
                },
            ),
            Self::ClassificationIncomplete => (
                StatusCode::CONFLICT,
                simple(
                    "product_tax_classification_incomplete",
                    "A product on this sale has no Tax Category.",
                ),
            ),
            Self::TaxRateNotFound => (
                StatusCode::CONFLICT,
                simple(
                    "tax_rate_not_found",
                    "No tax rate is in force on the sale date for a product's Tax Category.",
                ),
            ),
            Self::PackMismatch => (
                StatusCode::CONFLICT,
                simple(
                    "product_pack_mismatch",
                    "The pack does not belong to the selected product.",
                ),
            ),
            Self::PackNotSellable => (
                StatusCode::CONFLICT,
                simple(
                    "pack_not_sellable",
                    "This pack is not enabled for sale at this store.",
                ),
            ),
            Self::BatchPackMismatch => (
                StatusCode::CONFLICT,
                simple(
                    "batch_pack_mismatch",
                    "The batch does not belong to the selected pack.",
                ),
            ),
            Self::BatchExpired => (
                StatusCode::CONFLICT,
                simple(
                    "batch_expired",
                    "This batch has expired and cannot be sold.",
                ),
            ),
            Self::FractionalNotAllowed => (
                StatusCode::CONFLICT,
                simple(
                    "fractional_sale_not_allowed",
                    "This pack cannot be split: sell it as whole packs.",
                ),
            ),
            Self::QuantityIncrement => (
                StatusCode::CONFLICT,
                simple(
                    "quantity_increment_violation",
                    "That quantity is not a whole multiple of this pack's smallest sellable unit.",
                ),
            ),
            Self::InsufficientStock { available } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "insufficient_stock",
                    message: "This sale would leave a negative stock balance.",
                    issues: Vec::new(),
                    expected_revision: None,
                    current_revision: None,
                    available_atoms: Some(available),
                },
            ),
            Self::SellingRateAboveMrp => (
                StatusCode::CONFLICT,
                simple(
                    "selling_rate_above_mrp",
                    "The amount charged exceeds this batch's printed MRP.",
                ),
            ),
            Self::SellingRateAboveCeiling => (
                StatusCode::CONFLICT,
                simple(
                    "selling_rate_above_ceiling",
                    "The amount charged exceeds the notified ceiling price for this medicine.",
                ),
            ),
            Self::PriceControlUnresolved => (
                StatusCode::CONFLICT,
                simple(
                    "price_control_unresolved",
                    "This medicine is price-controlled but no ceiling is in force on the sale date.",
                ),
            ),
            Self::PriceControlIncomparable => (
                StatusCode::CONFLICT,
                simple(
                    "price_control_incomparable",
                    "This medicine's ceiling cannot be compared with the selling rate.",
                ),
            ),
            Self::TenderMismatch => (
                StatusCode::CONFLICT,
                simple(
                    "tender_mismatch",
                    "The tender must be a single payment for exactly the invoice total.",
                ),
            ),
            Self::ArithmeticOverflow => (
                StatusCode::UNPROCESSABLE_ENTITY,
                simple(
                    "arithmetic_overflow",
                    "The amounts on this sale are too large to record.",
                ),
            ),
            Self::IdempotencyConflict => (
                StatusCode::CONFLICT,
                simple(
                    "idempotency_conflict",
                    "This posting key was already used with different details.",
                ),
            ),
            Self::AlreadyPostedReplay => (
                StatusCode::OK,
                simple("posting_conflict", "This sale was already posted."),
            ),
            Self::ServiceBusy => (
                StatusCode::SERVICE_UNAVAILABLE,
                simple(
                    "service_busy",
                    "The local service is busy. Try again shortly.",
                ),
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                simple("internal_error", "The operation could not be completed."),
            ),
        };
        (status, Json(body)).into_response()
    }
}

impl From<AuthError> for SaleError {
    fn from(value: AuthError) -> Self {
        Self::Auth(value)
    }
}

impl From<MoneyError> for SaleError {
    fn from(_: MoneyError) -> Self {
        Self::ArithmeticOverflow
    }
}

impl From<SaleMoneyError> for SaleError {
    fn from(value: SaleMoneyError) -> Self {
        match value {
            SaleMoneyError::Overflow => Self::ArithmeticOverflow,
            SaleMoneyError::InvalidQuantity => {
                validation_of("quantity", "is not a usable quantity")
            }
            SaleMoneyError::InvalidRate => {
                validation_of("sellingRatePaise", "is not a usable amount")
            }
            SaleMoneyError::InvalidPack => Self::PackMismatch,
            // The domain says which rule refused, so the counter is told the truth rather than a
            // reason the service guessed at by re-deriving the policy.
            SaleMoneyError::IncrementViolation => Self::QuantityIncrement,
            SaleMoneyError::FractionalNotPermitted => Self::FractionalNotAllowed,
        }
    }
}

fn validation_of(field: &str, message: &str) -> SaleError {
    SaleError::Validation(vec![CatalogValidationIssue {
        field: field.to_owned(),
        message: message.to_owned(),
    }])
}

fn validation_issue(issue: CatalogValidationIssue) -> SaleError {
    SaleError::Validation(vec![issue])
}

fn map_database_error(error: sqlx::Error) -> SaleError {
    if let sqlx::Error::Database(database) = &error {
        let code = database.code().unwrap_or_default().to_string();
        let message = database.message().to_ascii_lowercase();
        if matches!(code.as_str(), "5" | "6" | "261" | "262" | "517")
            || message.contains("database is locked")
            || message.contains("database table is locked")
            || message.contains("database is busy")
        {
            return SaleError::ServiceBusy;
        }
        if message.contains("sale_document_is_posted") {
            return SaleError::NotDraft;
        }
        if message.contains("customer_not_eligible") {
            return SaleError::CustomerNotEligible;
        }
        if message.contains("registered_recipient_mixed_supply") {
            return SaleError::RegisteredRecipientMixedSupply;
        }
        // The service validates a draft's recipient fields before writing them, so the database
        // refusing them means a request slipped past that validation. Said as a validation failure
        // on the field involved, never as the trigger's name.
        if message.contains("recipient_draft_conflict") {
            return validation_of(
                "recipientAddress",
                "a named customer's address comes from their record, and a delivery address needs delivery elsewhere",
            );
        }
        if message.contains("inventory_movement_conflict") {
            return SaleError::BatchPackMismatch;
        }
        // SQLite names the offending COLUMNS in a unique violation, never the index — the Phase 1G
        // lesson. Matching index names would silently never fire.
        if message.contains("sale_documents.posting_idempotency_key") {
            return SaleError::IdempotencyConflict;
        }
        if message.contains("sale_documents.document_number")
            || message.contains("sale_documents.sequence_value")
        {
            // Two counters cannot be handed the same number; if it ever happened, saying so plainly
            // is far better than letting a duplicate invoice exist.
            return SaleError::ServiceBusy;
        }
        if message.contains("foreign key constraint failed") {
            return SaleError::PackMismatch;
        }
    }
    SaleError::Internal
}

// ---------------------------------------------------------------------------------------------
// Requests and responses
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DraftHeaderRequest {
    customer_party_id: Option<String>,
    customer_name_text: Option<String>,
    business_date: String,
    /// Rule 46(f): the customer asked for their details on the invoice. Recorded by the operator,
    /// never inferred.
    #[serde(default)]
    recipient_particulars_requested: bool,
    /// Typed at the counter for a walk-in. A named customer's address is read from their record at
    /// posting, so this is refused beside a `customerPartyId`.
    recipient_address: Option<AddressRequest>,
    #[serde(default = "delivered_to_recipient")]
    delivery_same_as_recipient: bool,
    delivery_address: Option<AddressRequest>,
}

fn delivered_to_recipient() -> bool {
    true
}

/// An address as the counter types it. Every field may be blank on a draft: completeness is judged
/// at posting, when the taxable value that decides whether an address is needed is final.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddressRequest {
    line1: Option<String>,
    line2: Option<String>,
    city: Option<String>,
    postal_code: Option<String>,
    state_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateDraftRequest {
    expected_revision: i64,
    #[serde(flatten)]
    header: DraftHeaderRequest,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LineRequest {
    expected_revision: i64,
    product_id: String,
    product_pack_id: String,
    batch_id: String,
    quantity_basis: String,
    /// The quantity in the basis above: a pack count, or base-unit atoms. One field, because a
    /// second one would let the two disagree.
    quantity: i64,
    selling_rate_paise: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TenderRequest {
    method: String,
    amount_paise: i64,
    reference_text: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PostRequest {
    expected_revision: i64,
    idempotency_key: String,
    tenders: Vec<TenderRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListQuery {
    status: Option<String>,
    customer_party_id: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct SaleHeaderResponse {
    id: String,
    store_id: String,
    customer_party_id: Option<String>,
    customer_name_text: Option<String>,
    business_date: String,
    status: String,
    revision: i64,
    series_code: Option<String>,
    financial_year: Option<String>,
    sequence_value: Option<i64>,
    document_number: Option<String>,
    store_gst_registration_status: Option<String>,
    store_normalized_gstin: Option<String>,
    store_place_of_supply_state_id: Option<String>,
    store_state_code: Option<String>,
    customer_display_name: Option<String>,
    customer_gst_registration_status: Option<String>,
    customer_normalized_gstin: Option<String>,
    customer_state_code: Option<String>,
    tax_treatment: Option<String>,
    taxable_value_paise: i64,
    cgst_paise: i64,
    sgst_paise: i64,
    igst_paise: i64,
    cess_paise: i64,
    grand_total_paise: i64,
    created_by_user_id: String,
    created_at_utc: String,
    updated_at_utc: String,
    posted_by_user_id: Option<String>,
    posted_at_utc: Option<String>,
    /// 0 on a Sale posted before recipient particulars existed, and on every draft; 1 once posting
    /// has evaluated and frozen them.
    recipient_snapshot_version: i64,
    recipient_particulars_requested: Option<bool>,
    recipient_address_source: Option<String>,
    recipient_address_line1: Option<String>,
    recipient_address_line2: Option<String>,
    recipient_city: Option<String>,
    recipient_postal_code: Option<String>,
    recipient_state_id: Option<String>,
    recipient_state_name: Option<String>,
    recipient_state_code: Option<String>,
    delivery_same_as_recipient: Option<bool>,
    delivery_address_line1: Option<String>,
    delivery_address_line2: Option<String>,
    delivery_city: Option<String>,
    delivery_postal_code: Option<String>,
    delivery_state_id: Option<String>,
    delivery_state_name: Option<String>,
    delivery_state_code: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct SaleLineResponse {
    id: String,
    sale_document_id: String,
    line_number: i64,
    product_id: String,
    product_pack_id: String,
    batch_id: String,
    quantity_basis: String,
    quantity_packs: Option<i64>,
    quantity_atoms: i64,
    selling_rate_paise: i64,
    product_display_name: Option<String>,
    pack_display_label: Option<String>,
    base_unit_label: Option<String>,
    batch_number: Option<String>,
    batch_expires_on: Option<String>,
    batch_mrp_paise: Option<i64>,
    hsn_code_id: Option<String>,
    hsn_code: Option<String>,
    tax_category_id: Option<String>,
    tax_treatment_kind: Option<String>,
    tax_rate_version_id: Option<String>,
    cgst_basis_points: i64,
    sgst_basis_points: i64,
    igst_basis_points: i64,
    cess_basis_points: i64,
    price_control_status: Option<String>,
    controlled_formulation_id: Option<String>,
    price_control_version_id: Option<String>,
    ceiling_price_paise: Option<i64>,
    ceiling_basis: Option<String>,
    taxable_value_paise: i64,
    cgst_paise: i64,
    sgst_paise: i64,
    igst_paise: i64,
    cess_paise: i64,
    line_total_paise: i64,
    /// What the catalogue calls these things **right now**.
    ///
    /// The four snapshot fields above are NULL until posting freezes them, which is correct: a
    /// snapshot is what was true when the invoice was issued, and a draft has issued nothing. But a
    /// counter still has to read its own bill while building it, and a row identified by a raw UUID
    /// with the word "Chosen" where the batch should be is unusable. These are read live and are
    /// never written anywhere, so they can never be mistaken for what the invoice recorded.
    current_product_display_name: Option<String>,
    current_pack_display_label: Option<String>,
    current_base_unit_label: Option<String>,
    current_batch_number: Option<String>,
    /// Phase 1M-B: the prescription item this line is dispensed against, if any.
    prescription_item_id: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct SaleTenderResponse {
    id: String,
    sale_document_id: String,
    method: String,
    amount_paise: i64,
    reference_text: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SaleDetailResponse {
    #[serde(flatten)]
    sale: SaleHeaderResponse,
    lines: Vec<SaleLineResponse>,
    tenders: Vec<SaleTenderResponse>,
    /// Phase 1M-B: who supervises a Schedule H supply, and whether its endorsement was confirmed.
    supply: DraftSupplyResponse,
    /// Phase 1M-B: the rule 65(3)(1) entries this Sale created. Serials and states only — the
    /// particulars, which name the patient, are read from the entry by the dispensing roles.
    prescription_records: Vec<SaleRecordSummary>,
    /// Phase 1M-C: this Sale's Schedule H1 working entries. AUSHADHARTH References and states only
    /// — the particulars, which name the patient, are read by the dispensing roles.
    h1_register_entries: Vec<SaleH1EntrySummary>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct SaleH1EntrySummary {
    id: String,
    reference: String,
    status: String,
    line_number: i64,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct DraftSupplyResponse {
    supervising_professional_id: Option<String>,
    prescription_endorsement_confirmed: bool,
    prescription_original_container_confirmed: bool,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct SaleRecordSummary {
    id: String,
    serial_number: String,
    record_method: String,
    status: String,
    prescription_reference: String,
}

const HEADER_COLUMNS: &str = "id,store_id,customer_party_id,customer_name_text,business_date,status,\
     revision,series_code,financial_year,sequence_value,document_number,\
     store_gst_registration_status,store_normalized_gstin,store_place_of_supply_state_id,\
     store_state_code,customer_display_name,customer_gst_registration_status,\
     customer_normalized_gstin,customer_state_code,tax_treatment,taxable_value_paise,cgst_paise,\
     sgst_paise,igst_paise,cess_paise,grand_total_paise,created_by_user_id,created_at_utc,\
     updated_at_utc,posted_by_user_id,posted_at_utc,recipient_snapshot_version,\
     recipient_particulars_requested,recipient_address_source,recipient_address_line1,\
     recipient_address_line2,recipient_city,recipient_postal_code,recipient_state_id,\
     recipient_state_name,recipient_state_code,delivery_same_as_recipient,delivery_address_line1,\
     delivery_address_line2,delivery_city,delivery_postal_code,delivery_state_id,\
     delivery_state_name,delivery_state_code";

const LINE_COLUMNS: &str = "id,sale_document_id,line_number,product_id,product_pack_id,batch_id,\
     quantity_basis,quantity_packs,quantity_atoms,selling_rate_paise,product_display_name,\
     pack_display_label,base_unit_label,batch_number,batch_expires_on,batch_mrp_paise,hsn_code_id,\
     hsn_code,tax_category_id,tax_treatment_kind,tax_rate_version_id,cgst_basis_points,\
     sgst_basis_points,igst_basis_points,cess_basis_points,price_control_status,\
     controlled_formulation_id,price_control_version_id,ceiling_price_paise,ceiling_basis,\
     taxable_value_paise,cgst_paise,sgst_paise,igst_paise,cess_paise,line_total_paise";

pub fn routes() -> Router<ReferenceState> {
    Router::new()
        .route("/api/v1/sales", get(list_sales).post(create_draft))
        .route("/api/v1/sales/{id}", get(get_sale).put(update_draft))
        .route("/api/v1/sales/{id}/lines", post(add_line))
        .route("/api/v1/sales/{id}/quote", get(quote_sale))
        .route("/api/v1/sales/{id}/post", post(post_sale))
        .route("/api/v1/sales/{id}/supply", put(put_supply))
        .route(
            "/api/v1/sales/{id}/prescription-records",
            post(prepare_prescription_records),
        )
        .route(
            "/api/v1/sale-lines/{id}/prescription",
            put(put_line_prescription),
        )
        .route(
            "/api/v1/sale-lines/{id}",
            delete(remove_line).put(update_line),
        )
        .route(
            "/api/v1/packs/{id}/sellable-batches",
            get(list_sellable_batches),
        )
}

/// Selling is counter work: a cashier and a pharmacist may do it, unlike a purchase.
async fn require_counter(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, SaleError> {
    auth::validate_mutation_request(headers)?;
    Ok(auth::require_authenticated_actor(&state.pool, headers).await?)
}

async fn require_reader(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, SaleError> {
    Ok(auth::require_authenticated_actor(&state.pool, headers).await?)
}

/// The Store is resolved from the installation, never accepted from the browser.
async fn current_store(pool: &SqlitePool) -> Result<String, SaleError> {
    sqlx::query_scalar("SELECT store_id FROM store_identity LIMIT 1")
        .fetch_optional(pool)
        .await
        .map_err(map_database_error)?
        .ok_or(SaleError::StoreTaxIncomplete)
}

// ---------------------------------------------------------------------------------------------
// Draft handlers
// ---------------------------------------------------------------------------------------------

async fn list_sales(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<SaleHeaderResponse>>, SaleError> {
    require_reader(&state, &headers).await?;
    let status = query.status.unwrap_or_else(|| "all".to_owned());
    if !matches!(status.as_str(), "draft" | "posted" | "all") {
        return Err(validation_of("status", "must be draft, posted, or all"));
    }
    let status_filter = (status != "all").then_some(status);
    let rows = sqlx::query_as::<_, SaleHeaderResponse>(&format!(
        "SELECT {HEADER_COLUMNS} FROM sale_documents \
         WHERE (?1 IS NULL OR status=?1) AND (?2 IS NULL OR customer_party_id=?2) \
         ORDER BY business_date DESC, created_at_utc DESC LIMIT 200"
    ))
    .bind(&status_filter)
    .bind(&query.customer_party_id)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(rows))
}

async fn get_sale(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<SaleDetailResponse>, SaleError> {
    require_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    Ok(Json(fetch_detail(&state.pool, &id).await?))
}

async fn create_draft(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<DraftHeaderRequest>,
) -> Result<(StatusCode, Json<SaleDetailResponse>), SaleError> {
    let actor = require_counter(&state, &headers).await?;
    let header = prepare_header(&state.pool, &request).await?;
    let store_id = current_store(&state.pool).await?;
    let id = Uuid::now_v7().to_string();

    let mut transaction = state.pool.begin().await.map_err(|_| SaleError::Internal)?;
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "INSERT INTO sale_documents (id,store_id,customer_party_id,customer_name_text,\
         business_date,created_by_user_id,created_at_utc,updated_at_utc,\
         recipient_particulars_requested,recipient_address_line1,recipient_address_line2,\
         recipient_city,recipient_postal_code,recipient_state_id,delivery_same_as_recipient,\
         delivery_address_line1,delivery_address_line2,delivery_city,delivery_postal_code,\
         delivery_state_id) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(&store_id)
    .bind(&header.customer_party_id)
    .bind(&header.customer_name_text)
    .bind(&header.business_date)
    .bind(&actor.id)
    .bind(&now)
    .bind(&now)
    .bind(header.recipient_particulars_requested)
    .bind(&header.recipient_address.line1)
    .bind(&header.recipient_address.line2)
    .bind(&header.recipient_address.city)
    .bind(&header.recipient_address.postal_code)
    .bind(&header.recipient_address.state_id)
    .bind(header.delivery_same_as_recipient)
    .bind(&header.delivery_address.line1)
    .bind(&header.delivery_address.line2)
    .bind(&header.delivery_address.city)
    .bind(&header.delivery_address.postal_code)
    .bind(&header.delivery_address.state_id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    audit(
        &mut transaction,
        &id,
        1,
        "created",
        &json!({ "businessDate": header.business_date }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_detail(&state.pool, &id).await?),
    ))
}

async fn update_draft(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<UpdateDraftRequest>,
) -> Result<Json<SaleDetailResponse>, SaleError> {
    let actor = require_counter(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let header = prepare_header(&state.pool, &request.header).await?;

    let mut transaction = state.pool.begin().await.map_err(|_| SaleError::Internal)?;
    let current = draft_state(&mut transaction, &id).await?;
    require_revision(&current, request.expected_revision)?;
    ensure_no_live_record(&mut transaction, &id).await?;
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "UPDATE sale_documents SET customer_party_id=?,customer_name_text=?,business_date=?,\
         recipient_particulars_requested=?,recipient_address_line1=?,recipient_address_line2=?,\
         recipient_city=?,recipient_postal_code=?,recipient_state_id=?,\
         delivery_same_as_recipient=?,delivery_address_line1=?,delivery_address_line2=?,\
         delivery_city=?,delivery_postal_code=?,delivery_state_id=?,\
         revision=?,updated_at_utc=? WHERE id=? AND revision=? AND status='draft'",
    )
    .bind(&header.customer_party_id)
    .bind(&header.customer_name_text)
    .bind(&header.business_date)
    .bind(header.recipient_particulars_requested)
    .bind(&header.recipient_address.line1)
    .bind(&header.recipient_address.line2)
    .bind(&header.recipient_address.city)
    .bind(&header.recipient_address.postal_code)
    .bind(&header.recipient_address.state_id)
    .bind(header.delivery_same_as_recipient)
    .bind(&header.delivery_address.line1)
    .bind(&header.delivery_address.line2)
    .bind(&header.delivery_address.city)
    .bind(&header.delivery_address.postal_code)
    .bind(&header.delivery_address.state_id)
    .bind(current.0 + 1)
    .bind(&now)
    .bind(&id)
    .bind(current.0)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    audit(
        &mut transaction,
        &id,
        current.0 + 1,
        "updated",
        &json!({ "businessDate": header.business_date }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_detail(&state.pool, &id).await?))
}

struct PreparedHeader {
    customer_party_id: Option<String>,
    customer_name_text: Option<String>,
    business_date: String,
    recipient_particulars_requested: bool,
    recipient_address: PreparedAddress,
    delivery_same_as_recipient: bool,
    delivery_address: PreparedAddress,
}

/// A counter-typed address after validation. All-`None` is "nothing typed".
#[derive(Debug, Default)]
struct PreparedAddress {
    line1: Option<String>,
    line2: Option<String>,
    city: Option<String>,
    postal_code: Option<String>,
    state_id: Option<String>,
}

impl PreparedAddress {
    fn is_empty(&self) -> bool {
        self.line1.is_none()
            && self.line2.is_none()
            && self.city.is_none()
            && self.postal_code.is_none()
            && self.state_id.is_none()
    }
}

/// Bounds and formats exactly as a Party address is held to, so an address that fits one fits the
/// other. The State must be a known, active State: an invented id is refused here rather than
/// surfacing later as a foreign-key failure.
async fn prepare_address(
    pool: &SqlitePool,
    request: Option<&AddressRequest>,
    prefix: &str,
) -> Result<PreparedAddress, SaleError> {
    let Some(request) = request else {
        return Ok(PreparedAddress::default());
    };
    let field = |name: &str| format!("{prefix}.{name}");
    let postal_code = match request
        .postal_code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => {
            let normalized = value.to_ascii_uppercase();
            if !(3..=16).contains(&normalized.len())
                || !normalized
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b" -".contains(&byte))
            {
                return Err(validation_of(
                    &field("postalCode"),
                    "may contain only letters, digits, spaces, and hyphens",
                ));
            }
            Some(normalized)
        }
        None => None,
    };
    let state_id = match request
        .state_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => {
            validate_uuid_v7(value, &field("stateId")).map_err(validation_issue)?;
            let known: Option<i64> = sqlx::query_scalar(
                "SELECT 1 FROM state_codes WHERE id=? AND status='active' LIMIT 1",
            )
            .bind(value)
            .fetch_optional(pool)
            .await
            .map_err(map_database_error)?;
            if known.is_none() {
                return Err(validation_of(&field("stateId"), "is not a known State"));
            }
            Some(value.to_owned())
        }
        None => None,
    };
    Ok(PreparedAddress {
        line1: optional_text(request.line1.as_deref(), &field("line1"), 200)
            .map_err(validation_issue)?,
        line2: optional_text(request.line2.as_deref(), &field("line2"), 200)
            .map_err(validation_issue)?,
        city: optional_text(request.city.as_deref(), &field("city"), 100)
            .map_err(validation_issue)?,
        postal_code,
        state_id,
    })
}

/// Validates the only header facts a browser may supply.
///
/// A NULL customer is a walk-in, which is a real and common case; a non-NULL one must be an active
/// party holding an active customer role, mirroring supplier eligibility on a purchase.
async fn prepare_header(
    pool: &SqlitePool,
    request: &DraftHeaderRequest,
) -> Result<PreparedHeader, SaleError> {
    let business_date = validate_date(Some(&request.business_date), "businessDate")
        .map_err(validation_issue)?
        .ok_or_else(|| validation_of("businessDate", "is required"))?;
    let customer_name_text = optional_text(
        request.customer_name_text.as_deref(),
        "customerNameText",
        200,
    )
    .map_err(validation_issue)?;
    let customer_party_id = match request.customer_party_id.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => {
            validate_uuid_v7(value, "customerPartyId").map_err(validation_issue)?;
            let eligible: Option<i64> = sqlx::query_scalar(
                "SELECT 1 FROM parties party \
                 JOIN party_roles role ON role.party_id = party.id \
                 WHERE party.id=? AND party.status='active' \
                   AND role.role='customer' AND role.status='active' LIMIT 1",
            )
            .bind(value)
            .fetch_optional(pool)
            .await
            .map_err(map_database_error)?;
            if eligible.is_none() {
                return Err(SaleError::CustomerNotEligible);
            }
            Some(value.to_owned())
        }
        _ => None,
    };
    let recipient_address =
        prepare_address(pool, request.recipient_address.as_ref(), "recipientAddress").await?;
    // A named customer's address is theirs to keep in Parties and is read from there at posting.
    // Typing a second one beside it would be a customer master by the back door.
    if customer_party_id.is_some() && !recipient_address.is_empty() {
        return Err(validation_of(
            "recipientAddress",
            "comes from the customer's record in Parties",
        ));
    }
    let delivery_address =
        prepare_address(pool, request.delivery_address.as_ref(), "deliveryAddress").await?;
    if request.delivery_same_as_recipient && !delivery_address.is_empty() {
        return Err(validation_of(
            "deliveryAddress",
            "is recorded only when delivery is to a different address",
        ));
    }
    Ok(PreparedHeader {
        customer_party_id,
        customer_name_text,
        business_date,
        recipient_particulars_requested: request.recipient_particulars_requested,
        recipient_address,
        delivery_same_as_recipient: request.delivery_same_as_recipient,
        delivery_address,
    })
}

// ---------------------------------------------------------------------------------------------
// Line handlers
// ---------------------------------------------------------------------------------------------

/// A line prepared from a request: everything the browser chose, plus the atoms the server derived.
struct PreparedLine {
    product_id: String,
    product_pack_id: String,
    batch_id: String,
    quantity: SaleQuantity,
    selling_rate_paise: i64,
    /// Quantity x rate, both in the line's own basis. Held on the draft so the POS can show an
    /// authoritative running subtotal without a browser ever computing money - and it is pre-tax,
    /// because the rate in force is resolved at posting and nowhere else.
    taxable_value_paise: i64,
}

#[derive(Debug, FromRow)]
struct SellablePack {
    product_id: String,
    base_quantity_atoms: i64,
    /// The Product's own precision, which decides what "one base unit" is in atoms. It lives on the
    /// Product rather than the Pack, so it is joined in here rather than assumed to be zero.
    quantity_scale: i64,
    pack_status: String,
    sale_enabled: Option<i64>,
    fractional_sale_allowed: Option<i64>,
    minimum_sale_increment_atoms: Option<i64>,
}

#[derive(Debug, FromRow)]
struct BatchFacts {
    product_pack_id: String,
    status: String,
    expires_on: Option<String>,
}

/// Validates a line against the Pack's own sale policy and the Batch's state.
///
/// The browser never supplies atoms: the quantity it sends is interpreted in the basis it names, and
/// the atoms come from the frozen Pack model.
async fn prepare_line(
    pool: &SqlitePool,
    store_id: &str,
    business_date: &str,
    request: &LineRequest,
) -> Result<PreparedLine, SaleError> {
    validate_uuid_v7(&request.product_id, "productId").map_err(validation_issue)?;
    validate_uuid_v7(&request.product_pack_id, "productPackId").map_err(validation_issue)?;
    validate_uuid_v7(&request.batch_id, "batchId").map_err(validation_issue)?;
    let basis = QuantityBasis::parse(request.quantity_basis.trim())
        .ok_or_else(|| validation_of("quantityBasis", "must be pack or base_unit"))?;

    let pack = sqlx::query_as::<_, SellablePack>(
        "SELECT pack.product_id,pack.base_quantity_atoms,product.quantity_scale,\
         pack.status AS pack_status,policy.sale_enabled,policy.fractional_sale_allowed,\
         policy.minimum_sale_increment_atoms \
         FROM product_packs pack \
         JOIN products product ON product.id = pack.product_id \
         LEFT JOIN store_pack_policies policy \
           ON policy.pack_id = pack.id AND policy.store_id = ? AND policy.status = 'active' \
         WHERE pack.id = ?",
    )
    .bind(store_id)
    .bind(&request.product_pack_id)
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?
    .ok_or(SaleError::NotFound)?;

    if pack.product_id != request.product_id {
        return Err(SaleError::PackMismatch);
    }
    if pack.pack_status != "active" {
        return Err(SaleError::PackNotSellable);
    }
    // A pack with no store policy has not been enabled for sale here, which is different from being
    // enabled and disabled — but both refuse, and both say the same honest thing to the operator.
    if pack.sale_enabled != Some(1) {
        return Err(SaleError::PackNotSellable);
    }

    let batch = sqlx::query_as::<_, BatchFacts>(
        "SELECT product_pack_id,status,expires_on FROM product_batches WHERE id=?",
    )
    .bind(&request.batch_id)
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?
    .ok_or(SaleError::NotFound)?;
    if batch.product_pack_id != request.product_pack_id {
        return Err(SaleError::BatchPackMismatch);
    }
    if batch.status != "active" {
        return Err(SaleError::BatchPackMismatch);
    }
    // Selling an expired drug is prohibited under the Drugs and Cosmetics Act, 1940. Applying the
    // block to every batch-bearing product, and refusing rather than warning, is our own stricter
    // policy — both are stated in the blueprint rather than conflated here.
    if batch
        .expires_on
        .as_deref()
        .is_some_and(|expiry| expiry < business_date)
    {
        return Err(SaleError::BatchExpired);
    }

    let quantity = sales::resolve_quantity(
        basis,
        request.quantity,
        pack.base_quantity_atoms,
        pack.quantity_scale,
        pack.fractional_sale_allowed == Some(1),
        pack.minimum_sale_increment_atoms.unwrap_or(1),
    )?;

    if !(0..=sales::MAX_SALE_RATE_PAISE).contains(&request.selling_rate_paise) {
        return Err(validation_of(
            "sellingRatePaise",
            "is not a usable amount in paise",
        ));
    }

    let taxable_value_paise = sales::taxable_value_paise(&quantity, request.selling_rate_paise)?;

    Ok(PreparedLine {
        product_id: request.product_id.clone(),
        product_pack_id: request.product_pack_id.clone(),
        batch_id: request.batch_id.clone(),
        quantity,
        selling_rate_paise: request.selling_rate_paise,
        taxable_value_paise,
    })
}

async fn add_line(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LineRequest>,
) -> Result<(StatusCode, Json<SaleDetailResponse>), SaleError> {
    let actor = require_counter(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let store_id = current_store(&state.pool).await?;
    let business_date = document_business_date(&state.pool, &id).await?;
    let prepared = prepare_line(&state.pool, &store_id, &business_date, &request).await?;

    let mut transaction = state.pool.begin().await.map_err(|_| SaleError::Internal)?;
    let current = draft_state(&mut transaction, &id).await?;
    require_revision(&current, request.expected_revision)?;
    ensure_no_live_record(&mut transaction, &id).await?;
    let next_line: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(line_number),0)+1 FROM sale_lines WHERE sale_document_id=?",
    )
    .bind(&id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    let line_id = Uuid::now_v7().to_string();
    let now = database_now(&mut transaction).await?;
    insert_line(&mut transaction, &line_id, &id, next_line, &prepared, &now).await?;
    bump_draft(&mut transaction, &id, current.0, &now).await?;
    audit(
        &mut transaction,
        &id,
        current.0 + 1,
        "updated",
        &json!({ "addedLine": line_id, "lineNumber": next_line }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_detail(&state.pool, &id).await?),
    ))
}

async fn update_line(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(line_id): Path<String>,
    Json(request): Json<LineRequest>,
) -> Result<Json<SaleDetailResponse>, SaleError> {
    let actor = require_counter(&state, &headers).await?;
    validate_uuid_v7(&line_id, "id").map_err(validation_issue)?;
    let document_id = line_document(&state.pool, &line_id).await?;
    let store_id = current_store(&state.pool).await?;
    let business_date = document_business_date(&state.pool, &document_id).await?;
    let prepared = prepare_line(&state.pool, &store_id, &business_date, &request).await?;

    let mut transaction = state.pool.begin().await.map_err(|_| SaleError::Internal)?;
    let current = draft_state(&mut transaction, &document_id).await?;
    require_revision(&current, request.expected_revision)?;
    ensure_no_live_record(&mut transaction, &document_id).await?;
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "UPDATE sale_lines SET product_id=?,product_pack_id=?,batch_id=?,quantity_basis=?,\
         quantity_packs=?,quantity_atoms=?,selling_rate_paise=?,taxable_value_paise=?,         updated_at_utc=? WHERE id=?",
    )
    .bind(&prepared.product_id)
    .bind(&prepared.product_pack_id)
    .bind(&prepared.batch_id)
    .bind(prepared.quantity.basis.as_str())
    .bind(prepared.quantity.quantity_packs)
    .bind(prepared.quantity.quantity_atoms)
    .bind(prepared.selling_rate_paise)
    .bind(prepared.taxable_value_paise)
    .bind(&now)
    .bind(&line_id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    bump_draft(&mut transaction, &document_id, current.0, &now).await?;
    audit(
        &mut transaction,
        &document_id,
        current.0 + 1,
        "updated",
        &json!({ "updatedLine": line_id }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_detail(&state.pool, &document_id).await?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveLineRequest {
    expected_revision: i64,
}

async fn remove_line(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(line_id): Path<String>,
    Json(request): Json<RemoveLineRequest>,
) -> Result<Json<SaleDetailResponse>, SaleError> {
    let actor = require_counter(&state, &headers).await?;
    validate_uuid_v7(&line_id, "id").map_err(validation_issue)?;
    let document_id = line_document(&state.pool, &line_id).await?;

    let mut transaction = state.pool.begin().await.map_err(|_| SaleError::Internal)?;
    let current = draft_state(&mut transaction, &document_id).await?;
    require_revision(&current, request.expected_revision)?;
    ensure_no_live_record(&mut transaction, &document_id).await?;
    sqlx::query("DELETE FROM sale_lines WHERE id=?")
        .bind(&line_id)
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
    let now = database_now(&mut transaction).await?;
    bump_draft(&mut transaction, &document_id, current.0, &now).await?;
    audit(
        &mut transaction,
        &document_id,
        current.0 + 1,
        "updated",
        &json!({ "removedLine": line_id }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_detail(&state.pool, &document_id).await?))
}

// ---------------------------------------------------------------------------------------------
// Phase 1M-B — the draft's prescription facts
// ---------------------------------------------------------------------------------------------

/// Linking a prescription and confirming a supply is dispensing work. `pharmacist` here is this
/// software's permission to do that work at the counter; whether a registered pharmacist
/// supervised the supply is a separate fact, taken from the professional record chosen below.
async fn require_dispenser(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, SaleError> {
    auth::validate_mutation_request(headers)?;
    let actor = auth::require_authenticated_actor(&state.pool, headers).await?;
    if actor.role != "owner_admin" && actor.role != "pharmacist" {
        return Err(AuthError::AuthorizationDenied.into());
    }
    Ok(actor)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinePrescriptionRequest {
    expected_revision: i64,
    prescription_item_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SupplyRequest {
    expected_revision: i64,
    supervising_professional_id: Option<String>,
    prescription_endorsement_confirmed: bool,
    /// Rule 65(3)(1), first proviso: the dispensing person attests the drug was not compounded here
    /// and left from or in its original container. Needed only where the Store elected the memo book.
    #[serde(default)]
    prescription_original_container_confirmed: bool,
}

/// Points one draft line at the prescription item it is dispensed against, or clears it.
///
/// The item must be this pharmacy's, still active, and name this line's product: rule 65(11A)
/// forbids another preparation in lieu, so a mismatched link is refused here as well as at posting.
/// Any change clears the endorsement confirmation, because what is being dispensed has changed.
async fn put_line_prescription(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(line_id): Path<String>,
    Json(request): Json<LinePrescriptionRequest>,
) -> Result<Json<SaleDetailResponse>, SaleError> {
    let actor = require_dispenser(&state, &headers).await?;
    validate_uuid_v7(&line_id, "id").map_err(validation_issue)?;
    let item_id = match request.prescription_item_id.as_deref() {
        Some(value) => Some(
            validate_uuid_v7(value, "prescriptionItemId")
                .map_err(validation_issue)?
                .to_owned(),
        ),
        None => None,
    };
    let document_id = line_document(&state.pool, &line_id).await?;

    let mut transaction = state.pool.begin().await.map_err(|_| SaleError::Internal)?;
    let current = draft_state(&mut transaction, &document_id).await?;
    require_revision(&current, request.expected_revision)?;
    ensure_no_live_record(&mut transaction, &document_id).await?;

    if let Some(item_id) = item_id.as_deref() {
        let matched: Option<(String, String, String)> = sqlx::query_as(
            "SELECT prescription.store_id,prescription.status,item.product_id \
             FROM prescription_items item \
             JOIN prescriptions prescription ON prescription.id=item.prescription_id \
             WHERE item.id=?",
        )
        .bind(item_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        let (line_product, store): (String, String) = sqlx::query_as(
            "SELECT line.product_id,document.store_id FROM sale_lines line \
             JOIN sale_documents document ON document.id=line.sale_document_id WHERE line.id=?",
        )
        .bind(&line_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        let code = match matched {
            None => Some("prescription_not_found"),
            Some((item_store, _, _)) if item_store != store => Some("prescription_not_found"),
            Some((_, status, _)) if status != "active" => Some("prescription_archived"),
            Some((_, _, product)) if product != line_product => {
                Some("prescription_substitution_not_permitted")
            }
            _ => None,
        };
        if let Some(code) = code {
            return Err(SaleError::PrescriptionRequirementsIncomplete(vec![
                PrescriptionIssue {
                    line_number: None,
                    code,
                },
            ]));
        }
    }

    sqlx::query("UPDATE sale_lines SET prescription_item_id=?,updated_at_utc=? WHERE id=?")
        .bind(&item_id)
        .bind(database_now(&mut transaction).await?)
        .bind(&line_id)
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
    sqlx::query(
        "UPDATE sale_documents SET prescription_endorsement_confirmed=0,\
         prescription_endorsement_confirmed_by_user_id=NULL,\
         prescription_endorsement_confirmed_at_utc=NULL,\
         prescription_original_container_confirmed=0 WHERE id=?",
    )
    .bind(&document_id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    let now = database_now(&mut transaction).await?;
    bump_draft(&mut transaction, &document_id, current.0, &now).await?;
    audit(
        &mut transaction,
        &document_id,
        current.0 + 1,
        "updated",
        &json!({ "saleLineId": line_id, "prescriptionItemId": item_id }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_detail(&state.pool, &document_id).await?))
}

/// Records who supervises this supply and whether the endorsement was written on the paper.
///
/// The confirmation is a person's statement of a physical act; this records who made it and when.
/// Nothing here writes on a prescription, and nothing here claims to have.
async fn put_supply(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<SupplyRequest>,
) -> Result<Json<SaleDetailResponse>, SaleError> {
    let actor = require_dispenser(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let professional = match request.supervising_professional_id.as_deref() {
        Some(value) => Some(
            validate_uuid_v7(value, "supervisingProfessionalId")
                .map_err(validation_issue)?
                .to_owned(),
        ),
        None => None,
    };

    let mut transaction = state.pool.begin().await.map_err(|_| SaleError::Internal)?;
    let current = draft_state(&mut transaction, &id).await?;
    require_revision(&current, request.expected_revision)?;
    ensure_no_live_record(&mut transaction, &id).await?;
    if let Some(professional) = professional.as_deref() {
        let exists: Option<String> = sqlx::query_scalar(
            "SELECT professional.id FROM store_professionals professional \
             JOIN sale_documents document ON document.store_id=professional.store_id \
             WHERE professional.id=? AND document.id=?",
        )
        .bind(professional)
        .bind(&id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        if exists.is_none() {
            return Err(SaleError::PrescriptionRequirementsIncomplete(vec![
                PrescriptionIssue {
                    line_number: None,
                    code: "supervising_pharmacist_invalid",
                },
            ]));
        }
    }
    let now = database_now(&mut transaction).await?;
    let confirmed = request.prescription_endorsement_confirmed;
    let original_container = request.prescription_original_container_confirmed;
    sqlx::query(
        "UPDATE sale_documents SET supervising_professional_id=?,\
         prescription_endorsement_confirmed=?,prescription_endorsement_confirmed_by_user_id=?,\
         prescription_endorsement_confirmed_at_utc=?,prescription_original_container_confirmed=? \
         WHERE id=?",
    )
    .bind(&professional)
    .bind(i64::from(confirmed))
    .bind(confirmed.then(|| actor.id.clone()))
    .bind(confirmed.then(|| now.clone()))
    .bind(i64::from(original_container))
    .bind(&id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    bump_draft(&mut transaction, &id, current.0, &now).await?;
    audit(
        &mut transaction,
        &id,
        current.0 + 1,
        "updated",
        &json!({
            "supervisingProfessionalId": professional,
            "prescriptionEndorsementConfirmed": confirmed,
            "prescriptionOriginalContainerConfirmed": original_container,
        }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_detail(&state.pool, &id).await?))
}

// ---------------------------------------------------------------------------------------------
// Sellable batches, for the POS chooser
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct SellableBatchResponse {
    id: String,
    product_pack_id: String,
    batch_number: String,
    expires_on: Option<String>,
    mrp_paise: Option<i64>,
    available_atoms: i64,
    expired: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SellableBatchQuery {
    as_of: Option<String>,
}

/// What the counter needs to choose a lot: its number, its expiry, what is left, and the printed
/// price. Ordered soonest-expiry-first as a presentation aid only — nothing is auto-selected.
async fn list_sellable_batches(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(pack_id): Path<String>,
    Query(query): Query<SellableBatchQuery>,
) -> Result<Json<Vec<SellableBatchResponse>>, SaleError> {
    require_reader(&state, &headers).await?;
    validate_uuid_v7(&pack_id, "id").map_err(validation_issue)?;
    let store_id = current_store(&state.pool).await?;
    let as_of = match query
        .as_of
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(value) => validate_date(Some(value), "asOf")
            .map_err(validation_issue)?
            .ok_or_else(|| validation_of("asOf", "is required"))?,
        None => sqlx::query_scalar("SELECT strftime('%Y-%m-%d','now')")
            .fetch_one(&state.pool)
            .await
            .map_err(map_database_error)?,
    };
    let rows = sqlx::query_as::<_, SellableBatchResponse>(
        "SELECT batch.id,batch.product_pack_id,batch.batch_number,batch.expires_on,batch.mrp_paise,\
         COALESCE((SELECT SUM(quantity_delta_atoms) FROM inventory_movements \
                   WHERE store_id=?1 AND product_pack_id=batch.product_pack_id \
                     AND batch_id=batch.id AND stock_status='sellable'),0) AS available_atoms,\
         (batch.expires_on IS NOT NULL AND batch.expires_on < ?2) AS expired \
         FROM product_batches batch \
         WHERE batch.product_pack_id=?3 AND batch.status='active' \
         ORDER BY batch.expires_on IS NULL, batch.expires_on, batch.batch_number",
    )
    .bind(&store_id)
    .bind(&as_of)
    .bind(&pack_id)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(rows))
}

// ---------------------------------------------------------------------------------------------
// Quote
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuoteLineResponse {
    id: String,
    line_number: i64,
    taxable_value_paise: i64,
    cgst_paise: i64,
    sgst_paise: i64,
    igst_paise: i64,
    cess_paise: i64,
    line_total_paise: i64,
    /// Phase 1M-A. What the Drugs Rules say about this line on this Sale's business date, so the
    /// counter learns before tendering what posting would refuse afterwards. The quote explains;
    /// posting decides, and re-resolves for itself.
    regulatory_gate: &'static str,
    regulatory_gate_scheme: Option<&'static str>,
    /// Phase 1M-B: this line's prescription status. References and quantities only — never the
    /// patient's particulars.
    prescription: LinePrescriptionSummary,
}

/// Phase 1M-B: what the whole supply still needs, for a basket with a Schedule H line.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SupplySummary {
    supervision_required: bool,
    supervising_professional_id: Option<String>,
    endorsement_confirmed: bool,
    /// Phase 1M-B: the book the rule 65(3)(2) election in force names for this supply, or `null`
    /// where none is recorded (posting refuses).
    record_method: Option<&'static str>,
    original_container_confirmed: bool,
    /// Unmet requirements of the whole supply, as codes.
    issues: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuoteResponse {
    sale_document_id: String,
    revision: i64,
    tax_treatment: &'static str,
    /// The seller status this quote was computed under: `registered` or `unregistered`. An unknown
    /// status never produces a quote.
    seller_gst_registration_status: &'static str,
    taxable_value_paise: i64,
    cgst_paise: i64,
    sgst_paise: i64,
    igst_paise: i64,
    cess_paise: i64,
    grand_total_paise: i64,
    lines: Vec<QuoteLineResponse>,
    supply: SupplySummary,
    recipient_particulars: RecipientRequirementResponse,
    /// Notification No. 14/2020-CT as it stands for this bill: `null` when it cannot reach the
    /// document (unregistered seller, registered customer, or no taxable line); otherwise the
    /// Store's answer — `unknown` (posting will be refused), `not_required` or `required` (a
    /// card or UPI payment needs its transaction reference). Informational: posting decides.
    dynamic_qr_applicability: Option<&'static str>,
    /// A registered customer's bill mixing taxable and untaxed items, which posting refuses.
    registered_recipient_mixed_supply: bool,
}

/// What Rule 46 will ask of this Sale's recipient if it is posted as it stands.
///
/// Worked out by the same `recipient::requirement` posting uses, from the same reads, so the counter
/// is told exactly what posting would refuse — and told it while the customer is still there.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecipientRequirementResponse {
    required: bool,
    reasons: Vec<&'static str>,
    missing: Vec<ErrorIssue>,
    threshold_paise: i64,
    /// The value the Rule 46(e) threshold was judged on: taxable lines only.
    taxable_supply_value_paise: i64,
}

impl RecipientRequirementResponse {
    fn from(requirement: recipient::Requirement, taxable_supply_value_paise: i64) -> Self {
        Self {
            required: requirement.required(),
            reasons: requirement
                .reasons
                .iter()
                .map(|reason| reason.as_str())
                .collect(),
            missing: requirement
                .missing
                .iter()
                .map(|fact| ErrorIssue {
                    field: fact.field().to_owned(),
                    message: fact.message().to_owned(),
                })
                .collect(),
            threshold_paise: recipient::RECIPIENT_PARTICULARS_THRESHOLD_PAISE,
            taxable_supply_value_paise,
        }
    }
}

/// The value of the TAXABLE supply on a Sale: `taxable_value_paise` summed over lines whose treatment
/// is `taxable`, and nothing else.
///
/// `compute_line` records the whole value of an exempt, nil-rated or non-GST line in its own
/// `taxable_value_paise` as well, so the header total of that column is the value of the basket.
/// Rule 46(e) asks about "the value of the taxable supply", so those lines are excluded here.
fn taxable_supply_value<'a>(
    lines: impl Iterator<Item = (Option<&'a str>, i64)>,
) -> Result<i64, SaleError> {
    lines
        .filter(|(kind, _)| *kind == Some("taxable"))
        .try_fold(0_i64, |total, (_, value)| total.checked_add(value))
        .ok_or(SaleError::ArithmeticOverflow)
}

/// What this bill would come to, so the counter can say the amount out loud before taking money.
///
/// **This is a quote, not a commitment.** It resolves nothing and freezes nothing: no number is
/// allocated, no snapshot is written, no stock moves. Posting recomputes every figure from scratch
/// under its own write lock and is the only authority for what was charged.
///
/// It reuses `resolve_and_compute` — the very function posting uses — rather than reimplementing the
/// tax path, so a quote and the invoice that follows it can never disagree about the arithmetic.
/// A line that posting would refuse is refused here too, with the same typed code, so the operator
/// learns about an above-MRP price while the customer is still at the counter rather than after the
/// cash is in the drawer.
async fn quote_sale(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<QuoteResponse>, SaleError> {
    require_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let header = sqlx::query_as::<_, PostingHeader>(&format!(
        "SELECT {POSTING_HEADER_COLUMNS} FROM sale_documents WHERE id=?"
    ))
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    .map_err(map_database_error)?
    .ok_or(SaleError::NotFound)?;

    let mut connection = state
        .pool
        .acquire()
        .await
        .map_err(|_| SaleError::Internal)?;
    let lines = load_lines(&mut connection, &id).await?;
    // The place of supply is the store, for the same reason posting says so: the goods are handed
    // over at the counter.
    let treatment = TaxTreatment::IntraState;
    // The same seller-status authority posting uses, so a quote never shows GST the posting would
    // not charge — or charge GST the quote did not show.
    let seller = seller_gst(&mut connection, &header.store_id).await?;
    let mut quoted = Vec::with_capacity(lines.len());
    let mut amounts = Vec::with_capacity(lines.len());
    let mut taxable_supply = Vec::with_capacity(lines.len());
    let regulatory = resolve_regulatory_for_lines(
        &mut connection,
        &header.store_id,
        &lines,
        &header.business_date,
    )
    .await?;
    // The same judgement posting will make, read-only, so the counter sees exactly what is missing.
    let plan = plan_dispensings(
        &mut connection,
        &header.store_id,
        &id,
        &lines,
        &regulatory,
        &header.business_date,
    )
    .await?;
    let (supply_professional, supply_endorsed): (Option<String>, i64) = sqlx::query_as(
        "SELECT supervising_professional_id,prescription_endorsement_confirmed \
         FROM sale_documents WHERE id=?",
    )
    .bind(&id)
    .fetch_one(&mut *connection)
    .await
    .map_err(map_database_error)?;
    let supply_endorsed = supply_endorsed == 1;
    for (index, line) in lines.iter().enumerate() {
        let entry = resolve_and_compute(
            &mut connection,
            line,
            &header.business_date,
            treatment,
            seller,
        )
        .await?;
        quoted.push(QuoteLineResponse {
            id: line.id.clone(),
            line_number: index as i64 + 1,
            taxable_value_paise: entry.amounts.taxable_value_paise,
            cgst_paise: entry.amounts.cgst_paise,
            sgst_paise: entry.amounts.sgst_paise,
            igst_paise: entry.amounts.igst_paise,
            cess_paise: entry.amounts.cess_paise,
            line_total_paise: entry.amounts.line_total_paise,
            // The same combined gate posting applies, so the counter is never told "clear" for a
            // line posting would refuse.
            regulatory_gate: match regulatory[index].gate {
                regulatory::SaleGate::Clear => "clear",
                regulatory::SaleGate::PrescriptionRequired => "prescription_required",
                regulatory::SaleGate::Unresolved | regulatory::SaleGate::StateUnresolved { .. } => {
                    "unresolved"
                }
                regulatory::SaleGate::WorkflowUnavailable { .. }
                | regulatory::SaleGate::UnsupportedIntersection { .. }
                | regulatory::SaleGate::StateWorkflowUnavailable { .. } => "workflow_unavailable",
            },
            regulatory_gate_scheme: match regulatory[index].gate {
                regulatory::SaleGate::WorkflowUnavailable { scheme }
                | regulatory::SaleGate::StateWorkflowUnavailable { scheme }
                | regulatory::SaleGate::StateUnresolved { scheme } => Some(scheme),
                regulatory::SaleGate::UnsupportedIntersection { .. } => Some("ndps_purview"),
                regulatory::SaleGate::PrescriptionRequired if regulatory[index].h1_any_day => {
                    Some("schedule_h1")
                }
                regulatory::SaleGate::PrescriptionRequired => Some("schedule_h"),
                _ => None,
            },
            prescription: plan.summaries[index].clone(),
        });
        taxable_supply.push((
            entry.tax_treatment_kind.clone(),
            entry.amounts.taxable_value_paise,
        ));
        amounts.push(entry.amounts);
    }
    let totals = money::sum_lines(&amounts)?;
    let taxable_supply_value_paise = taxable_supply_value(
        taxable_supply
            .iter()
            .map(|(kind, value)| (kind.as_deref(), *value)),
    )?;

    let customer = match header.customer_party_id.as_deref() {
        Some(party_id) => Some(load_customer(&mut connection, party_id).await?),
        None => None,
    };
    let source = load_recipient_source(
        &mut connection,
        &header,
        customer.as_ref(),
        taxable_supply_value_paise,
        seller,
    )
    .await?;
    let recipient_registered = customer
        .as_ref()
        .is_some_and(|party| party.gst_registration_status == "registered");
    let (has_taxable_line, has_untaxed_line) =
        treatment_mix(taxable_supply.iter().map(|(kind, _)| kind.as_deref()));
    let dynamic_qr_applicability = if invoice_compliance::dynamic_qr_in_scope(
        seller,
        recipient_registered,
        has_taxable_line,
    ) {
        let recorded: String = sqlx::query_scalar(
            "SELECT dynamic_qr_applicability FROM store_identity WHERE store_id=?",
        )
        .bind(&header.store_id)
        .fetch_one(&mut *connection)
        .await
        .map_err(map_database_error)?;
        Some(
            DynamicQrApplicability::parse(&recorded)
                .ok_or(SaleError::Internal)?
                .as_str(),
        )
    } else {
        None
    };

    Ok(Json(QuoteResponse {
        sale_document_id: id,
        revision: header.revision,
        tax_treatment: treatment.as_str(),
        seller_gst_registration_status: match seller {
            SellerGst::Registered => "registered",
            SellerGst::Unregistered => "unregistered",
        },
        taxable_value_paise: totals.taxable_value_paise,
        cgst_paise: totals.cgst_paise,
        sgst_paise: totals.sgst_paise,
        igst_paise: totals.igst_paise,
        cess_paise: totals.cess_paise,
        grand_total_paise: totals.line_total_paise,
        lines: quoted,
        supply: SupplySummary {
            supervision_required: plan.supervision_required,
            supervising_professional_id: supply_professional,
            endorsement_confirmed: supply_endorsed,
            record_method: plan.record.as_ref().map(|record| record.method),
            original_container_confirmed: plan.original_container_confirmed,
            issues: plan
                .issues
                .iter()
                .filter(|issue| issue.line_number.is_none())
                .map(|issue| issue.code)
                .collect(),
        },
        recipient_particulars: RecipientRequirementResponse::from(
            recipient::requirement(&source),
            taxable_supply_value_paise,
        ),
        dynamic_qr_applicability,
        registered_recipient_mixed_supply: invoice_compliance::registered_recipient_mixed_supply(
            seller,
            recipient_registered,
            has_taxable_line,
            has_untaxed_line,
        ),
    }))
}

// ---------------------------------------------------------------------------------------------
// Posting
// ---------------------------------------------------------------------------------------------

async fn post_sale(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<PostRequest>,
) -> Result<Json<SaleDetailResponse>, SaleError> {
    let actor = require_counter(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let idempotency_key =
        validate_uuid_v7(&request.idempotency_key, "idempotencyKey").map_err(validation_issue)?;
    let tenders = prepare_tenders(&request.tenders)?;

    let mut connection = state
        .pool
        .acquire()
        .await
        .map_err(|_| SaleError::Internal)?;
    // The frozen inventory concurrency model: take the write lock before reading any balance, so a
    // concurrent sale cannot interleave between the availability check and the movement.
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
    let outcome = post_within_transaction(
        &mut connection,
        &id,
        request.expected_revision,
        &idempotency_key,
        &tenders,
        &actor.id,
    )
    .await;
    match outcome {
        Ok(()) => {
            sqlx::query("COMMIT")
                .execute(&mut *connection)
                .await
                .map_err(map_database_error)?;
            drop(connection);
            Ok(Json(fetch_detail(&state.pool, &id).await?))
        }
        Err(error) => {
            // Any failure discards the whole posting: no partial stock, no partial snapshot, and
            // above all no consumed invoice number.
            let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
            drop(connection);
            if matches!(error, SaleError::AlreadyPostedReplay) {
                return Ok(Json(fetch_detail(&state.pool, &id).await?));
            }
            Err(error)
        }
    }
}

struct PreparedTender {
    method: String,
    amount_paise: i64,
    reference_text: Option<String>,
}

/// Phase 1H takes exactly one tender for exactly the invoice total. Part payment would be credit,
/// which this phase does not open, and split tender is a later validation change on the same table.
fn prepare_tenders(requests: &[TenderRequest]) -> Result<Vec<PreparedTender>, SaleError> {
    if requests.len() != 1 {
        return Err(SaleError::TenderMismatch);
    }
    let mut prepared = Vec::with_capacity(1);
    for tender in requests {
        let method = tender.method.trim().to_ascii_lowercase();
        if !matches!(method.as_str(), "cash" | "card" | "upi") {
            return Err(validation_of("method", "must be cash, card, or upi"));
        }
        if tender.amount_paise <= 0 {
            return Err(validation_of("amountPaise", "must be a positive amount"));
        }
        prepared.push(PreparedTender {
            method,
            amount_paise: tender.amount_paise,
            reference_text: optional_text(tender.reference_text.as_deref(), "referenceText", 120)
                .map_err(validation_issue)?,
        });
    }
    Ok(prepared)
}

/// The series every Phase 1H sale is numbered in. One series, because a second one is a business
/// decision nobody has taken yet, and inventing per-counter series now would fragment the numbering
/// of a store that has exactly one.
const SALE_SERIES_CODE: &str = "INV";

async fn post_within_transaction(
    connection: &mut PoolConnection<Sqlite>,
    id: &str,
    expected_revision: i64,
    idempotency_key: &str,
    tenders: &[PreparedTender],
    actor_id: &str,
) -> Result<(), SaleError> {
    let header = sqlx::query_as::<_, PostingHeader>(&format!(
        "SELECT {POSTING_HEADER_COLUMNS} FROM sale_documents WHERE id=?"
    ))
    .bind(id)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?;
    let header = header.ok_or(SaleError::NotFound)?;
    let status = header.status.clone();
    let revision = header.revision;
    let store_id = header.store_id.clone();
    let customer_party_id = header.customer_party_id.clone();
    let business_date = header.business_date.clone();
    let existing_key = header.posting_idempotency_key.clone();
    let existing_print = header.posting_fingerprint.clone();

    let lines = load_lines(connection, id).await?;
    let fingerprint = posting_fingerprint(
        id,
        customer_party_id.as_deref(),
        &business_date,
        &lines,
        tenders,
    );

    // A replay of the same key with the same facts returns the original invoice; the same key with
    // different facts is a conflict rather than a silent second sale.
    if status == "posted" {
        return match (existing_key.as_deref(), existing_print.as_deref()) {
            (Some(key), Some(print)) if key == idempotency_key && print == fingerprint => {
                Err(SaleError::AlreadyPostedReplay)
            }
            (Some(key), _) if key == idempotency_key => Err(SaleError::IdempotencyConflict),
            _ => Err(SaleError::NotDraft),
        };
    }
    if revision != expected_revision {
        return Err(SaleError::Revision {
            expected: expected_revision,
            current: revision,
        });
    }
    if lines.is_empty() {
        return Err(validation_of("lines", "a sale needs at least one line"));
    }

    // The Store's tax geography is re-read now, never taken from the draft or the browser.
    let store = sqlx::query_as::<_, StoreTaxSource>(
        "SELECT normalized_gstin,place_of_supply_state_id,gst_registration_status,\
         rule46s_declaration_applicability,einvoice_applicability,hsn_turnover_band,\
         hsn_turnover_financial_year,dynamic_qr_applicability FROM store_identity WHERE store_id=?",
    )
    .bind(&store_id)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?;
    let StoreTaxSource {
        normalized_gstin: store_gstin,
        place_of_supply_state_id: store_state_id,
        gst_registration_status: store_status,
        rule46s_declaration_applicability,
        einvoice_applicability,
        hsn_turnover_band,
        hsn_turnover_financial_year,
        dynamic_qr_applicability,
    } = store.ok_or(SaleError::NotFound)?;
    let store_state_id = store_state_id.ok_or(SaleError::StoreTaxIncomplete)?;
    let store_state_code = state_code(connection, &store_state_id).await?;
    // Read under the same write lock as everything else posting freezes: an owner changing the
    // registration mid-sale either commits before this read or waits for the lock.
    let seller_tax =
        SellerGst::from_status(&store_status).ok_or(SaleError::StoreGstStatusUnresolved)?;

    // The seller is resolved HERE, inside the same `BEGIN IMMEDIATE` transaction that read the
    // tax geography and will write the document. Every table the seller spans — identity, address,
    // licences — is read in one call against this connection, so the snapshot is one coherent
    // profile rather than a name from before an edit and an address from after it. An operator
    // saving Store Profile mid-sale either commits before this read or waits for the write lock;
    // there is no third outcome, and nothing here is taken from the browser.
    let seller_source = crate::api::store_profile::seller_profile_source(connection, &store_id)
        .await
        .map_err(map_database_error)?;
    let seller =
        store_profile::resolve(&seller_source).map_err(SaleError::StoreLegalProfileIncomplete)?;

    // The customer's facts are recorded so a B2B buyer has the document they need. They are
    // deliberately NOT used to decide the treatment: see below.
    let customer = match customer_party_id.as_deref() {
        Some(party_id) => Some(load_customer(connection, party_id).await?),
        None => None,
    };

    // The goods are handed over at the counter, so under IGST Act s.10(1)(c) the place of supply is
    // the Store and this supply is intra-State. That is a property of the transaction shape this
    // phase models, not a universal claim about retail sales — a delivered or dispatched sale has
    // different place-of-supply rules and is deliberately not representable here.
    let treatment = TaxTreatment::IntraState;

    let mut computed = Vec::with_capacity(lines.len());
    for line in &lines {
        computed.push(
            resolve_and_compute(connection, line, &business_date, treatment, seller_tax).await?,
        );
    }
    let totals = money::sum_lines(
        &computed
            .iter()
            .map(|entry| entry.amounts)
            .collect::<Vec<_>>(),
    )?;

    // A registered customer's mixed taxable/untaxed bill has no document AUSHADHARTH can issue.
    // Refused here, while nothing has been written: no number, no stock, no tender.
    let recipient_registered = customer
        .as_ref()
        .is_some_and(|party| party.gst_registration_status == "registered");
    let (has_taxable_line, has_untaxed_line) = treatment_mix(
        computed
            .iter()
            .map(|entry| entry.tax_treatment_kind.as_deref()),
    );
    if invoice_compliance::registered_recipient_mixed_supply(
        seller_tax,
        recipient_registered,
        has_taxable_line,
        has_untaxed_line,
    ) {
        return Err(SaleError::RegisteredRecipientMixedSupply);
    }

    // Phase 1M-A — the Drugs Rules gate.
    //
    // Resolved HERE, on the same connection and inside the same `BEGIN IMMEDIATE` transaction that
    // will write the lines, so a classification committed by another connection between the quote
    // and this moment cannot produce a Sale judged half by the old finding and half by the new one.
    // The frozen answer is the position on the Sale's BUSINESS DATE; the gate is the stricter of
    // that and the position on the store's actual posting day, so a backdated draft cannot carry a
    // drug scheduled since past the prescription rule.
    //
    // And it refuses here: before `allocate_document_number` below, before any inventory movement,
    // before any tender row and before the document becomes posted. A refused Sale leaves a draft
    // with its stock untouched and its series ungapped.
    let regulatory =
        resolve_regulatory_for_lines(connection, &store_id, &lines, &business_date).await?;
    // Every unsupported scheme, intersection or State boundary is refused before any prescription
    // is examined, so a basket holding one Schedule X line, one NDPS-intersection H1 line or one
    // Punjab-restricted line is refused as a whole, naming that line, however complete the
    // prescription paperwork on the other lines is.
    refuse_unsupported_schemes(&lines, &regulatory)?;

    // Phase 1M-B — the Schedule H prescription, under the same lock and still before any side
    // effect. Every unmet requirement is reported at once, so the counter can fix them together.
    let dispensing = plan_dispensings(
        connection,
        &store_id,
        id,
        &lines,
        &regulatory,
        &business_date,
    )
    .await?;
    if !dispensing.issues.is_empty() {
        // The refusal the counter cannot fix by itself is named on its own: no rule 65(3)(2)
        // election on record.
        if dispensing
            .issues
            .iter()
            .any(|issue| issue.code == "prescription_record_election_unresolved")
        {
            return Err(SaleError::PrescriptionRecordElectionUnresolved);
        }
        return Err(SaleError::PrescriptionRequirementsIncomplete(
            dispensing.issues,
        ));
    }

    // The recipient's statutory particulars are resolved HERE, once the taxable value that decides
    // Rule 46(e) is final, and on the same connection that already read the customer inside this
    // `BEGIN IMMEDIATE` transaction. Nothing is taken from the browser at this point: a named
    // customer's address comes from their record as it stands under the write lock, and a walk-in's
    // comes from what the draft recorded. The threshold is judged on the value of the taxable lines
    // only — never on the amount payable, and never on exempt or non-GST value.
    let taxable_supply_value_paise = taxable_supply_value(computed.iter().map(|entry| {
        (
            entry.tax_treatment_kind.as_deref(),
            entry.amounts.taxable_value_paise,
        )
    }))?;
    // Rule 46 recipient particulars belong to a tax invoice, so the seller's registration — already
    // resolved above, under this lock — decides whether they are asked at all.
    let recipient_source = load_recipient_source(
        connection,
        &header,
        customer.as_ref(),
        taxable_supply_value_paise,
        seller_tax,
    )
    .await?;
    let recipient_snapshot =
        recipient::resolve(&recipient_source).map_err(SaleError::RecipientParticularsIncomplete)?;
    let (recipient_address_source, recipient_address) = match &recipient_snapshot.address {
        Some((source, address)) => (Some(source.as_str()), address.clone()),
        None => (None, AddressFacts::default()),
    };
    let delivery_address = recipient_snapshot
        .delivery_address
        .clone()
        .unwrap_or_default();

    // The facts a later print will need and must never re-read, settled HERE under the same write
    // lock as the seller, the recipient and the tax: the seller's GST status (already applied to
    // every line above), the Rule 46(s) applicability, the Rule 48(4) applicability where the
    // recipient is registered, the HSN policy against the frozen line codes, and the designated
    // retail-memo licence.
    //
    // The licence is required for a `medicine` line only. The catalogue's `device` kind is a label
    // with no regulatory classification behind it, so nothing here can show that Drugs Rule 65
    // governs every product so labelled; assuming it would be a legal claim the data cannot support.
    let requires_retail_licence: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sale_lines line JOIN products product \
         ON product.id = line.product_id WHERE line.sale_document_id=? \
         AND product.product_kind = 'medicine')",
    )
    .bind(id)
    .fetch_one(&mut **connection)
    .await
    .map_err(map_database_error)?;
    let compliance = invoice_compliance::resolve(&ComplianceSource {
        seller: seller_tax,
        rule46s: Rule46sDeclaration::parse(&rule46s_declaration_applicability)
            .ok_or(SaleError::Internal)?,
        einvoice: EinvoiceApplicability::parse(&einvoice_applicability)
            .ok_or(SaleError::Internal)?,
        hsn_band: HsnBand::parse(&hsn_turnover_band).ok_or(SaleError::Internal)?,
        hsn_band_financial_year: hsn_turnover_financial_year.clone(),
        sale_financial_year: sales::indian_financial_year(&business_date)
            .ok_or_else(|| validation_of("businessDate", "must be a valid YYYY-MM-DD date"))?,
        recipient_registered,
        line_hsn: lines
            .iter()
            .zip(computed.iter())
            .map(|(line, entry)| (line.line_number, entry.hsn_code.clone()))
            .collect(),
        requires_retail_licence,
        designated_licence_text: seller.retail_memo_licence_text.clone(),
        dynamic_qr: DynamicQrApplicability::parse(&dynamic_qr_applicability)
            .ok_or(SaleError::Internal)?,
        has_taxable_line,
        tenders: tenders
            .iter()
            .map(|tender| TenderFact {
                method: tender.method.clone(),
                has_reference: tender.reference_text.is_some(),
            })
            .collect(),
    })
    .map_err(|refusal| match refusal {
        ComplianceRefusal::Incomplete(issues) => SaleError::SaleComplianceIncomplete(issues),
        ComplianceRefusal::EinvoiceRequired => SaleError::EinvoiceRequired,
        ComplianceRefusal::PaymentReferenceRequired(tenders) => {
            SaleError::PaymentReferenceRequired(tenders)
        }
    })?;

    // Tender is evidence, not accounting, but it must still add up to what was charged: a shortfall
    // would be credit, which this phase does not open.
    let tendered = tenders
        .iter()
        .try_fold(0_i64, |total, tender| {
            total.checked_add(tender.amount_paise)
        })
        .ok_or(SaleError::ArithmeticOverflow)?;
    if tendered != totals.line_total_paise {
        return Err(SaleError::TenderMismatch);
    }

    // Availability is evaluated per lot across the WHOLE document, so two lines drawing on one batch
    // cannot each pass a check the pair of them fails. The balance is read under the write lock this
    // posting already holds, so no concurrent sale can interleave between the check and the movement.
    let mut required: Vec<(String, String, i64)> = Vec::new();
    for line in &lines {
        match required
            .iter_mut()
            .find(|entry| entry.0 == line.product_pack_id && entry.1 == line.batch_id)
        {
            Some(entry) => {
                entry.2 = entry
                    .2
                    .checked_add(line.quantity_atoms)
                    .ok_or(SaleError::ArithmeticOverflow)?;
            }
            None => required.push((
                line.product_pack_id.clone(),
                line.batch_id.clone(),
                line.quantity_atoms,
            )),
        }
    }
    for (pack_id, batch_id, atoms) in &required {
        // Sellable only: goods sitting in quarantine after a customer returned them are in the
        // building, but they are not the counter's to sell until a pharmacist releases them.
        let available: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(quantity_delta_atoms),0) FROM inventory_movements \
             WHERE store_id=? AND product_pack_id=? AND batch_id IS ? AND stock_status='sellable'",
        )
        .bind(&store_id)
        .bind(pack_id)
        .bind(batch_id)
        .fetch_one(&mut **connection)
        .await
        .map_err(map_database_error)?;
        if available < *atoms {
            return Err(SaleError::InsufficientStock { available });
        }
    }

    let now: String = sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')")
        .fetch_one(&mut **connection)
        .await
        .map_err(map_database_error)?;

    // The number is allocated inside this transaction, so a refused posting rolls it back with
    // everything else and the series has no gaps.
    let number = allocate_document_number(connection, &store_id, &business_date, &now).await?;
    // Sale line -> dispensing, for the rule 65(3) entry written after the lines are frozen.
    let mut dispensing_ids: Vec<(String, String)> = Vec::new();

    for ((line, entry), drugs) in lines.iter().zip(computed.iter()).zip(regulatory.iter()) {
        // The regulatory answer is frozen in the same statement that freezes the money, so a line
        // can never carry one without the other. `regulatory_snapshot_version = 1` means every
        // scheme below was resolved from the classifications in force on this Sale's business date;
        // the manufacturer is the one in force on that date, or NULL where none was recorded, which
        // is the truth rather than today's marketer standing in for the maker.
        let (manufacturer_company_id, manufacturer_name) = match &drugs.manufacturer {
            Some((company_id, name)) => (Some(company_id.as_str()), Some(name.as_str())),
            None => (None, None),
        };
        sqlx::query(
            "UPDATE sale_lines SET product_display_name=?,pack_display_label=?,base_unit_label=?,\
             batch_number=?,batch_expires_on=?,batch_mrp_paise=?,hsn_code_id=?,hsn_code=?,\
             tax_category_id=?,tax_treatment_kind=?,tax_rate_version_id=?,quantity_scale=?,\
             cgst_basis_points=?,\
             sgst_basis_points=?,igst_basis_points=?,cess_basis_points=?,price_control_status=?,\
             controlled_formulation_id=?,price_control_version_id=?,ceiling_price_paise=?,\
             ceiling_basis=?,taxable_value_paise=?,cgst_paise=?,sgst_paise=?,igst_paise=?,\
             cess_paise=?,line_total_paise=?,regulatory_snapshot_version=1,\
             manufacturer_company_id=?,manufacturer_name=?,regulatory_schemes_snapshot=?,\
             regulatory_state_snapshot=?,updated_at_utc=? WHERE id=?",
        )
        .bind(&entry.product_display_name)
        .bind(&entry.pack_display_label)
        .bind(&entry.base_unit_label)
        .bind(&entry.batch_number)
        .bind(&entry.batch_expires_on)
        .bind(entry.batch_mrp_paise)
        .bind(&entry.hsn_code_id)
        .bind(&entry.hsn_code)
        .bind(&entry.tax_category_id)
        .bind(&entry.tax_treatment_kind)
        .bind(&entry.tax_rate_version_id)
        .bind(entry.quantity_scale)
        .bind(entry.rate.cgst_basis_points)
        .bind(entry.rate.sgst_basis_points)
        .bind(entry.rate.igst_basis_points)
        .bind(entry.rate.cess_basis_points)
        .bind(&entry.price_control_status)
        .bind(&entry.controlled_formulation_id)
        .bind(&entry.price_control_version_id)
        .bind(entry.ceiling_price_paise)
        .bind(&entry.ceiling_basis)
        .bind(entry.amounts.taxable_value_paise)
        .bind(entry.amounts.cgst_paise)
        .bind(entry.amounts.sgst_paise)
        .bind(entry.amounts.igst_paise)
        .bind(entry.amounts.cess_paise)
        .bind(entry.amounts.line_total_paise)
        .bind(manufacturer_company_id)
        .bind(manufacturer_name)
        .bind(drugs.schemes.to_snapshot_json())
        // Phase 1M-C: the premises State and the Punjab position on the business date, beside the
        // central answer and never inside it.
        .bind(drugs.state.to_snapshot_json())
        .bind(&now)
        .bind(&line.id)
        .execute(&mut **connection)
        .await
        .map_err(map_database_error)?;

        // Phase 1M-B — the dispensing, written while the Sale is still a draft so the database's
        // own coherence and cap triggers judge it, and before the Sale flips to posted so the
        // posting gate finds it. It is the ledger entry the prescription's remaining authority is
        // derived from; nothing is decremented anywhere.
        if let Some(planned) = dispensing
            .planned
            .iter()
            .find(|planned| planned.sale_line_id == line.id)
        {
            let supervision = dispensing.supervision.as_ref().ok_or(SaleError::Internal)?;
            let dispensing_id = Uuid::now_v7().to_string();
            sqlx::query(
                "INSERT INTO prescription_dispensings (id,store_id,prescription_id,\
                 prescription_item_id,sale_document_id,sale_line_id,product_id,quantity_atoms,\
                 dispensed_on,supervising_professional_id,supervising_professional_name,\
                 supervising_registration_number,supervising_registering_authority,\
                 endorsement_confirmed_by_user_id,endorsement_confirmed_at_utc,created_at_utc) \
                 VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            )
            .bind(&dispensing_id)
            .bind(&store_id)
            .bind(&planned.prescription_id)
            .bind(&planned.prescription_item_id)
            .bind(id)
            .bind(&planned.sale_line_id)
            .bind(&planned.product_id)
            .bind(planned.quantity_atoms)
            .bind(&business_date)
            .bind(&supervision.professional_id)
            .bind(&supervision.full_name)
            .bind(&supervision.registration_number)
            .bind(&supervision.registering_authority)
            .bind(&supervision.endorsement_confirmed_by_user_id)
            .bind(&supervision.endorsement_confirmed_at_utc)
            .bind(&now)
            .execute(&mut **connection)
            .await
            .map_err(map_database_error)?;
            dispensing_ids.push((planned.sale_line_id.clone(), dispensing_id.clone()));
            // Identifiers only: the patient's name and address stay on the prescription.
            audit_entity(
                connection,
                "prescription_dispensing",
                &dispensing_id,
                "created",
                &json!({
                    "saleDocumentId": id,
                    "saleLineId": planned.sale_line_id,
                    "prescriptionId": planned.prescription_id,
                    "prescriptionItemId": planned.prescription_item_id,
                    "quantityAtoms": planned.quantity_atoms,
                    "supervisingProfessionalId": supervision.professional_id,
                }),
                actor_id,
                &now,
            )
            .await?;
        }

        // One immutable outward movement per line, negative, carrying a real provenance key. The
        // ledger stays the sole authority for quantity: nothing anywhere holds a stock balance.
        sqlx::query(
            "INSERT INTO inventory_movements (id,store_id,product_id,product_pack_id,batch_id,\
             movement_type,quantity_delta_atoms,occurred_on,sale_line_id,idempotency_key,\
             posted_by_user_id,posted_at_utc) VALUES (?,?,?,?,?,'sale',?,?,?,?,?,?)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&store_id)
        .bind(&line.product_id)
        .bind(&line.product_pack_id)
        .bind(&line.batch_id)
        .bind(-line.quantity_atoms)
        .bind(&business_date)
        .bind(&line.id)
        .bind(Uuid::now_v7().to_string())
        .bind(actor_id)
        .bind(&now)
        .execute(&mut **connection)
        .await
        .map_err(map_database_error)?;
    }

    // Rule 65(3)(1) — the entry was prepared, signed by hand and confirmed before this posting
    // began; it is finalized here, in the same transaction as the supply. If anything after this
    // fails, the entry stays confirmed — truthfully, since it may be signed on paper — and the Sale
    // stays a draft; the posting can be retried, or the entry voided.
    if !dispensing.planned.is_empty() {
        finalize_supply_records(connection, id, &dispensing, &dispensing_ids, actor_id, &now)
            .await?;
    }
    // Phase 1M-C — each Schedule H1 working entry, confirmed before this posting began (its hard
    // copy placed in the separate register and authenticated by the registered pharmacist), is
    // linked to its dispensing and finalized with the Sale, or none of them is.
    if !dispensing.live_h1.is_empty() {
        finalize_h1_entries(connection, id, &dispensing, &dispensing_ids, actor_id, &now).await?;
    }

    for tender in tenders {
        sqlx::query(
            "INSERT INTO sale_tenders (id,sale_document_id,method,amount_paise,reference_text,\
             created_at_utc) VALUES (?,?,?,?,?,?)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(id)
        .bind(&tender.method)
        .bind(tender.amount_paise)
        .bind(&tender.reference_text)
        .bind(&now)
        .execute(&mut **connection)
        .await
        .map_err(map_database_error)?;
    }

    let next = revision + 1;
    sqlx::query(
        "UPDATE sale_documents SET status='posted',revision=?,series_code=?,financial_year=?,\
         sequence_value=?,document_number=?,store_gst_registration_status=?,store_normalized_gstin=?,\
         store_place_of_supply_state_id=?,store_state_code=?,customer_display_name=?,\
         customer_gst_registration_status=?,customer_normalized_gstin=?,customer_state_code=?,\
         tax_treatment=?,taxable_value_paise=?,cgst_paise=?,sgst_paise=?,igst_paise=?,cess_paise=?,\
         grand_total_paise=?,posted_by_user_id=?,posted_at_utc=?,posting_idempotency_key=?,\
         posting_fingerprint=?,seller_snapshot_version=1,seller_legal_name=?,seller_trade_name=?,\
         seller_address_line1=?,seller_address_line2=?,seller_city=?,seller_postal_code=?,\
         seller_state_name=?,seller_phone=?,seller_email=?,seller_licence_text=?,\
         recipient_snapshot_version=1,recipient_particulars_requested=?,\
         recipient_address_source=?,recipient_address_line1=?,recipient_address_line2=?,\
         recipient_city=?,recipient_postal_code=?,recipient_state_id=?,recipient_state_name=?,\
         recipient_state_code=?,delivery_same_as_recipient=?,delivery_address_line1=?,\
         delivery_address_line2=?,delivery_city=?,delivery_postal_code=?,delivery_state_id=?,\
         delivery_state_name=?,delivery_state_code=?,\
         compliance_snapshot_version=1,seller_retail_licence_text=?,\
         rule46s_declaration_snapshot=?,einvoice_applicability_snapshot=?,\
         hsn_turnover_band_snapshot=?,dynamic_qr_snapshot_version=1,\
         dynamic_qr_applicability_snapshot=?,\
         hsn_turnover_financial_year_snapshot=?,hsn_required_digits=?,\
         updated_at_utc=? WHERE id=? AND revision=? AND status='draft'",
    )
    .bind(next)
    .bind(&number.series_code)
    .bind(&number.financial_year)
    .bind(number.sequence_value)
    .bind(&number.document_number)
    .bind(&store_status)
    .bind(&store_gstin)
    .bind(&store_state_id)
    .bind(&store_state_code)
    .bind(customer.as_ref().map(|party| &party.display_name))
    .bind(
        customer
            .as_ref()
            .map(|party| &party.gst_registration_status),
    )
    .bind(customer.as_ref().and_then(|party| party.normalized_gstin.as_ref()))
    .bind(customer.as_ref().and_then(|party| party.state_code.as_ref()))
    .bind(treatment.as_str())
    .bind(totals.taxable_value_paise)
    .bind(totals.cgst_paise)
    .bind(totals.sgst_paise)
    .bind(totals.igst_paise)
    .bind(totals.cess_paise)
    .bind(totals.line_total_paise)
    .bind(actor_id)
    .bind(&now)
    .bind(idempotency_key)
    .bind(&fingerprint)
    .bind(&seller.legal_name)
    .bind(&seller.trade_name)
    .bind(&seller.address_line1)
    .bind(&seller.address_line2)
    .bind(&seller.city)
    .bind(&seller.postal_code)
    .bind(&seller.state_name)
    .bind(&seller.phone)
    .bind(&seller.email)
    .bind(&seller.licence_text)
    .bind(recipient_snapshot.particulars_requested)
    .bind(recipient_address_source)
    .bind(&recipient_address.line1)
    .bind(&recipient_address.line2)
    .bind(&recipient_address.city)
    .bind(&recipient_address.postal_code)
    .bind(&recipient_address.state_id)
    .bind(&recipient_address.state_name)
    .bind(&recipient_address.state_code)
    .bind(recipient_snapshot.delivery_same_as_recipient)
    .bind(&delivery_address.line1)
    .bind(&delivery_address.line2)
    .bind(&delivery_address.city)
    .bind(&delivery_address.postal_code)
    .bind(&delivery_address.state_id)
    .bind(&delivery_address.state_name)
    .bind(&delivery_address.state_code)
    .bind(&compliance.retail_licence_text)
    .bind(compliance.rule46s.map(Rule46sDeclaration::as_str))
    .bind(compliance.einvoice.map(EinvoiceApplicability::as_str))
    .bind(compliance.hsn_band.map(HsnBand::as_str))
    .bind(compliance.dynamic_qr.map(DynamicQrApplicability::as_str))
    .bind(&compliance.hsn_band_financial_year)
    .bind(compliance.hsn_required_digits.map(i64::from))
    .bind(&now)
    .bind(id)
    .bind(revision)
    .execute(&mut **connection)
    .await
    .map_err(map_database_error)?;

    sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) \
         VALUES (?,'sale_document',?,?,'posted',strftime('%Y-%m-%dT%H:%M:%fZ','now'),NULL,1,?,?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(id)
    .bind(next)
    .bind(
        json!({
            "documentNumber": number.document_number,
            "taxTreatment": treatment.as_str(),
            "grandTotalPaise": totals.line_total_paise,
            "lineCount": lines.len(),
        })
        .to_string(),
    )
    .bind(actor_id)
    .execute(&mut **connection)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

struct AllocatedNumber {
    series_code: String,
    financial_year: String,
    sequence_value: i64,
    document_number: String,
}

/// Allocates the next number in this store's sale series for the business date's financial year.
///
/// Never `MAX(document_number) + 1`: a deleted or refused document would then hand the same number
/// out twice. The counter is a row, read and advanced under the write lock the posting already
/// holds, and the advance rolls back with the posting if anything later fails.
async fn allocate_document_number(
    connection: &mut PoolConnection<Sqlite>,
    store_id: &str,
    business_date: &str,
    now: &str,
) -> Result<AllocatedNumber, SaleError> {
    let financial_year = sales::indian_financial_year(business_date)
        .ok_or_else(|| validation_of("businessDate", "must be a valid YYYY-MM-DD date"))?;
    sqlx::query(
        "INSERT INTO document_number_series (id,store_id,document_kind,series_code,financial_year,\
         next_value,created_at_utc,updated_at_utc) VALUES (?,?,'sale',?,?,1,?,?) \
         ON CONFLICT (store_id,document_kind,series_code,financial_year) DO NOTHING",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(store_id)
    .bind(SALE_SERIES_CODE)
    .bind(&financial_year)
    .bind(now)
    .bind(now)
    .execute(&mut **connection)
    .await
    .map_err(map_database_error)?;

    let sequence_value: i64 = sqlx::query_scalar(
        "SELECT next_value FROM document_number_series \
         WHERE store_id=? AND document_kind='sale' AND series_code=? AND financial_year=?",
    )
    .bind(store_id)
    .bind(SALE_SERIES_CODE)
    .bind(&financial_year)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?
    .ok_or(SaleError::Internal)?;

    // The counter only advances from the exact value that was read. Under BEGIN IMMEDIATE nothing
    // else can be writing, and if that ever stopped being true this would refuse rather than issue a
    // duplicate invoice number.
    let advanced = sqlx::query(
        "UPDATE document_number_series SET next_value=next_value+1,updated_at_utc=? \
         WHERE store_id=? AND document_kind='sale' AND series_code=? AND financial_year=? \
           AND next_value=?",
    )
    .bind(now)
    .bind(store_id)
    .bind(SALE_SERIES_CODE)
    .bind(&financial_year)
    .bind(sequence_value)
    .execute(&mut **connection)
    .await
    .map_err(map_database_error)?;
    if advanced.rows_affected() != 1 {
        return Err(SaleError::ServiceBusy);
    }

    // Rendered by the domain, never here, and refused rather than issued if it would breach the
    // sixteen-character statutory limit. An invoice number is permanent once issued: a
    // non-conforming one cannot be put right by re-issuing it.
    let document_number = sales::document_serial(SALE_SERIES_CODE, &financial_year, sequence_value)
        .ok_or(SaleError::Internal)?;

    Ok(AllocatedNumber {
        document_number,
        series_code: SALE_SERIES_CODE.to_owned(),
        financial_year,
        sequence_value,
    })
}

/// A resolved line: every snapshot the posted invoice keeps, plus the computed amounts.
struct ComputedLine {
    product_display_name: String,
    pack_display_label: Option<String>,
    base_unit_label: Option<String>,
    batch_number: String,
    batch_expires_on: Option<String>,
    batch_mrp_paise: Option<i64>,
    hsn_code_id: Option<String>,
    hsn_code: Option<String>,
    tax_category_id: Option<String>,
    tax_treatment_kind: Option<String>,
    tax_rate_version_id: Option<String>,
    /// How many decimal places the line's atom count represents. Frozen here so a renderer never
    /// has to ask the product catalogue what an integer means.
    quantity_scale: i64,
    rate: RateComponents,
    price_control_status: String,
    controlled_formulation_id: Option<String>,
    price_control_version_id: Option<String>,
    ceiling_price_paise: Option<i64>,
    ceiling_basis: Option<String>,
    amounts: LineAmounts,
}

#[derive(Debug, FromRow)]
struct ProductSource {
    display_name: String,
    base_unit_id: String,
    quantity_scale: i64,
    hsn_code_id: Option<String>,
    tax_category_id: Option<String>,
    price_control_status: String,
    controlled_formulation_id: Option<String>,
}

async fn resolve_and_compute(
    connection: &mut PoolConnection<Sqlite>,
    line: &DraftLine,
    business_date: &str,
    treatment: TaxTreatment,
    seller: SellerGst,
) -> Result<ComputedLine, SaleError> {
    let product = sqlx::query_as::<_, ProductSource>(
        "SELECT display_name,base_unit_id,quantity_scale,hsn_code_id,tax_category_id,\
         price_control_status,controlled_formulation_id FROM products WHERE id=?",
    )
    .bind(&line.product_id)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?
    .ok_or(SaleError::NotFound)?;

    // Pack and batch eligibility are re-checked here rather than trusted from the draft: a pack can
    // be withdrawn from sale, a lot recalled, or a date crossed while a draft sat open at the
    // counter.
    let pack = sqlx::query_as::<_, SellablePack>(
        "SELECT pack.product_id,pack.base_quantity_atoms,product.quantity_scale,\
         pack.status AS pack_status,policy.sale_enabled,policy.fractional_sale_allowed,\
         policy.minimum_sale_increment_atoms \
         FROM product_packs pack \
         JOIN products product ON product.id = pack.product_id \
         LEFT JOIN store_pack_policies policy \
           ON policy.pack_id = pack.id AND policy.store_id = \
              (SELECT store_id FROM sale_documents WHERE id = ?) AND policy.status = 'active' \
         WHERE pack.id = ?",
    )
    .bind(&line.sale_document_id)
    .bind(&line.product_pack_id)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?
    .ok_or(SaleError::NotFound)?;
    if pack.pack_status != "active" || pack.sale_enabled != Some(1) {
        return Err(SaleError::PackNotSellable);
    }

    let pack_label: Option<String> =
        sqlx::query_scalar("SELECT display_label FROM product_packs WHERE id=?")
            .bind(&line.product_pack_id)
            .fetch_optional(&mut **connection)
            .await
            .map_err(map_database_error)?
            .flatten();
    let base_unit_label: Option<String> =
        sqlx::query_scalar("SELECT display_name FROM units_of_measure WHERE id=?")
            .bind(&product.base_unit_id)
            .fetch_optional(&mut **connection)
            .await
            .map_err(map_database_error)?;

    let batch = sqlx::query_as::<_, PostingBatch>(
        "SELECT batch_number,status,expires_on,mrp_paise FROM product_batches WHERE id=?",
    )
    .bind(&line.batch_id)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?
    .ok_or(SaleError::NotFound)?;
    if batch.status != "active" {
        return Err(SaleError::BatchPackMismatch);
    }
    if batch
        .expires_on
        .as_deref()
        .is_some_and(|expiry| expiry < business_date)
    {
        return Err(SaleError::BatchExpired);
    }

    let hsn_code: Option<String> = match product.hsn_code_id.as_deref() {
        Some(id) => sqlx::query_scalar("SELECT hsn_code FROM hsn_codes WHERE id=?")
            .bind(id)
            .fetch_optional(&mut **connection)
            .await
            .map_err(map_database_error)?,
        None => None,
    };

    // A seller that is not GST-registered charges no GST, whatever rate the product carries: CGST
    // Act s.32(1) forbids it to collect "any amount by way of tax". The product's own tax category
    // is still recorded as product metadata when it has one, but no rate is resolved and nothing is
    // charged — and a missing category or rate is no reason to refuse a sale that charges no tax.
    // A registered seller's path below is unchanged.
    let (tax_category_id, treatment_kind, rate_version_id, rate) = if seller
        == SellerGst::Unregistered
    {
        let treatment_kind: Option<String> = match product.tax_category_id.as_deref() {
            Some(id) => sqlx::query_scalar("SELECT tax_treatment FROM tax_categories WHERE id=?")
                .bind(id)
                .fetch_optional(&mut **connection)
                .await
                .map_err(map_database_error)?,
            None => None,
        };
        (
            product.tax_category_id.clone(),
            treatment_kind,
            None,
            RateComponents::default(),
        )
    } else {
        let (tax_category_id, treatment_kind, rate_version_id, rate) =
            registered_rate(connection, &product, business_date).await?;
        (
            Some(tax_category_id),
            Some(treatment_kind),
            rate_version_id,
            rate,
        )
    };

    let quantity = SaleQuantity {
        basis: QuantityBasis::parse(&line.quantity_basis).ok_or(SaleError::Internal)?,
        quantity_packs: line.quantity_packs,
        quantity_atoms: line.quantity_atoms,
    };
    let amounts = sales::compute_line(&quantity, line.selling_rate_paise, treatment, rate)?;
    finish_computed_line(
        connection,
        line,
        business_date,
        ResolvedLineFacts {
            product,
            pack,
            pack_label,
            base_unit_label,
            batch,
            hsn_code,
        },
        (tax_category_id, treatment_kind, rate_version_id, rate),
        (quantity, amounts),
    )
    .await
}

/// A registered seller's tax: exactly the Phase 1H/1F resolution, moved here unchanged so the
/// unregistered branch above can bypass it.
async fn registered_rate(
    connection: &mut PoolConnection<Sqlite>,
    product: &ProductSource,
    business_date: &str,
) -> Result<(String, String, Option<String>, RateComponents), SaleError> {
    // A Product with no Tax Category cannot be taxed. That is an error, never zero tax.
    let tax_category_id = product
        .tax_category_id
        .clone()
        .ok_or(SaleError::ClassificationIncomplete)?;
    let treatment_kind: String =
        sqlx::query_scalar("SELECT tax_treatment FROM tax_categories WHERE id=?")
            .bind(&tax_category_id)
            .fetch_optional(&mut **connection)
            .await
            .map_err(map_database_error)?
            .ok_or(SaleError::ClassificationIncomplete)?;

    // A zero-rate category asserts zero tax by classification; a taxable one must resolve a rate in
    // force on the business date, and the absence of one is an error rather than zero.
    let (rate_version_id, rate) = if treatment_kind == "taxable" {
        let resolved =
            taxation::resolve_tax_rate(&mut **connection, &tax_category_id, business_date)
                .await
                .map_err(|error| match error {
                    taxation::TaxResolutionError::InvalidDate => {
                        validation_of("businessDate", "must be a valid YYYY-MM-DD date")
                    }
                    taxation::TaxResolutionError::Database(_) => SaleError::Internal,
                })?
                .ok_or(SaleError::TaxRateNotFound)?;
        (
            Some(resolved.id),
            RateComponents {
                cgst_basis_points: resolved.cgst_basis_points,
                sgst_basis_points: resolved.sgst_basis_points,
                igst_basis_points: resolved.igst_basis_points,
                cess_basis_points: resolved.cess_basis_points,
            },
        )
    } else {
        (None, RateComponents::default())
    };
    Ok((tax_category_id, treatment_kind, rate_version_id, rate))
}

/// The catalogue facts a line was resolved from, before any tax decision.
struct ResolvedLineFacts {
    product: ProductSource,
    pack: SellablePack,
    pack_label: Option<String>,
    base_unit_label: Option<String>,
    batch: PostingBatch,
    hsn_code: Option<String>,
}

/// The rest of a line's resolution, identical for either seller: both price ceilings, then the
/// frozen line.
async fn finish_computed_line(
    connection: &mut PoolConnection<Sqlite>,
    line: &DraftLine,
    business_date: &str,
    facts: ResolvedLineFacts,
    (tax_category_id, treatment_kind, rate_version_id, rate): (
        Option<String>,
        Option<String>,
        Option<String>,
        RateComponents,
    ),
    (quantity, amounts): (SaleQuantity, LineAmounts),
) -> Result<ComputedLine, SaleError> {
    let ResolvedLineFacts {
        product,
        pack,
        pack_label,
        base_unit_label,
        batch,
        hsn_code,
    } = facts;
    // Ceiling one: the printed MRP, which is INCLUSIVE of GST under Legal Metrology, so it is
    // compared against the GST-inclusive line total.
    if !sales::within_mrp_ceiling(
        &quantity,
        amounts.line_total_paise,
        pack.base_quantity_atoms,
        batch.mrp_paise,
    )? {
        return Err(SaleError::SellingRateAboveMrp);
    }

    // Ceiling two: the notified DPCO ceiling, which is EXCLUSIVE of GST, so it is compared against
    // the tax-exclusive selling rate. The two ceilings are never compared with each other — both are
    // enforced, which yields the stricter maximum without putting them on one axis.
    let (version_id, ceiling_price_paise, ceiling_basis) =
        resolve_ceiling(connection, &product, line, &pack, business_date).await?;

    Ok(ComputedLine {
        product_display_name: product.display_name,
        pack_display_label: pack_label,
        base_unit_label,
        batch_number: batch.batch_number,
        batch_expires_on: batch.expires_on,
        batch_mrp_paise: batch.mrp_paise,
        hsn_code_id: product.hsn_code_id,
        hsn_code,
        tax_category_id,
        tax_treatment_kind: treatment_kind,
        tax_rate_version_id: rate_version_id,
        quantity_scale: product.quantity_scale,
        rate,
        price_control_status: product.price_control_status,
        controlled_formulation_id: product.controlled_formulation_id,
        price_control_version_id: version_id,
        ceiling_price_paise,
        ceiling_basis,
        amounts,
    })
}

/// Enforces the Phase 1H-0 controlled ceiling and returns what the posted line should record.
///
/// A status of `unknown` means nobody has assessed this medicine yet. The sale proceeds under the
/// MRP rule alone and the honest `unknown` is snapshotted onto the line, so the gap is auditable
/// rather than invisible — it is never read as "not controlled", and there is no silent fallback.
async fn resolve_ceiling(
    connection: &mut PoolConnection<Sqlite>,
    product: &ProductSource,
    line: &DraftLine,
    pack: &SellablePack,
    business_date: &str,
) -> Result<(Option<String>, Option<i64>, Option<String>), SaleError> {
    if PriceControlStatus::parse(&product.price_control_status)
        != Some(PriceControlStatus::Controlled)
    {
        return Ok((None, None, None));
    }
    let formulation_id = product
        .controlled_formulation_id
        .as_deref()
        .ok_or(SaleError::PriceControlUnresolved)?;
    let ceiling =
        price_control::resolve_price_ceiling(&mut **connection, formulation_id, business_date)
            .await
            .map_err(|error| match error {
                price_control::PriceControlError::InvalidDate => {
                    validation_of("businessDate", "must be a valid YYYY-MM-DD date")
                }
                price_control::PriceControlError::Database(_) => SaleError::Internal,
            })?
            .ok_or(SaleError::PriceControlUnresolved)?;
    if price_control::compare_basis(&ceiling, &product.base_unit_id) != Comparability::Comparable {
        return Err(SaleError::PriceControlIncomparable);
    }

    let within = match QuantityBasis::parse(&line.quantity_basis).ok_or(SaleError::Internal)? {
        QuantityBasis::Pack => price_control::per_pack_rate_within_ceiling(
            line.selling_rate_paise,
            product.quantity_scale,
            pack.base_quantity_atoms,
            ceiling.ceiling_price_paise,
        ),
        QuantityBasis::BaseUnit => price_control::per_atom_rate_within_ceiling(
            line.selling_rate_paise,
            product.quantity_scale,
            ceiling.ceiling_price_paise,
        ),
    }
    .map_err(|_| SaleError::ArithmeticOverflow)?;
    if !within {
        return Err(SaleError::SellingRateAboveCeiling);
    }
    Ok((
        Some(ceiling.id),
        Some(ceiling.ceiling_price_paise),
        Some(ceiling.ceiling_basis),
    ))
}

/// The semantic payload a replay must match: the document, the customer, the date, each line's
/// commercial facts, and the tender. Presentation and timestamps are deliberately excluded.
fn posting_fingerprint(
    id: &str,
    customer_party_id: Option<&str>,
    business_date: &str,
    lines: &[DraftLine],
    tenders: &[PreparedTender],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(id.as_bytes());
    hasher.update(customer_party_id.unwrap_or("walk-in").as_bytes());
    hasher.update(business_date.as_bytes());
    for line in lines {
        hasher.update(line.product_id.as_bytes());
        hasher.update(line.product_pack_id.as_bytes());
        hasher.update(line.batch_id.as_bytes());
        hasher.update(line.quantity_basis.as_bytes());
        hasher.update(line.quantity_atoms.to_le_bytes());
        hasher.update(line.selling_rate_paise.to_le_bytes());
    }
    // The tender is part of what was agreed, so retrying with a different payment method is a
    // different posting rather than a replay of this one.
    for tender in tenders {
        hasher.update(tender.method.as_bytes());
        hasher.update(tender.amount_paise.to_le_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Persistence helpers
// ---------------------------------------------------------------------------------------------

#[derive(Debug, FromRow)]
struct PostingHeader {
    status: String,
    revision: i64,
    store_id: String,
    customer_party_id: Option<String>,
    business_date: String,
    posting_idempotency_key: Option<String>,
    posting_fingerprint: Option<String>,
    customer_name_text: Option<String>,
    /// NULL on a draft saved before 1L-A2 existed: read as "not requested".
    recipient_particulars_requested: Option<bool>,
    recipient_address_line1: Option<String>,
    recipient_address_line2: Option<String>,
    recipient_city: Option<String>,
    recipient_postal_code: Option<String>,
    recipient_state_id: Option<String>,
    /// NULL on a draft saved before 1L-A2 existed: read as "delivered to the recipient's address".
    delivery_same_as_recipient: Option<bool>,
    delivery_address_line1: Option<String>,
    delivery_address_line2: Option<String>,
    delivery_city: Option<String>,
    delivery_postal_code: Option<String>,
    delivery_state_id: Option<String>,
}

const POSTING_HEADER_COLUMNS: &str = "status,revision,store_id,customer_party_id,business_date,\
     posting_idempotency_key,posting_fingerprint,customer_name_text,\
     recipient_particulars_requested,recipient_address_line1,recipient_address_line2,\
     recipient_city,recipient_postal_code,recipient_state_id,delivery_same_as_recipient,\
     delivery_address_line1,delivery_address_line2,delivery_city,delivery_postal_code,\
     delivery_state_id";

/// A State as it will be printed, or nothing when no State was given or the id no longer resolves.
async fn resolve_state(
    connection: &mut PoolConnection<Sqlite>,
    state_id: Option<&str>,
) -> Result<(Option<String>, Option<String>, Option<String>), SaleError> {
    let Some(state_id) = state_id else {
        return Ok((None, None, None));
    };
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT display_name,state_code FROM state_codes WHERE id=?")
            .bind(state_id)
            .fetch_optional(&mut **connection)
            .await
            .map_err(map_database_error)?;
    Ok(match row {
        Some((name, code)) => (Some(state_id.to_owned()), Some(name), Some(code)),
        None => (None, None, None),
    })
}

async fn address_facts(
    connection: &mut PoolConnection<Sqlite>,
    line1: Option<&str>,
    line2: Option<&str>,
    city: Option<&str>,
    postal_code: Option<&str>,
    state_id: Option<&str>,
) -> Result<AddressFacts, SaleError> {
    let (state_id, state_name, state_code) = resolve_state(connection, state_id).await?;
    Ok(AddressFacts {
        line1: line1.map(str::to_owned),
        line2: line2.map(str::to_owned),
        city: city.map(str::to_owned),
        postal_code: postal_code.map(str::to_owned),
        state_id,
        state_name,
        state_code,
    })
}

#[derive(Debug, FromRow)]
struct BillingAddressRow {
    line1: String,
    line2: Option<String>,
    city: Option<String>,
    postal_code: Option<String>,
    state_id: Option<String>,
    is_primary: bool,
}

/// Everything Rule 46 needs to know about this Sale's recipient, read on `connection`.
///
/// Called by posting inside its `BEGIN IMMEDIATE` transaction, AFTER `load_customer` has read the
/// party on the same connection, so the name, the GSTIN and the billing address all come from one
/// committed state of the Party master. A Party edit that commits first is seen in full; one that
/// commits later waits for posting's write lock and is not seen at all.
///
/// The customer's address is their active billing address: the primary one, or the only one.
/// Several active billing addresses with none marked primary is reported as ambiguous rather than
/// resolved by picking one. A shipping address is never used as the recipient's address.
async fn load_recipient_source(
    connection: &mut PoolConnection<Sqlite>,
    header: &PostingHeader,
    customer: Option<&CustomerSnapshot>,
    taxable_supply_value_paise: i64,
    seller: SellerGst,
) -> Result<RecipientSource, SaleError> {
    let party = match (header.customer_party_id.as_deref(), customer) {
        (Some(party_id), Some(customer)) => {
            let rows = sqlx::query_as::<_, BillingAddressRow>(
                "SELECT line1,line2,city,postal_code,state_id,is_primary FROM party_addresses \
                 WHERE party_id=? AND address_role='billing' AND status='active' \
                 ORDER BY is_primary DESC,id",
            )
            .bind(party_id)
            .fetch_all(&mut **connection)
            .await
            .map_err(map_database_error)?;
            let chosen = match rows.as_slice() {
                [] => None,
                [only] => Some(Some(only)),
                [first, ..] if first.is_primary => Some(Some(first)),
                _ => Some(None),
            };
            let billing = match chosen {
                None => PartyBilling::None,
                Some(None) => PartyBilling::Ambiguous,
                Some(Some(row)) => PartyBilling::One(
                    address_facts(
                        connection,
                        Some(&row.line1),
                        row.line2.as_deref(),
                        row.city.as_deref(),
                        row.postal_code.as_deref(),
                        row.state_id.as_deref(),
                    )
                    .await?,
                ),
            };
            Some(PartyRecipient {
                display_name: customer.display_name.clone(),
                gst_registration_status: customer.gst_registration_status.clone(),
                normalized_gstin: customer.normalized_gstin.clone(),
                billing,
            })
        }
        _ => None,
    };
    let counter_address = address_facts(
        connection,
        header.recipient_address_line1.as_deref(),
        header.recipient_address_line2.as_deref(),
        header.recipient_city.as_deref(),
        header.recipient_postal_code.as_deref(),
        header.recipient_state_id.as_deref(),
    )
    .await?;
    let delivery_address = address_facts(
        connection,
        header.delivery_address_line1.as_deref(),
        header.delivery_address_line2.as_deref(),
        header.delivery_city.as_deref(),
        header.delivery_postal_code.as_deref(),
        header.delivery_state_id.as_deref(),
    )
    .await?;
    Ok(RecipientSource {
        seller_registered: seller == SellerGst::Registered,
        party,
        counter_name: header.customer_name_text.clone(),
        counter_address,
        particulars_requested: header.recipient_particulars_requested.unwrap_or(false),
        delivery_same_as_recipient: header.delivery_same_as_recipient.unwrap_or(true),
        delivery_address,
        taxable_supply_value_paise,
    })
}

#[derive(Debug, FromRow)]
struct StoreTaxSource {
    normalized_gstin: Option<String>,
    place_of_supply_state_id: Option<String>,
    gst_registration_status: String,
    rule46s_declaration_applicability: String,
    einvoice_applicability: String,
    hsn_turnover_band: String,
    hsn_turnover_financial_year: Option<String>,
    dynamic_qr_applicability: String,
}

/// Whether any line's treatment is `taxable`, and whether any is untaxed (exempt, nil-rated or
/// outside GST) — the two halves of the document classification `invoices::classify` makes.
fn treatment_mix<'a>(kinds: impl Iterator<Item = Option<&'a str>>) -> (bool, bool) {
    kinds.fold((false, false), |(taxable, untaxed), kind| match kind {
        Some("taxable") => (true, untaxed),
        Some("exempt" | "nil_rated" | "non_gst") => (taxable, true),
        _ => (taxable, untaxed),
    })
}

/// The seller-status authority the quote and the posting share. `unknown` decides nothing.
async fn seller_gst(
    connection: &mut PoolConnection<Sqlite>,
    store_id: &str,
) -> Result<SellerGst, SaleError> {
    let status: String =
        sqlx::query_scalar("SELECT gst_registration_status FROM store_identity WHERE store_id=?")
            .bind(store_id)
            .fetch_optional(&mut **connection)
            .await
            .map_err(map_database_error)?
            .ok_or(SaleError::NotFound)?;
    SellerGst::from_status(&status).ok_or(SaleError::StoreGstStatusUnresolved)
}

#[derive(Debug, FromRow)]
struct PostingBatch {
    batch_number: String,
    status: String,
    expires_on: Option<String>,
    mrp_paise: Option<i64>,
}

/// The customer facts snapshotted onto the posted invoice.
struct CustomerSnapshot {
    display_name: String,
    gst_registration_status: String,
    normalized_gstin: Option<String>,
    state_code: Option<String>,
}

#[derive(Debug, FromRow)]
struct CustomerSource {
    display_name: String,
    gst_registration_status: String,
    normalized_gstin: Option<String>,
    place_of_supply_state_id: Option<String>,
    status: String,
}

/// Eligibility is re-checked at posting, not merely when the draft named the customer: a party can
/// be archived, or its customer role withdrawn, while a draft sits open at the counter.
async fn load_customer(
    connection: &mut PoolConnection<Sqlite>,
    party_id: &str,
) -> Result<CustomerSnapshot, SaleError> {
    let party = sqlx::query_as::<_, CustomerSource>(
        "SELECT display_name,gst_registration_status,normalized_gstin,place_of_supply_state_id,\
         status FROM parties WHERE id=?",
    )
    .bind(party_id)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?
    .ok_or(SaleError::NotFound)?;
    if party.status != "active" {
        return Err(SaleError::CustomerNotEligible);
    }
    let has_role: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM party_roles WHERE party_id=? AND role='customer' AND status='active' LIMIT 1",
    )
    .bind(party_id)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?;
    if has_role.is_none() {
        return Err(SaleError::CustomerNotEligible);
    }
    let state_code = match party.place_of_supply_state_id.as_deref() {
        Some(state_id) => Some(state_code(connection, state_id).await?),
        None => None,
    };
    Ok(CustomerSnapshot {
        display_name: party.display_name,
        gst_registration_status: party.gst_registration_status,
        normalized_gstin: party.normalized_gstin,
        state_code,
    })
}

/// Phase 1M-A — what a line resolved to under the Drugs Rules, and what its Sale will freeze.
struct LineRegulatory {
    /// The position on the Sale's business date. This, and only this, is frozen on the line: it is
    /// the law of the day the document says the goods were sold.
    schemes: regulatory::ResolvedRegulatory,
    manufacturer: Option<(String, String)>,
    /// The gate: the STRICTER of the business-date position and the position on the day the Sale is
    /// actually being posted. See `resolve_regulatory_for_lines`.
    gate: regulatory::SaleGate,
    /// Phase 1M-C: the premises State and the Punjab position on the business date, frozen beside
    /// `schemes` in its own column.
    state: regulatory::StateAnswer,
    /// Phase 1M-C: Schedule H1 applies on the business date — the date its working entry records.
    h1_on_business_date: bool,
    /// Phase 1M-C: Schedule H1 applies on the business date or on any day the posting could be.
    h1_any_day: bool,
}

/// The calendar day(s) the posting instant can be in the store's own time.
///
/// Every store this software creates records `Asia/Kolkata`, which observes no daylight saving and
/// sits at a fixed UTC+05:30, so its day is exact. Any other recorded zone cannot be resolved here
/// without a time-zone database, so the answer widens to every day the instant could fall on
/// anywhere — yesterday, today and tomorrow in UTC — and the gate takes the strictest of them. That
/// can refuse a sale up to a day early around a commencement; it can never let one through late.
async fn posting_days(connection: &mut PoolConnection<Sqlite>) -> Result<Vec<String>, SaleError> {
    let zone: Option<String> =
        sqlx::query_scalar("SELECT business_time_zone FROM store_identity LIMIT 1")
            .fetch_optional(&mut **connection)
            .await
            .map_err(map_database_error)?;
    if zone.as_deref() == Some("Asia/Kolkata") {
        let today: String =
            sqlx::query_scalar("SELECT strftime('%Y-%m-%d','now','+5 hours','+30 minutes')")
                .fetch_one(&mut **connection)
                .await
                .map_err(map_database_error)?;
        return Ok(vec![today]);
    }
    let window: (String, String, String) = sqlx::query_as(
        "SELECT strftime('%Y-%m-%d','now','-1 day'),strftime('%Y-%m-%d','now'),\
         strftime('%Y-%m-%d','now','+1 day')",
    )
    .fetch_one(&mut **connection)
    .await
    .map_err(map_database_error)?;
    Ok(vec![window.0, window.1, window.2])
}

/// Which of two gates refuses harder: a missing workflow outranks a missing classification, which
/// outranks a prescription requirement, which outranks nothing.
fn stricter(first: regulatory::SaleGate, second: regulatory::SaleGate) -> regulatory::SaleGate {
    fn rank(gate: &regulatory::SaleGate) -> u8 {
        match gate {
            regulatory::SaleGate::Clear => 0,
            regulatory::SaleGate::PrescriptionRequired => 1,
            regulatory::SaleGate::Unresolved | regulatory::SaleGate::StateUnresolved { .. } => 2,
            regulatory::SaleGate::WorkflowUnavailable { .. }
            | regulatory::SaleGate::UnsupportedIntersection { .. }
            | regulatory::SaleGate::StateWorkflowUnavailable { .. } => 3,
        }
    }
    if rank(&second) > rank(&first) {
        second
    } else {
        first
    }
}

/// One pass over the draft's lines.
///
/// The frozen answer is the position on the Sale's BUSINESS DATE, because that is the law of the
/// day the document records. The gate, though, is the stricter of that answer and the position on
/// the day the Sale is actually being posted. Otherwise a draft dated to the day before a schedule
/// amendment commenced would resolve as unrestricted and post after commencement — a Schedule H1
/// drug handed over today, on paper yesterday, without the prescription rule 65(9)(a) requires.
/// A genuine late entry of an unrestricted sale is unaffected; the only sale this refuses that the
/// business date alone would allow is a drug scheduled since, which is the safe direction.
///
/// Reading the product's kind here rather than threading it through the tax computation keeps the
/// money path exactly as Phase 1H froze it.
async fn resolve_regulatory_for_lines(
    connection: &mut PoolConnection<Sqlite>,
    store_id: &str,
    lines: &[DraftLine],
    business_date: &str,
) -> Result<Vec<LineRegulatory>, SaleError> {
    let days = posting_days(connection).await?;
    // Phase 1M-C: the store's premises State, read under the same lock as everything else. Never
    // the GSTIN prefix, the place of supply, the customer or the device.
    let premises = regulatory::resolve_premises_state(connection, store_id)
        .await
        .map_err(map_database_error)?;
    let mut resolved = Vec::with_capacity(lines.len());
    for line in lines {
        let product_kind: String =
            sqlx::query_scalar("SELECT product_kind FROM products WHERE id=?")
                .bind(&line.product_id)
                .fetch_optional(&mut **connection)
                .await
                .map_err(map_database_error)?
                .ok_or(SaleError::NotFound)?;
        let schemes = regulatory::resolve_for_product(connection, &line.product_id, business_date)
            .await
            .map_err(map_database_error)?;
        let state = regulatory::resolve_state_for_product(
            connection,
            &line.product_id,
            business_date,
            premises.as_deref(),
        )
        .await
        .map_err(map_database_error)?;
        let h1_on_business_date = schemes.answer("schedule_h1") == regulatory::Resolution::Applies;
        let mut h1_any_day = h1_on_business_date;
        let mut gate = regulatory::combined_gate(&product_kind, &schemes, &state);
        for day in &days {
            let on_day = regulatory::resolve_for_product(connection, &line.product_id, day)
                .await
                .map_err(map_database_error)?;
            let state_on_day = regulatory::resolve_state_for_product(
                connection,
                &line.product_id,
                day,
                premises.as_deref(),
            )
            .await
            .map_err(map_database_error)?;
            h1_any_day |= on_day.answer("schedule_h1") == regulatory::Resolution::Applies;
            gate = stricter(
                gate,
                regulatory::combined_gate(&product_kind, &on_day, &state_on_day),
            );
        }
        let manufacturer =
            regulatory::resolve_manufacturer(connection, &line.product_id, business_date)
                .await
                .map_err(map_database_error)?;
        resolved.push(LineRegulatory {
            schemes,
            manufacturer,
            gate,
            state,
            h1_on_business_date,
            h1_any_day,
        });
    }
    Ok(resolved)
}

/// The operator's words for each unmet prescription requirement. Factual: nothing here says a
/// prescriber, a prescription or a pharmacist has been verified, because nothing has been.
fn prescription_issue_message(code: &str) -> &'static str {
    match code {
        "prescription_missing" => "This Schedule H line needs a prescription linked to it.",
        "prescription_not_found" => {
            "The linked prescription is not a prescription of this pharmacy."
        }
        "prescription_archived" => {
            "The linked prescription has been archived and cannot be dispensed."
        }
        "prescription_link_not_required" => "This line needs no prescription. Remove the link.",
        "prescription_substitution_not_permitted" => {
            "The prescription names a different product. Another preparation cannot be supplied in its place."
        }
        "prescription_quantity_exceeded" => "This is more than the prescription has left.",
        "prescription_repeat_not_authorised" => {
            "The prescription does not authorise dispensing it again."
        }
        "prescription_repeat_too_soon" => "The prescriber's stated interval has not yet passed.",
        "prescription_dated_after_supply" => "The prescription is dated after this supply.",
        "prescription_date_invalid" => "A date on the prescription could not be read.",
        "supervising_pharmacist_required" => {
            "Choose the registered pharmacist supervising this supply."
        }
        "supervising_pharmacist_invalid" => {
            "The chosen person is not a registered pharmacist on record who is valid today."
        }
        "endorsement_not_confirmed" => {
            "Confirm the pharmacy's name, address and the date have been written on the prescription."
        }
        "manufacturer_not_recorded" => {
            "No manufacturer is recorded for this product, so its prescription-register entry cannot name one."
        }
        "prescription_record_election_unresolved" => {
            "The pharmacy's rule 65(3)(2) election is not recorded, so this supply has no book to be entered in."
        }
        "prescription_memo_path_ineligible" => {
            "The memo book may be used only for a drug supplied from or in its original container. Confirm that, or record the supply otherwise."
        }
        "prescription_record_not_prepared" => {
            "Prepare the prescription-supply entry, have the registered pharmacist sign it by hand, and write its serial on the prescription."
        }
        "prescription_record_not_confirmed" => {
            "The prescription-supply entry is prepared but not confirmed: the registered pharmacist signs it by hand and its serial is written on the prescription."
        }
        "prescription_record_stale" => {
            "The prescription-supply entry no longer matches this sale. Void it and prepare a new one."
        }
        "schedule_h1_register_not_prepared" => {
            "Prepare the Schedule H1 working entry: its hard copy is placed in the separate H1 register and authenticated by the registered pharmacist before the sale is posted."
        }
        "schedule_h1_register_not_confirmed" => {
            "The Schedule H1 working entry is prepared but not confirmed: place its hard copy in the separate H1 register, have the registered pharmacist authenticate it by hand, then confirm both."
        }
        "schedule_h1_register_stale" => {
            "The Schedule H1 working entry no longer matches this sale. Void the prescription-supply entry and prepare both again."
        }
        "schedule_h1_veterinary_workflow_unresolved" => {
            "A Schedule H1 supply for an animal is not supported: rule 65(3)(1)(h) records the name of the patient, and how it applies to veterinary supply is unresolved."
        }
        "schedule_h1_commenced_after_business_date" => {
            "Schedule H1 applies to this product on the day it is being posted but not on the sale's business date. Date the sale today to record its H1 working entry."
        }
        _ => "A prescription requirement is not met.",
    }
}

/// Phase 1M-B — one unmet prescription requirement, named for the operator.
#[derive(Debug, Clone)]
pub(crate) struct PrescriptionIssue {
    /// The line it belongs to, or `None` for a requirement of the whole supply.
    line_number: Option<i64>,
    code: &'static str,
}

/// A dispensing the posting will write, once every requirement is met.
struct PlannedDispensing {
    sale_line_id: String,
    prescription_id: String,
    prescription_item_id: String,
    product_id: String,
    quantity_atoms: i64,
}

/// The supervising pharmacist's record, frozen onto every dispensing of this supply.
struct SupervisionSnapshot {
    professional_id: String,
    full_name: String,
    registration_number: String,
    registering_authority: Option<String>,
    endorsement_confirmed_by_user_id: String,
    endorsement_confirmed_at_utc: String,
}

/// What the counter sees about one line's prescription, with no patient details in it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LinePrescriptionSummary {
    required: bool,
    prescription_item_id: Option<String>,
    prescription_reference: Option<String>,
    /// What is left of the linked item's total before this Sale, in the line's base-unit atoms.
    remaining_atoms: Option<i64>,
    issue: Option<&'static str>,
}

/// Everything the Drugs Rules still ask of this supply, for the quote to show and posting to enforce.
struct DispensingPlan {
    planned: Vec<PlannedDispensing>,
    issues: Vec<PrescriptionIssue>,
    summaries: Vec<LinePrescriptionSummary>,
    supervision: Option<SupervisionSnapshot>,
    supervision_required: bool,
    /// Phase 1M-B: the rule 65(3) book this supply is entered in, from the election in force.
    record: Option<RecordPlan>,
    original_container_confirmed: bool,
    /// The rule 65(3)(1) line particulars the entry must carry, per prescription.
    expected_lines: Vec<(String, RecordLineFacts)>,
    /// This Sale's prepared or confirmed entries.
    live_records: Vec<LiveRecord>,
    /// Phase 1M-C: the rule 65(3)(1)(h) particulars each Schedule H1 line's working entry must carry.
    h1_expected: Vec<H1LineFacts>,
    /// Phase 1M-C: this Sale's prepared or confirmed Schedule H1 working entries.
    live_h1: Vec<LiveH1Entry>,
}

/// Which rule 65(3)(1) book, from which election.
struct RecordPlan {
    election_id: String,
    method: &'static str,
}

#[derive(Debug, FromRow)]
struct LinkedItem {
    prescription_id: String,
    store_id: String,
    status: String,
    reference: String,
    prescribed_on: String,
    repeat_authority: String,
    repeat_times: Option<i64>,
    repeat_interval_days: Option<i64>,
    product_id: String,
    prescribed_quantity_atoms: i64,
    subject_kind: String,
}

#[derive(Debug, FromRow)]
struct SupplyFacts {
    supervising_professional_id: Option<String>,
    prescription_endorsement_confirmed: i64,
    prescription_endorsement_confirmed_by_user_id: Option<String>,
    prescription_endorsement_confirmed_at_utc: Option<String>,
}

#[derive(Debug, FromRow)]
struct ProfessionalRow {
    store_id: String,
    status: String,
    capacity: String,
    full_name: String,
    registration_number: Option<String>,
    registering_authority: Option<String>,
    valid_from: Option<String>,
    valid_upto: Option<String>,
}

/// Phase 1M-B — judge every line against its prescription, from the ledger, under the write lock.
///
/// The same function answers the quote and gates the posting, so the counter is never told a line
/// is ready that posting would refuse. It reads only facts: the link each draft line carries, the
/// prescription and item it points at, every earlier dispensing and reversal, the professional
/// chosen to supervise, and the endorsement confirmation. It writes nothing.
async fn plan_dispensings(
    connection: &mut PoolConnection<Sqlite>,
    store_id: &str,
    sale_id: &str,
    lines: &[DraftLine],
    regulatory: &[LineRegulatory],
    business_date: &str,
) -> Result<DispensingPlan, SaleError> {
    let days = posting_days(connection).await?;
    let mut planned = Vec::new();
    let mut issues = Vec::new();
    let mut summaries = Vec::with_capacity(lines.len());
    // Atoms already claimed by earlier lines of THIS Sale, per item, so two lines cannot each take
    // the same remaining quantity.
    let mut claimed: Vec<(String, i64)> = Vec::new();
    let mut supervision_required = false;

    for (line, drugs) in lines.iter().zip(regulatory.iter()) {
        let required = drugs.gate == regulatory::SaleGate::PrescriptionRequired;
        supervision_required |= required;
        let link: Option<String> =
            sqlx::query_scalar("SELECT prescription_item_id FROM sale_lines WHERE id=?")
                .bind(&line.id)
                .fetch_one(&mut **connection)
                .await
                .map_err(map_database_error)?;
        let mut summary = LinePrescriptionSummary {
            required,
            prescription_item_id: link.clone(),
            prescription_reference: None,
            remaining_atoms: None,
            issue: None,
        };
        let mut flag = |summary: &mut LinePrescriptionSummary, code: &'static str| {
            summary.issue.get_or_insert(code);
            issues.push(PrescriptionIssue {
                line_number: Some(line.line_number),
                code,
            });
        };
        // Rule 65(3)(1)(f): a Schedule H entry names the manufacturer. With none recorded for the
        // Sale's date, the entry could not be made truthfully, so the supply is refused.
        if required && drugs.manufacturer.is_none() {
            flag(&mut summary, "manufacturer_not_recorded");
        }
        // Phase 1M-C: an H1 working entry records the Schedule H1 position of the business date.
        // A drug scheduled into H1 since that date cannot be entered truthfully under it.
        if required && drugs.h1_any_day && !drugs.h1_on_business_date {
            flag(&mut summary, "schedule_h1_commenced_after_business_date");
        }

        match (required, link) {
            (false, None) => {}
            // A link on a line that needs none is refused rather than silently ignored or silently
            // turned into a dispensing nobody asked for.
            (false, Some(_)) if drugs.gate == regulatory::SaleGate::Clear => {
                flag(&mut summary, "prescription_link_not_required")
            }
            // A line the gate refuses for another reason (Schedule H1, X, C, C(1), or an unresolved
            // position) may well carry a real prescription: it is not "a link nobody needs". The
            // gate names the refusal; the summary only reports which prescription is linked.
            (false, Some(item_id)) => {
                summary.prescription_reference = sqlx::query_scalar(
                    "SELECT prescription.reference FROM prescription_items item \
                     JOIN prescriptions prescription ON prescription.id=item.prescription_id \
                     WHERE item.id=? AND prescription.store_id=?",
                )
                .bind(&item_id)
                .bind(store_id)
                .fetch_optional(&mut **connection)
                .await
                .map_err(map_database_error)?;
            }
            (true, None) => flag(&mut summary, "prescription_missing"),
            (true, Some(item_id)) => {
                let item: Option<LinkedItem> = sqlx::query_as(
                    "SELECT prescription.id AS prescription_id,prescription.store_id,\
                     prescription.status,prescription.reference,prescription.prescribed_on,\
                     prescription.repeat_authority,prescription.repeat_times,\
                     prescription.repeat_interval_days,item.product_id,\
                     item.prescribed_quantity_atoms,prescription.subject_kind \
                     FROM prescription_items item \
                     JOIN prescriptions prescription ON prescription.id=item.prescription_id \
                     WHERE item.id=?",
                )
                .bind(&item_id)
                .fetch_optional(&mut **connection)
                .await
                .map_err(map_database_error)?;
                match item {
                    None => flag(&mut summary, "prescription_not_found"),
                    Some(item) if item.store_id != store_id => {
                        flag(&mut summary, "prescription_not_found")
                    }
                    Some(item) => {
                        summary.prescription_reference = Some(item.reference.clone());
                        let (dispensed, reversed, occasions, last): (
                            i64,
                            i64,
                            i64,
                            Option<String>,
                        ) = sqlx::query_as(
                            "SELECT \
                             (SELECT COALESCE(SUM(quantity_atoms),0) FROM prescription_dispensings \
                              WHERE prescription_item_id=?), \
                             (SELECT COALESCE(SUM(reversal.quantity_atoms),0) \
                              FROM prescription_dispensing_reversals reversal \
                              JOIN prescription_dispensings dispensing \
                                ON dispensing.id=reversal.dispensing_id \
                              WHERE dispensing.prescription_item_id=?), \
                             (SELECT COUNT(DISTINCT sale_document_id) FROM prescription_dispensings \
                              WHERE prescription_id=? AND sale_document_id<>?), \
                             (SELECT MAX(dispensed_on) FROM prescription_dispensings \
                              WHERE prescription_id=?)",
                        )
                        .bind(&item_id)
                        .bind(&item_id)
                        .bind(&item.prescription_id)
                        .bind(sale_id)
                        .bind(&item.prescription_id)
                        .fetch_one(&mut **connection)
                        .await
                        .map_err(map_database_error)?;
                        let already_in_sale = claimed
                            .iter()
                            .filter(|(claimed_item, _)| *claimed_item == item_id)
                            .map(|(_, atoms)| atoms)
                            .sum::<i64>();
                        let repeat = prescriptions::RepeatAuthority::parse(
                            &item.repeat_authority,
                            item.repeat_times,
                        );
                        let authority = repeat.map(|repeat| prescriptions::ItemAuthority {
                            prescribed_product_id: &item.product_id,
                            prescribed_quantity_atoms: item.prescribed_quantity_atoms,
                            net_dispensed_atoms: dispensed - reversed + already_in_sale,
                            earlier_occasions: occasions,
                            last_dispensed_on: last.as_deref(),
                            repeat,
                            repeat_interval_days: item.repeat_interval_days,
                            prescribed_on: &item.prescribed_on,
                        });
                        summary.remaining_atoms =
                            authority.as_ref().map(prescriptions::remaining_atoms);

                        if item.status != "active" {
                            flag(&mut summary, "prescription_archived");
                        } else if drugs.h1_any_day && item.subject_kind != "human" {
                            // Phase 1M-C: an unresolved veterinary-H1 workflow. Rule 65(3)(1)(h) asks
                            // for "the name of the patient"; the owner of an animal is not put in
                            // its place without authority. Not a statement that the supply is
                            // prohibited — only that AUSHADHARTH does not support it.
                            flag(&mut summary, "schedule_h1_veterinary_workflow_unresolved");
                        } else if let Some(authority) = authority {
                            let request = prescriptions::DispenseRequest {
                                product_id: &line.product_id,
                                quantity_atoms: line.quantity_atoms,
                                business_date,
                                posting_days: &days,
                            };
                            match prescriptions::check_dispense(&authority, &request) {
                                Ok(()) => {
                                    claimed.push((item_id.clone(), line.quantity_atoms));
                                    planned.push(PlannedDispensing {
                                        sale_line_id: line.id.clone(),
                                        prescription_id: item.prescription_id.clone(),
                                        prescription_item_id: item_id.clone(),
                                        product_id: line.product_id.clone(),
                                        quantity_atoms: line.quantity_atoms,
                                    });
                                }
                                Err(refusal) => flag(&mut summary, refusal.code()),
                            }
                        } else {
                            // A stored repeat authority this code cannot read is not authority.
                            flag(&mut summary, "prescription_repeat_not_authorised");
                        }
                    }
                }
            }
        }
        summaries.push(summary);
    }

    let mut supervision = None;
    if supervision_required {
        let supply: SupplyFacts = sqlx::query_as(
            "SELECT supervising_professional_id,prescription_endorsement_confirmed,\
             prescription_endorsement_confirmed_by_user_id,\
             prescription_endorsement_confirmed_at_utc FROM sale_documents WHERE id=?",
        )
        .bind(sale_id)
        .fetch_one(&mut **connection)
        .await
        .map_err(map_database_error)?;

        // Rule 65(2): a registered pharmacist's own record, valid on the day the Sale records AND
        // the day it is actually posted. `users.role` is never consulted.
        match supply.supervising_professional_id.as_deref() {
            None => issues.push(PrescriptionIssue {
                line_number: None,
                code: "supervising_pharmacist_required",
            }),
            Some(professional_id) => {
                let row: Option<ProfessionalRow> = sqlx::query_as(
                    "SELECT store_id,status,capacity,full_name,registration_number,\
                     registering_authority,valid_from,valid_upto \
                     FROM store_professionals WHERE id=?",
                )
                .bind(professional_id)
                .fetch_optional(&mut **connection)
                .await
                .map_err(map_database_error)?;
                let mut check_days: Vec<&str> = days.iter().map(String::as_str).collect();
                check_days.push(business_date);
                let valid = match &row {
                    Some(row)
                        if row.store_id == store_id
                            && row.status == "active"
                            && row.capacity == "registered_pharmacist"
                            && row
                                .registration_number
                                .as_deref()
                                .is_some_and(|number| !number.trim().is_empty()) =>
                    {
                        prescriptions::professional_valid_on(
                            row.valid_from.as_deref(),
                            row.valid_upto.as_deref(),
                            &check_days,
                        )
                        .unwrap_or(false)
                    }
                    _ => false,
                };
                match (valid, row) {
                    (true, Some(row)) => {
                        if supply.prescription_endorsement_confirmed == 1 {
                            match (
                                supply.prescription_endorsement_confirmed_by_user_id,
                                supply.prescription_endorsement_confirmed_at_utc,
                            ) {
                                (Some(by), Some(at)) => {
                                    supervision = Some(SupervisionSnapshot {
                                        professional_id: professional_id.to_owned(),
                                        full_name: row.full_name,
                                        registration_number: row
                                            .registration_number
                                            .unwrap_or_default(),
                                        registering_authority: row.registering_authority,
                                        endorsement_confirmed_by_user_id: by,
                                        endorsement_confirmed_at_utc: at,
                                    });
                                }
                                _ => issues.push(PrescriptionIssue {
                                    line_number: None,
                                    code: "endorsement_not_confirmed",
                                }),
                            }
                        } else {
                            issues.push(PrescriptionIssue {
                                line_number: None,
                                code: "endorsement_not_confirmed",
                            });
                        }
                    }
                    _ => issues.push(PrescriptionIssue {
                        line_number: None,
                        code: "supervising_pharmacist_invalid",
                    }),
                }
            }
        }
    }

    // Phase 1M-B — the rule 65(3) record the supply will be entered in.
    let mut record = None;
    let mut original_container_confirmed = false;
    let mut expected_lines = Vec::new();
    let mut live_records = Vec::new();
    let mut h1_expected = Vec::new();
    let mut live_h1 = Vec::new();
    if supervision_required {
        original_container_confirmed = sqlx::query_scalar::<_, i64>(
            "SELECT prescription_original_container_confirmed FROM sale_documents WHERE id=?",
        )
        .bind(sale_id)
        .fetch_one(&mut **connection)
        .await
        .map_err(map_database_error)?
            == 1;
        let mut record_days: Vec<&str> = days.iter().map(String::as_str).collect();
        record_days.push(business_date);
        record = resolve_record_election(connection, store_id, &record_days).await?;
        match &record {
            None => issues.push(PrescriptionIssue {
                line_number: None,
                code: "prescription_record_election_unresolved",
            }),
            // First proviso: the memo book is available only for a drug not compounded here and
            // supplied from or in its original container. Unattested, it is not available — and
            // the supply is refused rather than entered in a book the Store did not elect.
            Some(plan)
                if plan.method == "cash_or_credit_memo_book" && !original_container_confirmed =>
            {
                issues.push(PrescriptionIssue {
                    line_number: None,
                    code: "prescription_memo_path_ineligible",
                })
            }
            Some(_) => {}
        }
        // Signature before supply. Once every other requirement is met, the last one is the entry
        // itself: prepared, signed by the registered pharmacist's own hand, its serial written on the
        // prescription, both confirmed — and still exactly what is about to be supplied.
        if issues.is_empty() {
            expected_lines = expected_record_lines(connection, lines, regulatory, &planned).await?;
            h1_expected = expected_h1_lines(connection, lines, regulatory, &planned).await?;
            if let (Some(plan_record), Some(snapshot)) = (record.as_ref(), supervision.as_ref()) {
                live_records = evaluate_records(
                    connection,
                    sale_id,
                    business_date,
                    plan_record,
                    snapshot,
                    original_container_confirmed,
                    &planned,
                    &expected_lines,
                    &mut issues,
                )
                .await?;
                // Phase 1M-C — the second, separate layer: the Schedule H1 working entry of every
                // H1 line, confirmed (hard copy placed and authenticated) and still exact.
                live_h1 = evaluate_h1_entries(
                    connection,
                    sale_id,
                    business_date,
                    snapshot,
                    &h1_expected,
                    &mut issues,
                )
                .await?;
            }
        }
    }

    Ok(DispensingPlan {
        planned,
        issues,
        summaries,
        supervision,
        supervision_required,
        record,
        original_container_confirmed,
        expected_lines,
        live_records,
        h1_expected,
        live_h1,
    })
}

/// Phase 1M-C — the rule 65(3)(1)(h) particulars of one Schedule H1 line, from exactly the sources
/// the database proves the working entry against: the prescription's prescriber and patient, the
/// product's name, and the line's quantity.
#[derive(Debug, Clone, PartialEq, FromRow)]
struct H1LineFacts {
    sale_line_id: String,
    prescription_id: String,
    prescription_item_id: String,
    product_id: String,
    prescriber_name: String,
    prescriber_address: String,
    patient_name: String,
    drug_name: String,
    quantity_atoms: i64,
    quantity_unit_label: Option<String>,
}

/// One entry per planned dispensing whose line is Schedule H1 on the business date. Two H1 drugs
/// are two entries, never one merged row.
async fn expected_h1_lines(
    connection: &mut PoolConnection<Sqlite>,
    lines: &[DraftLine],
    regulatory: &[LineRegulatory],
    planned: &[PlannedDispensing],
) -> Result<Vec<H1LineFacts>, SaleError> {
    let mut expected = Vec::new();
    for planned in planned {
        let Some(index) = lines
            .iter()
            .position(|line| line.id == planned.sale_line_id)
        else {
            return Err(SaleError::Internal);
        };
        if !regulatory[index].h1_on_business_date {
            continue;
        }
        let facts: H1LineFacts = sqlx::query_as(
            "SELECT ? AS sale_line_id,prescription.id AS prescription_id,\
             ? AS prescription_item_id,? AS product_id,prescription.prescriber_name,\
             prescription.prescriber_address,prescription.subject_name AS patient_name,\
             product.display_name AS drug_name,? AS quantity_atoms,\
             unit.display_name AS quantity_unit_label \
             FROM prescriptions prescription JOIN products product ON product.id=? \
             LEFT JOIN units_of_measure unit ON unit.id=product.base_unit_id \
             WHERE prescription.id=?",
        )
        .bind(&planned.sale_line_id)
        .bind(&planned.prescription_item_id)
        .bind(&planned.product_id)
        .bind(planned.quantity_atoms)
        .bind(&planned.product_id)
        .bind(&planned.prescription_id)
        .fetch_one(&mut **connection)
        .await
        .map_err(map_database_error)?;
        expected.push(facts);
    }
    Ok(expected)
}

/// A live (prepared or confirmed) Schedule H1 working entry of this Sale.
#[derive(Debug, Clone, FromRow)]
struct LiveH1Entry {
    id: String,
    sale_line_id: String,
    status: String,
    prescription_id: String,
    prescription_item_id: String,
    product_id: String,
    prescriber_name: String,
    prescriber_address: String,
    patient_name: String,
    drug_name: String,
    quantity_atoms: i64,
    quantity_unit_label: Option<String>,
    date_of_supply: String,
    supervising_professional_id: String,
    supervising_professional_name: String,
    supervising_registration_number: String,
}

/// Where this Sale's Schedule H1 working entries stand: one per H1 line, confirmed, and still
/// exactly the prescriber, patient, drug, quantity, date and pharmacist of the supply.
async fn evaluate_h1_entries(
    connection: &mut PoolConnection<Sqlite>,
    sale_id: &str,
    business_date: &str,
    supervision: &SupervisionSnapshot,
    expected: &[H1LineFacts],
    issues: &mut Vec<PrescriptionIssue>,
) -> Result<Vec<LiveH1Entry>, SaleError> {
    let live: Vec<LiveH1Entry> = sqlx::query_as(
        "SELECT id,sale_line_id,status,prescription_id,prescription_item_id,product_id,\
         prescriber_name,prescriber_address,patient_name,drug_name,quantity_atoms,\
         quantity_unit_label,date_of_supply,supervising_professional_id,\
         supervising_professional_name,supervising_registration_number \
         FROM prescription_h1_register_entries \
         WHERE sale_document_id=? AND status IN ('prepared','confirmed') ORDER BY reference_value",
    )
    .bind(sale_id)
    .fetch_all(&mut **connection)
    .await
    .map_err(map_database_error)?;
    let mut raise = |code: &'static str| {
        if !issues.iter().any(|issue| issue.code == code) {
            issues.push(PrescriptionIssue {
                line_number: None,
                code,
            });
        }
    };
    for facts in expected {
        let Some(entry) = live
            .iter()
            .find(|entry| entry.sale_line_id == facts.sale_line_id)
        else {
            raise("schedule_h1_register_not_prepared");
            continue;
        };
        let same = entry.prescription_id == facts.prescription_id
            && entry.prescription_item_id == facts.prescription_item_id
            && entry.product_id == facts.product_id
            && entry.prescriber_name == facts.prescriber_name
            && entry.prescriber_address == facts.prescriber_address
            && entry.patient_name == facts.patient_name
            && entry.drug_name == facts.drug_name
            && entry.quantity_atoms == facts.quantity_atoms
            && entry.quantity_unit_label == facts.quantity_unit_label
            && entry.date_of_supply == business_date
            && entry.supervising_professional_id == supervision.professional_id
            && entry.supervising_professional_name == supervision.full_name
            && entry.supervising_registration_number == supervision.registration_number;
        if !same {
            raise("schedule_h1_register_stale");
        } else if entry.status == "prepared" {
            raise("schedule_h1_register_not_confirmed");
        }
    }
    // An entry for a line that is no longer Schedule H1, or no longer dispensed, cannot be left
    // behind to be finalized.
    if live.iter().any(|entry| {
        !expected
            .iter()
            .any(|facts| facts.sale_line_id == entry.sale_line_id)
    }) {
        raise("schedule_h1_register_stale");
    }
    Ok(live)
}

/// The rule 65(3)(1) particulars (e) and (f) of one Sale line, read from exactly the sources posting
/// freezes the line from, so the entry signed before the supply can be proved identical to it.
#[derive(Debug, Clone, PartialEq, FromRow)]
struct RecordLineFacts {
    sale_line_id: String,
    drug_name: String,
    quantity_atoms: i64,
    quantity_unit_label: Option<String>,
    manufacturer_name: String,
    batch_number: String,
    batch_expires_on: Option<String>,
}

/// The expected line facts, with the prescription each line is dispensed against.
async fn expected_record_lines(
    connection: &mut PoolConnection<Sqlite>,
    lines: &[DraftLine],
    regulatory: &[LineRegulatory],
    planned: &[PlannedDispensing],
) -> Result<Vec<(String, RecordLineFacts)>, SaleError> {
    let mut expected = Vec::with_capacity(planned.len());
    for planned in planned {
        let Some(index) = lines
            .iter()
            .position(|line| line.id == planned.sale_line_id)
        else {
            return Err(SaleError::Internal);
        };
        let line = &lines[index];
        let manufacturer = regulatory[index]
            .manufacturer
            .as_ref()
            .map(|(_, name)| name.clone())
            .ok_or(SaleError::Internal)?;
        let (drug_name, quantity_unit_label): (String, Option<String>) = sqlx::query_as(
            "SELECT product.display_name,unit.display_name FROM products product \
             LEFT JOIN units_of_measure unit ON unit.id=product.base_unit_id WHERE product.id=?",
        )
        .bind(&line.product_id)
        .fetch_one(&mut **connection)
        .await
        .map_err(map_database_error)?;
        let (batch_number, batch_expires_on): (String, Option<String>) =
            sqlx::query_as("SELECT batch_number,expires_on FROM product_batches WHERE id=?")
                .bind(&line.batch_id)
                .fetch_one(&mut **connection)
                .await
                .map_err(map_database_error)?;
        expected.push((
            planned.prescription_id.clone(),
            RecordLineFacts {
                sale_line_id: line.id.clone(),
                drug_name,
                quantity_atoms: planned.quantity_atoms,
                quantity_unit_label,
                manufacturer_name: manufacturer,
                batch_number,
                batch_expires_on,
            },
        ));
    }
    Ok(expected)
}

/// A live (prepared or confirmed) entry of this Sale, and the prescription it records.
#[derive(Debug, Clone, FromRow)]
struct LiveRecord {
    id: String,
    prescription_id: String,
    status: String,
    election_id: String,
    date_of_supply: String,
    supervising_professional_id: String,
    supervising_professional_name: String,
    supervising_registration_number: String,
    original_container_confirmed: i64,
}

/// Where this Sale's entries stand: one per prescription dispensed from, confirmed, and still
/// matching the election, the date, the pharmacist, the attestation and every line.
#[allow(clippy::too_many_arguments)]
async fn evaluate_records(
    connection: &mut PoolConnection<Sqlite>,
    sale_id: &str,
    business_date: &str,
    plan_record: &RecordPlan,
    supervision: &SupervisionSnapshot,
    original_container_confirmed: bool,
    planned: &[PlannedDispensing],
    expected: &[(String, RecordLineFacts)],
    issues: &mut Vec<PrescriptionIssue>,
) -> Result<Vec<LiveRecord>, SaleError> {
    let live: Vec<LiveRecord> = sqlx::query_as(
        "SELECT id,prescription_id,status,election_id,date_of_supply,supervising_professional_id,\
         supervising_professional_name,supervising_registration_number,\
         original_container_confirmed FROM prescription_supply_records \
         WHERE sale_document_id=? AND status IN ('prepared','confirmed')",
    )
    .bind(sale_id)
    .fetch_all(&mut **connection)
    .await
    .map_err(map_database_error)?;
    let mut raise = |code: &'static str| {
        if !issues.iter().any(|issue| issue.code == code) {
            issues.push(PrescriptionIssue {
                line_number: None,
                code,
            });
        }
    };
    let mut needed: Vec<&str> = Vec::new();
    for planned in planned {
        if !needed.contains(&planned.prescription_id.as_str()) {
            needed.push(&planned.prescription_id);
        }
    }
    for prescription_id in &needed {
        let Some(entry) = live
            .iter()
            .find(|entry| entry.prescription_id == *prescription_id)
        else {
            raise("prescription_record_not_prepared");
            continue;
        };
        if entry.status == "prepared" {
            raise("prescription_record_not_confirmed");
            continue;
        }
        let mut recorded: Vec<RecordLineFacts> = sqlx::query_as(
            "SELECT sale_line_id,drug_name,quantity_atoms,quantity_unit_label,manufacturer_name,\
             batch_number,batch_expires_on FROM prescription_supply_record_lines WHERE record_id=? \
             ORDER BY sale_line_id",
        )
        .bind(&entry.id)
        .fetch_all(&mut **connection)
        .await
        .map_err(map_database_error)?;
        let mut wanted: Vec<RecordLineFacts> = expected
            .iter()
            .filter(|(prescription, _)| prescription == prescription_id)
            .map(|(_, facts)| facts.clone())
            .collect();
        recorded.sort_by(|left, right| left.sale_line_id.cmp(&right.sale_line_id));
        wanted.sort_by(|left, right| left.sale_line_id.cmp(&right.sale_line_id));
        let same = entry.election_id == plan_record.election_id
            && entry.date_of_supply == business_date
            && entry.supervising_professional_id == supervision.professional_id
            && entry.supervising_professional_name == supervision.full_name
            && entry.supervising_registration_number == supervision.registration_number
            && (entry.original_container_confirmed == 1) == original_container_confirmed
            && recorded == wanted;
        if !same {
            raise("prescription_record_stale");
        }
    }
    // An entry for a prescription this Sale no longer dispenses from cannot be left behind.
    if live
        .iter()
        .any(|entry| !needed.contains(&entry.prescription_id.as_str()))
    {
        raise("prescription_record_stale");
    }
    Ok(live)
}

/// The record-state issues, which only preparing, confirming or voiding an entry resolves.
const RECORD_STATE_ISSUES: [&str; 6] = [
    "prescription_record_not_prepared",
    "prescription_record_not_confirmed",
    "prescription_record_stale",
    "schedule_h1_register_not_prepared",
    "schedule_h1_register_not_confirmed",
    "schedule_h1_register_stale",
];

/// Rule 65(3)(2): the election in force for this supply.
///
/// It must be the same election on the Sale's business date and on every day the posting could be
/// happening, so a backdated Sale cannot be entered under a method the Store has since left, and an
/// election being changed at this moment yields one answer or a refusal — never a mixture. No
/// election, or a disagreement between the days, is no election at all.
async fn resolve_record_election(
    connection: &mut PoolConnection<Sqlite>,
    store_id: &str,
    days: &[&str],
) -> Result<Option<RecordPlan>, SaleError> {
    let mut found: Option<(String, String)> = None;
    for day in days {
        let election: Option<(String, String)> = sqlx::query_as(
            "SELECT id,method FROM store_record_elections \
             WHERE store_id=? AND election='rule_65_3_prescription_supply' AND status='active' \
               AND effective_from<=? AND (effective_to IS NULL OR effective_to>?)",
        )
        .bind(store_id)
        .bind(day)
        .bind(day)
        .fetch_optional(&mut **connection)
        .await
        .map_err(map_database_error)?;
        match (&found, election) {
            (_, None) => return Ok(None),
            (None, Some(election)) => found = Some(election),
            (Some(existing), Some(election)) if *existing != election => return Ok(None),
            _ => {}
        }
    }
    Ok(found.and_then(|(election_id, method)| {
        let method = match method.as_str() {
            "prescription_register" => "prescription_register",
            "cash_or_credit_memo_book" => "cash_or_credit_memo_book",
            _ => return None,
        };
        Some(RecordPlan {
            election_id,
            method,
        })
    }))
}

/// The Drugs Rules scheme gate, shared by preparing an entry and by posting, so preparing a rule 65
/// record can never be a way round it: an unresolved medicine, Schedule X, a Schedule C / C(1) line,
/// an NDPS-intersection H1 line, or a Punjab-boundary line is refused here, naming the line, before
/// anything is written.
fn refuse_unsupported_schemes(
    lines: &[DraftLine],
    regulatory: &[LineRegulatory],
) -> Result<(), SaleError> {
    for (line, resolved) in lines.iter().zip(regulatory.iter()) {
        match resolved.gate {
            regulatory::SaleGate::Clear | regulatory::SaleGate::PrescriptionRequired => {}
            regulatory::SaleGate::Unresolved => {
                return Err(SaleError::RegulatoryClassificationUnresolved {
                    line_number: line.line_number,
                });
            }
            regulatory::SaleGate::UnsupportedIntersection { .. } => {
                return Err(SaleError::ScheduleH1NdpsWorkflowNotAvailable {
                    line_number: line.line_number,
                });
            }
            regulatory::SaleGate::StateWorkflowUnavailable { .. } => {
                return Err(SaleError::StateRestrictedDrugWorkflowNotAvailable {
                    line_number: line.line_number,
                });
            }
            regulatory::SaleGate::StateUnresolved { .. } => {
                return Err(SaleError::StateRegulatoryPositionUnresolved {
                    line_number: line.line_number,
                });
            }
            regulatory::SaleGate::WorkflowUnavailable {
                scheme: "schedule_x",
            } => {
                return Err(SaleError::ScheduleXWorkflowNotAvailable {
                    line_number: line.line_number,
                });
            }
            regulatory::SaleGate::WorkflowUnavailable { scheme } => {
                return Err(SaleError::RegulatedSaleWorkflowNotAvailable {
                    line_number: line.line_number,
                    scheme,
                });
            }
        }
    }
    Ok(())
}

/// A draft whose rule 65(3) entry is prepared or confirmed is held as it is: the pharmacist signs
/// the entry for exactly these lines, this pharmacist, this date. To change the Sale, void the entry
/// first; its serial is kept and never reused.
async fn ensure_no_live_record(
    connection: &mut SqliteConnection,
    sale_id: &str,
) -> Result<(), SaleError> {
    let live: Option<String> = sqlx::query_scalar(
        "SELECT serial_number FROM prescription_supply_records \
         WHERE sale_document_id=? AND status IN ('prepared','confirmed') LIMIT 1",
    )
    .bind(sale_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(map_database_error)?;
    if let Some(serial) = live {
        return Err(SaleError::PrescriptionRecordPrepared { serial });
    }
    let live_h1: Option<String> = sqlx::query_scalar(
        "SELECT reference FROM prescription_h1_register_entries \
         WHERE sale_document_id=? AND status IN ('prepared','confirmed') LIMIT 1",
    )
    .bind(sale_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(map_database_error)?;
    match live_h1 {
        Some(serial) => Err(SaleError::PrescriptionRecordPrepared { serial }),
        None => Ok(()),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrepareRecordsRequest {
    expected_revision: i64,
}

/// Prepares the rule 65(3)(1) entry for each prescription this Sale dispenses from.
///
/// Every other requirement must already be met: the scheme gate, the prescription, the pharmacist,
/// the endorsement, the election and, for the memo book, the original-container attestation. Then,
/// under the write lock, each entry takes the next serial of the elected book and the particulars
/// the pharmacist will sign by hand. Nothing is sold: no stock moves, no dispensing is written, no
/// tender is taken and the Sale stays a draft.
async fn prepare_prescription_records(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<PrepareRecordsRequest>,
) -> Result<Json<SaleDetailResponse>, SaleError> {
    let actor = require_dispenser(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let mut connection = state
        .pool
        .acquire()
        .await
        .map_err(|_| SaleError::Internal)?;
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
    let outcome =
        prepare_within_transaction(&mut connection, &id, request.expected_revision, &actor.id)
            .await;
    match outcome {
        Ok(()) => {
            sqlx::query("COMMIT")
                .execute(&mut *connection)
                .await
                .map_err(map_database_error)?;
            drop(connection);
            Ok(Json(fetch_detail(&state.pool, &id).await?))
        }
        Err(error) => {
            let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
            Err(error)
        }
    }
}

async fn prepare_within_transaction(
    connection: &mut PoolConnection<Sqlite>,
    id: &str,
    expected_revision: i64,
    actor_id: &str,
) -> Result<(), SaleError> {
    let header: Option<(String, String, String, i64)> = sqlx::query_as(
        "SELECT store_id,business_date,status,revision FROM sale_documents WHERE id=?",
    )
    .bind(id)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?;
    let (store_id, business_date, status, revision) = header.ok_or(SaleError::NotFound)?;
    if status != "draft" {
        return Err(SaleError::NotDraft);
    }
    if revision != expected_revision {
        return Err(SaleError::Revision {
            expected: expected_revision,
            current: revision,
        });
    }
    let lines = load_lines(connection, id).await?;
    let regulatory =
        resolve_regulatory_for_lines(connection, &store_id, &lines, &business_date).await?;
    refuse_unsupported_schemes(&lines, &regulatory)?;
    let plan = plan_dispensings(
        connection,
        &store_id,
        id,
        &lines,
        &regulatory,
        &business_date,
    )
    .await?;
    if !plan.supervision_required {
        return Err(validation_of(
            "lines",
            "no line of this sale needs a prescription-supply record",
        ));
    }
    if plan
        .issues
        .iter()
        .any(|issue| issue.code == "prescription_record_election_unresolved")
    {
        return Err(SaleError::PrescriptionRecordElectionUnresolved);
    }
    let blocking: Vec<PrescriptionIssue> = plan
        .issues
        .iter()
        .filter(|issue| !RECORD_STATE_ISSUES.contains(&issue.code))
        .cloned()
        .collect();
    if !blocking.is_empty() {
        return Err(SaleError::PrescriptionRequirementsIncomplete(blocking));
    }
    // A live entry that no longer matches must be voided, not overwritten: its serial may be on
    // paper already.
    for stale in ["prescription_record_stale", "schedule_h1_register_stale"] {
        if plan.issues.iter().any(|issue| issue.code == stale) {
            return Err(SaleError::PrescriptionRequirementsIncomplete(vec![
                PrescriptionIssue {
                    line_number: None,
                    code: stale,
                },
            ]));
        }
    }
    let record = plan.record.as_ref().ok_or(SaleError::Internal)?;
    let supervision = plan.supervision.as_ref().ok_or(SaleError::Internal)?;
    let now = sqlx::query_scalar::<_, String>("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')")
        .fetch_one(&mut **connection)
        .await
        .map_err(map_database_error)?;
    let prefix = if record.method == "prescription_register" {
        "PR"
    } else {
        "PM"
    };
    let mut needed: Vec<&str> = Vec::new();
    for planned in &plan.planned {
        if !needed.contains(&planned.prescription_id.as_str()) {
            needed.push(&planned.prescription_id);
        }
    }
    let mut prepared_any = false;
    // Which rule 65(3)(1) entry records each prescription: the live one, or the one written now.
    let mut record_for: Vec<(String, String)> = plan
        .live_records
        .iter()
        .map(|entry| (entry.prescription_id.clone(), entry.id.clone()))
        .collect();
    for prescription_id in needed {
        if plan
            .live_records
            .iter()
            .any(|entry| entry.prescription_id == prescription_id)
        {
            continue;
        }
        // The next serial of this book, void entries included, so no serial is ever issued twice.
        let serial_value: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(serial_value),0)+1 FROM prescription_supply_records \
             WHERE store_id=? AND record_method=?",
        )
        .bind(&store_id)
        .bind(record.method)
        .fetch_one(&mut **connection)
        .await
        .map_err(map_database_error)?;
        let serial_number = format!("{prefix}-{serial_value:06}");
        // Second proviso: a repeat supply may be entered with "a sufficient reference to an entry
        // in the register recording the dispensing of the medicine on the previous occasion".
        // Every particular is entered anyway; the reference is added beside them.
        let previous: Option<String> = sqlx::query_scalar(
            "SELECT id FROM prescription_supply_records WHERE prescription_id=? \
             AND status='finalized' ORDER BY finalized_at_utc DESC,serial_value DESC LIMIT 1",
        )
        .bind(prescription_id)
        .fetch_optional(&mut **connection)
        .await
        .map_err(map_database_error)?;
        let record_id = Uuid::now_v7().to_string();
        record_for.push((prescription_id.to_owned(), record_id.clone()));
        sqlx::query(
            "INSERT INTO prescription_supply_records (id,store_id,sale_document_id,prescription_id,\
             record_method,election_id,serial_value,serial_number,date_of_supply,prescriber_name,\
             prescriber_address,subject_kind,subject_name,subject_address,\
             supervising_professional_id,supervising_professional_name,\
             supervising_registration_number,original_container_confirmed,previous_record_id,\
             prepared_by_user_id,prepared_at_utc) \
             SELECT ?,?,?,prescription.id,?,?,?,?,?,prescription.prescriber_name,\
             prescription.prescriber_address,prescription.subject_kind,prescription.subject_name,\
             prescription.subject_address,?,?,?,?,?,?,? FROM prescriptions prescription WHERE id=?",
        )
        .bind(&record_id)
        .bind(&store_id)
        .bind(id)
        .bind(record.method)
        .bind(&record.election_id)
        .bind(serial_value)
        .bind(&serial_number)
        .bind(&business_date)
        .bind(&supervision.professional_id)
        .bind(&supervision.full_name)
        .bind(&supervision.registration_number)
        .bind(i64::from(plan.original_container_confirmed))
        .bind(&previous)
        .bind(actor_id)
        .bind(&now)
        .bind(prescription_id)
        .execute(&mut **connection)
        .await
        .map_err(map_database_error)?;
        for (_, facts) in plan
            .expected_lines
            .iter()
            .filter(|(prescription, _)| prescription == prescription_id)
        {
            sqlx::query(
                "INSERT INTO prescription_supply_record_lines (id,record_id,sale_line_id,drug_name,\
                 quantity_atoms,quantity_unit_label,manufacturer_name,batch_number,\
                 batch_expires_on,created_at_utc) VALUES (?,?,?,?,?,?,?,?,?,?)",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(&record_id)
            .bind(&facts.sale_line_id)
            .bind(&facts.drug_name)
            .bind(facts.quantity_atoms)
            .bind(&facts.quantity_unit_label)
            .bind(&facts.manufacturer_name)
            .bind(&facts.batch_number)
            .bind(&facts.batch_expires_on)
            .bind(&now)
            .execute(&mut **connection)
            .await
            .map_err(map_database_error)?;
        }
        // Identifiers and the serial only; the particulars stay in the entry.
        audit_entity(
            connection,
            "prescription_supply_record",
            &record_id,
            "created",
            &json!({
                "saleDocumentId": id,
                "prescriptionId": prescription_id,
                "recordMethod": record.method,
                "serialNumber": serial_number,
                "electionId": record.election_id,
                "previousRecordId": previous,
                "status": "prepared",
            }),
            actor_id,
            &now,
        )
        .await?;
        prepared_any = true;
    }
    // Phase 1M-C — the separate Schedule H1 working entry of each H1 line: one per line, each with
    // the next AUSHADHARTH Reference (internal; not a register serial). The database proves every
    // particular against the prescription, the product and the line.
    for facts in &plan.h1_expected {
        if plan
            .live_h1
            .iter()
            .any(|entry| entry.sale_line_id == facts.sale_line_id)
        {
            continue;
        }
        let supply_record_id = record_for
            .iter()
            .find(|(prescription, _)| *prescription == facts.prescription_id)
            .map(|(_, record)| record.clone())
            .ok_or(SaleError::Internal)?;
        let reference_value: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(reference_value),0)+1 FROM prescription_h1_register_entries \
             WHERE store_id=?",
        )
        .bind(&store_id)
        .fetch_one(&mut **connection)
        .await
        .map_err(map_database_error)?;
        let reference = format!("AH1-{reference_value:06}");
        let entry_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO prescription_h1_register_entries (id,store_id,sale_document_id,\
             sale_line_id,supply_record_id,prescription_id,prescription_item_id,product_id,\
             reference_value,reference,date_of_supply,prescriber_name,prescriber_address,\
             patient_name,drug_name,quantity_atoms,quantity_unit_label,\
             supervising_professional_id,supervising_professional_name,\
             supervising_registration_number,prepared_by_user_id,prepared_at_utc) \
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(&entry_id)
        .bind(&store_id)
        .bind(id)
        .bind(&facts.sale_line_id)
        .bind(&supply_record_id)
        .bind(&facts.prescription_id)
        .bind(&facts.prescription_item_id)
        .bind(&facts.product_id)
        .bind(reference_value)
        .bind(&reference)
        .bind(&business_date)
        .bind(&facts.prescriber_name)
        .bind(&facts.prescriber_address)
        .bind(&facts.patient_name)
        .bind(&facts.drug_name)
        .bind(facts.quantity_atoms)
        .bind(&facts.quantity_unit_label)
        .bind(&supervision.professional_id)
        .bind(&supervision.full_name)
        .bind(&supervision.registration_number)
        .bind(actor_id)
        .bind(&now)
        .execute(&mut **connection)
        .await
        .map_err(map_database_error)?;
        // Identifiers, the reference and the state only: never the patient or the prescriber.
        audit_entity(
            connection,
            "prescription_h1_register_entry",
            &entry_id,
            "created",
            &json!({
                "saleDocumentId": id,
                "saleLineId": facts.sale_line_id,
                "supplyRecordId": supply_record_id,
                "reference": reference,
                "status": "prepared",
            }),
            actor_id,
            &now,
        )
        .await?;
        prepared_any = true;
    }
    if prepared_any {
        // The draft's revision moves, so a screen that read it before the preparation must reload.
        sqlx::query("UPDATE sale_documents SET revision=revision+1,updated_at_utc=? WHERE id=?")
            .bind(&now)
            .bind(id)
            .execute(&mut **connection)
            .await
            .map_err(map_database_error)?;
    }
    Ok(())
}

/// Finalizes this Sale's confirmed entries inside its posting: each line is linked to the dispensing
/// just written — the database checks it against the line as posting froze it — and the entry
/// becomes finalized with the Sale, or neither happens.
async fn finalize_supply_records(
    connection: &mut PoolConnection<Sqlite>,
    sale_id: &str,
    plan: &DispensingPlan,
    dispensing_ids: &[(String, String)],
    actor_id: &str,
    now: &str,
) -> Result<(), SaleError> {
    for planned in &plan.planned {
        let entry = plan
            .live_records
            .iter()
            .find(|entry| {
                entry.prescription_id == planned.prescription_id && entry.status == "confirmed"
            })
            .ok_or(SaleError::Internal)?;
        let dispensing_id = dispensing_ids
            .iter()
            .find(|(line_id, _)| *line_id == planned.sale_line_id)
            .map(|(_, dispensing_id)| dispensing_id.as_str())
            .ok_or(SaleError::Internal)?;
        let linked = sqlx::query(
            "UPDATE prescription_supply_record_lines SET dispensing_id=? \
             WHERE record_id=? AND sale_line_id=? AND dispensing_id IS NULL",
        )
        .bind(dispensing_id)
        .bind(&entry.id)
        .bind(&planned.sale_line_id)
        .execute(&mut **connection)
        .await
        .map_err(map_database_error)?;
        if linked.rows_affected() != 1 {
            return Err(SaleError::Internal);
        }
    }
    for entry in &plan.live_records {
        sqlx::query(
            "UPDATE prescription_supply_records SET status='finalized',finalized_at_utc=? WHERE id=?",
        )
        .bind(now)
        .bind(&entry.id)
        .execute(&mut **connection)
        .await
        .map_err(map_database_error)?;
        audit_entity(
            connection,
            "prescription_supply_record",
            &entry.id,
            "posted",
            &json!({ "saleDocumentId": sale_id, "status": "finalized" }),
            actor_id,
            now,
        )
        .await?;
    }
    Ok(())
}

/// Phase 1M-C — finalizes this Sale's confirmed Schedule H1 working entries inside its posting,
/// each linked to the dispensing of its own line. The database checks the link and refuses the
/// posting if any H1 line is left without a finalized entry.
async fn finalize_h1_entries(
    connection: &mut PoolConnection<Sqlite>,
    sale_id: &str,
    plan: &DispensingPlan,
    dispensing_ids: &[(String, String)],
    actor_id: &str,
    now: &str,
) -> Result<(), SaleError> {
    for entry in &plan.live_h1 {
        if entry.status != "confirmed" {
            return Err(SaleError::Internal);
        }
        let dispensing_id = dispensing_ids
            .iter()
            .find(|(line_id, _)| *line_id == entry.sale_line_id)
            .map(|(_, dispensing_id)| dispensing_id.as_str())
            .ok_or(SaleError::Internal)?;
        let finalized = sqlx::query(
            "UPDATE prescription_h1_register_entries SET status='finalized',dispensing_id=?,\
             finalized_at_utc=? WHERE id=? AND status='confirmed'",
        )
        .bind(dispensing_id)
        .bind(now)
        .bind(&entry.id)
        .execute(&mut **connection)
        .await
        .map_err(map_database_error)?;
        if finalized.rows_affected() != 1 {
            return Err(SaleError::Internal);
        }
        audit_entity(
            connection,
            "prescription_h1_register_entry",
            &entry.id,
            "posted",
            &json!({ "saleDocumentId": sale_id, "dispensingId": dispensing_id, "status": "finalized" }),
            actor_id,
            now,
        )
        .await?;
    }
    Ok(())
}

#[derive(Debug, FromRow)]
struct DraftLine {
    id: String,
    sale_document_id: String,
    line_number: i64,
    product_id: String,
    product_pack_id: String,
    batch_id: String,
    quantity_basis: String,
    quantity_packs: Option<i64>,
    quantity_atoms: i64,
    selling_rate_paise: i64,
}

async fn load_lines(
    connection: &mut PoolConnection<Sqlite>,
    document_id: &str,
) -> Result<Vec<DraftLine>, SaleError> {
    sqlx::query_as::<_, DraftLine>(
        "SELECT id,sale_document_id,line_number,product_id,product_pack_id,batch_id,\
         quantity_basis,quantity_packs,quantity_atoms,selling_rate_paise \
         FROM sale_lines WHERE sale_document_id=? ORDER BY line_number",
    )
    .bind(document_id)
    .fetch_all(&mut **connection)
    .await
    .map_err(map_database_error)
}

async fn state_code(
    connection: &mut PoolConnection<Sqlite>,
    state_id: &str,
) -> Result<String, SaleError> {
    sqlx::query_scalar("SELECT state_code FROM state_codes WHERE id=?")
        .bind(state_id)
        .fetch_optional(&mut **connection)
        .await
        .map_err(map_database_error)?
        .ok_or(SaleError::Internal)
}

async fn draft_state(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    id: &str,
) -> Result<(i64, String), SaleError> {
    let row: Option<(i64, String)> =
        sqlx::query_as("SELECT revision,status FROM sale_documents WHERE id=?")
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(map_database_error)?;
    let (revision, status) = row.ok_or(SaleError::NotFound)?;
    if status != "draft" {
        return Err(SaleError::NotDraft);
    }
    Ok((revision, status))
}

fn require_revision(current: &(i64, String), expected: i64) -> Result<(), SaleError> {
    if current.0 != expected {
        return Err(SaleError::Revision {
            expected,
            current: current.0,
        });
    }
    Ok(())
}

async fn line_document(pool: &SqlitePool, line_id: &str) -> Result<String, SaleError> {
    sqlx::query_scalar("SELECT sale_document_id FROM sale_lines WHERE id=?")
        .bind(line_id)
        .fetch_optional(pool)
        .await
        .map_err(map_database_error)?
        .ok_or(SaleError::NotFound)
}

/// The business date a line must be judged against — expiry above all — read from the document
/// rather than taken from the browser.
async fn document_business_date(pool: &SqlitePool, id: &str) -> Result<String, SaleError> {
    sqlx::query_scalar("SELECT business_date FROM sale_documents WHERE id=?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(map_database_error)?
        .ok_or(SaleError::NotFound)
}

async fn bump_draft(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    id: &str,
    current: i64,
    now: &str,
) -> Result<(), SaleError> {
    sqlx::query(
        "UPDATE sale_documents SET revision=?,updated_at_utc=? \
         WHERE id=? AND revision=? AND status='draft'",
    )
    .bind(current + 1)
    .bind(now)
    .bind(id)
    .bind(current)
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

async fn insert_line(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    id: &str,
    document_id: &str,
    line_number: i64,
    line: &PreparedLine,
    now: &str,
) -> Result<(), SaleError> {
    sqlx::query(
        "INSERT INTO sale_lines (id,sale_document_id,line_number,product_id,product_pack_id,\
         batch_id,quantity_basis,quantity_packs,quantity_atoms,selling_rate_paise,taxable_value_paise,\
         created_at_utc,updated_at_utc) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(id)
    .bind(document_id)
    .bind(line_number)
    .bind(&line.product_id)
    .bind(&line.product_pack_id)
    .bind(&line.batch_id)
    .bind(line.quantity.basis.as_str())
    .bind(line.quantity.quantity_packs)
    .bind(line.quantity.quantity_atoms)
    .bind(line.selling_rate_paise)
    .bind(line.taxable_value_paise)
    .bind(now)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

async fn database_now(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
) -> Result<String, SaleError> {
    sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')")
        .fetch_one(&mut **transaction)
        .await
        .map_err(map_database_error)
}

async fn audit(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    entity_id: &str,
    revision: i64,
    action: &str,
    payload: &Value,
    actor_id: &str,
) -> Result<(), SaleError> {
    sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) \
         VALUES (?,'sale_document',?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),NULL,1,?,?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(entity_id)
    .bind(revision)
    .bind(action)
    .bind(payload.to_string())
    .bind(actor_id)
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

/// An audited event on a connection already inside the posting transaction, for entity types other
/// than the Sale itself. Payloads carry identifiers, never a patient's particulars.
async fn audit_entity(
    connection: &mut PoolConnection<Sqlite>,
    entity_type: &str,
    entity_id: &str,
    action: &str,
    payload: &Value,
    actor_id: &str,
    now: &str,
) -> Result<(), SaleError> {
    sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) \
         VALUES (?,?,?,1,?,?,NULL,1,?,?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(entity_type)
    .bind(entity_id)
    .bind(action)
    .bind(now)
    .bind(payload.to_string())
    .bind(actor_id)
    .execute(&mut **connection)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

async fn fetch_detail(pool: &SqlitePool, id: &str) -> Result<SaleDetailResponse, SaleError> {
    let sale = sqlx::query_as::<_, SaleHeaderResponse>(&format!(
        "SELECT {HEADER_COLUMNS} FROM sale_documents WHERE id=?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?
    .ok_or(SaleError::NotFound)?;
    // The live names are joined in beside the snapshot rather than replacing it: a posted invoice
    // still reprints exactly as issued, even if the product has since been renamed.
    // Every snapshot column is qualified to the line, because the joins below bring three more
    // tables that each carry an `id` of their own.
    let qualified = LINE_COLUMNS
        .split(',')
        .map(|column| format!("line.{}", column.trim()))
        .collect::<Vec<_>>()
        .join(",");
    let lines = sqlx::query_as::<_, SaleLineResponse>(&format!(
        "SELECT {qualified},product.display_name AS current_product_display_name,\
         pack.display_label AS current_pack_display_label,\
         unit.display_name AS current_base_unit_label,\
         batch.batch_number AS current_batch_number,line.prescription_item_id \
         FROM sale_lines line \
         LEFT JOIN products product ON product.id = line.product_id \
         LEFT JOIN product_packs pack ON pack.id = line.product_pack_id \
         LEFT JOIN units_of_measure unit ON unit.id = product.base_unit_id \
         LEFT JOIN product_batches batch ON batch.id = line.batch_id \
         WHERE line.sale_document_id=? ORDER BY line.line_number"
    ))
    .bind(id)
    .fetch_all(pool)
    .await
    .map_err(map_database_error)?;
    let tenders = sqlx::query_as::<_, SaleTenderResponse>(
        "SELECT id,sale_document_id,method,amount_paise,reference_text FROM sale_tenders \
         WHERE sale_document_id=? ORDER BY created_at_utc,id",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .map_err(map_database_error)?;
    let supply = sqlx::query_as::<_, DraftSupplyResponse>(
        "SELECT supervising_professional_id,\
         prescription_endorsement_confirmed=1 AS prescription_endorsement_confirmed,\
         prescription_original_container_confirmed=1 AS prescription_original_container_confirmed \
         FROM sale_documents WHERE id=?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(map_database_error)?;
    let prescription_records = sqlx::query_as::<_, SaleRecordSummary>(
        "SELECT record.id,record.serial_number,record.record_method,record.status,\
         prescription.reference AS prescription_reference FROM prescription_supply_records record \
         JOIN prescriptions prescription ON prescription.id=record.prescription_id \
         WHERE record.sale_document_id=? ORDER BY record.record_method,record.serial_value",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .map_err(map_database_error)?;
    let h1_register_entries = sqlx::query_as::<_, SaleH1EntrySummary>(
        "SELECT entry.id,entry.reference,entry.status,line.line_number \
         FROM prescription_h1_register_entries entry \
         JOIN sale_lines line ON line.id=entry.sale_line_id \
         WHERE entry.sale_document_id=? ORDER BY entry.reference_value",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .map_err(map_database_error)?;
    Ok(SaleDetailResponse {
        sale,
        lines,
        tenders,
        supply,
        prescription_records,
        h1_register_entries,
    })
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;

    const TABLET: &str = "01997000-0000-7000-8000-000000000001";
    const STRIP: &str = "01997000-0000-7000-8000-000000000004";
    const MILLILITRE: &str = "01997000-0000-7000-8000-000000000011";
    const MAHARASHTRA: &str = "01997300-0000-7000-8000-000000000027";
    const KARNATAKA: &str = "01997300-0000-7000-8000-000000000029";
    const OWNER: &str = "sale-owner-session-token";
    const CASHIER: &str = "sale-cashier-session-token";
    const TODAY: &str = "2026-06-15";

    struct Fixture {
        _temp: tempfile::TempDir,
        pool: SqlitePool,
        store_id: String,
        owner_id: String,
        customer_id: String,
        product_id: String,
        pack_id: String,
        batch_id: String,
        category_id: String,
    }

    /// A store in Maharashtra, one customer, and one taxable product at 6% + 6% with a lot of 100
    /// tablets on hand whose printed MRP is 95.50 a strip.
    async fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let pool = crate::infrastructure::database::connect(&temp.path().join("sale.sqlite3"))
            .await
            .unwrap();
        let store_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO store_identity (store_id,display_name,business_time_zone,created_at_utc,\
             gst_registration_status,gstin,normalized_gstin,place_of_supply_state_id) \
             VALUES (?,'Care Pharmacy','Asia/Kolkata',strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             'registered','27AAPFU0939F1ZV','27AAPFU0939F1ZV',?)",
        )
        .bind(&store_id)
        .bind(MAHARASHTRA)
        .execute(&pool)
        .await
        .unwrap();
        record_legal_profile(&pool, &store_id).await;
        let owner_id = insert_session(&pool, "owner_admin", OWNER).await;
        insert_session(&pool, "cashier", CASHIER).await;

        let customer_id = insert_customer(&pool, "Rahul Deshmukh", Some(MAHARASHTRA)).await;
        let category_id = insert_category(&pool, "gst-12", "taxable").await;
        insert_rate(&pool, &category_id, "2020-01-01", 600).await;
        let (product_id, pack_id) = insert_product(&pool, "Crocin 500", TABLET, 0, 10).await;
        enable_sale(&pool, &store_id, &product_id, &pack_id, 1, 0).await;
        classify(&pool, &product_id, &category_id).await;
        assign_hsn(&pool, &product_id, "30049099").await;
        let batch_id = insert_batch(&pool, &pack_id, "B-1", Some("2027-12-31"), Some(9550)).await;
        add_stock(
            &pool,
            &store_id,
            &product_id,
            &pack_id,
            &batch_id,
            100,
            &owner_id,
        )
        .await;

        Fixture {
            _temp: temp,
            pool,
            store_id,
            owner_id,
            customer_id,
            product_id,
            pack_id,
            batch_id,
            category_id,
        }
    }

    async fn insert_customer(pool: &SqlitePool, name: &str, state: Option<&str>) -> String {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO parties (id,display_name,normalized_search_name,gst_registration_status,\
             place_of_supply_state_id,created_at_utc,updated_at_utc) \
             VALUES (?,?,?,'unregistered',?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(name)
        .bind(name.to_lowercase())
        .bind(state)
        .execute(pool)
        .await
        .unwrap();
        add_role(pool, &id, "customer").await;
        id
    }

    async fn add_role(pool: &SqlitePool, party_id: &str, role: &str) {
        sqlx::query(
            "INSERT INTO party_roles (id,party_id,role,created_at_utc,updated_at_utc) \
             VALUES (?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(party_id)
        .bind(role)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_product(
        pool: &SqlitePool,
        name: &str,
        base_unit: &str,
        scale: i64,
        pack_atoms: i64,
    ) -> (String, String) {
        let product_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO products (id,product_kind,base_unit_id,quantity_scale,display_name,\
             normalized_search_name,created_at_utc,updated_at_utc) \
             VALUES (?,'general_pharmacy_item',?,?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&product_id)
        .bind(base_unit)
        .bind(scale)
        .bind(name)
        .bind(name.to_lowercase())
        .execute(pool)
        .await
        .unwrap();
        let pack_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO product_packs (id,product_id,container_unit_id,base_quantity_atoms,\
             display_label,created_at_utc,updated_at_utc) VALUES (?,?,?,?,'Strip',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&pack_id)
        .bind(&product_id)
        .bind(STRIP)
        .bind(pack_atoms)
        .execute(pool)
        .await
        .unwrap();
        (product_id, pack_id)
    }

    async fn enable_sale(
        pool: &SqlitePool,
        store_id: &str,
        product_id: &str,
        pack_id: &str,
        sale_enabled: i64,
        fractional: i64,
    ) {
        sqlx::query(
            "INSERT INTO store_pack_policies (id,store_id,product_id,pack_id,purchase_enabled,\
             sale_enabled,whole_pack_only_purchase,fractional_sale_allowed,\
             minimum_sale_increment_atoms,default_purchase_pack,default_sale_pack,created_at_utc,\
             updated_at_utc) VALUES (?,?,?,?,1,?,0,?,1,0,0,\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(store_id)
        .bind(product_id)
        .bind(pack_id)
        .bind(sale_enabled)
        .bind(fractional)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Gives a product an HSN code from the master, creating the master row on first use.
    async fn assign_hsn(pool: &SqlitePool, product_id: &str, code: &str) {
        sqlx::query(
            "INSERT OR IGNORE INTO hsn_codes (id,jurisdiction,hsn_code,description,created_at_utc,\
             updated_at_utc) VALUES (?,'IN',?,'Medicaments',strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(code)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE products SET hsn_code_id=(SELECT id FROM hsn_codes WHERE jurisdiction='IN' \
             AND hsn_code=?) WHERE id=?",
        )
        .bind(code)
        .bind(product_id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn classify(pool: &SqlitePool, product_id: &str, category_id: &str) {
        sqlx::query("UPDATE products SET tax_category_id=? WHERE id=?")
            .bind(category_id)
            .bind(product_id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn insert_category(pool: &SqlitePool, code: &str, treatment: &str) -> String {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO tax_categories (id,jurisdiction,category_code,display_name,tax_treatment,\
             created_at_utc,updated_at_utc) VALUES (?,'IN',?,?,?,\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(code)
        .bind(code)
        .bind(treatment)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn insert_rate(pool: &SqlitePool, category_id: &str, from: &str, half: i64) -> String {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO tax_rate_versions (id,tax_category_id,effective_from,effective_to,\
             cgst_basis_points,sgst_basis_points,igst_basis_points,cess_basis_points,\
             created_at_utc,updated_at_utc) VALUES (?,?,?,NULL,?,?,?,0,\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(category_id)
        .bind(from)
        .bind(half)
        .bind(half)
        .bind(half * 2)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn insert_batch(
        pool: &SqlitePool,
        pack_id: &str,
        number: &str,
        expires: Option<&str>,
        mrp: Option<i64>,
    ) -> String {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO product_batches (id,product_pack_id,batch_number,normalized_batch_number,\
             expires_on,mrp_paise,created_at_utc,updated_at_utc) VALUES (?,?,?,?,?,?,\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(pack_id)
        .bind(number)
        .bind(number.to_uppercase())
        .bind(expires)
        .bind(mrp)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn add_stock(
        pool: &SqlitePool,
        store_id: &str,
        product_id: &str,
        pack_id: &str,
        batch_id: &str,
        atoms: i64,
        actor: &str,
    ) {
        sqlx::query(
            "INSERT INTO inventory_movements (id,store_id,product_id,product_pack_id,batch_id,\
             movement_type,quantity_delta_atoms,occurred_on,idempotency_key,posted_by_user_id,\
             posted_at_utc) VALUES (?,?,?,?,?,'opening_stock',?,'2026-04-01',?,?,\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(store_id)
        .bind(product_id)
        .bind(pack_id)
        .bind(batch_id)
        .bind(atoms)
        .bind(Uuid::now_v7().to_string())
        .bind(actor)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A controlled medicine with a notified per-tablet ceiling, on its own lot.
    async fn controlled_product(
        f: &Fixture,
        ceiling_paise: i64,
        basis: &str,
        basis_unit: Option<&str>,
        effective_from: &str,
    ) -> (String, String, String) {
        let (product_id, pack_id) = insert_product(&f.pool, "Amlodipine 5", TABLET, 0, 10).await;
        enable_sale(&f.pool, &f.store_id, &product_id, &pack_id, 1, 0).await;
        classify(&f.pool, &product_id, &f.category_id).await;
        let formulation_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO controlled_formulations (id,jurisdiction,formulation_code,display_name,\
             created_at_utc,updated_at_utc) VALUES (?,'IN','AMLO-5','Amlodipine 5 mg tablet',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&formulation_id)
        .execute(&f.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO price_control_versions (id,controlled_formulation_id,effective_from,\
             ceiling_price_paise,ceiling_basis,ceiling_basis_unit_id,created_at_utc,updated_at_utc) \
             VALUES (?,?,?,?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&formulation_id)
        .bind(effective_from)
        .bind(ceiling_paise)
        .bind(basis)
        .bind(basis_unit)
        .execute(&f.pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE products SET price_control_status='controlled',controlled_formulation_id=? \
             WHERE id=?",
        )
        .bind(&formulation_id)
        .bind(&product_id)
        .execute(&f.pool)
        .await
        .unwrap();
        // A generous printed MRP, so the controlled ceiling is what this lot is judged by.
        let batch_id =
            insert_batch(&f.pool, &pack_id, "C-1", Some("2027-12-31"), Some(50000)).await;
        add_stock(
            &f.pool,
            &f.store_id,
            &product_id,
            &pack_id,
            &batch_id,
            100,
            &f.owner_id,
        )
        .await;
        (product_id, pack_id, batch_id)
    }

    fn draft_body(customer: Option<&str>) -> Value {
        json!({ "customerPartyId": customer, "businessDate": TODAY })
    }

    fn line_body(revision: i64, f: &Fixture, basis: &str, quantity: i64, rate: i64) -> Value {
        json!({
            "expectedRevision": revision,
            "productId": f.product_id,
            "productPackId": f.pack_id,
            "batchId": f.batch_id,
            "quantityBasis": basis,
            "quantity": quantity,
            "sellingRatePaise": rate
        })
    }

    async fn draft_with_line(f: &Fixture, basis: &str, quantity: i64, rate: i64) -> String {
        let (status, created) =
            request(f.pool.clone(), "POST", "/api/v1/sales", draft_body(None)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let id = created["id"].as_str().unwrap().to_owned();
        let (status, withline) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            line_body(1, f, basis, quantity, rate),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{withline}");
        id
    }

    async fn post_sale_request(
        f: &Fixture,
        id: &str,
        revision: i64,
        key: &str,
        amount: i64,
    ) -> (StatusCode, Value) {
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/post"),
            json!({
                "expectedRevision": revision,
                "idempotencyKey": key,
                "tenders": [{ "method": "cash", "amountPaise": amount }]
            }),
        )
        .await
    }

    async fn detail(f: &Fixture, id: &str) -> Value {
        let (status, body) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/sales/{id}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    async fn balance(f: &Fixture, batch_id: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT COALESCE(SUM(quantity_delta_atoms),0) FROM inventory_movements WHERE batch_id=?",
        )
        .bind(batch_id)
        .fetch_one(&f.pool)
        .await
        .unwrap()
    }

    // -------------------------------------------------------------------------------------
    // Draft
    // -------------------------------------------------------------------------------------

    /// A draft is a shopping basket, not a document: no number, no tax, no stock effect.
    #[tokio::test]
    async fn a_draft_issues_no_number_and_moves_no_stock() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 2, 8000).await;
        let body = detail(&f, &id).await;
        assert_eq!(body["status"], "draft");
        assert_eq!(body["documentNumber"], Value::Null);
        assert_eq!(body["taxTreatment"], Value::Null);
        // The browser sent "2 packs"; the server derived 20 atoms from the frozen pack model.
        assert_eq!(body["lines"][0]["quantityAtoms"], 20);
        assert_eq!(body["lines"][0]["quantityPacks"], 2);
        // A pre-tax subtotal is authoritative even on a draft; the tax is not, and stays zero.
        assert_eq!(body["lines"][0]["taxableValuePaise"], 16000);
        assert_eq!(body["lines"][0]["lineTotalPaise"], 0);
        assert_eq!(balance(&f, &f.batch_id).await, 100, "a draft must not sell");
    }

    /// The commonest sale in an Indian pharmacy has no named customer at all.
    #[tokio::test]
    async fn a_walk_in_needs_no_customer_and_no_invented_cash_party() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["customerPartyId"], Value::Null);
        assert_eq!(posted["customerDisplayName"], Value::Null);
        let parties: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM parties")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(parties, 1, "a walk-in must not create a party row");
    }

    /// Enabling the customer role did not make it optional to hold it.
    #[tokio::test]
    async fn a_party_without_an_active_customer_role_cannot_be_billed() {
        let f = fixture().await;
        let supplier_only = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO parties (id,display_name,normalized_search_name,gst_registration_status,\
             created_at_utc,updated_at_utc) VALUES (?,'Sharma Medicals','sharma medicals',\
             'unregistered',strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&supplier_only)
        .execute(&f.pool)
        .await
        .unwrap();
        add_role(&f.pool, &supplier_only, "supplier").await;
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/sales",
            draft_body(Some(&supplier_only)),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "customer_not_eligible");
    }

    #[tokio::test]
    async fn a_stale_revision_cannot_change_a_draft() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, conflict) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            line_body(1, &f, "pack", 1, 8000),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
        assert_eq!(conflict["code"], "revision_conflict");
        assert_eq!(conflict["currentRevision"], 2);
    }

    /// A pack the store has not enabled for sale is refused, and so is a pack with no policy at
    /// all — not enabled is not the same as enabled, but both refuse.
    #[tokio::test]
    async fn a_pack_not_enabled_for_sale_here_is_refused() {
        let f = fixture().await;
        sqlx::query("UPDATE store_pack_policies SET sale_enabled=0 WHERE pack_id=?")
            .bind(&f.pack_id)
            .execute(&f.pool)
            .await
            .unwrap();
        let (status, created) =
            request(f.pool.clone(), "POST", "/api/v1/sales", draft_body(None)).await;
        let id = created["id"].as_str().unwrap().to_owned();
        assert_eq!(status, StatusCode::CREATED);
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            line_body(1, &f, "pack", 1, 8000),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "pack_not_sellable");
    }

    #[tokio::test]
    async fn a_batch_from_another_pack_cannot_be_sold_on_this_line() {
        let f = fixture().await;
        let (_, other_pack) = insert_product(&f.pool, "Dolo 650", TABLET, 0, 15).await;
        let stranger =
            insert_batch(&f.pool, &other_pack, "X-1", Some("2027-12-31"), Some(1000)).await;
        let (_, created) = request(f.pool.clone(), "POST", "/api/v1/sales", draft_body(None)).await;
        let id = created["id"].as_str().unwrap().to_owned();
        let mut body = line_body(1, &f, "pack", 1, 8000);
        body["batchId"] = json!(stranger);
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            body,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "batch_pack_mismatch");
    }

    /// Selling an expired drug is prohibited under the Drugs and Cosmetics Act, 1940. Refusing
    /// rather than warning, for every batch-bearing product, is this project's own stricter policy.
    #[tokio::test]
    async fn an_expired_lot_cannot_be_sold() {
        let f = fixture().await;
        sqlx::query("UPDATE product_batches SET expires_on='2026-05-31' WHERE id=?")
            .bind(&f.batch_id)
            .execute(&f.pool)
            .await
            .unwrap();
        let (_, created) = request(f.pool.clone(), "POST", "/api/v1/sales", draft_body(None)).await;
        let id = created["id"].as_str().unwrap().to_owned();
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            line_body(1, &f, "pack", 1, 8000),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "batch_expired");
    }

    /// A lot that expires ON the business date is still sellable: the period is inclusive of its
    /// last day, and an off-by-one here would refuse a lawful sale.
    #[tokio::test]
    async fn a_lot_expiring_today_is_still_sellable() {
        let f = fixture().await;
        sqlx::query("UPDATE product_batches SET expires_on=? WHERE id=?")
            .bind(TODAY)
            .bind(&f.batch_id)
            .execute(&f.pool)
            .await
            .unwrap();
        let (_, created) = request(f.pool.clone(), "POST", "/api/v1/sales", draft_body(None)).await;
        let id = created["id"].as_str().unwrap().to_owned();
        let (status, accepted) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            line_body(1, &f, "pack", 1, 8000),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{accepted}");
    }

    /// Three tablets out of a strip of ten, sold by a store that holds no sub-unit permission at
    /// all — and could not hold one, because the frozen Phase 1B triggers pin a discrete unit to
    /// scale 0 and then forbid `fractional_sale_allowed` on it. Breaking the strip is the
    /// increment's business, and nothing else's.
    #[tokio::test]
    async fn loose_tablets_are_governed_by_the_increment_not_by_fractional_permission() {
        let f = fixture().await;
        let fractional: i64 = sqlx::query_scalar(
            "SELECT fractional_sale_allowed FROM store_pack_policies WHERE pack_id=?",
        )
        .bind(&f.pack_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(fractional, 0, "a tablet can never carry this permission");

        let (_, created) = request(f.pool.clone(), "POST", "/api/v1/sales", draft_body(None)).await;
        let id = created["id"].as_str().unwrap().to_owned();
        let (status, accepted) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            line_body(1, &f, "base_unit", 3, 900),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{accepted}");
        assert_eq!(accepted["lines"][0]["quantityAtoms"], 3);
        assert_eq!(accepted["lines"][0]["quantityPacks"], Value::Null);

        // The store now declares that this strip is not to be broken, by raising the increment to a
        // whole strip. That, and only that, is the whole-pack-only setting.
        sqlx::query(
            "UPDATE store_pack_policies SET minimum_sale_increment_atoms=10 WHERE pack_id=?",
        )
        .bind(&f.pack_id)
        .execute(&f.pool)
        .await
        .unwrap();
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            line_body(2, &f, "base_unit", 3, 900),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "quantity_increment_violation");

        let (status, accepted) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            line_body(2, &f, "base_unit", 10, 900),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{accepted}");
        assert_eq!(accepted["lines"][1]["quantityAtoms"], 10);
    }

    /// What the permission actually governs: less than one whole base unit. A 100 ml syrup at
    /// scale 1 holds 1 000 atoms, and one millilitre is ten of them.
    #[tokio::test]
    async fn a_measurable_product_needs_permission_only_below_one_whole_base_unit() {
        let f = fixture().await;
        let (syrup, bottle) = insert_product(&f.pool, "Ascoril 100", MILLILITRE, 1, 1_000).await;
        enable_sale(&f.pool, &f.store_id, &syrup, &bottle, 1, 0).await;
        classify(&f.pool, &syrup, &f.category_id).await;
        let lot = insert_batch(&f.pool, &bottle, "S-1", Some("2027-12-31"), Some(20_000)).await;
        add_stock(
            &f.pool,
            &f.store_id,
            &syrup,
            &bottle,
            &lot,
            5_000,
            &f.owner_id,
        )
        .await;
        let (_, created) = request(f.pool.clone(), "POST", "/api/v1/sales", draft_body(None)).await;
        let id = created["id"].as_str().unwrap().to_owned();
        let measured = |revision: i64, atoms: i64| {
            json!({
                "expectedRevision": revision,
                "productId": syrup,
                "productPackId": bottle,
                "batchId": lot,
                "quantityBasis": "base_unit",
                "quantity": atoms,
                "sellingRatePaise": 10
            })
        };

        // 50 atoms is 5 whole millilitres: no permission needed.
        let (status, accepted) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            measured(1, 50),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{accepted}");

        // 5 atoms is half a millilitre, which is a genuine fraction of a base unit.
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            measured(2, 5),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "fractional_sale_not_allowed");

        sqlx::query("UPDATE store_pack_policies SET fractional_sale_allowed=1 WHERE pack_id=?")
            .bind(&bottle)
            .execute(&f.pool)
            .await
            .unwrap();
        let (status, accepted) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            measured(2, 5),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{accepted}");
        assert_eq!(accepted["lines"][1]["quantityAtoms"], 5);

        // The increment still refuses on its own terms, permission or not.
        sqlx::query(
            "UPDATE store_pack_policies SET minimum_sale_increment_atoms=10 WHERE pack_id=?",
        )
        .bind(&bottle)
        .execute(&f.pool)
        .await
        .unwrap();
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            measured(3, 5),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "quantity_increment_violation");
    }

    #[tokio::test]
    async fn a_line_can_be_changed_and_removed_while_the_sale_is_a_draft() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 2, 8000).await;
        let body = detail(&f, &id).await;
        let line_id = body["lines"][0]["id"].as_str().unwrap().to_owned();
        let mut changed = line_body(2, &f, "pack", 3, 7000);
        changed["expectedRevision"] = json!(2);
        let (status, updated) = request(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/sale-lines/{line_id}"),
            changed,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{updated}");
        assert_eq!(updated["lines"][0]["quantityAtoms"], 30);
        assert_eq!(updated["lines"][0]["taxableValuePaise"], 21000);

        let (status, removed) = request(
            f.pool.clone(),
            "DELETE",
            &format!("/api/v1/sale-lines/{line_id}"),
            json!({ "expectedRevision": 3 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{removed}");
        assert_eq!(removed["lines"].as_array().unwrap().len(), 0);
    }

    // -------------------------------------------------------------------------------------
    // Posting
    // -------------------------------------------------------------------------------------

    /// The whole posting contract in one sale: a number is issued, every snapshot is frozen, the
    /// tax is computed once, and exactly the sold atoms leave the ledger.
    #[tokio::test]
    async fn posting_issues_a_number_freezes_the_snapshot_and_takes_the_stock_out() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 2, 8000).await;
        // 2 x 80.00 = 160.00 taxable; 6% + 6% is 9.60 each; 179.20 in all.
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 17920).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["status"], "posted");
        assert_eq!(posted["documentNumber"], "INV/2627/000001");
        assert_eq!(posted["sequenceValue"], 1);
        assert_eq!(posted["taxTreatment"], "intra_state");
        assert_eq!(posted["taxableValuePaise"], 16000);
        assert_eq!(posted["cgstPaise"], 960);
        assert_eq!(posted["sgstPaise"], 960);
        assert_eq!(posted["igstPaise"], 0);
        assert_eq!(posted["grandTotalPaise"], 17920);
        // Identity is snapshotted so the invoice reprints as issued.
        assert_eq!(posted["lines"][0]["productDisplayName"], "Crocin 500");
        assert_eq!(posted["lines"][0]["batchNumber"], "B-1");
        assert_eq!(posted["lines"][0]["batchMrpPaise"], 9550);
        assert_eq!(posted["storeStateCode"], "27");
        assert_eq!(posted["tenders"][0]["amountPaise"], 17920);
        // 20 atoms left the ledger, and the movement carries real provenance.
        assert_eq!(balance(&f, &f.batch_id).await, 80);
        let (kind, delta, line): (String, i64, String) = sqlx::query_as(
            "SELECT movement_type,quantity_delta_atoms,sale_line_id FROM inventory_movements \
             WHERE movement_type='sale'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(kind, "sale");
        assert_eq!(delta, -20);
        assert_eq!(line, posted["lines"][0]["id"].as_str().unwrap());
    }

    /// The place of supply is the Store because the goods are handed over at the counter, so an
    /// out-of-State customer does not turn a counter sale into an inter-State supply. Their State is
    /// still recorded on the document.
    #[tokio::test]
    async fn a_counter_sale_stays_intra_state_even_for_an_out_of_state_customer() {
        let f = fixture().await;
        let outsider = insert_customer(&f.pool, "Bengaluru Buyer", Some(KARNATAKA)).await;
        let (_, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/sales",
            draft_body(Some(&outsider)),
        )
        .await;
        let id = created["id"].as_str().unwrap().to_owned();
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            line_body(1, &f, "pack", 1, 8000),
        )
        .await;
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["taxTreatment"], "intra_state");
        assert_eq!(posted["cgstPaise"], 480);
        assert_eq!(posted["sgstPaise"], 480);
        assert_eq!(posted["igstPaise"], 0);
        // Recorded, but not used to decide the treatment.
        assert_eq!(posted["customerStateCode"], "29");
        assert_eq!(posted["customerDisplayName"], "Bengaluru Buyer");
    }

    /// The counter is the sole authority for the number. It advances by one and starts again in the
    /// next financial year, because a series is per (store, series, year).
    #[tokio::test]
    async fn the_series_advances_by_one_and_restarts_each_financial_year() {
        let f = fixture().await;
        for expected in ["INV/2627/000001", "INV/2627/000002"] {
            let id = draft_with_line(&f, "pack", 1, 8000).await;
            let (status, posted) =
                post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
            assert_eq!(status, StatusCode::OK, "{posted}");
            assert_eq!(posted["documentNumber"], expected);
        }
        // A sale dated after 31 March opens a new year's series at one.
        let (_, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/sales",
            json!({ "customerPartyId": null, "businessDate": "2027-04-01" }),
        )
        .await;
        let id = created["id"].as_str().unwrap().to_owned();
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            line_body(1, &f, "pack", 1, 8000),
        )
        .await;
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["documentNumber"], "INV/2728/000001");
    }

    /// A posting that fails AFTER it has allocated a number, rewritten every line and inserted the
    /// stock movements must undo all of it — the number above all, because a consumed number is a
    /// gap in a statutory series that cannot be explained later.
    ///
    /// The failure is real, not simulated: the second posting reuses the first one's idempotency
    /// key, and the unique index refuses it at the very last write of the transaction.
    #[tokio::test]
    async fn a_posting_that_fails_after_partial_work_consumes_no_number_and_no_stock() {
        let f = fixture().await;
        let key = Uuid::now_v7().to_string();
        let first = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, posted) = post_sale_request(&f, &first, 2, &key, 8960).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["documentNumber"], "INV/2627/000001");
        assert_eq!(balance(&f, &f.batch_id).await, 90);

        let second = draft_with_line(&f, "pack", 3, 8000).await;
        let (status, refused) = post_sale_request(&f, &second, 2, &key, 26880).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "idempotency_conflict");

        // Nothing the failed posting did survives.
        let after = detail(&f, &second).await;
        assert_eq!(after["status"], "draft");
        assert_eq!(after["documentNumber"], Value::Null);
        assert_eq!(after["lines"][0]["batchNumber"], Value::Null);
        assert_eq!(after["lines"][0]["lineTotalPaise"], 0);
        assert_eq!(balance(&f, &f.batch_id).await, 90, "no stock may have left");
        let movements: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM inventory_movements WHERE movement_type='sale'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(movements, 1);
        let tenders: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sale_tenders")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(tenders, 1);

        // The counter did not move, so the next sale takes the very next number.
        let next_value: i64 = sqlx::query_scalar(
            "SELECT next_value FROM document_number_series WHERE series_code='INV'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(next_value, 2, "a refused posting must consume no number");
        let third = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, posted) =
            post_sale_request(&f, &third, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["documentNumber"], "INV/2627/000002");
    }

    /// A retry after a successful post — the counter's network dropped, the operator pressed Post
    /// again — returns the original invoice rather than selling the goods a second time.
    #[tokio::test]
    async fn a_replay_of_the_same_posting_returns_the_original_invoice() {
        let f = fixture().await;
        let key = Uuid::now_v7().to_string();
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, first) = post_sale_request(&f, &id, 2, &key, 8960).await;
        assert_eq!(status, StatusCode::OK, "{first}");
        let (status, replay) = post_sale_request(&f, &id, 2, &key, 8960).await;
        assert_eq!(status, StatusCode::OK, "{replay}");
        assert_eq!(replay["documentNumber"], first["documentNumber"]);
        assert_eq!(replay["revision"], first["revision"]);
        assert_eq!(balance(&f, &f.batch_id).await, 90, "a replay sells nothing");
        let movements: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM inventory_movements WHERE movement_type='sale'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(movements, 1);
    }

    /// Tendering differently is a different posting, not a replay of this one.
    #[tokio::test]
    async fn the_same_key_with_different_facts_is_refused_rather_than_overwriting() {
        let f = fixture().await;
        let key = Uuid::now_v7().to_string();
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, first) = post_sale_request(&f, &id, 2, &key, 8960).await;
        assert_eq!(status, StatusCode::OK, "{first}");
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/post"),
            json!({
                "expectedRevision": 2,
                "idempotencyKey": key,
                "tenders": [{ "method": "card", "amountPaise": 8960 }]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "idempotency_conflict");
    }

    /// Negative stock is prohibited, and the refusal says how much is actually there.
    #[tokio::test]
    async fn a_sale_may_not_drive_the_ledger_below_zero() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 11, 8000).await;
        let (status, refused) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 98560).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "insufficient_stock");
        assert_eq!(refused["availableAtoms"], 100);
        assert_eq!(balance(&f, &f.batch_id).await, 100);
    }

    /// Two lines drawing on one lot are checked together. Each would pass alone, which is exactly
    /// the bug a per-line check would ship.
    #[tokio::test]
    async fn two_lines_on_one_lot_are_checked_against_the_lot_once() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 6, 8000).await;
        let (status, second) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            line_body(2, &f, "pack", 6, 8000),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{second}");
        let (status, refused) =
            post_sale_request(&f, &id, 3, &Uuid::now_v7().to_string(), 107520).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "insufficient_stock");
        assert_eq!(refused["availableAtoms"], 100);
    }

    /// MRP is a price INCLUSIVE of GST, so the comparison is against the GST-inclusive total. At 12%
    /// on a 95.50 strip the largest lawful rate is 85.26: one paisa more prints 95.51 on the bill.
    #[tokio::test]
    async fn the_mrp_ceiling_is_enforced_against_the_gst_inclusive_total() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 1, 8527).await;
        let (status, refused) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 9551).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "selling_rate_above_mrp");

        let lawful = draft_with_line(&f, "pack", 1, 8526).await;
        let (status, posted) =
            post_sale_request(&f, &lawful, 2, &Uuid::now_v7().to_string(), 9550).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["grandTotalPaise"], 9550);
    }

    /// A loose sale is held to its pro-rata share of the printed MRP without ever inventing an
    /// "MRP per tablet" and treating that rounded number as legal truth.
    #[tokio::test]
    async fn a_loose_sale_is_held_to_a_pro_rata_share_without_inventing_a_per_tablet_mrp() {
        let f = fixture().await;
        // Three of the ten tablets in a 95.50 strip: the pro-rata share is 95.50 x 3 / 10 = 28.65,
        // and it is the GST-INCLUSIVE total that may not exceed it. At 12% the boundary falls
        // between 8.53 a tablet (28.67, refused) and 8.52 (28.62, permitted) - and it is expressed
        // as the cross-product 2867 x 10 > 9550 x 3, never as an invented 9.55 "MRP per tablet".
        let id = draft_with_line(&f, "base_unit", 3, 853).await;
        let (status, refused) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 2867).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "selling_rate_above_mrp");

        let lawful = draft_with_line(&f, "base_unit", 3, 852).await;
        let (status, posted) =
            post_sale_request(&f, &lawful, 2, &Uuid::now_v7().to_string(), 2862).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["grandTotalPaise"], 2862);
        assert_eq!(posted["lines"][0]["quantityAtoms"], 3);
        assert_eq!(posted["lines"][0]["quantityPacks"], Value::Null);
    }

    /// A lot with no printed price has no ceiling. Borrowing one from a sibling batch would
    /// fabricate a legal fact, so the sale proceeds and the absence is visible on the line.
    #[tokio::test]
    async fn a_lot_with_no_printed_mrp_has_no_ceiling_rather_than_an_invented_one() {
        let f = fixture().await;
        sqlx::query("UPDATE product_batches SET mrp_paise=NULL WHERE id=?")
            .bind(&f.batch_id)
            .execute(&f.pool)
            .await
            .unwrap();
        let id = draft_with_line(&f, "pack", 1, 90000).await;
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 100800).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["lines"][0]["batchMrpPaise"], Value::Null);
    }

    // -------------------------------------------------------------------------------------
    // Price control
    // -------------------------------------------------------------------------------------

    /// A DPCO ceiling is notified EXCLUSIVE of GST, so it is compared against the tax-exclusive
    /// rate. At 9.00 a tablet the largest lawful rate for a strip of ten is 90.00 exactly.
    #[tokio::test]
    async fn a_controlled_medicine_is_held_to_its_notified_ceiling() {
        let f = fixture().await;
        let (product, pack, batch) =
            controlled_product(&f, 900, "per_base_unit", Some(TABLET), "2020-01-01").await;
        let refused = post_controlled(&f, &product, &pack, &batch, "pack", 1, 9001).await;
        assert_eq!(refused.0, StatusCode::CONFLICT, "{}", refused.1);
        assert_eq!(refused.1["code"], "selling_rate_above_ceiling");

        let (status, posted) = post_controlled(&f, &product, &pack, &batch, "pack", 1, 9000).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["lines"][0]["priceControlStatus"], "controlled");
        assert_eq!(posted["lines"][0]["ceilingPricePaise"], 900);
        assert_eq!(posted["lines"][0]["ceilingBasis"], "per_base_unit");
        assert!(
            posted["lines"][0]["priceControlVersionId"].is_string(),
            "the version in force is snapshotted"
        );
    }

    /// A loose sale of a controlled medicine is checked per tablet against the per-tablet ceiling.
    #[tokio::test]
    async fn a_loose_sale_of_a_controlled_medicine_is_checked_per_base_unit() {
        let f = fixture().await;
        let (product, pack, batch) =
            controlled_product(&f, 900, "per_base_unit", Some(TABLET), "2020-01-01").await;
        let (status, refused) =
            post_controlled(&f, &product, &pack, &batch, "base_unit", 3, 901).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "selling_rate_above_ceiling");

        let (status, posted) =
            post_controlled(&f, &product, &pack, &batch, "base_unit", 3, 900).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["taxableValuePaise"], 2700);
    }

    /// Controlled with no ceiling in force on the sale date is an error, never a silent fallback to
    /// the MRP rule alone.
    #[tokio::test]
    async fn a_controlled_medicine_with_no_ceiling_in_force_is_refused() {
        let f = fixture().await;
        let (product, pack, batch) =
            controlled_product(&f, 900, "per_base_unit", Some(TABLET), "2027-01-01").await;
        let (status, refused) = post_controlled(&f, &product, &pack, &batch, "pack", 1, 5000).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "price_control_unresolved");
    }

    /// A ceiling quoted per pack does not say WHICH pack, and a ceiling quoted in another unit
    /// cannot be converted. Neither is guessed at.
    #[tokio::test]
    async fn a_ceiling_that_cannot_be_compared_is_refused_rather_than_guessed() {
        let f = fixture().await;
        let (product, pack, batch) =
            controlled_product(&f, 9000, "per_pack", None, "2020-01-01").await;
        let (status, refused) = post_controlled(&f, &product, &pack, &batch, "pack", 1, 5000).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "price_control_incomparable");

        let f2 = fixture().await;
        let (product, pack, batch) =
            controlled_product(&f2, 900, "per_base_unit", Some(MILLILITRE), "2020-01-01").await;
        let (status, refused) =
            post_controlled(&f2, &product, &pack, &batch, "pack", 1, 5000).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "price_control_incomparable");
    }

    /// An unassessed medicine sells under the MRP rule alone, and the honest 'unknown' is written
    /// onto the posted line so the gap is auditable rather than invisible.
    #[tokio::test]
    async fn an_unassessed_medicine_records_unknown_rather_than_claiming_it_is_uncontrolled() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["lines"][0]["priceControlStatus"], "unknown");
        assert_eq!(posted["lines"][0]["ceilingPricePaise"], Value::Null);
        assert_eq!(posted["lines"][0]["priceControlVersionId"], Value::Null);
    }

    // -------------------------------------------------------------------------------------
    // Posted immutability, tender and access
    // -------------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_posted_sale_cannot_be_changed_by_any_route() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let line_id = posted["lines"][0]["id"].as_str().unwrap().to_owned();

        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            line_body(3, &f, "pack", 1, 8000),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "sale_not_draft");

        let (status, refused) = request(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/sales/{id}"),
            json!({ "expectedRevision": 3, "customerPartyId": null, "businessDate": TODAY }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "sale_not_draft");

        let (status, refused) = request(
            f.pool.clone(),
            "DELETE",
            &format!("/api/v1/sale-lines/{line_id}"),
            json!({ "expectedRevision": 3 }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "sale_not_draft");

        // A different key against an already posted sale is a plain refusal, not a second invoice.
        let (status, refused) =
            post_sale_request(&f, &id, 3, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "sale_not_draft");
    }

    /// Tender is evidence, not accounting, but it must add up: a shortfall would be credit, which
    /// this phase does not open.
    #[tokio::test]
    async fn the_tender_must_be_a_single_payment_for_exactly_the_invoice_total() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, refused) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8000).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "tender_mismatch");

        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/post"),
            json!({
                "expectedRevision": 2,
                "idempotencyKey": Uuid::now_v7().to_string(),
                "tenders": [
                    { "method": "cash", "amountPaise": 4000 },
                    { "method": "upi", "amountPaise": 4960 }
                ]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "tender_mismatch");
        assert_eq!(balance(&f, &f.batch_id).await, 100);
    }

    /// Eligibility is re-checked at posting, not merely when the draft named the customer.
    #[tokio::test]
    async fn a_customer_whose_role_is_withdrawn_after_the_draft_is_refused_at_posting() {
        let f = fixture().await;
        let (_, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/sales",
            draft_body(Some(&f.customer_id)),
        )
        .await;
        let id = created["id"].as_str().unwrap().to_owned();
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            line_body(1, &f, "pack", 1, 8000),
        )
        .await;
        sqlx::query(
            "UPDATE party_roles SET status='archived',archived_at_utc=\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='left town' WHERE party_id=?",
        )
        .bind(&f.customer_id)
        .execute(&f.pool)
        .await
        .unwrap();
        let (status, refused) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "customer_not_eligible");
    }

    /// A product nobody has classified cannot be taxed. That is an error, never zero tax.
    #[tokio::test]
    async fn an_unclassified_product_is_refused_rather_than_taxed_at_zero() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        sqlx::query("UPDATE products SET tax_category_id=NULL WHERE id=?")
            .bind(&f.product_id)
            .execute(&f.pool)
            .await
            .unwrap();
        let (status, refused) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "product_tax_classification_incomplete");
    }

    /// A taxable category with no rate in force on the sale date is an error, not zero tax.
    #[tokio::test]
    async fn a_taxable_category_with_no_rate_in_force_is_refused() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        sqlx::query(
            "UPDATE tax_rate_versions SET effective_from='2027-01-01' WHERE tax_category_id=?",
        )
        .bind(&f.category_id)
        .execute(&f.pool)
        .await
        .unwrap();
        let (status, refused) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "tax_rate_not_found");
    }

    /// Selling is counter work: a cashier may do it, unlike a purchase. An anonymous request may not.
    #[tokio::test]
    async fn a_cashier_may_sell_and_an_unauthenticated_request_may_not() {
        let f = fixture().await;
        let (status, created) = request_as(
            f.pool.clone(),
            "POST",
            "/api/v1/sales",
            draft_body(None),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");

        let (status, refused) = request_as(
            f.pool.clone(),
            "POST",
            "/api/v1/sales",
            draft_body(None),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{refused}");
    }

    /// What the counter needs to choose a lot, and nothing it should not trust: the balance comes
    /// from the ledger, and expiry is stated rather than silently hiding the row.
    #[tokio::test]
    async fn sellable_batches_report_the_ledger_balance_and_flag_expiry() {
        let f = fixture().await;
        let expired =
            insert_batch(&f.pool, &f.pack_id, "B-0", Some("2026-01-31"), Some(9000)).await;
        add_stock(
            &f.pool,
            &f.store_id,
            &f.product_id,
            &f.pack_id,
            &expired,
            40,
            &f.owner_id,
        )
        .await;
        let (status, body) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/packs/{}/sellable-batches?asOf={TODAY}", f.pack_id),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let rows = body.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        // Soonest expiry first, as a presentation aid only: nothing is auto-selected.
        assert_eq!(rows[0]["batchNumber"], "B-0");
        assert_eq!(rows[0]["expired"], true);
        assert_eq!(rows[0]["availableAtoms"], 40);
        assert_eq!(rows[1]["batchNumber"], "B-1");
        assert_eq!(rows[1]["expired"], false);
        assert_eq!(rows[1]["availableAtoms"], 100);
    }

    /// Two counters selling the last of a lot at the same moment.
    ///
    /// The balance is read inside the same `BEGIN IMMEDIATE` transaction that writes the movement,
    /// so the two cannot interleave between the check and the issue. Exactly one sale may succeed,
    /// and the ledger may never go negative — the refusal may name either the stock or a busy
    /// service, and both are honest answers.
    #[tokio::test]
    async fn two_counters_selling_the_last_of_a_lot_cannot_both_succeed() {
        let f = fixture().await;
        // 100 atoms on hand; each sale wants 6 packs, which is 60.
        let left = draft_with_line(&f, "pack", 6, 8000).await;
        let right = draft_with_line(&f, "pack", 6, 8000).await;
        let left_key = Uuid::now_v7().to_string();
        let right_key = Uuid::now_v7().to_string();
        let (one, two) = tokio::join!(
            post_sale_request(&f, &left, 2, &left_key, 53760),
            post_sale_request(&f, &right, 2, &right_key, 53760),
        );

        let outcomes = [one, two];
        let succeeded = outcomes
            .iter()
            .filter(|(status, _)| *status == StatusCode::OK)
            .count();
        assert_eq!(
            succeeded, 1,
            "exactly one of two concurrent sales may take the stock: {outcomes:?}"
        );
        for (status, body) in &outcomes {
            if *status != StatusCode::OK {
                assert!(
                    body["code"] == "insufficient_stock" || body["code"] == "service_busy",
                    "the loser must be told the truth, not a surprise: {body}"
                );
            }
        }
        let remaining = balance(&f, &f.batch_id).await;
        assert_eq!(remaining, 40, "one sale of 60 atoms, and only one");
        assert!(remaining >= 0, "the ledger may never go negative");
    }

    /// Two postings that both fit cannot be handed the same invoice number, and cannot skip one.
    ///
    /// The counter is advanced from the exact value that was read, inside the posting's own write
    /// transaction, so the series is dense: 1 then 2, with nothing in between and nothing repeated.
    #[tokio::test]
    async fn two_concurrent_postings_receive_distinct_consecutive_numbers() {
        let f = fixture().await;
        let left = draft_with_line(&f, "pack", 1, 8000).await;
        let right = draft_with_line(&f, "pack", 1, 8000).await;
        let left_key = Uuid::now_v7().to_string();
        let right_key = Uuid::now_v7().to_string();
        let (one, two) = tokio::join!(
            post_sale_request(&f, &left, 2, &left_key, 8960),
            post_sale_request(&f, &right, 2, &right_key, 8960),
        );
        assert_eq!(one.0, StatusCode::OK, "{}", one.1);
        assert_eq!(two.0, StatusCode::OK, "{}", two.1);
        let mut numbers = [
            one.1["documentNumber"].as_str().unwrap().to_owned(),
            two.1["documentNumber"].as_str().unwrap().to_owned(),
        ];
        numbers.sort();
        assert_eq!(numbers, ["INV/2627/000001", "INV/2627/000002"]);
        let next_value: i64 = sqlx::query_scalar(
            "SELECT next_value FROM document_number_series WHERE series_code='INV'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(next_value, 3);
    }

    /// A sale movement may only be born of a posting. The manual ledger endpoint refuses to mint
    /// one, so an outward can never exist without the invoice that explains it.
    #[tokio::test]
    async fn the_ledger_endpoint_will_not_mint_a_sale_movement_by_hand() {
        let f = fixture().await;
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/inventory/movements",
            json!({
                "idempotencyKey": Uuid::now_v7().to_string(),
                "movementType": "sale",
                "productPackId": f.pack_id,
                "batchId": f.batch_id,
                "quantityDeltaAtoms": -10,
                "occurredOn": TODAY
            }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        assert_eq!(refused["issues"][0]["field"], "movementType");
        assert_eq!(balance(&f, &f.batch_id).await, 100);
    }

    /// The ledger reads back what the posting wrote: an outward, negative, on the right lot, with a
    /// way back to the invoice line that caused it.
    #[tokio::test]
    async fn the_ledger_reports_the_outward_with_its_provenance_and_derived_balance() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 2, 8000).await;
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 17920).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let line_id = posted["lines"][0]["id"].as_str().unwrap();

        let (status, movements) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/inventory/movements?packId={}", f.pack_id),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{movements}");
        let outward = movements
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["movementType"] == "sale")
            .expect("the sale must appear in the ledger");
        assert_eq!(outward["quantityDeltaAtoms"], -20);
        assert_eq!(outward["batchId"], f.batch_id.as_str());
        assert_eq!(outward["saleLineId"], line_id);
        assert_eq!(outward["purchaseLineId"], Value::Null);
        assert_eq!(outward["occurredOn"], TODAY);

        // The balance is summed from the ledger, never stored.
        let (status, balances) = request(
            f.pool.clone(),
            "GET",
            "/api/v1/inventory/stock",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{balances}");
        let row = balances
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["productPackId"] == f.pack_id.as_str())
            .expect("the pack must have a derived balance");
        assert_eq!(row["balanceAtoms"], 80);
    }

    /// The counter must be able to say the amount out loud before taking money, so a draft can be
    /// quoted. The quote is the same arithmetic the posting will do — it must agree to the paisa —
    /// and it must commit nothing at all.
    #[tokio::test]
    async fn a_quote_matches_the_invoice_it_precedes_and_commits_nothing() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 2, 8000).await;
        let (status, quote) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/sales/{id}/quote"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{quote}");
        assert_eq!(quote["taxableValuePaise"], 16_000);
        assert_eq!(quote["cgstPaise"], 960);
        assert_eq!(quote["sgstPaise"], 960);
        assert_eq!(quote["grandTotalPaise"], 17_920);
        assert_eq!(quote["taxTreatment"], "intra_state");
        assert_eq!(quote["lines"][0]["lineTotalPaise"], 17_920);

        // Quoting allocated no number, wrote no snapshot and moved no stock.
        let still = detail(&f, &id).await;
        assert_eq!(still["status"], "draft");
        assert_eq!(still["documentNumber"], Value::Null);
        assert_eq!(still["lines"][0]["batchNumber"], Value::Null);
        assert_eq!(balance(&f, &f.batch_id).await, 100);
        let series: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM document_number_series")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(series, 0, "a quote must not open a numbering series");

        // And the invoice that follows charges exactly what was quoted.
        let (status, posted) = post_sale_request(
            &f,
            &id,
            2,
            &Uuid::now_v7().to_string(),
            quote["grandTotalPaise"].as_i64().unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["grandTotalPaise"], quote["grandTotalPaise"]);
        assert_eq!(posted["cgstPaise"], quote["cgstPaise"]);
        assert_eq!(posted["taxableValuePaise"], quote["taxableValuePaise"]);
    }

    /// A quote refuses exactly what the posting would refuse, with the same typed code, so an
    /// above-MRP price is discovered while the customer is still at the counter.
    #[tokio::test]
    async fn a_quote_refuses_what_the_posting_would_refuse() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 1, 8_527).await;
        let (status, refused) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/sales/{id}/quote"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "selling_rate_above_mrp");

        // The very same refusal arrives at posting, from the same resolution path.
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 9_551).await;
        assert_eq!(status, StatusCode::CONFLICT, "{posted}");
        assert_eq!(posted["code"], "selling_rate_above_mrp");
    }

    /// A draft bill must be readable while it is being built.
    ///
    /// The browser preview caught this and every automated test had missed it: the four identity
    /// snapshots are NULL until posting freezes them — which is right — so the counter's own bill
    /// showed a raw UUID for the item, the word "Chosen" for the batch, and "2 packs" for a strip of
    /// ten. The live names are joined in beside the snapshot, never instead of it.
    #[tokio::test]
    async fn a_draft_line_carries_live_names_even_though_its_snapshot_is_still_empty() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 2, 8000).await;
        let body = detail(&f, &id).await;
        let line = &body["lines"][0];

        // The snapshot is empty, because nothing has been issued.
        assert_eq!(line["productDisplayName"], Value::Null);
        assert_eq!(line["packDisplayLabel"], Value::Null);
        assert_eq!(line["batchNumber"], Value::Null);
        assert_eq!(line["baseUnitLabel"], Value::Null);

        // And the counter can still read every field of its own bill.
        assert_eq!(line["currentProductDisplayName"], "Crocin 500");
        assert_eq!(line["currentPackDisplayLabel"], "Strip");
        assert_eq!(line["currentBatchNumber"], "B-1");
        assert_eq!(line["currentBaseUnitLabel"], "Tablet");
    }

    /// A posted invoice reprints as issued, even after the catalogue moves on.
    #[tokio::test]
    async fn a_posted_line_keeps_its_own_snapshot_after_the_product_is_renamed() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::OK, "{posted}");

        sqlx::query("UPDATE products SET display_name='Crocin 500 (renamed)' WHERE id=?")
            .bind(&f.product_id)
            .execute(&f.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE product_batches SET batch_number='B-9',normalized_batch_number='B-9' WHERE id=?")
            .bind(&f.batch_id)
            .execute(&f.pool)
            .await
            .unwrap();

        let body = detail(&f, &id).await;
        let line = &body["lines"][0];
        // What the invoice recorded is unchanged...
        assert_eq!(line["productDisplayName"], "Crocin 500");
        assert_eq!(line["batchNumber"], "B-1");
        // ...and the live view honestly reports the rename beside it, without overwriting anything.
        assert_eq!(line["currentProductDisplayName"], "Crocin 500 (renamed)");
        assert_eq!(line["currentBatchNumber"], "B-9");
    }

    /// Posting carries its own revision check, separate from the one every line write makes.
    ///
    /// The earlier hardening proof aimed at the line-level guard and so proved nothing about this
    /// one: they are different code, and a sale posted from a stale view would charge a bill the
    /// operator is no longer looking at.
    #[tokio::test]
    async fn a_posting_from_a_stale_view_is_refused() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        // The draft is at revision 2 after its line; posting from revision 1 is a stale view.
        let (status, refused) =
            post_sale_request(&f, &id, 1, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "revision_conflict");
        assert_eq!(refused["expectedRevision"], 1);
        assert_eq!(refused["currentRevision"], 2);
        // And nothing was issued or taken.
        assert_eq!(detail(&f, &id).await["status"], "draft");
        assert_eq!(balance(&f, &f.batch_id).await, 100);
        let series: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM document_number_series")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(series, 0, "a refused posting must open no numbering series");
    }

    // -------------------------------------------------------------------------------------
    // Harness
    // -------------------------------------------------------------------------------------

    async fn post_controlled(
        f: &Fixture,
        product: &str,
        pack: &str,
        batch: &str,
        basis: &str,
        quantity: i64,
        rate: i64,
    ) -> (StatusCode, Value) {
        let (_, created) = request(f.pool.clone(), "POST", "/api/v1/sales", draft_body(None)).await;
        let id = created["id"].as_str().unwrap().to_owned();
        let (status, added) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            json!({
                "expectedRevision": 1,
                "productId": product,
                "productPackId": pack,
                "batchId": batch,
                "quantityBasis": basis,
                "quantity": quantity,
                "sellingRatePaise": rate
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{added}");
        let total = added["lines"][0]["taxableValuePaise"].as_i64().unwrap();
        // 12% on the taxable value, rounded once per component exactly as the service will.
        let half = (total * 600).div_euclid(10000) + i64::from((total * 600) % 10000 >= 5000);
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/post"),
            json!({
                "expectedRevision": 2,
                "idempotencyKey": Uuid::now_v7().to_string(),
                "tenders": [{ "method": "cash", "amountPaise": total + half * 2 }]
            }),
        )
        .await
    }

    /// The seller facts a pharmacy must hold before it may issue a memo: a registered name, an
    /// operating address, and an active drug sale licence. Recorded here because a Sale cannot be
    /// posted without them — see `store_legal_profile_incomplete`.
    async fn record_legal_profile(pool: &SqlitePool, store_id: &str) {
        sqlx::query(
            "UPDATE store_identity SET legal_name='Care Pharmacy Private Limited',\
             primary_phone='02012345678',primary_email='care@example.test' WHERE store_id=?",
        )
        .bind(store_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO store_addresses (id,store_id,line1,city,state_id,postal_code,\
             created_at_utc,updated_at_utc) VALUES (?,?,'12 Market Road','Pune',?,'411001',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(store_id)
        .bind(MAHARASHTRA)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO store_licences (id,store_id,licence_type,licence_number,\
             normalized_licence_number,created_at_utc,updated_at_utc,include_on_retail_memo) \
             VALUES (?,?,'Form 20','MH-20-1234','MH201234',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'),1)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(store_id)
        .execute(pool)
        .await
        .unwrap();
        // Phase 1L-A3: the facts a registered pharmacy records before selling. Turnover never
        // crossed the e-invoicing threshold (so no Rule 46(s) declaration, and Rule 48(4) does not
        // require e-invoices), and it was up to Rs 5 crore in the year before the fixture's
        // financial year — so HSN is required on B2B invoices only.
        sqlx::query(
            "UPDATE store_identity SET rule46s_declaration_applicability='not_applicable',\
             einvoice_applicability='not_required',\
             dynamic_qr_applicability='not_required',\
             hsn_turnover_band='up_to_5_crore',hsn_turnover_financial_year='2026-27' \
             WHERE store_id=?",
        )
        .bind(store_id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_session(pool: &SqlitePool, role: &str, token: &str) -> String {
        let user_id = Uuid::now_v7().to_string();
        let login = format!("{role}-{}", &user_id[24..32]);
        sqlx::query(
            "INSERT INTO users (id,login_identifier,normalized_login_identifier,display_name,\
             password_hash,role,created_at_utc,updated_at_utc) \
             VALUES (?,?,?,?,'$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA',?,?,?)",
        )
        .bind(&user_id)
        .bind(&login)
        .bind(&login)
        .bind(format!("{role} sale user"))
        .bind(role)
        .bind("2026-01-01T00:00:00.000Z")
        .bind("2026-01-01T00:00:00.000Z")
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO user_sessions (id,user_id,token_hash,created_at_utc,expires_at_utc,\
             last_seen_at_utc) VALUES (?,?,?,?,?,?)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&user_id)
        .bind(crate::api::auth::sha256_hex(token.as_bytes()))
        .bind("2026-01-01T00:00:00.000Z")
        .bind("2099-01-01T00:00:00.000Z")
        .bind("2026-01-01T00:00:00.000Z")
        .execute(pool)
        .await
        .unwrap();
        user_id
    }

    async fn request(
        pool: SqlitePool,
        method: &str,
        uri: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        request_as(pool, method, uri, body, Some(OWNER)).await
    }

    async fn request_as(
        pool: SqlitePool,
        method: &str,
        uri: &str,
        body: Value,
        token: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1:47831")
            .header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header("cookie", format!("aushadharth_session={token}"));
        }
        let response = crate::api::router(pool, None)
            .oneshot(
                request
                    .body(Body::from(if body.is_null() {
                        String::new()
                    } else {
                        body.to_string()
                    }))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, body)
    }

    // -----------------------------------------------------------------------------------------
    // Phase 1L-A — the seller snapshot and the canonical invoice
    //
    // A posted Sale is a document that was handed to a customer. These prove that it keeps saying
    // what it said, whatever happens to the Store, the catalogue or the party master afterwards.
    // -----------------------------------------------------------------------------------------

    /// Posts one complete Sale and returns its id.
    async fn post_one(f: &Fixture) -> String {
        let (_, created) = request(f.pool.clone(), "POST", "/api/v1/sales", draft_body(None)).await;
        let id = created["id"].as_str().unwrap().to_owned();
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            line_body(1, f, "pack", 1, 8000),
        )
        .await;
        let (status, posted) =
            post_sale_request(f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        id
    }

    async fn invoice(f: &Fixture, id: &str) -> (StatusCode, Value) {
        request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/sales/{id}/invoice"),
            Value::Null,
        )
        .await
    }

    /// Removes one of the three particulars Rule 65(4)(3)(i) requires, so the refusal can be seen.
    async fn strip(pool: &SqlitePool, what: &str) {
        let statement = match what {
            "name" => "UPDATE store_identity SET legal_name=NULL",
            "address" => "DELETE FROM store_addresses",
            "licence" => "DELETE FROM store_licences",
            other => panic!("unknown particular {other}"),
        };
        sqlx::query(statement).execute(pool).await.unwrap();
    }

    #[tokio::test]
    async fn a_posted_sale_freezes_the_seller_and_reports_it_as_version_one() {
        let f = fixture().await;
        let id = post_one(&f).await;

        let (status, body) = invoice(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["regulatory"]["sellerSnapshotVersion"], 1);
        assert_eq!(body["regulatory"]["legacyDocument"], false);
        assert_eq!(
            body["sellerSnapshot"]["legalName"],
            "Care Pharmacy Private Limited"
        );
        assert_eq!(body["sellerSnapshot"]["tradeName"], "Care Pharmacy");
        assert_eq!(body["sellerSnapshot"]["addressLine1"], "12 Market Road");
        assert_eq!(body["sellerSnapshot"]["city"], "Pune");
        assert_eq!(body["sellerSnapshot"]["postalCode"], "411001");
        assert_eq!(body["sellerSnapshot"]["stateName"], "Maharashtra");
        assert_eq!(body["sellerSnapshot"]["licenceText"], "Form 20: MH-20-1234");
        assert_eq!(body["sellerSnapshot"]["gstin"], "27AAPFU0939F1ZV");
        // The current profile is for legacy documents only and must not appear beside a snapshot.
        assert!(body["currentSellerProfile"].is_null());
    }

    /// The gate: each missing particular refuses the posting, by name.
    #[tokio::test]
    async fn a_sale_cannot_be_posted_while_the_seller_profile_is_incomplete() {
        for (particular, field) in [
            ("name", "legalName"),
            ("address", "address.line1"),
            ("licence", "licences"),
        ] {
            let f = fixture().await;
            strip(&f.pool, particular).await;

            let (_, created) =
                request(f.pool.clone(), "POST", "/api/v1/sales", draft_body(None)).await;
            let id = created["id"].as_str().unwrap().to_owned();
            request(
                f.pool.clone(),
                "POST",
                &format!("/api/v1/sales/{id}/lines"),
                line_body(1, &f, "pack", 1, 8000),
            )
            .await;
            let (status, body) =
                post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;

            assert_eq!(status, StatusCode::CONFLICT, "{particular}: {body}");
            assert_eq!(body["code"], "store_legal_profile_incomplete");
            assert_eq!(body["issues"][0]["field"], field);
            // Refused before anything was allocated: the series must not have advanced.
            let numbers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM document_number_series")
                .fetch_one(&f.pool)
                .await
                .unwrap();
            assert_eq!(numbers, 0, "a refused posting consumed a number");
        }
    }

    /// An archived licence is not an active one, and archiving the last one closes the counter.
    #[tokio::test]
    async fn archiving_the_only_licence_stops_new_sales_without_touching_old_ones() {
        let f = fixture().await;
        let before = post_one(&f).await;

        sqlx::query(
            "UPDATE store_licences SET status='archived',\
             archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='surrendered'",
        )
        .execute(&f.pool)
        .await
        .unwrap();

        let (_, created) = request(f.pool.clone(), "POST", "/api/v1/sales", draft_body(None)).await;
        let id = created["id"].as_str().unwrap().to_owned();
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            line_body(1, &f, "pack", 1, 8000),
        )
        .await;
        let (status, body) = post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "store_legal_profile_incomplete");

        // The document issued while the licence was active still carries it.
        let (status, old) = invoice(&f, &before).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(old["sellerSnapshot"]["licenceText"], "Form 20: MH-20-1234");
    }

    /// The heart of the phase: everything about the seller can change, and the document does not.
    #[tokio::test]
    async fn editing_the_store_after_posting_never_changes_the_issued_document() {
        let f = fixture().await;
        let id = post_one(&f).await;
        let (_, before) = invoice(&f, &id).await;

        // Every seller fact is changed, including the GSTIN and the licence.
        sqlx::query(
            "UPDATE store_identity SET display_name='Renamed Chemists',\
             legal_name='Renamed Chemists LLP',primary_phone='9999999999',\
             primary_email='new@example.test',gstin='29AAGCB7383J1Z4',\
             normalized_gstin='29AAGCB7383J1Z4',place_of_supply_state_id=?",
        )
        .bind(KARNATAKA)
        .execute(&f.pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE store_addresses SET line1='99 New Road',city='Mumbai',postal_code='400001'",
        )
        .execute(&f.pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE store_licences SET licence_type='Form 21',licence_number='MH-21-9999',\
             normalized_licence_number='MH219999'",
        )
        .execute(&f.pool)
        .await
        .unwrap();

        let (status, after) = invoice(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{after}");
        assert_eq!(
            before["sellerSnapshot"], after["sellerSnapshot"],
            "the issued document acquired the store's later details"
        );
        assert_eq!(
            after["sellerSnapshot"]["legalName"],
            "Care Pharmacy Private Limited"
        );
        assert_eq!(after["sellerSnapshot"]["addressLine1"], "12 Market Road");
        assert_eq!(
            after["sellerSnapshot"]["licenceText"],
            "Form 20: MH-20-1234"
        );
        assert_eq!(after["sellerSnapshot"]["gstin"], "27AAPFU0939F1ZV");
    }

    /// A Sale posted after the edit carries the new details, so the snapshot is not merely frozen —
    /// it tracks the profile at the moment of each posting.
    #[tokio::test]
    async fn a_later_sale_carries_the_later_profile() {
        let f = fixture().await;
        let first = post_one(&f).await;

        sqlx::query("UPDATE store_addresses SET line1='99 New Road'")
            .execute(&f.pool)
            .await
            .unwrap();
        let second = post_one(&f).await;

        let (_, old) = invoice(&f, &first).await;
        let (_, new) = invoice(&f, &second).await;
        assert_eq!(old["sellerSnapshot"]["addressLine1"], "12 Market Road");
        assert_eq!(new["sellerSnapshot"]["addressLine1"], "99 New Road");
    }

    /// Direct SQL cannot rewrite a posted seller snapshot; the frozen posted-row trigger covers it.
    #[tokio::test]
    async fn the_seller_snapshot_cannot_be_mutated_by_direct_sql() {
        let f = fixture().await;
        let id = post_one(&f).await;
        for statement in [
            "UPDATE sale_documents SET seller_legal_name='Someone Else' WHERE id=?",
            "UPDATE sale_documents SET seller_address_line1='Elsewhere' WHERE id=?",
            "UPDATE sale_documents SET seller_licence_text='Forged' WHERE id=?",
            "UPDATE sale_documents SET seller_snapshot_version=0 WHERE id=?",
            "DELETE FROM sale_documents WHERE id=?",
        ] {
            let outcome = sqlx::query(statement).bind(&id).execute(&f.pool).await;
            assert!(outcome.is_err(), "posted document accepted: {statement}");
        }
    }

    /// Renaming a product, changing its HSN, or archiving the lot leaves the document alone and
    /// still readable.
    #[tokio::test]
    async fn catalogue_changes_after_posting_leave_the_document_readable_and_unchanged() {
        let f = fixture().await;
        let id = post_one(&f).await;
        let (_, before) = invoice(&f, &id).await;

        sqlx::query("UPDATE products SET display_name='Paracetamol 500 mg Tablet' WHERE id=?")
            .bind(&f.product_id)
            .execute(&f.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE product_batches SET status='archived',\
             archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='recalled' WHERE id=?")
            .bind(&f.batch_id)
            .execute(&f.pool)
            .await
            .unwrap();

        let (status, after) = invoice(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{after}");
        assert_eq!(
            before["lines"], after["lines"],
            "the lines followed the catalogue"
        );
        assert_eq!(after["lines"][0]["description"], "Crocin 500");
    }

    /// The DTO adds up to the posted header, and says so by refusing when it does not.
    #[tokio::test]
    async fn the_invoice_totals_are_the_posted_totals() {
        let f = fixture().await;
        let id = post_one(&f).await;
        let (_, body) = invoice(&f, &id).await;

        assert_eq!(body["totals"]["taxableValuePaise"], 8000);
        assert_eq!(body["totals"]["cgstPaise"], 480);
        assert_eq!(body["totals"]["sgstPaise"], 480);
        assert_eq!(body["totals"]["grandTotalPaise"], 8960);
        assert_eq!(body["taxSummary"][0]["taxableValuePaise"], 8000);
        assert_eq!(body["taxSummary"][0]["cgstPaise"], 480);
        assert_eq!(body["tender"][0]["amountPaise"], 8960);
        assert_eq!(body["lines"][0]["quantityText"], "1 × Strip");
        assert!(body["lines"][0]["batchNumber"].is_string());
        assert_eq!(body["lines"][0]["batchNumber"], "B-1");
    }

    /// A corrupted stored document is reported, never silently corrected into something plausible.
    #[tokio::test]
    async fn an_invoice_that_does_not_add_up_is_refused_rather_than_repaired() {
        let f = fixture().await;
        let id = post_one(&f).await;
        // Reached around the posted-row trigger deliberately, to simulate corruption rather than a
        // supported edit: this is what a damaged file would look like.
        sqlx::query("PRAGMA writable_schema=ON")
            .execute(&f.pool)
            .await
            .unwrap();
        sqlx::query("DROP TRIGGER sale_documents_posted_no_update")
            .execute(&f.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE sale_documents SET grand_total_paise=999999 WHERE id=?")
            .bind(&id)
            .execute(&f.pool)
            .await
            .unwrap();

        let (status, body) = invoice(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "invoice_invariant_failed");
    }

    /// A draft is not a document and must never be described as one.
    #[tokio::test]
    async fn a_draft_has_no_invoice() {
        let f = fixture().await;
        let (_, created) = request(f.pool.clone(), "POST", "/api/v1/sales", draft_body(None)).await;
        let id = created["id"].as_str().unwrap().to_owned();
        let (status, body) = invoice(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "invoice_not_posted");
    }

    #[tokio::test]
    async fn an_unknown_or_malformed_sale_is_refused_safely() {
        let f = fixture().await;
        let (status, body) = invoice(&f, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "invoice_not_found");

        let (status, body) = invoice(&f, "not-a-uuid").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["code"], "validation_failed");
    }

    /// Every operational role can read the invoice for a Sale they can already see; nobody outside
    /// a session can.
    #[tokio::test]
    async fn reading_an_invoice_follows_the_existing_sale_read_policy() {
        let f = fixture().await;
        let id = post_one(&f).await;
        let uri = format!("/api/v1/sales/{id}/invoice");

        for token in [OWNER, CASHIER] {
            let (status, body) =
                request_as(f.pool.clone(), "GET", &uri, Value::Null, Some(token)).await;
            assert_eq!(status, StatusCode::OK, "{token}: {body}");
        }
        let (status, body) = request_as(f.pool.clone(), "GET", &uri, Value::Null, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    }

    /// A registered seller selling only taxable goods issues a tax invoice.
    #[tokio::test]
    async fn a_taxable_basket_from_a_registered_seller_is_a_tax_invoice() {
        let f = fixture().await;
        let id = post_one(&f).await;
        let (_, body) = invoice(&f, &id).await;
        assert_eq!(body["document"]["documentType"], "tax_invoice");
        assert!(body["document"]["documentTypeReason"].is_null());
        assert_eq!(body["regulatory"]["sellerRegistered"], true);
        assert_eq!(body["regulatory"]["einvoiceApplicable"], false);
    }

    /// An unregistered pharmacy issues no GST document at all. The Drugs Rules memo still applies,
    /// which is why the seller profile is still required.
    #[tokio::test]
    async fn an_unregistered_seller_issues_a_retail_cash_memo() {
        let f = fixture().await;
        sqlx::query(
            "UPDATE store_identity SET gst_registration_status='unregistered',gstin=NULL,\
             normalized_gstin=NULL",
        )
        .execute(&f.pool)
        .await
        .unwrap();
        // Phase 1L-A3: this test used to post an Rs 80 line for Rs 89.60, i.e. an unregistered
        // seller collecting CGST and SGST. CGST Act s.32(1) forbids that, so the Rs 80 line now
        // costs Rs 80 and the posted document carries no GST at all.
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        assert_eq!(quote_of(&f, &id).await["grandTotalPaise"], 8000);
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["grandTotalPaise"], 8000);
        let (_, body) = invoice(&f, &id).await;
        assert_eq!(body["document"]["documentType"], "retail_cash_memo");
        assert_eq!(body["regulatory"]["sellerRegistered"], false);
        assert_eq!(body["totals"]["cgstPaise"], 0);
        assert_eq!(body["totals"]["sgstPaise"], 0);
        // Still a complete seller snapshot: Rule 65 does not care about GST.
        assert_eq!(body["sellerSnapshot"]["licenceText"], "Form 20: MH-20-1234");
    }

    /// The one classification this project refuses to guess: Rule 46A covers a mixed basket sold to
    /// an UNREGISTERED person and says nothing about a registered one.
    #[tokio::test]
    async fn a_mixed_basket_to_a_registered_recipient_is_left_unresolved() {
        let f = fixture().await;
        // A genuinely registered recipient, posted through the API with the Rule 46(d) particulars
        // it needs. Phase 1L-A2's database guard refuses a registered-recipient snapshot with no
        // GSTIN or address, so this can no longer be faked by rewriting the status on a walk-in.
        let party = registered_customer(
            &f.pool,
            "Mehta Medical Stores",
            &valid_gstin("27", "AAACM1234K"),
        )
        .await;
        add_billing_address(&f.pool, &party, "7 Mill Road", Some(MAHARASHTRA), true).await;
        let id = open_sale(&f, json!({ "customerPartyId": party })).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        // The exempt half of the mix is written directly because the fixture catalogue has one tax
        // category. A zero-value line leaves every posted total unchanged.
        sqlx::query(
            "INSERT INTO sale_lines (id,sale_document_id,line_number,product_id,product_pack_id,\
             batch_id,quantity_basis,quantity_packs,quantity_atoms,selling_rate_paise,\
             tax_treatment_kind,taxable_value_paise,line_total_paise,regulatory_snapshot_version,\
             regulatory_schemes_snapshot,regulatory_state_snapshot,manufacturer_company_id,\
             manufacturer_name,created_at_utc,updated_at_utc) \
             SELECT ?,?,2,product_id,product_pack_id,batch_id,'pack',1,10,0,'exempt',0,0,\
             regulatory_snapshot_version,regulatory_schemes_snapshot,regulatory_state_snapshot,\
             manufacturer_company_id,manufacturer_name,created_at_utc,updated_at_utc \
             FROM sale_lines WHERE sale_document_id=? LIMIT 1",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&id)
        .bind(&id)
        .execute(&f.pool)
        .await
        .unwrap();

        let (status, body) = invoice(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["document"]["documentType"],
            "document_classification_unresolved"
        );
        assert!(
            body["document"]["documentTypeReason"]
                .as_str()
                .unwrap()
                .contains("Rule 46A"),
            "the refusal does not say why"
        );
        // The historical Sale still reads in full: only the claim about its type is withheld.
        assert_eq!(body["lines"].as_array().unwrap().len(), 2);
    }

    /// A Sale that predates the snapshot keeps its history and never borrows the current Store's.
    #[tokio::test]
    async fn a_legacy_sale_reports_itself_and_keeps_current_details_separate() {
        let f = fixture().await;
        let id = post_one(&f).await;
        sqlx::query("PRAGMA writable_schema=ON")
            .execute(&f.pool)
            .await
            .unwrap();
        sqlx::query("DROP TRIGGER sale_documents_posted_no_update")
            .execute(&f.pool)
            .await
            .unwrap();
        sqlx::query("DROP TRIGGER sale_documents_seller_snapshot_update")
            .execute(&f.pool)
            .await
            .unwrap();
        // Exactly what a Sale posted before migration 0017 looks like.
        sqlx::query(
            "UPDATE sale_documents SET seller_snapshot_version=0,seller_legal_name=NULL,\
             seller_trade_name=NULL,seller_address_line1=NULL,seller_licence_text=NULL WHERE id=?",
        )
        .bind(&id)
        .execute(&f.pool)
        .await
        .unwrap();

        let (status, body) = invoice(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["regulatory"]["legacyDocument"], true);
        assert_eq!(body["regulatory"]["sellerSnapshotVersion"], 0);
        assert!(
            body["sellerSnapshot"].is_null(),
            "current details were presented as history"
        );
        assert_eq!(
            body["currentSellerProfile"]["displayName"], "Care Pharmacy",
            "the current profile is not offered for inspection"
        );
        assert_eq!(
            body["currentSellerProfile"]["legalName"],
            "Care Pharmacy Private Limited"
        );
    }

    /// A walk-in stays a walk-in: no name is invented, and no address is required of them.
    #[tokio::test]
    async fn a_walk_in_recipient_is_explicit_and_carries_no_invented_details() {
        let f = fixture().await;
        let id = post_one(&f).await;
        let (_, body) = invoice(&f, &id).await;
        assert_eq!(body["recipient"]["walkIn"], true);
        assert!(body["recipient"]["name"].is_null());
        assert!(body["recipient"]["gstin"].is_null());
    }

    /// A named customer's details are the posted ones, even after the party master is edited or
    /// archived.
    #[tokio::test]
    async fn a_named_recipient_keeps_the_name_that_was_on_the_document() {
        let f = fixture().await;
        let (_, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/sales",
            draft_body(Some(&f.customer_id)),
        )
        .await;
        let id = created["id"].as_str().unwrap().to_owned();
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            line_body(1, &f, "pack", 1, 8000),
        )
        .await;
        post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        let (_, before) = invoice(&f, &id).await;

        sqlx::query(
            "UPDATE parties SET display_name='Someone Else',status='archived',\
             archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='moved' WHERE id=?",
        )
        .bind(&f.customer_id)
        .execute(&f.pool)
        .await
        .unwrap();

        let (status, after) = invoice(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{after}");
        assert_eq!(before["recipient"], after["recipient"]);
        assert_eq!(after["recipient"]["walkIn"], false);
    }

    /// Posting and a Store Profile edit race. Whatever the interleaving, the document must carry one
    /// coherent profile — never a name from before an edit beside an address from after it.
    #[tokio::test]
    async fn a_sale_posted_while_the_profile_is_edited_snapshots_one_coherent_state() {
        for attempt in 0..12 {
            let f = fixture().await;
            let (_, created) =
                request(f.pool.clone(), "POST", "/api/v1/sales", draft_body(None)).await;
            let id = created["id"].as_str().unwrap().to_owned();
            request(
                f.pool.clone(),
                "POST",
                &format!("/api/v1/sales/{id}/lines"),
                line_body(1, &f, "pack", 1, 8000),
            )
            .await;

            // Both halves of the profile change together, as one transaction, exactly as the API
            // would. A posting that read between them would produce a torn snapshot.
            let pool = f.pool.clone();
            let editor = tokio::spawn(async move {
                let mut transaction = pool.begin().await.unwrap();
                sqlx::query("UPDATE store_identity SET legal_name='Second Name Limited'")
                    .execute(&mut *transaction)
                    .await
                    .unwrap();
                sqlx::query("UPDATE store_addresses SET line1='Second Road'")
                    .execute(&mut *transaction)
                    .await
                    .unwrap();
                transaction.commit().await.unwrap();
            });
            let key = Uuid::now_v7().to_string();
            let poster = post_sale_request(&f, &id, 2, &key, 8960);
            let (posted, edited) = tokio::join!(poster, editor);
            edited.unwrap();
            assert_eq!(posted.0, StatusCode::OK, "attempt {attempt}: {}", posted.1);

            let (name, line1): (String, String) = sqlx::query_as(
                "SELECT seller_legal_name,seller_address_line1 FROM sale_documents WHERE id=?",
            )
            .bind(&id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
            let coherent = (name == "Care Pharmacy Private Limited" && line1 == "12 Market Road")
                || (name == "Second Name Limited" && line1 == "Second Road");
            assert!(
                coherent,
                "attempt {attempt} produced a torn snapshot: {name} / {line1}"
            );
        }
    }

    /// The same race against a licence change, which lives in a third table.
    #[tokio::test]
    async fn a_sale_posted_while_a_licence_changes_snapshots_one_coherent_state() {
        for attempt in 0..12 {
            let f = fixture().await;
            let (_, created) =
                request(f.pool.clone(), "POST", "/api/v1/sales", draft_body(None)).await;
            let id = created["id"].as_str().unwrap().to_owned();
            request(
                f.pool.clone(),
                "POST",
                &format!("/api/v1/sales/{id}/lines"),
                line_body(1, &f, "pack", 1, 8000),
            )
            .await;

            let pool = f.pool.clone();
            let editor = tokio::spawn(async move {
                let mut transaction = pool.begin().await.unwrap();
                sqlx::query(
                    "UPDATE store_licences SET licence_type='Form 21',licence_number='MH-21-7777',\
                     normalized_licence_number='MH217777'",
                )
                .execute(&mut *transaction)
                .await
                .unwrap();
                transaction.commit().await.unwrap();
            });
            let key = Uuid::now_v7().to_string();
            let poster = post_sale_request(&f, &id, 2, &key, 8960);
            let (posted, edited) = tokio::join!(poster, editor);
            edited.unwrap();
            assert_eq!(posted.0, StatusCode::OK, "attempt {attempt}: {}", posted.1);

            let text: String =
                sqlx::query_scalar("SELECT seller_licence_text FROM sale_documents WHERE id=?")
                    .bind(&id)
                    .fetch_one(&f.pool)
                    .await
                    .unwrap();
            assert!(
                text == "Form 20: MH-20-1234" || text == "Form 21: MH-21-7777",
                "attempt {attempt} produced a half-applied licence: {text}"
            );
        }
    }

    /// Text an operator typed is carried to the document unchanged. Escaping is a renderer's job,
    /// and a snapshot that rewrote it would no longer be what was issued.
    #[tokio::test]
    async fn hostile_and_unicode_store_text_survives_to_the_document_verbatim() {
        let f = fixture().await;
        let hostile = "<script>alert('x')</script> श्री & Söhne";
        sqlx::query("UPDATE store_identity SET legal_name=?")
            .bind(hostile)
            .execute(&f.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE store_addresses SET line1=?")
            .bind(hostile)
            .execute(&f.pool)
            .await
            .unwrap();
        let id = post_one(&f).await;
        let (status, body) = invoice(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["sellerSnapshot"]["legalName"], hostile);
        assert_eq!(body["sellerSnapshot"]["addressLine1"], hostile);
    }

    // -----------------------------------------------------------------------------------------
    // Phase 1L-A2 — recipient statutory particulars
    //
    // Rule 46(d): a registered recipient needs name, address and GSTIN at any value. Rule 46(e): an
    // unregistered one needs name, address, address of delivery and State once the TAXABLE supply
    // reaches ₹50,000. Rule 46(f): the same below that, when the customer asks. These prove the
    // service asks for exactly that, freezes it at posting, and never lets it change afterwards.
    // -----------------------------------------------------------------------------------------

    const COUNTER_ADDRESS: &str = "4 Lake View Society";

    /// A GSTIN for `state` whose check character is genuinely right, found by asking the production
    /// validator rather than restating its algorithm in a test.
    fn valid_gstin(state: &str, pan: &str) -> String {
        let body = format!("{state}{pan}1Z");
        "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"
            .chars()
            .map(|check| format!("{body}{check}"))
            .find(|candidate| crate::domain::parties::normalize_gstin(candidate).is_ok())
            .expect("some check character is valid")
    }

    async fn registered_customer(pool: &SqlitePool, name: &str, gstin: &str) -> String {
        let state = if gstin.starts_with("29") {
            KARNATAKA
        } else {
            MAHARASHTRA
        };
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO parties (id,display_name,normalized_search_name,gst_registration_status,\
             gstin,normalized_gstin,place_of_supply_state_id,created_at_utc,updated_at_utc) \
             VALUES (?,?,?,'registered',?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(name)
        .bind(name.to_lowercase())
        .bind(gstin)
        .bind(gstin)
        .bind(state)
        .execute(pool)
        .await
        .unwrap();
        add_role(pool, &id, "customer").await;
        id
    }

    async fn add_billing_address(
        pool: &SqlitePool,
        party_id: &str,
        line1: &str,
        state: Option<&str>,
        primary: bool,
    ) -> String {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO party_addresses (id,party_id,address_role,line1,city,state_id,postal_code,\
             is_primary,created_at_utc,updated_at_utc) VALUES (?,?,'billing',?,'Pune',?,'411001',?,\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(party_id)
        .bind(line1)
        .bind(state)
        .bind(i64::from(primary))
        .execute(pool)
        .await
        .unwrap();
        id
    }

    /// A lot of the fixture product with no printed MRP and deep stock, so one line can be priced to
    /// any exact taxable value a threshold test needs without an MRP ceiling getting in the way.
    async fn unpriced_lot(f: &Fixture) -> String {
        let batch = insert_batch(&f.pool, &f.pack_id, "BIG-1", Some("2027-12-31"), None).await;
        add_stock(
            &f.pool,
            &f.store_id,
            &f.product_id,
            &f.pack_id,
            &batch,
            100_000,
            &f.owner_id,
        )
        .await;
        batch
    }

    /// An exempt product on its own lot, for baskets whose payable total is not their taxable value.
    async fn exempt_lot(f: &Fixture) -> (String, String, String) {
        let category = insert_category(&f.pool, "exempt-goods", "exempt").await;
        let (product, pack) = insert_product(&f.pool, "Cotton Bandage", TABLET, 0, 10).await;
        enable_sale(&f.pool, &f.store_id, &product, &pack, 1, 0).await;
        classify(&f.pool, &product, &category).await;
        let batch = insert_batch(&f.pool, &pack, "EX-1", Some("2027-12-31"), None).await;
        add_stock(
            &f.pool,
            &f.store_id,
            &product,
            &pack,
            &batch,
            100_000,
            &f.owner_id,
        )
        .await;
        (product, pack, batch)
    }

    fn with_date(header: Value) -> Value {
        let mut body = header;
        body["businessDate"] = json!(TODAY);
        body
    }

    async fn open_sale(f: &Fixture, header: Value) -> String {
        let (status, created) =
            request(f.pool.clone(), "POST", "/api/v1/sales", with_date(header)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        created["id"].as_str().unwrap().to_owned()
    }

    async fn revision_of(f: &Fixture, id: &str) -> i64 {
        detail(f, id).await["revision"].as_i64().unwrap()
    }

    async fn add_line(
        f: &Fixture,
        id: &str,
        product: &str,
        pack: &str,
        batch: &str,
        packs: i64,
        rate: i64,
    ) -> String {
        let revision = revision_of(f, id).await;
        let (status, body) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            json!({
                "expectedRevision": revision,
                "productId": product,
                "productPackId": pack,
                "batchId": batch,
                "quantityBasis": "pack",
                "quantity": packs,
                "sellingRatePaise": rate
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["lines"].as_array().unwrap().last().unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn save_header(f: &Fixture, id: &str, header: Value) -> (StatusCode, Value) {
        let mut body = with_date(header);
        body["expectedRevision"] = json!(revision_of(f, id).await);
        request(f.pool.clone(), "PUT", &format!("/api/v1/sales/{id}"), body).await
    }

    async fn quote_of(f: &Fixture, id: &str) -> Value {
        let (status, body) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/sales/{id}/quote"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    /// Posts for exactly the quoted amount, as the counter does.
    async fn post_as_quoted(f: &Fixture, id: &str) -> (StatusCode, Value) {
        post_as_quoted_by(f, id, OWNER).await
    }

    async fn post_as_quoted_by(f: &Fixture, id: &str, token: &str) -> (StatusCode, Value) {
        if quote_of(f, id).await["supply"]["supervisionRequired"] == true {
            prepare_and_confirm(f, id).await;
        }
        let revision = revision_of(f, id).await;
        let total = quote_of(f, id).await["grandTotalPaise"].as_i64().unwrap();
        request_as(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/post"),
            json!({
                "expectedRevision": revision,
                "idempotencyKey": Uuid::now_v7().to_string(),
                "tenders": [{ "method": "cash", "amountPaise": total }]
            }),
            Some(token),
        )
        .await
    }

    fn counter_address() -> Value {
        json!({
            "line1": COUNTER_ADDRESS, "city": "Pune", "postalCode": "411014", "stateId": MAHARASHTRA
        })
    }

    fn issue_fields(body: &Value) -> Vec<String> {
        body["issues"]
            .as_array()
            .map(|issues| {
                issues
                    .iter()
                    .map(|issue| issue["field"].as_str().unwrap().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn numbers_issued(f: &Fixture) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM sale_documents WHERE status='posted'")
            .fetch_one(&f.pool)
            .await
            .unwrap()
    }

    /// A refused posting leaves no trace: still a draft, no number, no stock movement.
    async fn assert_nothing_posted(f: &Fixture, id: &str, batch: &str, stock_before: i64) {
        let sale = detail(f, id).await;
        assert_eq!(sale["status"], "draft");
        assert!(sale["documentNumber"].is_null());
        assert_eq!(sale["recipientSnapshotVersion"], 0);
        assert_eq!(
            balance(f, batch).await,
            stock_before,
            "a refused posting moved stock"
        );
    }

    // --- Rule 46(d): registered recipient --------------------------------------------------------

    /// A registered recipient at ₹1 of taxable value still needs, and gets, its particulars: there
    /// is no threshold on this path.
    #[tokio::test]
    async fn a_registered_recipient_with_an_address_posts_at_one_rupee_and_freezes_it() {
        let f = fixture().await;
        let gstin = valid_gstin("27", "AAACM1234K");
        let party = registered_customer(&f.pool, "Mehta Medical Stores", &gstin).await;
        add_billing_address(&f.pool, &party, "7 Mill Road", Some(MAHARASHTRA), true).await;
        let id = open_sale(&f, json!({ "customerPartyId": party })).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 100).await;

        let quote = quote_of(&f, &id).await;
        assert_eq!(quote["recipientParticulars"]["required"], true);
        assert_eq!(
            quote["recipientParticulars"]["reasons"],
            json!(["registered_recipient"])
        );
        assert_eq!(quote["recipientParticulars"]["missing"], json!([]));

        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["taxableValuePaise"], 100);
        assert_eq!(posted["recipientSnapshotVersion"], 1);
        assert_eq!(posted["recipientAddressSource"], "party");
        assert_eq!(posted["recipientAddressLine1"], "7 Mill Road");
        assert_eq!(posted["recipientStateName"], "Maharashtra");
        assert_eq!(posted["recipientStateCode"], "27");
        // Rule 46(d) names no address of delivery, so none is asserted.
        assert!(posted["deliverySameAsRecipient"].is_null());

        let (status, document) = invoice(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{document}");
        let recipient = &document["recipient"];
        assert_eq!(recipient["snapshotVersion"], 1);
        assert_eq!(recipient["gstin"], gstin);
        assert_eq!(recipient["name"], "Mehta Medical Stores");
        assert_eq!(recipient["address"]["source"], "party");
        assert_eq!(recipient["address"]["line1"], "7 Mill Road");
        assert_eq!(recipient["address"]["postalCode"], "411001");
        assert!(recipient["delivery"].is_null());
    }

    #[tokio::test]
    async fn a_registered_recipient_without_an_address_is_refused_and_nothing_is_posted() {
        let f = fixture().await;
        let party = registered_customer(
            &f.pool,
            "Mehta Medical Stores",
            &valid_gstin("27", "AAACM1234K"),
        )
        .await;
        let id = open_sale(&f, json!({ "customerPartyId": party })).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let before = balance(&f, &f.batch_id).await;
        let issued = numbers_issued(&f).await;

        let quote = quote_of(&f, &id).await;
        assert_eq!(
            quote["recipientParticulars"]["missing"][0]["field"],
            "customer.billingAddress"
        );
        let (status, body) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "recipient_particulars_incomplete");
        assert_eq!(issue_fields(&body), vec!["customer.billingAddress"]);
        assert!(!body.to_string().to_lowercase().contains("sqlite"));
        assert_nothing_posted(&f, &id, &f.batch_id, before).await;
        assert_eq!(
            numbers_issued(&f).await,
            issued,
            "a refused posting consumed a number"
        );
    }

    /// A shipping address is not the recipient's address, and is never borrowed as one.
    #[tokio::test]
    async fn a_shipping_address_is_not_used_as_the_recipient_address() {
        let f = fixture().await;
        let party = registered_customer(
            &f.pool,
            "Mehta Medical Stores",
            &valid_gstin("27", "AAACM1234K"),
        )
        .await;
        sqlx::query(
            "INSERT INTO party_addresses (id,party_id,address_role,line1,state_id,is_primary,\
             created_at_utc,updated_at_utc) VALUES (?,?,'shipping','Warehouse 4',?,1,\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&party)
        .bind(MAHARASHTRA)
        .execute(&f.pool)
        .await
        .unwrap();
        let id = open_sale(&f, json!({ "customerPartyId": party })).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let (status, body) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(issue_fields(&body), vec!["customer.billingAddress"]);
    }

    /// An archived billing address is not an address the customer has.
    #[tokio::test]
    async fn an_archived_billing_address_does_not_satisfy_the_rule() {
        let f = fixture().await;
        let party = registered_customer(
            &f.pool,
            "Mehta Medical Stores",
            &valid_gstin("27", "AAACM1234K"),
        )
        .await;
        let address =
            add_billing_address(&f.pool, &party, "7 Mill Road", Some(MAHARASHTRA), true).await;
        sqlx::query(
            "UPDATE party_addresses SET status='archived',\
             archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='moved' WHERE id=?",
        )
        .bind(&address)
        .execute(&f.pool)
        .await
        .unwrap();
        let id = open_sale(&f, json!({ "customerPartyId": party })).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let (status, body) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "recipient_particulars_incomplete");
    }

    /// Several billing addresses and no primary: choosing one would be a guess about which the
    /// customer uses, so the service refuses and says how to settle it.
    #[tokio::test]
    async fn several_billing_addresses_without_a_primary_are_refused_rather_than_guessed() {
        let f = fixture().await;
        let party = registered_customer(
            &f.pool,
            "Mehta Medical Stores",
            &valid_gstin("27", "AAACM1234K"),
        )
        .await;
        add_billing_address(&f.pool, &party, "7 Mill Road", Some(MAHARASHTRA), false).await;
        add_billing_address(&f.pool, &party, "9 Station Road", Some(MAHARASHTRA), false).await;
        let id = open_sale(&f, json!({ "customerPartyId": party })).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let (status, body) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(
            body["issues"][0]["message"]
                .as_str()
                .unwrap()
                .contains("primary"),
            "{body}"
        );

        // Marking one primary settles it, and that one is frozen.
        sqlx::query("UPDATE party_addresses SET is_primary=1 WHERE line1='9 Station Road'")
            .execute(&f.pool)
            .await
            .unwrap();
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["recipientAddressLine1"], "9 Station Road");
    }

    /// A GSTIN that is well-shaped but fails its check digit is not a valid GSTIN, whatever the
    /// Party master holds.
    #[tokio::test]
    async fn a_registered_recipient_whose_gstin_fails_its_check_digit_is_refused() {
        let f = fixture().await;
        let good = valid_gstin("27", "AAACM1234K");
        let party = registered_customer(&f.pool, "Mehta Medical Stores", &good).await;
        add_billing_address(&f.pool, &party, "7 Mill Road", Some(MAHARASHTRA), true).await;
        let wrong_check = format!(
            "{}{}",
            &good[..14],
            if good.ends_with('A') { 'B' } else { 'A' }
        );
        sqlx::query("UPDATE parties SET gstin=?,normalized_gstin=? WHERE id=?")
            .bind(&wrong_check)
            .bind(&wrong_check)
            .bind(&party)
            .execute(&f.pool)
            .await
            .unwrap();
        let id = open_sale(&f, json!({ "customerPartyId": party })).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let (status, body) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(issue_fields(&body), vec!["customer.gstin"]);
    }

    #[tokio::test]
    async fn an_archived_registered_recipient_is_refused_at_posting() {
        let f = fixture().await;
        let party = registered_customer(
            &f.pool,
            "Mehta Medical Stores",
            &valid_gstin("27", "AAACM1234K"),
        )
        .await;
        add_billing_address(&f.pool, &party, "7 Mill Road", Some(MAHARASHTRA), true).await;
        let id = open_sale(&f, json!({ "customerPartyId": party })).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        sqlx::query(
            "UPDATE parties SET status='archived',\
             archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='closed' WHERE id=?",
        )
        .bind(&party)
        .execute(&f.pool)
        .await
        .unwrap();
        let revision = revision_of(&f, &id).await;
        let (status, body) =
            post_sale_request(&f, &id, revision, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "customer_not_eligible");
    }

    /// A registered customer from another State still buys at the counter as before: Rule 46(d)
    /// asks for no address of delivery, so nothing on the document contradicts intra-State tax.
    #[tokio::test]
    async fn a_registered_recipient_from_another_state_still_posts_as_a_counter_sale() {
        let f = fixture().await;
        let party = registered_customer(
            &f.pool,
            "Mysuru Pharma Distributors",
            &valid_gstin("29", "AAACM1234K"),
        )
        .await;
        add_billing_address(
            &f.pool,
            &party,
            "12 Sayyaji Rao Road",
            Some(KARNATAKA),
            true,
        )
        .await;
        let id = open_sale(&f, json!({ "customerPartyId": party })).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["taxTreatment"], "intra_state");
        assert_eq!(posted["recipientStateCode"], "29");
        assert_eq!(posted["customerStateCode"], "29");
    }

    // --- Rule 46(e): the ₹50,000 taxable-value threshold ---------------------------------------

    /// ₹49,999.99 of taxable value is below the line even though the payable total, with GST, is
    /// well above ₹50,000. No address is demanded, and none is frozen.
    #[tokio::test]
    async fn just_below_the_threshold_an_ordinary_walk_in_posts_without_an_address() {
        let f = fixture().await;
        let batch = unpriced_lot(&f).await;
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &batch, 1, 4_999_999).await;
        let quote = quote_of(&f, &id).await;
        assert_eq!(quote["taxableValuePaise"], 4_999_999);
        assert!(quote["grandTotalPaise"].as_i64().unwrap() > 5_000_000);
        assert_eq!(quote["recipientParticulars"]["required"], false);
        assert_eq!(
            quote["recipientParticulars"]["taxableSupplyValuePaise"],
            4_999_999
        );

        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["recipientSnapshotVersion"], 1);
        assert_eq!(posted["recipientParticularsRequested"], false);
        assert!(posted["recipientAddressLine1"].is_null());
        assert!(posted["deliverySameAsRecipient"].is_null());
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(document["recipient"]["walkIn"], true);
        assert!(document["recipient"]["address"].is_null());
    }

    #[tokio::test]
    async fn exactly_fifty_thousand_rupees_of_taxable_value_requires_the_particulars() {
        let f = fixture().await;
        let batch = unpriced_lot(&f).await;
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &batch, 1, 5_000_000).await;
        let before = balance(&f, &batch).await;
        let (status, body) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "recipient_particulars_incomplete");
        assert_eq!(
            issue_fields(&body),
            vec![
                "customerNameText",
                "recipientAddress.line1",
                "recipientAddress.stateId"
            ]
        );
        assert_nothing_posted(&f, &id, &batch, before).await;
    }

    #[tokio::test]
    async fn one_paisa_over_the_threshold_requires_the_particulars() {
        let f = fixture().await;
        let batch = unpriced_lot(&f).await;
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &batch, 1, 5_000_001).await;
        let quote = quote_of(&f, &id).await;
        assert_eq!(
            quote["recipientParticulars"]["reasons"],
            json!(["taxable_value_threshold"])
        );
        let (status, body) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "recipient_particulars_incomplete");
    }

    /// The same Sale, completed at the counter, posts and freezes exactly what was typed — without
    /// creating a customer record. The seller and tax snapshots are unaffected.
    #[tokio::test]
    async fn over_the_threshold_complete_counter_particulars_post_without_creating_a_party() {
        let f = fixture().await;
        let batch = unpriced_lot(&f).await;
        let parties_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM parties")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &batch, 1, 5_000_001).await;
        let (status, saved) = save_header(
            &f,
            &id,
            json!({ "customerNameText": "Asha Patil", "recipientAddress": counter_address() }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(
            quote_of(&f, &id).await["recipientParticulars"]["missing"],
            json!([])
        );

        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["recipientAddressSource"], "counter");
        assert_eq!(posted["deliverySameAsRecipient"], true);
        let parties_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM parties")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(
            parties_before, parties_after,
            "a statutory recipient created a Party"
        );

        let (_, document) = invoice(&f, &id).await;
        let recipient = &document["recipient"];
        assert_eq!(recipient["walkIn"], true);
        assert_eq!(recipient["name"], "Asha Patil");
        assert_eq!(recipient["address"]["source"], "counter");
        assert_eq!(recipient["address"]["line1"], COUNTER_ADDRESS);
        assert_eq!(recipient["address"]["stateName"], "Maharashtra");
        assert_eq!(recipient["address"]["stateCode"], "27");
        assert_eq!(recipient["delivery"]["sameAsRecipient"], true);
        assert!(recipient["delivery"]["address"].is_null());
        // Neither neighbouring snapshot was disturbed.
        assert_eq!(
            document["sellerSnapshot"]["legalName"],
            "Care Pharmacy Private Limited"
        );
        assert_eq!(document["regulatory"]["sellerSnapshotVersion"], 1);
        assert_eq!(document["totals"]["taxableValuePaise"], 5_000_001);
        assert_eq!(document["lines"][0]["cgstBasisPoints"], 600);
    }

    /// Fixture A. Taxable ₹49,999 beside ₹10,000 of exempt goods: the basket, and the header's
    /// `taxable_value_paise`, both exceed ₹50,000, but the value of the TAXABLE supply does not.
    #[tokio::test]
    async fn exempt_value_does_not_carry_a_basket_over_the_threshold() {
        let f = fixture().await;
        let batch = unpriced_lot(&f).await;
        let (exempt_product, exempt_pack, exempt_batch) = exempt_lot(&f).await;
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &batch, 1, 4_999_900).await;
        add_line(
            &f,
            &id,
            &exempt_product,
            &exempt_pack,
            &exempt_batch,
            1,
            1_000_000,
        )
        .await;

        let quote = quote_of(&f, &id).await;
        // The repository fact this test exists for: the header column counts the exempt line.
        assert_eq!(quote["taxableValuePaise"], 5_999_900);
        assert_eq!(
            quote["recipientParticulars"]["taxableSupplyValuePaise"],
            4_999_900
        );
        assert_eq!(quote["recipientParticulars"]["required"], false);

        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert!(posted["recipientAddressLine1"].is_null());
    }

    /// Fixture B, with exempt value alongside: taxable exactly ₹50,000 triggers whatever else is in
    /// the basket.
    #[tokio::test]
    async fn taxable_value_at_the_threshold_triggers_beside_exempt_goods() {
        let f = fixture().await;
        let batch = unpriced_lot(&f).await;
        let (exempt_product, exempt_pack, exempt_batch) = exempt_lot(&f).await;
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &batch, 1, 5_000_000).await;
        add_line(
            &f,
            &id,
            &exempt_product,
            &exempt_pack,
            &exempt_batch,
            1,
            500,
        )
        .await;
        let (status, body) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "recipient_particulars_incomplete");
    }

    /// The threshold follows the bill as it is built: crossing it adds the requirement, falling back
    /// removes the one Rule 46(e) caused, and a request keeps particulars required regardless.
    #[tokio::test]
    async fn the_requirement_follows_the_taxable_value_as_lines_change() {
        let f = fixture().await;
        let batch = unpriced_lot(&f).await;
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &batch, 1, 4_000_000).await;
        assert_eq!(
            quote_of(&f, &id).await["recipientParticulars"]["required"],
            false
        );

        let second = add_line(&f, &id, &f.product_id, &f.pack_id, &batch, 1, 1_000_000).await;
        let crossed = quote_of(&f, &id).await;
        assert_eq!(crossed["recipientParticulars"]["required"], true);
        assert_eq!(
            crossed["recipientParticulars"]["reasons"],
            json!(["taxable_value_threshold"])
        );

        let revision = revision_of(&f, &id).await;
        let (status, removed) = request(
            f.pool.clone(),
            "DELETE",
            &format!("/api/v1/sale-lines/{second}"),
            json!({ "expectedRevision": revision }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{removed}");
        assert_eq!(
            quote_of(&f, &id).await["recipientParticulars"]["required"],
            false
        );

        let (status, _) =
            save_header(&f, &id, json!({ "recipientParticularsRequested": true })).await;
        assert_eq!(status, StatusCode::OK);
        let requested = quote_of(&f, &id).await;
        assert_eq!(requested["recipientParticulars"]["required"], true);
        assert_eq!(
            requested["recipientParticulars"]["reasons"],
            json!(["recipient_requested"])
        );
    }

    // --- Rule 46(f): particulars recorded on request ------------------------------------------

    #[tokio::test]
    async fn a_request_below_the_threshold_needs_every_particular_before_posting() {
        let f = fixture().await;
        let id = open_sale(&f, json!({ "recipientParticularsRequested": true })).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let (status, body) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(
            issue_fields(&body),
            vec![
                "customerNameText",
                "recipientAddress.line1",
                "recipientAddress.stateId"
            ]
        );

        // Name and line typed, State still missing.
        let (status, _) = save_header(
            &f,
            &id,
            json!({
                "recipientParticularsRequested": true, "customerNameText": "Asha Patil",
                "recipientAddress": { "line1": COUNTER_ADDRESS }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(issue_fields(&body), vec!["recipientAddress.stateId"]);

        // Complete.
        let (status, _) = save_header(
            &f,
            &id,
            json!({
                "recipientParticularsRequested": true, "customerNameText": "Asha Patil",
                "recipientAddress": counter_address()
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["recipientParticularsRequested"], true);
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(document["recipient"]["particularsRequested"], true);
        assert_eq!(document["recipient"]["address"]["line1"], COUNTER_ADDRESS);
    }

    /// Without the request, an address typed at a small counter sale is not put on the invoice:
    /// Rule 46(f) is the customer asking, not the operator happening to fill in a form.
    #[tokio::test]
    async fn an_address_typed_without_a_request_below_the_threshold_is_not_frozen() {
        let f = fixture().await;
        let id = open_sale(
            &f,
            json!({ "customerNameText": "Asha Patil", "recipientAddress": counter_address() }),
        )
        .await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        assert_eq!(
            quote_of(&f, &id).await["recipientParticulars"]["required"],
            false
        );
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["recipientParticularsRequested"], false);
        assert!(posted["recipientAddressLine1"].is_null());
        assert!(posted["recipientAddressSource"].is_null());
    }

    /// A draft saved before this phase existed has NULL flags. They read as "not requested" and
    /// "delivered to the recipient", which is what an ordinary counter sale was.
    #[tokio::test]
    async fn a_draft_from_before_the_upgrade_posts_as_an_ordinary_sale() {
        let f = fixture().await;
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        sqlx::query(
            "UPDATE sale_documents SET recipient_particulars_requested=NULL,\
             delivery_same_as_recipient=NULL WHERE id=?",
        )
        .bind(&id)
        .execute(&f.pool)
        .await
        .unwrap();
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["recipientSnapshotVersion"], 1);
    }

    // --- Address of delivery -----------------------------------------------------------------

    #[tokio::test]
    async fn delivery_elsewhere_needs_its_own_address_and_freezes_it() {
        let f = fixture().await;
        let batch = unpriced_lot(&f).await;
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &batch, 1, 5_000_000).await;
        let (status, _) = save_header(
            &f,
            &id,
            json!({
                "customerNameText": "Asha Patil", "recipientAddress": counter_address(),
                "deliverySameAsRecipient": false
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(
            issue_fields(&body),
            vec!["deliveryAddress.line1", "deliveryAddress.stateId"]
        );

        let (status, _) = save_header(
            &f,
            &id,
            json!({
                "customerNameText": "Asha Patil", "recipientAddress": counter_address(),
                "deliverySameAsRecipient": false,
                "deliveryAddress": { "line1": "Site Office, Plot 9", "city": "Pimpri", "stateId": MAHARASHTRA }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (_, document) = invoice(&f, &id).await;
        let delivery = &document["recipient"]["delivery"];
        assert_eq!(delivery["sameAsRecipient"], false);
        assert_eq!(delivery["address"]["line1"], "Site Office, Plot 9");
        assert_eq!(delivery["address"]["city"], "Pimpri");
        assert_eq!(delivery["address"]["stateCode"], "27");
    }

    const PUNJAB: &str = "01997300-0000-7000-8000-000000000003";

    /// Moves the fixture pharmacy to Punjab: its GSTIN, place of supply and address all change
    /// together, as the Store Profile would record them.
    async fn relocate_store_to_punjab(f: &Fixture) {
        let gstin = valid_gstin("03", "AAPFU0939F");
        sqlx::query(
            "UPDATE store_identity SET gstin=?,normalized_gstin=?,place_of_supply_state_id=? \
             WHERE store_id=?",
        )
        .bind(&gstin)
        .bind(&gstin)
        .bind(PUNJAB)
        .bind(&f.store_id)
        .execute(&f.pool)
        .await
        .unwrap();
        sqlx::query("UPDATE store_addresses SET city='Ludhiana',state_id=? WHERE store_id=?")
            .bind(PUNJAB)
            .bind(&f.store_id)
            .execute(&f.pool)
            .await
            .unwrap();
    }

    /// Posts one ₹50,000 walk-in counter sale in the Punjab store with the given recipient and
    /// delivery particulars, and returns the posted detail and its invoice.
    async fn punjab_counter_sale(f: &Fixture, batch: &str, header: Value) -> (Value, Value) {
        let id = open_sale(f, json!({})).await;
        add_line(f, &id, &f.product_id, &f.pack_id, batch, 1, 5_000_000).await;
        let (status, saved) = save_header(f, &id, header).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        let (status, posted) = post_as_quoted(f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (status, document) = invoice(f, &id).await;
        assert_eq!(status, StatusCode::OK, "{document}");
        (posted, document)
    }

    /// Test A. A Punjab pharmacy hands goods over the counter to an unregistered customer whose
    /// address is in Karnataka, with Rule 46(e) particulars required. The address is a documentary
    /// particular of the invoice; by itself it proves no movement of goods out of Punjab, so it is
    /// recorded as given and the sale posts.
    #[tokio::test]
    async fn a_counter_handover_to_a_customer_with_an_address_in_another_state_posts() {
        let f = fixture().await;
        relocate_store_to_punjab(&f).await;
        let batch = unpriced_lot(&f).await;
        let (posted, document) = punjab_counter_sale(
            &f,
            &batch,
            json!({
                "customerNameText": "Asha Patil",
                "recipientAddress": { "line1": "4 Brigade Road", "city": "Bengaluru", "stateId": KARNATAKA }
            }),
        )
        .await;
        assert_eq!(posted["storeStateCode"], "03");
        assert_eq!(posted["recipientStateCode"], "29");
        assert_eq!(posted["deliverySameAsRecipient"], true);
        let recipient = &document["recipient"];
        assert_eq!(recipient["address"]["line1"], "4 Brigade Road");
        assert_eq!(recipient["address"]["stateName"], "Karnataka");
        assert_eq!(recipient["address"]["stateCode"], "29");
        assert_eq!(recipient["delivery"]["sameAsRecipient"], true);
    }

    /// The Rule 46(f) path behaves the same way below the threshold: a requested particular in
    /// another State is recorded, not refused.
    #[tokio::test]
    async fn a_requested_particular_in_another_state_is_recorded_below_the_threshold() {
        let f = fixture().await;
        relocate_store_to_punjab(&f).await;
        let id = open_sale(
            &f,
            json!({
                "recipientParticularsRequested": true, "customerNameText": "Asha Patil",
                "recipientAddress": { "line1": "4 Brigade Road", "stateId": KARNATAKA }
            }),
        )
        .await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["recipientParticularsRequested"], true);
        assert_eq!(posted["recipientStateCode"], "29");
    }

    /// Test B. Neither a different-State recipient address nor a different-State delivery address
    /// changes the place of supply, the CGST/SGST/IGST split, or the document classification. The
    /// same basket posted with every address in Punjab produces exactly the same tax facts.
    #[tokio::test]
    async fn a_different_state_address_changes_no_tax_fact_of_a_counter_sale() {
        let f = fixture().await;
        relocate_store_to_punjab(&f).await;
        let batch = unpriced_lot(&f).await;
        let (home, home_document) = punjab_counter_sale(
            &f,
            &batch,
            json!({
                "customerNameText": "Asha Patil",
                "recipientAddress": { "line1": "22 Mall Road", "stateId": PUNJAB }
            }),
        )
        .await;
        let (away, away_document) = punjab_counter_sale(
            &f,
            &batch,
            json!({
                "customerNameText": "Asha Patil",
                "recipientAddress": { "line1": "4 Brigade Road", "stateId": KARNATAKA },
                "deliverySameAsRecipient": false,
                "deliveryAddress": { "line1": "Warehouse 2, Mysore Road", "stateId": KARNATAKA }
            }),
        )
        .await;

        for field in [
            "taxTreatment",
            "storePlaceOfSupplyStateId",
            "storeStateCode",
            "taxableValuePaise",
            "cgstPaise",
            "sgstPaise",
            "igstPaise",
            "cessPaise",
            "grandTotalPaise",
        ] {
            assert_eq!(
                home[field], away[field],
                "{field} changed with the address State"
            );
        }
        assert_eq!(away["taxTreatment"], "intra_state");
        assert_eq!(away["igstPaise"], 0);
        assert!(away["cgstPaise"].as_i64().unwrap() > 0);
        assert_eq!(away["storeStateCode"], "03");
        assert_eq!(
            home_document["document"]["documentType"],
            away_document["document"]["documentType"]
        );
        assert_eq!(away_document["document"]["documentType"], "tax_invoice");
        assert_eq!(home_document["taxSummary"], away_document["taxSummary"]);
        assert_eq!(away_document["regulatory"]["taxTreatment"], "intra_state");
        // The delivery address itself is frozen exactly as supplied.
        let delivery = &away_document["recipient"]["delivery"];
        assert_eq!(delivery["sameAsRecipient"], false);
        assert_eq!(delivery["address"]["line1"], "Warehouse 2, Mysore Road");
        assert_eq!(delivery["address"]["stateCode"], "29");
    }

    /// Tests C and D. Removing the State comparison removed nothing else: a required delivery
    /// address in another State that is missing, or incomplete, is still refused.
    #[tokio::test]
    async fn a_different_state_delivery_address_that_is_missing_or_incomplete_is_still_refused() {
        let f = fixture().await;
        relocate_store_to_punjab(&f).await;
        let batch = unpriced_lot(&f).await;
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &batch, 1, 5_000_000).await;
        let before = balance(&f, &batch).await;

        // C: delivery elsewhere, no delivery address at all.
        let (status, _) = save_header(
            &f,
            &id,
            json!({
                "customerNameText": "Asha Patil",
                "recipientAddress": { "line1": "4 Brigade Road", "stateId": KARNATAKA },
                "deliverySameAsRecipient": false
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "recipient_particulars_incomplete");
        assert_eq!(
            issue_fields(&body),
            vec!["deliveryAddress.line1", "deliveryAddress.stateId"]
        );

        // D: a delivery line in another State, but no State recorded for it.
        let (status, _) = save_header(
            &f,
            &id,
            json!({
                "customerNameText": "Asha Patil",
                "recipientAddress": { "line1": "4 Brigade Road", "stateId": KARNATAKA },
                "deliverySameAsRecipient": false,
                "deliveryAddress": { "line1": "Warehouse 2, Mysore Road", "city": "Mysuru" }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(issue_fields(&body), vec!["deliveryAddress.stateId"]);
        assert_nothing_posted(&f, &id, &batch, before).await;
    }

    // --- Draft validation -------------------------------------------------------------------

    /// A named customer's address lives in their record. The counter cannot type a second one beside
    /// it — through the API or straight into the database.
    #[tokio::test]
    async fn a_counter_address_beside_a_named_customer_is_refused() {
        let f = fixture().await;
        let (status, body) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/sales",
            with_date(
                json!({ "customerPartyId": f.customer_id, "recipientAddress": counter_address() }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(issue_fields(&body), vec!["recipientAddress"]);

        let id = open_sale(&f, json!({ "customerPartyId": f.customer_id })).await;
        let direct =
            sqlx::query("UPDATE sale_documents SET recipient_address_line1='Back Door' WHERE id=?")
                .bind(&id)
                .execute(&f.pool)
                .await;
        assert!(
            direct.is_err(),
            "the database accepted a second address for a named customer"
        );
    }

    #[tokio::test]
    async fn a_delivery_address_without_delivery_elsewhere_is_refused() {
        let f = fixture().await;
        let (status, body) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/sales",
            with_date(json!({ "deliveryAddress": { "line1": "Somewhere" } })),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(issue_fields(&body), vec!["deliveryAddress"]);
    }

    /// Bounds and formats are the Party address's own, and a malformed field is named precisely.
    #[tokio::test]
    async fn malformed_counter_address_fields_are_refused_by_name() {
        let f = fixture().await;
        let unknown_state = Uuid::now_v7().to_string();
        for (address, field) in [
            (
                json!({ "line1": "x".repeat(201) }),
                "recipientAddress.line1",
            ),
            (
                json!({ "line1": "ok", "line2": "y".repeat(201) }),
                "recipientAddress.line2",
            ),
            (
                json!({ "line1": "ok", "city": "c".repeat(101) }),
                "recipientAddress.city",
            ),
            (
                json!({ "line1": "ok", "postalCode": "4110<1>" }),
                "recipientAddress.postalCode",
            ),
            (
                json!({ "line1": "ok", "postalCode": "1".repeat(17) }),
                "recipientAddress.postalCode",
            ),
            (
                json!({ "line1": "ok", "stateId": "not-a-uuid" }),
                "recipientAddress.stateId",
            ),
            (
                json!({ "line1": "ok", "stateId": unknown_state }),
                "recipientAddress.stateId",
            ),
        ] {
            let (status, body) = request(
                f.pool.clone(),
                "POST",
                "/api/v1/sales",
                with_date(json!({ "recipientAddress": address })),
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field}: {body}");
            assert_eq!(issue_fields(&body), vec![field]);
        }
    }

    /// A line of spaces is no address: it is stored as nothing and reported as missing.
    #[tokio::test]
    async fn a_blank_address_line_is_not_an_address() {
        let f = fixture().await;
        let batch = unpriced_lot(&f).await;
        let id = open_sale(
            &f,
            json!({ "customerNameText": "Asha", "recipientAddress": { "line1": "   ", "stateId": MAHARASHTRA } }),
        )
        .await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &batch, 1, 5_000_000).await;
        let (status, body) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(issue_fields(&body), vec!["recipientAddress.line1"]);
    }

    /// What the operator typed reaches the document as typed. Escaping is the renderer's job.
    #[tokio::test]
    async fn unicode_and_markup_in_a_counter_address_survive_verbatim() {
        let f = fixture().await;
        let batch = unpriced_lot(&f).await;
        let hostile = "<img src=x onerror=alert(1)> फ्लैट ७, \"शांति\" निवास & Co";
        let name = "आशा पाटील <b>O'Brien</b>";
        let id = open_sale(
            &f,
            json!({
                "customerNameText": name,
                "recipientAddress": { "line1": hostile, "city": "पुणे", "stateId": MAHARASHTRA }
            }),
        )
        .await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &batch, 1, 5_000_000).await;
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(document["recipient"]["name"], name);
        assert_eq!(document["recipient"]["address"]["line1"], hostile);
        assert_eq!(document["recipient"]["address"]["city"], "पुणे");
    }

    /// Recording statutory particulars is counter work, like the rest of the Sale. No new privilege.
    #[tokio::test]
    async fn a_cashier_records_and_posts_statutory_particulars() {
        let f = fixture().await;
        let batch = unpriced_lot(&f).await;
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &batch, 1, 5_000_000).await;
        let revision = revision_of(&f, &id).await;
        let (status, body) = request_as(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/sales/{id}"),
            json!({
                "expectedRevision": revision, "businessDate": TODAY,
                "customerNameText": "Asha Patil", "recipientAddress": counter_address()
            }),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, posted) = post_as_quoted_by(&f, &id, CASHIER).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
    }

    #[tokio::test]
    async fn an_unknown_party_cannot_be_named_on_a_sale() {
        let f = fixture().await;
        let (status, body) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/sales",
            with_date(json!({ "customerPartyId": Uuid::now_v7().to_string() })),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "customer_not_eligible");
    }

    // --- Authority, immutability and history --------------------------------------------------

    /// The browser that opened the draft saw one address; the record changed before posting. The
    /// posted Sale carries the record as it stood at posting — the browser is not the authority.
    #[tokio::test]
    async fn posting_reads_the_customer_record_not_what_the_screen_last_showed() {
        let f = fixture().await;
        let party = registered_customer(
            &f.pool,
            "Mehta Medical Stores",
            &valid_gstin("27", "AAACM1234K"),
        )
        .await;
        add_billing_address(&f.pool, &party, "7 Mill Road", Some(MAHARASHTRA), true).await;
        let id = open_sale(&f, json!({ "customerPartyId": party })).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let seen = quote_of(&f, &id).await;
        assert_eq!(seen["recipientParticulars"]["missing"], json!([]));

        sqlx::query("UPDATE party_addresses SET line1='21 New Mill Road' WHERE party_id=?")
            .bind(&party)
            .execute(&f.pool)
            .await
            .unwrap();
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["recipientAddressLine1"], "21 New Mill Road");
    }

    /// After posting, nothing that happens to the customer's record reaches the issued document:
    /// not a rename, not an address change, not archiving the address, not archiving the party.
    #[tokio::test]
    async fn customer_record_changes_after_posting_never_reach_the_document() {
        let f = fixture().await;
        let party = registered_customer(
            &f.pool,
            "Mehta Medical Stores",
            &valid_gstin("27", "AAACM1234K"),
        )
        .await;
        add_billing_address(&f.pool, &party, "7 Mill Road", Some(MAHARASHTRA), true).await;
        let id = open_sale(&f, json!({ "customerPartyId": party })).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let (status, _) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK);
        let (_, before) = invoice(&f, &id).await;

        for statement in [
            "UPDATE parties SET display_name='Renamed Traders' WHERE id=?1",
            "UPDATE party_addresses SET line1='99 Elsewhere',city='Nashik',postal_code='422001' WHERE party_id=?1",
            "UPDATE party_addresses SET status='archived',archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='moved' WHERE party_id=?1",
            "UPDATE parties SET status='archived',archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='closed' WHERE id=?1",
        ] {
            sqlx::query(statement)
                .bind(&party)
                .execute(&f.pool)
                .await
                .unwrap_or_else(|error| panic!("{statement}: {error}"));
            let (status, after) = invoice(&f, &id).await;
            assert_eq!(status, StatusCode::OK, "{after}");
            assert_eq!(
                before["recipient"], after["recipient"],
                "after: {statement}"
            );
        }
    }

    /// Direct SQL cannot rewrite a frozen recipient particular on a posted Sale, column by column.
    #[tokio::test]
    async fn the_recipient_snapshot_cannot_be_mutated_by_direct_sql() {
        let f = fixture().await;
        let batch = unpriced_lot(&f).await;
        let id = open_sale(
            &f,
            json!({ "customerNameText": "Asha Patil", "recipientAddress": counter_address() }),
        )
        .await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &batch, 1, 5_000_000).await;
        let (status, _) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK);
        let (_, before) = invoice(&f, &id).await;

        for assignment in [
            "recipient_snapshot_version=0",
            "recipient_particulars_requested=1",
            "recipient_address_source='party'",
            "recipient_address_line1='Forged Road'",
            "recipient_address_line2='Forged'",
            "recipient_city='Forged'",
            "recipient_postal_code='000000'",
            "recipient_state_id=NULL",
            "recipient_state_name='Karnataka'",
            "recipient_state_code='29'",
            "delivery_same_as_recipient=0",
            "delivery_address_line1='Forged'",
            "delivery_state_code='29'",
            "customer_name_text='Someone Else'",
        ] {
            let attempt = sqlx::query(&format!(
                "UPDATE sale_documents SET {assignment} WHERE id=?"
            ))
            .bind(&id)
            .execute(&f.pool)
            .await;
            assert!(
                attempt.is_err(),
                "{assignment} was accepted on a posted Sale"
            );
        }
        let (_, after) = invoice(&f, &id).await;
        assert_eq!(before["recipient"], after["recipient"]);
    }

    /// The database refuses a version-1 snapshot that lacks what Rule 46 requires, even when the
    /// posted-row guard is out of the way and the service is bypassed entirely.
    #[tokio::test]
    async fn the_database_refuses_an_incomplete_recipient_snapshot_whoever_writes_it() {
        let f = fixture().await;
        let party = registered_customer(
            &f.pool,
            "Mehta Medical Stores",
            &valid_gstin("27", "AAACM1234K"),
        )
        .await;
        add_billing_address(&f.pool, &party, "7 Mill Road", Some(MAHARASHTRA), true).await;
        let id = open_sale(&f, json!({ "customerPartyId": party })).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let (status, _) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK);
        sqlx::query("DROP TRIGGER sale_documents_posted_no_update")
            .execute(&f.pool)
            .await
            .unwrap();
        for assignment in [
            // Rule 46(d) without its address.
            "recipient_address_line1=NULL,recipient_address_line2=NULL,recipient_city=NULL,\
             recipient_postal_code=NULL,recipient_state_id=NULL,recipient_state_name=NULL,\
             recipient_state_code=NULL,recipient_address_source=NULL",
            // An address with no provenance.
            "recipient_address_source=NULL",
            // Provenance that contradicts the named party.
            "recipient_address_source='counter'",
            // A State name with no code.
            "recipient_state_code=NULL",
            // Delivery elsewhere with nowhere named.
            "delivery_same_as_recipient=0",
            // The request flag erased.
            "recipient_particulars_requested=NULL",
        ] {
            let attempt = sqlx::query(&format!(
                "UPDATE sale_documents SET {assignment} WHERE id=?"
            ))
            .bind(&id)
            .execute(&f.pool)
            .await;
            let message = attempt
                .err()
                .map(|error| error.to_string())
                .unwrap_or_default();
            assert!(
                message.contains("recipient_snapshot_incomplete"),
                "{assignment} was accepted: {message}"
            );
        }
    }

    /// A Sale posted before recipient particulars existed reports that honestly: version 0, no
    /// address, no delivery, no request flag — and the customer's CURRENT address, although the
    /// record holds one, is not presented as history.
    #[tokio::test]
    async fn a_legacy_sale_reports_unknown_recipient_particulars_without_borrowing_current_ones() {
        let f = fixture().await;
        let party = registered_customer(
            &f.pool,
            "Mehta Medical Stores",
            &valid_gstin("27", "AAACM1234K"),
        )
        .await;
        add_billing_address(&f.pool, &party, "7 Mill Road", Some(MAHARASHTRA), true).await;
        let id = open_sale(&f, json!({ "customerPartyId": party })).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let (status, _) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK);
        for trigger in [
            "sale_documents_posted_no_update",
            "sale_documents_recipient_snapshot_update",
        ] {
            sqlx::query(&format!("DROP TRIGGER {trigger}"))
                .execute(&f.pool)
                .await
                .unwrap();
        }
        // Exactly what a Sale posted before migration 0018 looks like.
        sqlx::query(
            "UPDATE sale_documents SET recipient_snapshot_version=0,\
             recipient_particulars_requested=NULL,recipient_address_source=NULL,\
             recipient_address_line1=NULL,recipient_address_line2=NULL,recipient_city=NULL,\
             recipient_postal_code=NULL,recipient_state_id=NULL,recipient_state_name=NULL,\
             recipient_state_code=NULL,delivery_same_as_recipient=NULL WHERE id=?",
        )
        .bind(&id)
        .execute(&f.pool)
        .await
        .unwrap();

        let (status, document) = invoice(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{document}");
        let recipient = &document["recipient"];
        assert_eq!(recipient["snapshotVersion"], 0);
        assert!(
            recipient["address"].is_null(),
            "a current address was presented as history"
        );
        assert!(recipient["delivery"].is_null());
        assert!(recipient["particularsRequested"].is_null());
        // What Phase 1H did freeze is still reported.
        assert_eq!(recipient["name"], "Mehta Medical Stores");
        assert_eq!(recipient["gstRegistrationStatus"], "registered");
        assert!(!document.to_string().contains("7 Mill Road"));
    }

    /// Posting races an edit to the customer's name and address, committed together. Whatever the
    /// interleaving, the Sale carries one coherent record: never the old name beside the new address.
    #[tokio::test]
    async fn a_sale_posted_while_the_customer_record_is_edited_snapshots_one_coherent_state() {
        for attempt in 0..12 {
            let f = fixture().await;
            let party =
                registered_customer(&f.pool, "Old Traders", &valid_gstin("27", "AAACM1234K")).await;
            add_billing_address(&f.pool, &party, "1 Old Road", Some(MAHARASHTRA), true).await;
            let id = open_sale(&f, json!({ "customerPartyId": party })).await;
            add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
            let revision = revision_of(&f, &id).await;

            let pool = f.pool.clone();
            let party_id = party.clone();
            let editor = tokio::spawn(async move {
                let mut transaction = pool.begin().await.unwrap();
                sqlx::query("UPDATE parties SET display_name='New Traders' WHERE id=?")
                    .bind(&party_id)
                    .execute(&mut *transaction)
                    .await
                    .unwrap();
                sqlx::query("UPDATE party_addresses SET line1='2 New Road' WHERE party_id=?")
                    .bind(&party_id)
                    .execute(&mut *transaction)
                    .await
                    .unwrap();
                transaction.commit().await.unwrap();
            });
            let key = Uuid::now_v7().to_string();
            let poster = post_sale_request(&f, &id, revision, &key, 8960);
            let (posted, edited) = tokio::join!(poster, editor);
            edited.unwrap();
            assert_eq!(posted.0, StatusCode::OK, "attempt {attempt}: {}", posted.1);

            let (name, line1): (String, String) = sqlx::query_as(
                "SELECT customer_display_name,recipient_address_line1 FROM sale_documents WHERE id=?",
            )
            .bind(&id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
            let coherent = (name == "Old Traders" && line1 == "1 Old Road")
                || (name == "New Traders" && line1 == "2 New Road");
            assert!(
                coherent,
                "attempt {attempt} produced a torn recipient: {name} / {line1}"
            );
        }
    }

    /// The same race against a GSTIN correction made together with an address change.
    #[tokio::test]
    async fn a_sale_posted_while_the_gstin_and_address_change_snapshots_one_coherent_state() {
        let old_gstin = valid_gstin("27", "AAACM1234K");
        let new_gstin = valid_gstin("27", "AAACN5678L");
        for attempt in 0..12 {
            let f = fixture().await;
            let party = registered_customer(&f.pool, "Mehta Medical Stores", &old_gstin).await;
            add_billing_address(&f.pool, &party, "1 Old Road", Some(MAHARASHTRA), true).await;
            let id = open_sale(&f, json!({ "customerPartyId": party })).await;
            add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
            let revision = revision_of(&f, &id).await;

            let pool = f.pool.clone();
            let party_id = party.clone();
            let replacement = new_gstin.clone();
            let editor = tokio::spawn(async move {
                let mut transaction = pool.begin().await.unwrap();
                sqlx::query("UPDATE parties SET gstin=?,normalized_gstin=? WHERE id=?")
                    .bind(&replacement)
                    .bind(&replacement)
                    .bind(&party_id)
                    .execute(&mut *transaction)
                    .await
                    .unwrap();
                sqlx::query("UPDATE party_addresses SET line1='2 New Road' WHERE party_id=?")
                    .bind(&party_id)
                    .execute(&mut *transaction)
                    .await
                    .unwrap();
                transaction.commit().await.unwrap();
            });
            let key = Uuid::now_v7().to_string();
            let poster = post_sale_request(&f, &id, revision, &key, 8960);
            let (posted, edited) = tokio::join!(poster, editor);
            edited.unwrap();
            assert_eq!(posted.0, StatusCode::OK, "attempt {attempt}: {}", posted.1);

            let (gstin, line1): (String, String) = sqlx::query_as(
                "SELECT customer_normalized_gstin,recipient_address_line1 FROM sale_documents WHERE id=?",
            )
            .bind(&id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
            let coherent = (gstin == old_gstin && line1 == "1 Old Road")
                || (gstin == new_gstin && line1 == "2 New Road");
            assert!(
                coherent,
                "attempt {attempt} produced a torn recipient: {gstin} / {line1}"
            );
        }
    }

    // -----------------------------------------------------------------------------------------
    // Phase 1L-A3 — seller GST authority and the invoice-compliance facts
    //
    // An unregistered pharmacy may not collect GST (CGST Act s.32(1)); an unknown registration
    // decides nothing. Rule 46(s), the HSN band (Notification No. 78/2020-CT) and the retail-memo
    // licence are facts the pharmacy records, frozen onto each posted Sale.
    // -----------------------------------------------------------------------------------------

    async fn set_seller_status(f: &Fixture, status: &str) {
        let (gstin, normalized): (Option<&str>, Option<&str>) = if status == "registered" {
            (Some("27AAPFU0939F1ZV"), Some("27AAPFU0939F1ZV"))
        } else {
            (None, None)
        };
        sqlx::query(
            "UPDATE store_identity SET gst_registration_status=?,gstin=?,normalized_gstin=? \
             WHERE store_id=?",
        )
        .bind(status)
        .bind(gstin)
        .bind(normalized)
        .bind(&f.store_id)
        .execute(&f.pool)
        .await
        .unwrap();
    }

    /// Sets the Rule 46(s) fact and the HSN band. Rule 48(4) is left alone: see `set_einvoice`.
    async fn set_invoice_facts(f: &Fixture, declaration: &str, band: &str, year: Option<&str>) {
        sqlx::query(
            "UPDATE store_identity SET rule46s_declaration_applicability=?,hsn_turnover_band=?,\
             hsn_turnover_financial_year=? WHERE store_id=?",
        )
        .bind(declaration)
        .bind(band)
        .bind(year)
        .bind(&f.store_id)
        .execute(&f.pool)
        .await
        .unwrap();
    }

    async fn set_einvoice(f: &Fixture, applicability: &str) {
        sqlx::query("UPDATE store_identity SET einvoice_applicability=? WHERE store_id=?")
            .bind(applicability)
            .bind(&f.store_id)
            .execute(&f.pool)
            .await
            .unwrap();
    }

    /// Turns the fixture product into a medicine, which the universal retail memo covers.
    async fn make_medicine(f: &Fixture, product_id: &str) {
        let form = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO dosage_forms (id,canonical_code,display_name,created_at_utc,updated_at_utc) \
             VALUES (?,?,'Tablet',strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&form)
        .bind(format!("tab-{}", &form[28..36]))
        .execute(&f.pool)
        .await
        .unwrap();
        sqlx::query("UPDATE products SET product_kind='medicine',dosage_form_id=? WHERE id=?")
            .bind(&form)
            .bind(product_id)
            .execute(&f.pool)
            .await
            .unwrap();
    }

    /// A medicine established to be outside every schedule that gates a sale.
    ///
    /// Since Phase 1M-A an unclassified medicine cannot be posted, so a test whose subject is
    /// something else — the retail-memo licence, the HSN policy — needs a medicine the Drugs Rules
    /// let it sell, or the regulatory gate answers first and the test proves nothing about its
    /// own rule. Recorded as findings with a source, exactly as an owner would record them.
    async fn make_lawful_medicine(f: &Fixture, product_id: &str) {
        make_medicine(f, product_id).await;
        for scheme in crate::domain::regulatory::SALE_GATING_SCHEMES {
            sqlx::query(
                "INSERT INTO product_regulatory_classifications (id,product_id,scheme,applies,\
                 effective_from,source_citation,determined_by_user_id,revision,status,\
                 created_at_utc,updated_at_utc) VALUES (?,?,?,0,'2020-01-01',\
                 'Drugs Rules, 1945, Schedules as amended',?,1,'active',\
                 strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(product_id)
            .bind(scheme)
            .bind(&f.owner_id)
            .execute(&f.pool)
            .await
            .unwrap();
        }
    }

    async fn add_store_licence(f: &Fixture, licence_type: &str, number: &str, designated: bool) {
        sqlx::query(
            "INSERT INTO store_licences (id,store_id,licence_type,licence_number,\
             normalized_licence_number,created_at_utc,updated_at_utc,include_on_retail_memo) \
             VALUES (?,?,?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),?)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&f.store_id)
        .bind(licence_type)
        .bind(number)
        .bind(
            number
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
                .to_ascii_uppercase(),
        )
        .bind(i64::from(designated))
        .execute(&f.pool)
        .await
        .unwrap();
    }

    async fn undesignate_all(f: &Fixture) {
        sqlx::query("UPDATE store_licences SET include_on_retail_memo=0")
            .execute(&f.pool)
            .await
            .unwrap();
    }

    async fn clear_hsn(f: &Fixture, product_id: &str) {
        sqlx::query("UPDATE products SET hsn_code_id=NULL WHERE id=?")
            .bind(product_id)
            .execute(&f.pool)
            .await
            .unwrap();
    }

    /// Posts one line of the fixture product at `rate` per pack and returns (id, posted detail).
    async fn post_line(
        f: &Fixture,
        customer: Option<&str>,
        rate: i64,
    ) -> (String, StatusCode, Value) {
        let id = open_sale(f, json!({ "customerPartyId": customer })).await;
        add_line(f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, rate).await;
        let (status, body) = post_as_quoted(f, &id).await;
        (id, status, body)
    }

    async fn registered_buyer(f: &Fixture) -> String {
        let party = registered_customer(
            &f.pool,
            "Mehta Medical Stores",
            &valid_gstin("27", "AAACM1234K"),
        )
        .await;
        add_billing_address(&f.pool, &party, "7 Mill Road", Some(MAHARASHTRA), true).await;
        party
    }

    fn issue_fields_of(body: &Value) -> Vec<String> {
        issue_fields(body)
    }

    // --- Seller GST status governs the tax -------------------------------------------------

    /// Registered seller: the Phase 1H/1F arithmetic is unchanged — Rs 100 at 6% + 6%.
    #[tokio::test]
    async fn a_registered_seller_still_charges_gst_exactly_as_before() {
        let f = fixture().await;
        let (id, status, posted) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["taxableValuePaise"], 8000);
        assert_eq!(posted["cgstPaise"], 480);
        assert_eq!(posted["sgstPaise"], 480);
        assert_eq!(posted["grandTotalPaise"], 8960);
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(document["lines"][0]["cgstBasisPoints"], 600);
        assert_eq!(document["document"]["documentType"], "tax_invoice");
        assert_eq!(document["regulatory"]["complianceSnapshotVersion"], 1);
        assert_eq!(document["regulatory"]["reverseCharge"], false);
    }

    /// Registered seller, exempt goods: no GST, exactly as before.
    #[tokio::test]
    async fn a_registered_seller_charges_no_gst_on_exempt_goods() {
        let f = fixture().await;
        let (product, pack, batch) = exempt_lot(&f).await;
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &product, &pack, &batch, 1, 5000).await;
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["cgstPaise"], 0);
        assert_eq!(posted["grandTotalPaise"], 5000);
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(document["document"]["documentType"], "bill_of_supply");
    }

    /// Registered seller, mixed basket to a walk-in: the taxable line is taxed and the exempt one is
    /// not, and the document is still an invoice-cum-bill of supply.
    #[tokio::test]
    async fn a_registered_sellers_mixed_basket_is_unchanged() {
        let f = fixture().await;
        let (product, pack, batch) = exempt_lot(&f).await;
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        add_line(&f, &id, &product, &pack, &batch, 1, 5000).await;
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["cgstPaise"], 480);
        assert_eq!(posted["grandTotalPaise"], 8960 + 5000);
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(
            document["document"]["documentType"],
            "invoice_cum_bill_of_supply"
        );
    }

    /// Unregistered seller, product with a 12% GST rate: Rs 100 costs Rs 100. No CGST, SGST, IGST or
    /// cess anywhere — header, lines, rates — while the product's own classification is kept as
    /// metadata, not as tax charged.
    #[tokio::test]
    async fn an_unregistered_seller_charges_no_gst_on_a_taxable_rate_product() {
        let f = fixture().await;
        set_seller_status(&f, "unregistered").await;
        // The no-MRP lot: Rs 100 is above the fixture batch's printed MRP of Rs 95.50.
        let batch = unpriced_lot(&f).await;
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &batch, 1, 10000).await;
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        for field in ["cgstPaise", "sgstPaise", "igstPaise", "cessPaise"] {
            assert_eq!(posted[field], 0, "{field}");
        }
        assert_eq!(posted["taxableValuePaise"], 10000);
        assert_eq!(posted["grandTotalPaise"], 10000);
        let line = &posted["lines"][0];
        for field in [
            "cgstBasisPoints",
            "sgstBasisPoints",
            "igstBasisPoints",
            "cessBasisPoints",
            "cgstPaise",
            "sgstPaise",
            "igstPaise",
            "cessPaise",
        ] {
            assert_eq!(line[field], 0, "line {field}");
        }
        assert!(
            line["taxRateVersionId"].is_null(),
            "a rate was resolved for an unregistered seller"
        );
        assert_eq!(
            line["taxTreatmentKind"], "taxable",
            "product metadata was lost"
        );
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(document["document"]["documentType"], "retail_cash_memo");
        assert_eq!(document["regulatory"]["sellerRegistered"], false);
        assert!(document["regulatory"]["rule46sDeclaration"].is_null());
        assert!(document["regulatory"]["einvoiceApplicability"].is_null());
        assert!(document["regulatory"]["hsnRequiredDigits"].is_null());
    }

    #[tokio::test]
    async fn an_unregistered_seller_charges_no_gst_on_exempt_or_mixed_baskets() {
        let f = fixture().await;
        set_seller_status(&f, "unregistered").await;
        let (product, pack, batch) = exempt_lot(&f).await;
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        add_line(&f, &id, &product, &pack, &batch, 1, 5000).await;
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["cgstPaise"], 0);
        assert_eq!(posted["sgstPaise"], 0);
        assert_eq!(posted["grandTotalPaise"], 13000);
    }

    /// With no GST to charge, a missing tax category is no reason to refuse the sale.
    #[tokio::test]
    async fn an_unregistered_seller_can_sell_a_product_with_no_tax_category() {
        let f = fixture().await;
        set_seller_status(&f, "unregistered").await;
        sqlx::query("UPDATE products SET tax_category_id=NULL WHERE id=?")
            .bind(&f.product_id)
            .execute(&f.pool)
            .await
            .unwrap();
        let (_, status, posted) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert!(posted["lines"][0]["taxTreatmentKind"].is_null());
    }

    /// Unknown registration: neither the quote nor the posting decides whether GST is collected.
    #[tokio::test]
    async fn an_unknown_seller_status_is_refused_rather_than_guessed() {
        let f = fixture().await;
        set_seller_status(&f, "unknown").await;
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let before = balance(&f, &f.batch_id).await;
        let (status, quote) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/sales/{id}/quote"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{quote}");
        assert_eq!(quote["code"], "store_gst_status_unresolved");
        let revision = revision_of(&f, &id).await;
        for amount in [8000, 8960] {
            let (status, body) =
                post_sale_request(&f, &id, revision, &Uuid::now_v7().to_string(), amount).await;
            assert_eq!(status, StatusCode::CONFLICT, "{body}");
            assert_eq!(body["code"], "store_gst_status_unresolved");
        }
        assert_nothing_posted(&f, &id, &f.batch_id, before).await;
    }

    /// The registration and the GSTIN cannot disagree: the frozen 1G-0 trigger refuses it.
    #[tokio::test]
    async fn a_registration_status_that_contradicts_the_gstin_cannot_be_stored() {
        let f = fixture().await;
        for statement in [
            "UPDATE store_identity SET gst_registration_status='unregistered'",
            "UPDATE store_identity SET gst_registration_status='registered',gstin=NULL,normalized_gstin=NULL",
            "UPDATE store_identity SET gst_registration_status='sometimes'",
        ] {
            assert!(
                sqlx::query(statement).execute(&f.pool).await.is_err(),
                "{statement} was accepted"
            );
        }
    }

    /// Quote and posting come from one authority: identical subtotal, tax and total, to the paisa,
    /// for a registered and an unregistered seller alike.
    #[tokio::test]
    async fn the_quote_and_the_posting_agree_for_either_seller() {
        for status in ["registered", "unregistered"] {
            let f = fixture().await;
            set_seller_status(&f, status).await;
            let id = open_sale(&f, json!({})).await;
            add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 3, 7777).await;
            let quote = quote_of(&f, &id).await;
            let (code, posted) = post_as_quoted(&f, &id).await;
            assert_eq!(code, StatusCode::OK, "{status}: {posted}");
            for field in [
                "taxableValuePaise",
                "cgstPaise",
                "sgstPaise",
                "igstPaise",
                "cessPaise",
                "grandTotalPaise",
            ] {
                assert_eq!(quote[field], posted[field], "{status}: {field} drifted");
            }
        }
    }

    /// A Sale already posted keeps the tax it was issued with, whatever the Store later records —
    /// including a legacy Sale that was taxed while the Store was unregistered.
    #[tokio::test]
    async fn historical_sales_keep_their_frozen_tax() {
        let f = fixture().await;
        let (taxed, status, _) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK);
        let (_, before) = invoice(&f, &taxed).await;
        set_seller_status(&f, "unregistered").await;
        let (_, after) = invoice(&f, &taxed).await;
        assert_eq!(before["totals"], after["totals"]);
        assert_eq!(after["totals"]["cgstPaise"], 480);

        // A pre-1L-A3 Sale: version 0, unregistered snapshot, phantom tax. Reported as frozen.
        sqlx::query("DROP TRIGGER sale_documents_posted_no_update")
            .execute(&f.pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE sale_documents SET compliance_snapshot_version=0,\
             store_gst_registration_status='unregistered',seller_retail_licence_text=NULL,\
             rule46s_declaration_snapshot=NULL,einvoice_applicability_snapshot=NULL,\
             hsn_turnover_band_snapshot=NULL,hsn_turnover_financial_year_snapshot=NULL,\
             hsn_required_digits=NULL,dynamic_qr_snapshot_version=0,\
             dynamic_qr_applicability_snapshot=NULL WHERE id=?",
        )
        .bind(&taxed)
        .execute(&f.pool)
        .await
        .unwrap();
        let (_, legacy) = invoice(&f, &taxed).await;
        assert_eq!(legacy["regulatory"]["complianceSnapshotVersion"], 0);
        assert_eq!(legacy["totals"]["cgstPaise"], 480, "history was recomputed");
        assert!(legacy["sellerSnapshot"]["retailMemoLicenceText"].is_null());
    }

    // --- Rule 46(s) ---------------------------------------------------------------------------

    #[tokio::test]
    async fn the_rule_46s_fact_is_frozen_onto_each_sale() {
        let f = fixture().await;
        let (plain, _, _) = post_line(&f, None, 8000).await;
        set_invoice_facts(&f, "applicable", "up_to_5_crore", Some("2026-27")).await;
        let (declared, status, posted) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (_, first) = invoice(&f, &plain).await;
        let (_, second) = invoice(&f, &declared).await;
        assert_eq!(first["regulatory"]["rule46sDeclaration"], "not_applicable");
        assert_eq!(second["regulatory"]["rule46sDeclaration"], "applicable");

        // Changing the Store afterwards changes neither document.
        set_invoice_facts(&f, "not_applicable", "up_to_5_crore", Some("2026-27")).await;
        let (_, second_after) = invoice(&f, &declared).await;
        assert_eq!(second["regulatory"], second_after["regulatory"]);
    }

    #[tokio::test]
    async fn an_unknown_rule_46s_fact_blocks_a_registered_seller_only() {
        let f = fixture().await;
        set_invoice_facts(&f, "unknown", "up_to_5_crore", Some("2026-27")).await;
        let (_, status, body) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "sale_compliance_incomplete");
        assert_eq!(
            issue_fields_of(&body),
            vec!["store.rule46sDeclarationApplicability"]
        );

        // An unregistered seller issues no GST document, so the fact is irrelevant to it.
        set_seller_status(&f, "unregistered").await;
        let (_, status, body) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    // --- Retail-memo licence ------------------------------------------------------------------

    #[tokio::test]
    async fn a_medicine_sale_freezes_only_the_designated_licences() {
        let f = fixture().await;
        make_lawful_medicine(&f, &f.product_id).await;
        add_store_licence(&f, "Form 20B", "MH-20B-9999", false).await;
        add_store_licence(&f, "Form 21", "MH-21-4321", true).await;
        let (id, status, posted) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (_, document) = invoice(&f, &id).await;
        let seller = &document["sellerSnapshot"];
        assert_eq!(
            seller["retailMemoLicenceText"],
            "Form 20: MH-20-1234, Form 21: MH-21-4321"
        );
        // The broad 1L-A text still lists every active licence; the undesignated one is only there.
        assert!(
            seller["licenceText"]
                .as_str()
                .unwrap()
                .contains("MH-20B-9999")
        );
        assert!(
            !seller["retailMemoLicenceText"]
                .as_str()
                .unwrap()
                .contains("MH-20B-9999")
        );
    }

    /// A licence typed to look like a retail licence is not treated as one: only the designation
    /// counts.
    #[tokio::test]
    async fn a_medicine_sale_without_a_designated_licence_is_refused() {
        let f = fixture().await;
        make_lawful_medicine(&f, &f.product_id).await;
        undesignate_all(&f).await;
        add_store_licence(&f, "Retail Drug Licence Form 20", "MH-RETAIL-1", false).await;
        let (_, status, body) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(
            issue_fields_of(&body),
            vec!["store.licences.includeOnRetailMemo"]
        );
    }

    /// The catalogue's `device` kind is a label with no regulatory classification behind it, so a
    /// device line does not by itself assert that Drugs Rule 65 governs it: no designation is
    /// demanded and none is frozen. A medicine on the same Sale still demands one.
    #[tokio::test]
    async fn a_device_line_alone_does_not_assert_the_drugs_retail_memo_licence() {
        let f = fixture().await;
        sqlx::query("UPDATE products SET product_kind='device' WHERE id=?")
            .bind(&f.product_id)
            .execute(&f.pool)
            .await
            .unwrap();
        undesignate_all(&f).await;
        let (id, status, body) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, document) = invoice(&f, &id).await;
        assert!(document["sellerSnapshot"]["retailMemoLicenceText"].is_null());

        // Direct SQL agrees: a device-only Sale with no licence text is coherent.
        let (device_only, _, _) = post_line(&f, None, 8000).await;
        let text: Option<String> =
            sqlx::query_scalar("SELECT seller_retail_licence_text FROM sale_documents WHERE id=?")
                .bind(&device_only)
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(text, None);

        // Turn the same product into a medicine and the requirement is back.
        make_lawful_medicine(&f, &f.product_id).await;
        let (_, status, body) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(
            issue_fields_of(&body),
            vec!["store.licences.includeOnRetailMemo"]
        );
    }

    /// General merchandise needs no designated licence beyond what 1L-A already requires.
    #[tokio::test]
    async fn a_general_merchandise_sale_needs_no_designation() {
        let f = fixture().await;
        undesignate_all(&f).await;
        let (id, status, posted) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (_, document) = invoice(&f, &id).await;
        assert!(document["sellerSnapshot"]["retailMemoLicenceText"].is_null());
    }

    /// Archiving, renaming or redesignating a licence after posting never reaches the document.
    #[tokio::test]
    async fn licence_changes_after_posting_never_reach_the_document() {
        let f = fixture().await;
        make_lawful_medicine(&f, &f.product_id).await;
        let (id, status, _) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK);
        let (_, before) = invoice(&f, &id).await;
        for statement in [
            "UPDATE store_licences SET licence_type='Renamed',licence_number='X-1',normalized_licence_number='X1'",
            "UPDATE store_licences SET include_on_retail_memo=0",
            "UPDATE store_licences SET status='archived',archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='expired'",
        ] {
            sqlx::query(statement).execute(&f.pool).await.unwrap();
            let (_, after) = invoice(&f, &id).await;
            assert_eq!(
                before["sellerSnapshot"], after["sellerSnapshot"],
                "after {statement}"
            );
        }
        // With its only designated licence archived, the next medicine sale is refused.
        add_store_licence(&f, "Form 20B", "MH-20B-1", false).await;
        let (_, status, body) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
    }

    #[tokio::test]
    async fn unicode_licence_text_is_frozen_verbatim() {
        let f = fixture().await;
        make_lawful_medicine(&f, &f.product_id).await;
        undesignate_all(&f).await;
        add_store_licence(&f, "फॉर्म २० <b>", "MH/२०-१२३४", true).await;
        let (id, status, posted) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(
            document["sellerSnapshot"]["retailMemoLicenceText"],
            "फॉर्म २० <b>: MH/२०-१२३४"
        );
    }

    /// Designation is the owner's to set, through the licence API; omitted on an update it is kept.
    #[tokio::test]
    async fn the_licence_api_sets_and_keeps_the_designation_and_denies_a_cashier() {
        let f = fixture().await;
        let (status, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/store/licences",
            json!({ "licenceType": "Form 21", "licenceNumber": "MH-21-1", "includeOnRetailMemo": true }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let licence = created["licences"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["licenceNumber"] == "MH-21-1")
            .unwrap()
            .clone();
        assert_eq!(licence["includeOnRetailMemo"], true);
        let id = licence["id"].as_str().unwrap();

        let (status, kept) = request(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/store/licences/{id}"),
            json!({ "expectedRevision": 1, "licenceType": "Form 21", "licenceNumber": "MH-21-1" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{kept}");
        let after = kept["licences"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == id)
            .unwrap()
            .clone();
        assert_eq!(
            after["includeOnRetailMemo"], true,
            "an omitted designation was cleared"
        );

        let (status, denied) = request_as(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/store/licences/{id}"),
            json!({ "expectedRevision": 2, "licenceType": "Form 21", "licenceNumber": "MH-21-1", "includeOnRetailMemo": false }),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
    }

    // --- HSN policy (Notification No. 78/2020-CT) ----------------------------------------------

    /// Up to Rs 5 crore: HSN "may not" be mentioned on a tax invoice to an unregistered person.
    #[tokio::test]
    async fn up_to_five_crore_a_b2c_sale_needs_no_hsn() {
        let f = fixture().await;
        clear_hsn(&f, &f.product_id).await;
        let (id, status, posted) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(document["regulatory"]["hsnRequiredDigits"], 0);
        assert_eq!(document["regulatory"]["hsnTurnoverBand"], "up_to_5_crore");
        assert_eq!(
            document["regulatory"]["hsnTurnoverFinancialYear"],
            "2026-27"
        );
    }

    /// Up to Rs 5 crore, registered recipient: four digits, and nothing is padded or guessed.
    #[tokio::test]
    async fn up_to_five_crore_a_b2b_sale_needs_four_digits() {
        let f = fixture().await;
        let buyer = registered_buyer(&f).await;
        clear_hsn(&f, &f.product_id).await;
        let (_, status, body) = post_line(&f, Some(&buyer), 8000).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(issue_fields_of(&body), vec!["lines[1].hsnCode"]);

        for (code, expected) in [
            ("30", StatusCode::CONFLICT),
            ("30A4", StatusCode::CONFLICT),
            ("3004", StatusCode::OK),
        ] {
            assign_hsn(&f.pool, &f.product_id, code).await;
            let (id, status, body) = post_line(&f, Some(&buyer), 8000).await;
            assert_eq!(status, expected, "{code}: {body}");
            if status == StatusCode::OK {
                let (_, document) = invoice(&f, &id).await;
                assert_eq!(document["regulatory"]["hsnRequiredDigits"], 4);
                assert_eq!(document["lines"][0]["hsnCode"], "3004");
            }
        }
    }

    /// More than Rs 5 crore: six digits on every invoice, B2C included.
    #[tokio::test]
    async fn above_five_crore_every_sale_needs_six_digits() {
        let f = fixture().await;
        set_invoice_facts(&f, "not_applicable", "above_5_crore", Some("2026-27")).await;
        assign_hsn(&f.pool, &f.product_id, "3004").await;
        let (_, status, body) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(
            body["issues"][0]["message"]
                .as_str()
                .unwrap()
                .contains("6 digits")
        );
        assign_hsn(&f.pool, &f.product_id, "300490").await;
        let (id, status, body) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(document["regulatory"]["hsnRequiredDigits"], 6);
    }

    /// A band recorded for another financial year governs nothing now. Six digits satisfy any band,
    /// so they still post — with the policy honestly recorded as undetermined.
    #[tokio::test]
    async fn a_band_for_another_financial_year_is_not_applied() {
        let f = fixture().await;
        set_invoice_facts(&f, "not_applicable", "up_to_5_crore", Some("2025-26")).await;
        assign_hsn(&f.pool, &f.product_id, "3004").await;
        let (_, status, body) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(issue_fields_of(&body), vec!["store.hsnTurnoverBand"]);
        assign_hsn(&f.pool, &f.product_id, "30049099").await;
        let (id, status, body) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(document["regulatory"]["hsnTurnoverBand"], "unknown");
        assert!(document["regulatory"]["hsnRequiredDigits"].is_null());
        assert!(document["regulatory"]["hsnTurnoverFinancialYear"].is_null());
    }

    /// An unregistered seller issues no GST document, so no HSN policy applies to it.
    #[tokio::test]
    async fn an_unregistered_seller_needs_no_hsn_at_any_band() {
        let f = fixture().await;
        set_seller_status(&f, "unregistered").await;
        set_invoice_facts(&f, "unknown", "above_5_crore", Some("2026-27")).await;
        clear_hsn(&f, &f.product_id).await;
        let (_, status, body) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    #[tokio::test]
    async fn an_hsn_edited_after_posting_never_reaches_the_document() {
        let f = fixture().await;
        let (id, status, _) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK);
        let (_, before) = invoice(&f, &id).await;
        assign_hsn(&f.pool, &f.product_id, "90189099").await;
        sqlx::query("UPDATE hsn_codes SET description='Changed' WHERE hsn_code='30049099'")
            .execute(&f.pool)
            .await
            .unwrap();
        let (_, after) = invoice(&f, &id).await;
        assert_eq!(before["lines"], after["lines"]);
        assert_eq!(after["lines"][0]["hsnCode"], "30049099");
    }

    // --- Store facts API ------------------------------------------------------------------------

    #[tokio::test]
    async fn the_invoice_compliance_facts_are_the_owners_to_record_and_validated() {
        let f = fixture().await;
        let revision = |body: &Value| body["revision"].as_i64().unwrap();
        let (_, profile) =
            request(f.pool.clone(), "GET", "/api/v1/store/profile", Value::Null).await;
        assert_eq!(profile["rule46sDeclarationApplicability"], "not_applicable");
        assert_eq!(profile["einvoiceApplicability"], "not_required");
        let current = revision(&profile);

        for (body, field) in [
            (
                json!({ "rule46sDeclarationApplicability": "maybe", "einvoiceApplicability": "unknown", "dynamicQrApplicability": "unknown", "hsnTurnoverBand": "unknown" }),
                "rule46sDeclarationApplicability",
            ),
            (
                // The two facts are not interchangeable: Rule 46(s) has no 'required' value.
                json!({ "rule46sDeclarationApplicability": "required", "einvoiceApplicability": "unknown", "dynamicQrApplicability": "unknown", "hsnTurnoverBand": "unknown" }),
                "rule46sDeclarationApplicability",
            ),
            (
                json!({ "rule46sDeclarationApplicability": "unknown", "einvoiceApplicability": "applicable", "dynamicQrApplicability": "unknown", "hsnTurnoverBand": "unknown" }),
                "einvoiceApplicability",
            ),
            (
                json!({ "rule46sDeclarationApplicability": "unknown", "einvoiceApplicability": "unknown", "dynamicQrApplicability": "unknown", "hsnTurnoverBand": "5_crore" }),
                "hsnTurnoverBand",
            ),
            (
                json!({ "rule46sDeclarationApplicability": "unknown", "einvoiceApplicability": "unknown", "dynamicQrApplicability": "unknown", "hsnTurnoverBand": "up_to_5_crore" }),
                "hsnTurnoverFinancialYear",
            ),
            (
                json!({ "rule46sDeclarationApplicability": "unknown", "einvoiceApplicability": "unknown", "dynamicQrApplicability": "unknown", "hsnTurnoverBand": "unknown", "hsnTurnoverFinancialYear": "2026-27" }),
                "hsnTurnoverFinancialYear",
            ),
            (
                json!({ "rule46sDeclarationApplicability": "unknown", "einvoiceApplicability": "unknown", "dynamicQrApplicability": "unknown", "hsnTurnoverBand": "above_5_crore", "hsnTurnoverFinancialYear": "2026-28" }),
                "hsnTurnoverFinancialYear",
            ),
        ] {
            let mut body = body;
            body["expectedRevision"] = json!(current);
            let (status, refused) = request(
                f.pool.clone(),
                "PUT",
                "/api/v1/store/invoice-compliance",
                body,
            )
            .await;
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{field}: {refused}"
            );
            assert_eq!(issue_fields_of(&refused), vec![field]);
        }

        let good = json!({
            "expectedRevision": current, "rule46sDeclarationApplicability": "applicable",
            "einvoiceApplicability": "not_required", "dynamicQrApplicability": "required",
            "hsnTurnoverBand": "above_5_crore", "hsnTurnoverFinancialYear": "2026-27"
        });
        let (status, denied) = request_as(
            f.pool.clone(),
            "PUT",
            "/api/v1/store/invoice-compliance",
            good.clone(),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
        let (status, saved) = request(
            f.pool.clone(),
            "PUT",
            "/api/v1/store/invoice-compliance",
            good.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(saved["hsnTurnoverBand"], "above_5_crore");
        // Each fact is stored as given; neither was derived from the other.
        assert_eq!(saved["rule46sDeclarationApplicability"], "applicable");
        assert_eq!(saved["einvoiceApplicability"], "not_required");
        // A stale write is refused, not merged.
        let (status, stale) = request(
            f.pool.clone(),
            "PUT",
            "/api/v1/store/invoice-compliance",
            good,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{stale}");
        assert_eq!(stale["code"], "revision_conflict");
        let audited: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM master_change_events WHERE entity_type='store_tax_identity' \
             AND change_payload LIKE '%hsnTurnoverBand%'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(audited, 1);
    }

    #[tokio::test]
    async fn a_cashier_posts_under_the_recorded_facts_but_cannot_change_them() {
        let f = fixture().await;
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let (status, posted) = post_as_quoted_by(&f, &id, CASHIER).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (status, _) = request_as(
            f.pool.clone(),
            "PUT",
            "/api/v1/store/tax-identity",
            json!({ "expectedRevision": 1, "gstRegistrationStatus": "unregistered" }),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    // --- Database guards -------------------------------------------------------------------

    #[tokio::test]
    async fn the_compliance_snapshot_cannot_be_mutated_by_direct_sql() {
        let f = fixture().await;
        make_lawful_medicine(&f, &f.product_id).await;
        let (id, status, _) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK);
        for assignment in [
            "compliance_snapshot_version=0",
            "store_gst_registration_status='unregistered'",
            "seller_retail_licence_text='Forged: 1'",
            "rule46s_declaration_snapshot='applicable'",
            "einvoice_applicability_snapshot='not_required'",
            "hsn_turnover_band_snapshot='above_5_crore'",
            "hsn_turnover_financial_year_snapshot='2030-31'",
            "hsn_required_digits=6",
            "cgst_paise=0",
        ] {
            let attempt = sqlx::query(&format!(
                "UPDATE sale_documents SET {assignment} WHERE id=?"
            ))
            .bind(&id)
            .execute(&f.pool)
            .await;
            assert!(
                attempt.is_err(),
                "{assignment} was accepted on a posted Sale"
            );
        }
        let line = sqlx::query("UPDATE sale_lines SET hsn_code='0000' WHERE sale_document_id=?")
            .bind(&id)
            .execute(&f.pool)
            .await;
        assert!(line.is_err(), "a posted line's HSN was rewritten");
    }

    /// With the posted-row guard removed, the completeness trigger still refuses an incoherent
    /// version-1 snapshot: an unregistered seller carrying GST, or a registered one with no Rule
    /// 46(s) fact.
    #[tokio::test]
    async fn the_database_refuses_an_incoherent_compliance_snapshot() {
        let f = fixture().await;
        let (id, status, _) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK);
        sqlx::query("DROP TRIGGER sale_documents_posted_no_update")
            .execute(&f.pool)
            .await
            .unwrap();
        for assignment in [
            "store_gst_registration_status='unregistered',rule46s_declaration_snapshot=NULL,\
             hsn_turnover_band_snapshot=NULL,hsn_turnover_financial_year_snapshot=NULL,hsn_required_digits=NULL,\
             dynamic_qr_applicability_snapshot=NULL",
            "rule46s_declaration_snapshot=NULL",
            // A B2C Sale records no Rule 48(4) determination.
            "einvoice_applicability_snapshot='not_required'",
            "store_gst_registration_status='unknown',dynamic_qr_applicability_snapshot=NULL",
            "hsn_required_digits=NULL",
        ] {
            let attempt = sqlx::query(&format!(
                "UPDATE sale_documents SET {assignment} WHERE id=?"
            ))
            .bind(&id)
            .execute(&f.pool)
            .await;
            let message = attempt
                .err()
                .map(|error| error.to_string())
                .unwrap_or_default();
            assert!(
                message.contains("compliance_snapshot_incomplete"),
                "{assignment}: {message}"
            );
        }
    }

    #[tokio::test]
    async fn a_draft_cannot_carry_a_compliance_snapshot() {
        let f = fixture().await;
        let id = open_sale(&f, json!({})).await;
        let attempt =
            sqlx::query("UPDATE sale_documents SET compliance_snapshot_version=1 WHERE id=?")
                .bind(&id)
                .execute(&f.pool)
                .await;
        assert!(attempt.is_err());
    }

    // --- Concurrency -------------------------------------------------------------------------

    /// Posting races the owner switching the Store to unregistered. Whatever the interleaving, a
    /// posted Sale is either taxed under 'registered' or untaxed under 'unregistered' — or refused
    /// because the tender no longer matches. Never taxed-while-unregistered or the reverse.
    #[tokio::test]
    async fn a_sale_posted_while_the_registration_changes_is_coherent() {
        for attempt in 0..12 {
            let f = fixture().await;
            let id = open_sale(&f, json!({})).await;
            add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
            let revision = revision_of(&f, &id).await;
            let pool = f.pool.clone();
            let editor = tokio::spawn(async move {
                sqlx::query(
                    "UPDATE store_identity SET gst_registration_status='unregistered',gstin=NULL,\
                     normalized_gstin=NULL",
                )
                .execute(&pool)
                .await
                .unwrap();
            });
            let key = Uuid::now_v7().to_string();
            let (posted, edited) =
                tokio::join!(post_sale_request(&f, &id, revision, &key, 8960), editor);
            edited.unwrap();
            if posted.0 == StatusCode::OK {
                assert_eq!(
                    posted.1["storeGstRegistrationStatus"], "registered",
                    "attempt {attempt}"
                );
                assert_eq!(posted.1["cgstPaise"], 480, "attempt {attempt}");
            } else {
                assert_eq!(
                    posted.1["code"], "tender_mismatch",
                    "attempt {attempt}: {}",
                    posted.1
                );
                assert_eq!(detail(&f, &id).await["status"], "draft");
            }
        }
    }

    /// Posting races a redesignation that moves the retail memo from one licence to another in one
    /// transaction. The Sale freezes one side, never both or neither.
    #[tokio::test]
    async fn a_sale_posted_while_the_designation_moves_freezes_one_state() {
        for attempt in 0..12 {
            let f = fixture().await;
            make_lawful_medicine(&f, &f.product_id).await;
            add_store_licence(&f, "Form 21", "MH-21-4321", false).await;
            let id = open_sale(&f, json!({})).await;
            add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
            let revision = revision_of(&f, &id).await;
            let pool = f.pool.clone();
            let editor = tokio::spawn(async move {
                let mut transaction = pool.begin().await.unwrap();
                sqlx::query("UPDATE store_licences SET include_on_retail_memo=0 WHERE licence_number='MH-20-1234'")
                    .execute(&mut *transaction)
                    .await
                    .unwrap();
                sqlx::query("UPDATE store_licences SET include_on_retail_memo=1 WHERE licence_number='MH-21-4321'")
                    .execute(&mut *transaction)
                    .await
                    .unwrap();
                transaction.commit().await.unwrap();
            });
            let key = Uuid::now_v7().to_string();
            let (posted, edited) =
                tokio::join!(post_sale_request(&f, &id, revision, &key, 8960), editor);
            edited.unwrap();
            assert_eq!(posted.0, StatusCode::OK, "attempt {attempt}: {}", posted.1);
            let text: String = sqlx::query_scalar(
                "SELECT seller_retail_licence_text FROM sale_documents WHERE id=?",
            )
            .bind(&id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
            assert!(
                text == "Form 20: MH-20-1234" || text == "Form 21: MH-21-4321",
                "attempt {attempt} froze {text}"
            );
        }
    }

    /// Posting races a change to both turnover facts together. The snapshot carries one pair.
    #[tokio::test]
    async fn a_sale_posted_while_the_turnover_facts_change_freezes_one_pair() {
        for attempt in 0..12 {
            let f = fixture().await;
            let id = open_sale(&f, json!({})).await;
            add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
            let revision = revision_of(&f, &id).await;
            let pool = f.pool.clone();
            let editor = tokio::spawn(async move {
                sqlx::query(
                    "UPDATE store_identity SET rule46s_declaration_applicability='applicable',\
                     hsn_turnover_band='above_5_crore'",
                )
                .execute(&pool)
                .await
                .unwrap();
            });
            let key = Uuid::now_v7().to_string();
            let (posted, edited) =
                tokio::join!(post_sale_request(&f, &id, revision, &key, 8960), editor);
            edited.unwrap();
            assert_eq!(posted.0, StatusCode::OK, "attempt {attempt}: {}", posted.1);
            let (declaration, band, digits): (String, String, i64) = sqlx::query_as(
                "SELECT rule46s_declaration_snapshot,hsn_turnover_band_snapshot,\
                 hsn_required_digits FROM sale_documents WHERE id=?",
            )
            .bind(&id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
            let coherent =
                (declaration == "not_applicable" && band == "up_to_5_crore" && digits == 0)
                    || (declaration == "applicable" && band == "above_5_crore" && digits == 6);
            assert!(
                coherent,
                "attempt {attempt}: {declaration} / {band} / {digits}"
            );
        }
    }

    /// Everything 1L-A and 1L-A2 froze is still frozen beside the new facts.
    #[tokio::test]
    async fn the_seller_and_recipient_snapshots_are_intact_beside_the_new_facts() {
        let f = fixture().await;
        let buyer = registered_buyer(&f).await;
        let (id, status, posted) = post_line(&f, Some(&buyer), 8000).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(document["regulatory"]["sellerSnapshotVersion"], 1);
        assert_eq!(document["recipient"]["snapshotVersion"], 1);
        assert_eq!(document["recipient"]["address"]["line1"], "7 Mill Road");
        assert_eq!(
            document["sellerSnapshot"]["legalName"],
            "Care Pharmacy Private Limited"
        );
        assert_eq!(document["regulatory"]["hsnRequiredDigits"], 4);
        assert_eq!(document["document"]["documentType"], "tax_invoice");
    }

    // --- 1L-A3 corrective: Rule 46 belongs to a REGISTERED seller's tax invoice ---------------

    /// Posts one pack of `lot` — a lot with no MRP, from `unpriced_lot` — at `rate`, so a boundary
    /// amount is not refused by a batch's printed ceiling.
    async fn post_unpriced(
        f: &Fixture,
        lot: &str,
        header: Value,
        rate: i64,
    ) -> (String, StatusCode, Value) {
        let id = open_sale(f, header).await;
        add_line(f, &id, &f.product_id, &f.pack_id, lot, 1, rate).await;
        let (status, body) = post_as_quoted(f, &id).await;
        (id, status, body)
    }

    /// A. A registered seller's Sale to a registered buyer still needs Rule 46(d)'s address.
    #[tokio::test]
    async fn corrective_a_registered_seller_still_owes_rule_46d_to_a_registered_buyer() {
        let f = fixture().await;
        let gstin = valid_gstin("27", "AAACM1234K");
        let buyer = registered_customer(&f.pool, "Mehta Medical Stores", &gstin).await;
        let (id, status, body) = post_line(&f, Some(&buyer), 8000).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "recipient_particulars_incomplete");
        assert_eq!(issue_fields(&body), vec!["customer.billingAddress"]);
        assert_nothing_posted(&f, &id, &f.batch_id, 100).await;

        add_billing_address(&f.pool, &buyer, "7 Mill Road", Some(MAHARASHTRA), true).await;
        let (id, status, body) = post_line(&f, Some(&buyer), 8000).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(document["document"]["documentType"], "tax_invoice");
        assert_eq!(document["recipient"]["address"]["line1"], "7 Mill Road");
    }

    /// B and C. A registered seller's walk-in: nothing at ₹49,999.99, Rule 46(e) at ₹50,000.
    #[tokio::test]
    async fn corrective_b_c_a_registered_seller_keeps_the_rule_46e_boundary() {
        let f = fixture().await;
        let lot = unpriced_lot(&f).await;
        let (_, status, body) = post_unpriced(&f, &lot, json!({}), 4_999_999).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["recipientAddressLine1"].is_null());

        let (id, status, body) = post_unpriced(&f, &lot, json!({}), 5_000_000).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "recipient_particulars_incomplete");
        assert_eq!(
            quote_of(&f, &id).await["recipientParticulars"]["reasons"],
            json!(["taxable_value_threshold"])
        );
    }

    /// D. A registered seller's customer who asks for their particulars still gets Rule 46(f).
    #[tokio::test]
    async fn corrective_d_a_registered_seller_keeps_the_rule_46f_request() {
        let f = fixture().await;
        let id = open_sale(&f, json!({ "recipientParticularsRequested": true })).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let quote = quote_of(&f, &id).await;
        assert_eq!(quote["sellerGstRegistrationStatus"], "registered");
        assert_eq!(
            quote["recipientParticulars"]["reasons"],
            json!(["recipient_requested"])
        );
        let (status, body) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "recipient_particulars_incomplete");
    }

    /// E and I. An unregistered seller billing a GST-registered buyer issues its ordinary retail
    /// cash memo: no GST, no Rule 46(d) address demanded, and the buyer's GSTIN — a fact — kept.
    #[tokio::test]
    async fn corrective_e_an_unregistered_seller_bills_a_registered_buyer_without_rule_46d() {
        let f = fixture().await;
        set_seller_status(&f, "unregistered").await;
        let gstin = valid_gstin("27", "AAACM1234K");
        // No billing address at all: exactly what Rule 46(d) would have refused.
        let buyer = registered_customer(&f.pool, "Mehta Medical Stores", &gstin).await;
        let id = open_sale(&f, json!({ "customerPartyId": buyer })).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let quote = quote_of(&f, &id).await;
        assert_eq!(quote["sellerGstRegistrationStatus"], "unregistered");
        assert_eq!(quote["recipientParticulars"]["required"], false);
        assert_eq!(quote["recipientParticulars"]["missing"], json!([]));
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["grandTotalPaise"], 8000);
        for field in ["cgstPaise", "sgstPaise", "igstPaise", "cessPaise"] {
            assert_eq!(posted[field], 0, "{field}");
        }
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(document["document"]["documentType"], "retail_cash_memo");
        assert_eq!(document["regulatory"]["sellerRegistered"], false);
        assert_eq!(document["recipient"]["gstin"], gstin.as_str());
        assert!(document["recipient"]["address"].is_null());
        assert_eq!(posted["recipientParticularsRequested"], false);
    }

    /// F and G. An unregistered seller either side of ₹50,000: Rule 46(e) never activates, no GST
    /// is charged, and the document is never a tax invoice.
    #[tokio::test]
    async fn corrective_f_g_an_unregistered_seller_crosses_fifty_thousand_without_rule_46e() {
        let f = fixture().await;
        set_seller_status(&f, "unregistered").await;
        let lot = unpriced_lot(&f).await;
        for rate in [4_999_999, 5_000_000, 5_000_001] {
            let (id, status, body) = post_unpriced(&f, &lot, json!({}), rate).await;
            assert_eq!(status, StatusCode::OK, "{rate}: {body}");
            assert_eq!(body["grandTotalPaise"], rate);
            assert_eq!(body["cgstPaise"], 0);
            assert!(body["recipientAddressLine1"].is_null(), "{rate}");
            let (_, document) = invoice(&f, &id).await;
            assert_eq!(document["document"]["documentType"], "retail_cash_memo");
        }
    }

    /// The Drugs Rules memo is not relaxed with Rule 46: a medicine above ₹50,000 from an
    /// unregistered seller still needs the designated licence.
    #[tokio::test]
    async fn corrective_the_drugs_memo_still_binds_an_unregistered_seller_above_fifty_thousand() {
        let f = fixture().await;
        set_seller_status(&f, "unregistered").await;
        make_lawful_medicine(&f, &f.product_id).await;
        undesignate_all(&f).await;
        let lot = unpriced_lot(&f).await;
        let (_, status, body) = post_unpriced(&f, &lot, json!({}), 5_000_000).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(
            issue_fields_of(&body),
            vec!["store.licences.includeOnRetailMemo"]
        );
    }

    /// H. The Rule 46(f) request has no meaning for an unregistered seller: a draft that says so
    /// is not asked for particulars, and the posted memo does not claim a request was honoured.
    #[tokio::test]
    async fn corrective_h_a_rule_46f_request_asks_nothing_of_an_unregistered_seller() {
        let f = fixture().await;
        set_seller_status(&f, "unregistered").await;
        let id = open_sale(&f, json!({ "recipientParticularsRequested": true })).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let quote = quote_of(&f, &id).await;
        assert_eq!(quote["recipientParticulars"]["required"], false);
        assert_eq!(quote["recipientParticulars"]["reasons"], json!([]));
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["recipientParticularsRequested"], false);
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(document["document"]["documentType"], "retail_cash_memo");
    }

    /// K. The high-turnover pharmacy's ordinary counter bill is not refused because the Rule 46(s)
    /// declaration will later print — whatever Rule 48(4) says about its B2B supplies.
    #[tokio::test]
    async fn corrective_k_a_rule_46s_b2c_sale_posts_and_freezes_the_declaration() {
        for einvoice in ["unknown", "not_required", "required"] {
            let f = fixture().await;
            set_invoice_facts(&f, "applicable", "up_to_5_crore", Some("2026-27")).await;
            set_einvoice(&f, einvoice).await;
            let (id, status, body) = post_line(&f, None, 8000).await;
            assert_eq!(status, StatusCode::OK, "{einvoice}: {body}");
            let (_, document) = invoice(&f, &id).await;
            assert_eq!(document["document"]["documentType"], "tax_invoice");
            assert_eq!(document["regulatory"]["rule46sDeclaration"], "applicable");
            assert!(document["regulatory"]["einvoiceApplicability"].is_null());
            assert_eq!(document["regulatory"]["einvoiceApplicable"], false);
        }
    }

    /// L and N. For a registered recipient the decision is Rule 48(4)'s, and the Rule 46(s) fact
    /// neither helps nor hinders: 'not_required' posts under either Rule 46(s) answer.
    #[tokio::test]
    async fn corrective_l_n_a_b2b_sale_turns_on_einvoice_applicability_not_rule_46s() {
        for rule46s in ["not_applicable", "applicable"] {
            let f = fixture().await;
            set_invoice_facts(&f, rule46s, "up_to_5_crore", Some("2026-27")).await;
            set_einvoice(&f, "not_required").await;
            let buyer = registered_buyer(&f).await;
            let (id, status, body) = post_line(&f, Some(&buyer), 8000).await;
            assert_eq!(status, StatusCode::OK, "{rule46s}: {body}");
            let (_, document) = invoice(&f, &id).await;
            assert_eq!(document["document"]["documentType"], "tax_invoice");
            assert_eq!(document["regulatory"]["rule46sDeclaration"], rule46s);
            assert_eq!(
                document["regulatory"]["einvoiceApplicability"],
                "not_required"
            );
        }
    }

    /// M. Rule 48(4) requires an e-invoice and AUSHADHARTH cannot obtain an IRN: a typed refusal,
    /// under either Rule 46(s) answer, and nothing is issued.
    #[tokio::test]
    async fn corrective_m_an_einvoice_required_b2b_sale_is_refused_with_a_typed_error() {
        for rule46s in ["not_applicable", "applicable"] {
            let f = fixture().await;
            set_invoice_facts(&f, rule46s, "up_to_5_crore", Some("2026-27")).await;
            set_einvoice(&f, "required").await;
            let buyer = registered_buyer(&f).await;
            let (id, status, body) = post_line(&f, Some(&buyer), 8000).await;
            assert_eq!(status, StatusCode::CONFLICT, "{body}");
            assert_eq!(body["code"], "einvoice_required_unsupported");
            assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
            assert_eq!(numbers_issued(&f).await, 0);
        }
    }

    /// O. An unknown Rule 48(4) answer fails only the Sale that depends on it.
    #[tokio::test]
    async fn corrective_o_an_unknown_einvoice_answer_fails_only_a_b2b_sale() {
        let f = fixture().await;
        set_einvoice(&f, "unknown").await;
        let buyer = registered_buyer(&f).await;
        let (id, status, body) = post_line(&f, Some(&buyer), 8000).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "sale_compliance_incomplete");
        assert_eq!(issue_fields_of(&body), vec!["store.einvoiceApplicability"]);
        assert_nothing_posted(&f, &id, &f.batch_id, 100).await;

        let (_, status, body) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// P. A posted document keeps the facts it was issued under when the Store's answers change.
    #[tokio::test]
    async fn corrective_p_the_snapshot_survives_every_store_fact_changing() {
        let f = fixture().await;
        let buyer = registered_buyer(&f).await;
        let (b2b, status, body) = post_line(&f, Some(&buyer), 8000).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        set_invoice_facts(&f, "applicable", "above_5_crore", Some("2026-27")).await;
        let (b2c, status, body) = post_line(&f, None, 8000).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, b2b_before) = invoice(&f, &b2b).await;
        let (_, b2c_before) = invoice(&f, &b2c).await;

        set_einvoice(&f, "required").await;
        set_invoice_facts(&f, "unknown", "unknown", None).await;
        set_seller_status(&f, "unregistered").await;

        let (_, b2b_after) = invoice(&f, &b2b).await;
        let (_, b2c_after) = invoice(&f, &b2c).await;
        assert_eq!(b2b_before["regulatory"], b2b_after["regulatory"]);
        assert_eq!(b2c_before["regulatory"], b2c_after["regulatory"]);
        assert_eq!(
            b2b_after["regulatory"]["einvoiceApplicability"],
            "not_required"
        );
        assert_eq!(
            b2b_after["regulatory"]["rule46sDeclaration"],
            "not_applicable"
        );
        assert_eq!(b2c_after["regulatory"]["rule46sDeclaration"], "applicable");
        assert_eq!(b2c_after["regulatory"]["hsnRequiredDigits"], 6);
        assert_eq!(b2b_after["totals"], b2b_before["totals"]);
    }

    /// Direct SQL cannot write a posted B2B document without its Rule 48(4) determination, cannot
    /// record one as 'required', and cannot relax Rule 46(d) for a registered seller — while the
    /// recreated 0018 trigger does release an unregistered seller from it.
    #[tokio::test]
    async fn corrective_the_database_holds_the_seller_gate_and_the_einvoice_fact() {
        let f = fixture().await;
        let buyer = registered_buyer(&f).await;
        let (b2b, status, body) = post_line(&f, Some(&buyer), 8000).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        sqlx::query("DROP TRIGGER sale_documents_posted_no_update")
            .execute(&f.pool)
            .await
            .unwrap();
        for (assignment, code) in [
            (
                "einvoice_applicability_snapshot=NULL",
                "compliance_snapshot_incomplete",
            ),
            ("einvoice_applicability_snapshot='required'", "CHECK"),
            (
                "recipient_address_source=NULL,recipient_address_line1=NULL,recipient_city=NULL,\
                 recipient_postal_code=NULL,recipient_state_id=NULL,recipient_state_name=NULL,\
                 recipient_state_code=NULL",
                "recipient_snapshot_incomplete",
            ),
        ] {
            let message = sqlx::query(&format!(
                "UPDATE sale_documents SET {assignment} WHERE id=?"
            ))
            .bind(&b2b)
            .execute(&f.pool)
            .await
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
            assert!(message.contains(code), "{assignment}: {message}");
        }

        // The same address-less B2B snapshot is coherent for an unregistered seller with no GST.
        set_seller_status(&f, "unregistered").await;
        let buyer_without_address = registered_customer(
            &f.pool,
            "Shah Distributors",
            &valid_gstin("27", "AAACS5678L"),
        )
        .await;
        let (memo, status, body) = post_line(&f, Some(&buyer_without_address), 8000).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let address: Option<String> =
            sqlx::query_scalar("SELECT recipient_address_line1 FROM sale_documents WHERE id=?")
                .bind(&memo)
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(address, None);
        // ...and it still cannot be relabelled as a registered seller's document.
        let message = sqlx::query(
            "UPDATE sale_documents SET store_gst_registration_status='registered' WHERE id=?",
        )
        .bind(&memo)
        .execute(&f.pool)
        .await
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
        assert!(
            message.contains("snapshot_incomplete"),
            "relabelled: {message}"
        );
    }

    /// Posting races the owner switching Rule 48(4) to 'required'. Either the Sale posted first
    /// under 'not_required', or it is refused — never a posted B2B document while 'required' held.
    #[tokio::test]
    async fn corrective_a_b2b_sale_racing_the_einvoice_answer_is_never_issued_under_required() {
        for attempt in 0..12 {
            let f = fixture().await;
            let buyer = registered_buyer(&f).await;
            let id = open_sale(&f, json!({ "customerPartyId": buyer })).await;
            add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
            let revision = revision_of(&f, &id).await;
            let pool = f.pool.clone();
            let editor = tokio::spawn(async move {
                sqlx::query("UPDATE store_identity SET einvoice_applicability='required'")
                    .execute(&pool)
                    .await
                    .unwrap();
            });
            let key = Uuid::now_v7().to_string();
            let (posted, edited) =
                tokio::join!(post_sale_request(&f, &id, revision, &key, 8960), editor);
            edited.unwrap();
            match posted.0 {
                StatusCode::OK => {
                    let snapshot: Option<String> = sqlx::query_scalar(
                        "SELECT einvoice_applicability_snapshot FROM sale_documents WHERE id=?",
                    )
                    .bind(&id)
                    .fetch_one(&f.pool)
                    .await
                    .unwrap();
                    assert_eq!(
                        snapshot.as_deref(),
                        Some("not_required"),
                        "attempt {attempt}"
                    );
                }
                StatusCode::CONFLICT => {
                    assert_eq!(
                        posted.1["code"], "einvoice_required_unsupported",
                        "attempt {attempt}"
                    );
                    assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
                }
                other => panic!("attempt {attempt}: {other} {}", posted.1),
            }
        }
    }

    // --- Phase 1L-A4: Notification No. 14/2020-CT and document issuability -------------------

    async fn set_dynamic_qr(f: &Fixture, applicability: &str) {
        sqlx::query("UPDATE store_identity SET dynamic_qr_applicability=? WHERE store_id=?")
            .bind(applicability)
            .bind(&f.store_id)
            .execute(&f.pool)
            .await
            .unwrap();
    }

    /// Posts for exactly the quoted amount with one tender of `method`, carrying `reference`.
    async fn post_paid_by(
        f: &Fixture,
        id: &str,
        method: &str,
        reference: Option<&str>,
    ) -> (StatusCode, Value) {
        let revision = revision_of(f, id).await;
        let total = quote_of(f, id).await["grandTotalPaise"].as_i64().unwrap();
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/post"),
            json!({
                "expectedRevision": revision,
                "idempotencyKey": Uuid::now_v7().to_string(),
                "tenders": [{ "method": method, "amountPaise": total, "referenceText": reference }]
            }),
        )
        .await
    }

    /// A walk-in draft with one taxable line of the fixture product.
    async fn b2c_draft(f: &Fixture) -> String {
        let id = open_sale(f, json!({})).await;
        add_line(f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        id
    }

    /// A product of the given GST treatment on its own lot.
    async fn untaxed_lot(f: &Fixture, treatment: &str) -> (String, String, String) {
        let slug = treatment.replace('_', "-");
        let category = insert_category(&f.pool, &format!("{slug}-goods"), treatment).await;
        let (product, pack) =
            insert_product(&f.pool, &format!("{treatment} item"), TABLET, 0, 10).await;
        enable_sale(&f.pool, &f.store_id, &product, &pack, 1, 0).await;
        classify(&f.pool, &product, &category).await;
        // A real HSN, so the A3 HSN policy is never the reason a B2B bill posts or not here.
        assign_hsn(&f.pool, &product, "30059040").await;
        let batch = insert_batch(
            &f.pool,
            &pack,
            &format!("{slug}-1"),
            Some("2027-12-31"),
            None,
        )
        .await;
        add_stock(
            &f.pool,
            &f.store_id,
            &product,
            &pack,
            &batch,
            100_000,
            &f.owner_id,
        )
        .await;
        (product, pack, batch)
    }

    async fn tender_rows(f: &Fixture, id: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM sale_tenders WHERE sale_document_id=?")
            .bind(id)
            .fetch_one(&f.pool)
            .await
            .unwrap()
    }

    async fn dynamic_qr_snapshot(f: &Fixture, id: &str) -> (i64, Option<String>) {
        sqlx::query_as(
            "SELECT dynamic_qr_snapshot_version,dynamic_qr_applicability_snapshot \
             FROM sale_documents WHERE id=?",
        )
        .bind(id)
        .fetch_one(&f.pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a4_not_required_b2c_posts_with_ordinary_tenders() {
        let f = fixture().await;
        let id = b2c_draft(&f).await;
        let (status, body) = post_paid_by(&f, &id, "upi", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            dynamic_qr_snapshot(&f, &id).await,
            (1, Some("not_required".to_owned()))
        );
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(document["regulatory"]["dynamicQrSnapshotVersion"], 1);
        assert_eq!(
            document["regulatory"]["dynamicQrApplicability"],
            "not_required"
        );
        assert!(document["tender"][0]["referenceText"].is_null());
    }

    #[tokio::test]
    async fn a4_unknown_b2c_is_refused_before_anything_is_written() {
        let f = fixture().await;
        set_dynamic_qr(&f, "unknown").await;
        let id = b2c_draft(&f).await;
        assert_eq!(quote_of(&f, &id).await["dynamicQrApplicability"], "unknown");
        let (status, body) = post_paid_by(&f, &id, "cash", None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "sale_compliance_incomplete");
        assert_eq!(issue_fields_of(&body), vec!["store.dynamicQrApplicability"]);
        assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
        assert_eq!(tender_rows(&f, &id).await, 0);
        assert_eq!(numbers_issued(&f).await, 0);
    }

    #[tokio::test]
    async fn a4_required_cash_posts_with_its_cross_reference_facts() {
        let f = fixture().await;
        set_dynamic_qr(&f, "required").await;
        let id = b2c_draft(&f).await;
        let (status, body) = post_paid_by(&f, &id, "cash", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(document["regulatory"]["dynamicQrApplicability"], "required");
        let tender = &document["tender"][0];
        assert_eq!(tender["method"], "cash");
        assert_eq!(tender["amountPaise"], 8960);
        assert!(tender["referenceText"].is_null());
        // The tender's own recorded time, exactly as stored.
        let stored: String =
            sqlx::query_scalar("SELECT created_at_utc FROM sale_tenders WHERE sale_document_id=?")
                .bind(&id)
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(tender["recordedAtUtc"], stored.as_str());
    }

    #[tokio::test]
    async fn a4_required_upi_and_card_post_with_their_references() {
        for method in ["upi", "card"] {
            let f = fixture().await;
            set_dynamic_qr(&f, "required").await;
            let id = b2c_draft(&f).await;
            let (status, body) = post_paid_by(&f, &id, method, Some("  TXN-9281-A  ")).await;
            assert_eq!(status, StatusCode::OK, "{method}: {body}");
            let (_, document) = invoice(&f, &id).await;
            let tender = &document["tender"][0];
            assert_eq!(tender["method"], method);
            assert_eq!(tender["referenceText"], "TXN-9281-A");
            assert_eq!(tender["amountPaise"], 8960);
            assert!(tender["recordedAtUtc"].as_str().unwrap().ends_with('Z'));
        }
    }

    #[tokio::test]
    async fn a4_required_electronic_payment_without_a_reference_is_refused() {
        for (method, reference) in [
            ("upi", None),
            ("card", None),
            ("upi", Some("   ")),
            ("card", Some("")),
        ] {
            let f = fixture().await;
            set_dynamic_qr(&f, "required").await;
            let id = b2c_draft(&f).await;
            let (status, body) = post_paid_by(&f, &id, method, reference).await;
            assert_eq!(
                status,
                StatusCode::CONFLICT,
                "{method} {reference:?}: {body}"
            );
            assert_eq!(body["code"], "payment_reference_required");
            assert_eq!(issue_fields_of(&body), vec!["tenders[0].referenceText"]);
            assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
            assert_eq!(tender_rows(&f, &id).await, 0);
            assert_eq!(numbers_issued(&f).await, 0);
        }
    }

    /// The POS takes one payment per Sale. A split tender stays refused whatever the Store's answer,
    /// so no partial cross-reference can ever be recorded.
    #[tokio::test]
    async fn a4_a_split_tender_is_still_refused_without_side_effects() {
        let f = fixture().await;
        set_dynamic_qr(&f, "required").await;
        let id = b2c_draft(&f).await;
        let revision = revision_of(&f, &id).await;
        let (status, body) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/post"),
            json!({
                "expectedRevision": revision,
                "idempotencyKey": Uuid::now_v7().to_string(),
                "tenders": [
                    { "method": "cash", "amountPaise": 4000 },
                    { "method": "upi", "amountPaise": 4960, "referenceText": "TXN-1" }
                ]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "tender_mismatch");
        assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
        assert_eq!(tender_rows(&f, &id).await, 0);
    }

    /// The answer changes what the document records, never what the customer pays.
    #[tokio::test]
    async fn a4_the_dynamic_qr_answer_never_changes_the_arithmetic() {
        let f = fixture().await;
        let id = b2c_draft(&f).await;
        let mut quotes = Vec::new();
        for answer in ["unknown", "not_required", "required"] {
            set_dynamic_qr(&f, answer).await;
            let quote = quote_of(&f, &id).await;
            assert_eq!(quote["dynamicQrApplicability"], answer);
            quotes.push(quote);
        }
        for field in [
            "taxableValuePaise",
            "cgstPaise",
            "sgstPaise",
            "igstPaise",
            "cessPaise",
            "grandTotalPaise",
        ] {
            assert_eq!(quotes[0][field], quotes[1][field], "{field}");
            assert_eq!(quotes[1][field], quotes[2][field], "{field}");
        }
        assert_eq!(quotes[0]["lines"], quotes[2]["lines"]);
        let (status, posted) = post_paid_by(&f, &id, "cash", None).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["grandTotalPaise"], quotes[2]["grandTotalPaise"]);
    }

    #[tokio::test]
    async fn a4_the_snapshot_survives_the_store_changing_its_answer() {
        let f = fixture().await;
        set_dynamic_qr(&f, "required").await;
        let id = b2c_draft(&f).await;
        let (status, body) = post_paid_by(&f, &id, "card", Some("AUTH-771")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, before) = invoice(&f, &id).await;
        for answer in ["not_required", "unknown"] {
            set_dynamic_qr(&f, answer).await;
            let (_, after) = invoice(&f, &id).await;
            assert_eq!(before["regulatory"], after["regulatory"]);
            assert_eq!(before["tender"], after["tender"]);
        }
    }

    #[tokio::test]
    async fn a4_posted_payment_evidence_cannot_be_rewritten_or_added_to() {
        let f = fixture().await;
        set_dynamic_qr(&f, "required").await;
        let id = b2c_draft(&f).await;
        let (status, body) = post_paid_by(&f, &id, "upi", Some("UPI-1")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        for assignment in [
            "dynamic_qr_snapshot_version=0",
            "dynamic_qr_applicability_snapshot='not_required'",
            "dynamic_qr_applicability_snapshot=NULL",
        ] {
            let attempt = sqlx::query(&format!(
                "UPDATE sale_documents SET {assignment} WHERE id=?"
            ))
            .bind(&id)
            .execute(&f.pool)
            .await;
            assert!(attempt.is_err(), "{assignment} accepted on a posted Sale");
        }
        for statement in [
            "UPDATE sale_tenders SET reference_text='FORGED' WHERE sale_document_id=?",
            "UPDATE sale_tenders SET created_at_utc='2020-01-01T00:00:00.000Z' WHERE sale_document_id=?",
            "DELETE FROM sale_tenders WHERE sale_document_id=?",
        ] {
            let attempt = sqlx::query(statement).bind(&id).execute(&f.pool).await;
            assert!(attempt.is_err(), "{statement} accepted");
        }
        let forged = sqlx::query(
            "INSERT INTO sale_tenders (id,sale_document_id,method,amount_paise,reference_text,\
             created_at_utc) VALUES (?,?,'upi',1,'FORGED','2026-09-18T00:00:00.000Z')",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&id)
        .execute(&f.pool)
        .await;
        assert!(
            forged
                .err()
                .is_some_and(|error| error.to_string().contains("sale_document_is_posted")),
            "a tender was added to a posted Sale"
        );
    }

    /// With the posted-row guard removed, the coherence trigger still refuses a version-1 snapshot
    /// that contradicts the document it sits on.
    #[tokio::test]
    async fn a4_the_database_refuses_an_incoherent_dynamic_qr_snapshot() {
        let f = fixture().await;
        set_dynamic_qr(&f, "required").await;
        let b2c = b2c_draft(&f).await;
        let (status, body) = post_paid_by(&f, &b2c, "card", Some("AUTH-1")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let buyer = registered_buyer(&f).await;
        let (b2b, status, body) = post_line(&f, Some(&buyer), 8000).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        for trigger in [
            "sale_documents_posted_no_update",
            "sale_tenders_posted_no_update",
        ] {
            sqlx::query(&format!("DROP TRIGGER {trigger}"))
                .execute(&f.pool)
                .await
                .unwrap();
        }
        let refused = |message: String| message.contains("dynamic_qr_snapshot_incomplete");
        for (id, assignment) in [
            (&b2c, "dynamic_qr_applicability_snapshot=NULL"),
            (&b2b, "dynamic_qr_applicability_snapshot='not_required'"),
            (&b2c, "compliance_snapshot_version=0"),
        ] {
            let message = sqlx::query(&format!(
                "UPDATE sale_documents SET {assignment} WHERE id=?"
            ))
            .bind(id)
            .execute(&f.pool)
            .await
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
            assert!(
                refused(message.clone()) || message.contains("snapshot_incomplete"),
                "{assignment}: {message}"
            );
        }
        // Stripping the reference from a 'required' card payment makes the header incoherent.
        sqlx::query("UPDATE sale_tenders SET reference_text=NULL WHERE sale_document_id=?")
            .bind(&b2c)
            .execute(&f.pool)
            .await
            .unwrap();
        let message =
            sqlx::query("UPDATE sale_documents SET updated_at_utc=updated_at_utc WHERE id=?")
                .bind(&b2c)
                .execute(&f.pool)
                .await
                .err()
                .map(|error| error.to_string())
                .unwrap_or_default();
        assert!(refused(message.clone()), "{message}");
    }

    /// A draft carries no applicability. Setting the version on a draft trips two guards at once —
    /// the draft guard and the coherence guard (a draft has no compliance snapshot) — and SQLite
    /// reports whichever runs first, so either refusal is the right one. Setting the value alone
    /// trips only the draft guard, which is asserted exactly.
    #[tokio::test]
    async fn a4_a_draft_cannot_carry_a_dynamic_qr_snapshot() {
        let f = fixture().await;
        let id = b2c_draft(&f).await;
        let refusal = |assignment: &str| {
            let pool = f.pool.clone();
            let id = id.clone();
            let statement = format!("UPDATE sale_documents SET {assignment} WHERE id=?");
            async move {
                sqlx::query(&statement)
                    .bind(&id)
                    .execute(&pool)
                    .await
                    .err()
                    .map(|error| error.to_string())
                    .unwrap_or_default()
            }
        };
        let message = refusal("dynamic_qr_snapshot_version=1").await;
        assert!(
            message.contains("dynamic_qr_snapshot_before_posting")
                || message.contains("dynamic_qr_snapshot_incomplete"),
            "{message}"
        );
        let message = refusal("dynamic_qr_applicability_snapshot='required'").await;
        assert!(
            message.contains("dynamic_qr_snapshot_before_posting"),
            "{message}"
        );
        assert_eq!(dynamic_qr_snapshot(&f, &id).await, (0, None));
    }

    /// B2B is outside the notification: an unknown or 'required' answer changes nothing there, and
    /// the A3 e-invoice decision is exactly as it was.
    #[tokio::test]
    async fn a4_b2b_is_untouched_by_the_dynamic_qr_answer() {
        for answer in ["unknown", "required"] {
            let f = fixture().await;
            set_dynamic_qr(&f, answer).await;
            let buyer = registered_buyer(&f).await;
            let id = open_sale(&f, json!({ "customerPartyId": buyer })).await;
            add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
            assert!(quote_of(&f, &id).await["dynamicQrApplicability"].is_null());
            let (status, body) = post_paid_by(&f, &id, "card", None).await;
            assert_eq!(status, StatusCode::OK, "{answer}: {body}");
            assert_eq!(dynamic_qr_snapshot(&f, &id).await, (1, None));

            set_einvoice(&f, "required").await;
            let (_, status, body) = post_line(&f, Some(&buyer), 8000).await;
            assert_eq!(status, StatusCode::CONFLICT, "{body}");
            assert_eq!(body["code"], "einvoice_required_unsupported");
        }
    }

    /// A bill of supply is not a tax invoice, so the notification cannot reach it.
    #[tokio::test]
    async fn a4_a_bill_of_supply_is_untouched_by_the_dynamic_qr_answer() {
        let f = fixture().await;
        set_dynamic_qr(&f, "unknown").await;
        let (product, pack, batch) = untaxed_lot(&f, "exempt").await;
        let id = open_sale(&f, json!({})).await;
        add_line(&f, &id, &product, &pack, &batch, 1, 5000).await;
        assert!(quote_of(&f, &id).await["dynamicQrApplicability"].is_null());
        let (status, body) = post_paid_by(&f, &id, "upi", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(dynamic_qr_snapshot(&f, &id).await, (1, None));
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(document["document"]["documentType"], "bill_of_supply");
    }

    #[tokio::test]
    async fn a4_an_unregistered_seller_is_untouched_by_the_dynamic_qr_answer() {
        for answer in ["unknown", "required"] {
            let f = fixture().await;
            set_seller_status(&f, "unregistered").await;
            set_dynamic_qr(&f, answer).await;
            let id = b2c_draft(&f).await;
            assert!(quote_of(&f, &id).await["dynamicQrApplicability"].is_null());
            let (status, body) = post_paid_by(&f, &id, "upi", None).await;
            assert_eq!(status, StatusCode::OK, "{answer}: {body}");
            assert_eq!(body["cgstPaise"], 0);
            assert_eq!(dynamic_qr_snapshot(&f, &id).await, (1, None));
            let (_, document) = invoice(&f, &id).await;
            assert_eq!(document["document"]["documentType"], "retail_cash_memo");
        }
    }

    /// Whether Notification No. 14/2020-CT reaches a Rule 46A invoice-cum-bill of supply is not settled
    /// by any primary source; it is kept in scope as a conservative product decision (see
    /// `invoice_compliance::dynamic_qr_in_scope`). This pins that decision.
    #[tokio::test]
    async fn a4_an_invoice_cum_bill_of_supply_follows_the_dynamic_qr_answer() {
        let f = fixture().await;
        let (product, pack, batch) = untaxed_lot(&f, "exempt").await;
        let id = b2c_draft(&f).await;
        add_line(&f, &id, &product, &pack, &batch, 1, 5000).await;

        set_dynamic_qr(&f, "unknown").await;
        let (status, body) = post_paid_by(&f, &id, "cash", None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(issue_fields_of(&body), vec!["store.dynamicQrApplicability"]);

        set_dynamic_qr(&f, "required").await;
        let (status, body) = post_paid_by(&f, &id, "upi", None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "payment_reference_required");

        let (status, body) = post_paid_by(&f, &id, "upi", Some("UPI-55")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(
            document["document"]["documentType"],
            "invoice_cum_bill_of_supply"
        );
        assert_eq!(document["regulatory"]["dynamicQrApplicability"], "required");
    }

    /// A registered customer's bill that mixes taxable and untaxed goods is refused before posting,
    /// for every untaxed treatment, and leaves no trace.
    #[tokio::test]
    async fn a4_a_registered_recipients_mixed_supply_is_refused_before_posting() {
        for treatment in ["exempt", "nil_rated", "non_gst"] {
            let f = fixture().await;
            let buyer = registered_buyer(&f).await;
            let (product, pack, batch) = untaxed_lot(&f, treatment).await;
            let id = open_sale(&f, json!({ "customerPartyId": buyer })).await;
            add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
            add_line(&f, &id, &product, &pack, &batch, 1, 5000).await;
            assert_eq!(
                quote_of(&f, &id).await["registeredRecipientMixedSupply"],
                true,
                "{treatment}"
            );
            let (status, body) = post_paid_by(&f, &id, "cash", None).await;
            assert_eq!(status, StatusCode::CONFLICT, "{treatment}: {body}");
            assert_eq!(
                body["code"],
                "registered_recipient_mixed_supply_unsupported"
            );
            assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
            assert_eq!(
                balance(&f, &batch).await,
                100_000,
                "{treatment} stock moved"
            );
            assert_eq!(tender_rows(&f, &id).await, 0);
            assert_eq!(numbers_issued(&f).await, 0, "a number was consumed");
            let snapshots: (i64, i64, i64) = sqlx::query_as(
                "SELECT seller_snapshot_version,compliance_snapshot_version,\
                 dynamic_qr_snapshot_version FROM sale_documents WHERE id=?",
            )
            .bind(&id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
            assert_eq!(snapshots, (0, 0, 0));
        }
    }

    /// The refusal is only for the mix: taxable-only and untaxed-only bills to the same registered
    /// customer post exactly as before, and so does a mixed bill to a walk-in.
    #[tokio::test]
    async fn a4_single_treatment_b2b_and_walk_in_mixed_bills_still_post() {
        let f = fixture().await;
        let buyer = registered_buyer(&f).await;
        let (product, pack, batch) = untaxed_lot(&f, "exempt").await;

        let taxable = open_sale(&f, json!({ "customerPartyId": buyer })).await;
        add_line(
            &f,
            &taxable,
            &f.product_id,
            &f.pack_id,
            &f.batch_id,
            1,
            8000,
        )
        .await;
        let (status, body) = post_paid_by(&f, &taxable, "cash", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let untaxed = open_sale(&f, json!({ "customerPartyId": buyer })).await;
        add_line(&f, &untaxed, &product, &pack, &batch, 1, 5000).await;
        assert_eq!(
            quote_of(&f, &untaxed).await["registeredRecipientMixedSupply"],
            false
        );
        let (status, body) = post_paid_by(&f, &untaxed, "cash", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, document) = invoice(&f, &untaxed).await;
        assert_eq!(document["document"]["documentType"], "bill_of_supply");

        let walk_in = b2c_draft(&f).await;
        add_line(&f, &walk_in, &product, &pack, &batch, 1, 5000).await;
        let (status, body) = post_paid_by(&f, &walk_in, "cash", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, document) = invoice(&f, &walk_in).await;
        assert_eq!(
            document["document"]["documentType"],
            "invoice_cum_bill_of_supply"
        );
    }

    /// The database refuses the mixed-supply transition itself, whoever attempts it.
    #[tokio::test]
    async fn a4_the_database_refuses_to_post_a_registered_mixed_supply() {
        let f = fixture().await;
        let buyer = registered_buyer(&f).await;
        let (product, pack, batch) = untaxed_lot(&f, "exempt").await;
        let id = open_sale(&f, json!({ "customerPartyId": buyer })).await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        add_line(&f, &id, &product, &pack, &batch, 1, 5000).await;
        // Freeze the two treatments on the draft lines, as posting would, then try the header.
        // Since Phase 1M-A posting also freezes a regulatory answer per line; without one the
        // regulatory gate would refuse first and this test would prove nothing about its own guard.
        sqlx::query(
            "UPDATE sale_lines SET tax_treatment_kind=CASE WHEN product_id=? THEN 'exempt' \
             ELSE 'taxable' END,regulatory_snapshot_version=1,regulatory_schemes_snapshot=?,\
             regulatory_state_snapshot='{\"premises_state_code\":\"27\",\"punjab_restricted_supply\":\"not_applicable\"}' \
             WHERE sale_document_id=?",
        )
        .bind(&product)
        .bind(crate::domain::regulatory::ResolvedRegulatory::unknown().to_snapshot_json())
        .bind(&id)
        .execute(&f.pool)
        .await
        .unwrap();
        let message = sqlx::query(
            "UPDATE sale_documents SET status='posted',store_gst_registration_status='registered',\
             customer_gst_registration_status='registered',series_code='INV',financial_year='2026-27',\
             sequence_value=99,document_number='INV/2627/000099',posted_by_user_id=created_by_user_id,\
             posted_at_utc=updated_at_utc,posting_idempotency_key=?,posting_fingerprint=? WHERE id=?",
        )
        .bind(Uuid::now_v7().to_string())
        .bind("d".repeat(64))
        .bind(&id)
        .execute(&f.pool)
        .await
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
        assert!(
            message.contains("registered_recipient_mixed_supply"),
            "{message}"
        );
        assert_eq!(detail(&f, &id).await["status"], "draft");
    }

    /// Rule 46(s), Rule 48(4) and the HSN policy come out exactly as they would without the new
    /// answer, and the new answer comes out exactly as recorded whatever they say.
    #[tokio::test]
    async fn a4_dynamic_qr_is_independent_of_rule_46s_einvoice_and_hsn() {
        for (rule46s, einvoice, dynamic_qr) in [
            ("applicable", "required", "not_required"),
            ("not_applicable", "not_required", "required"),
            ("applicable", "not_required", "required"),
        ] {
            let f = fixture().await;
            set_invoice_facts(&f, rule46s, "above_5_crore", Some("2026-27")).await;
            set_einvoice(&f, einvoice).await;
            set_dynamic_qr(&f, dynamic_qr).await;
            let id = b2c_draft(&f).await;
            let (status, body) = post_paid_by(&f, &id, "card", Some("AUTH-5")).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let (_, document) = invoice(&f, &id).await;
            let regulatory = &document["regulatory"];
            assert_eq!(regulatory["rule46sDeclaration"], rule46s);
            assert!(regulatory["einvoiceApplicability"].is_null());
            assert_eq!(regulatory["hsnRequiredDigits"], 6);
            assert_eq!(regulatory["dynamicQrApplicability"], dynamic_qr);
        }
    }

    /// A Sale posted before this phase reports its applicability as unknown (version 0), and its
    /// tender still carries the time it was actually recorded.
    #[tokio::test]
    async fn a4_a_pre_a4_sale_reports_unknown_without_inventing_anything() {
        let f = fixture().await;
        let id = b2c_draft(&f).await;
        let (status, body) = post_paid_by(&f, &id, "card", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        sqlx::query("DROP TRIGGER sale_documents_posted_no_update")
            .execute(&f.pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE sale_documents SET dynamic_qr_snapshot_version=0,\
             dynamic_qr_applicability_snapshot=NULL WHERE id=?",
        )
        .bind(&id)
        .execute(&f.pool)
        .await
        .unwrap();
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(document["regulatory"]["dynamicQrSnapshotVersion"], 0);
        assert!(document["regulatory"]["dynamicQrApplicability"].is_null());
        assert!(document["tender"][0]["referenceText"].is_null());
        let stored: String =
            sqlx::query_scalar("SELECT created_at_utc FROM sale_tenders WHERE sale_document_id=?")
                .bind(&id)
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(document["tender"][0]["recordedAtUtc"], stored.as_str());
    }

    /// The seller, recipient and A3 compliance snapshots are all intact beside the new facts.
    #[tokio::test]
    async fn a4_every_earlier_snapshot_is_intact_beside_the_new_facts() {
        let f = fixture().await;
        set_dynamic_qr(&f, "required").await;
        let id = open_sale(
            &f,
            json!({ "customerNameText": "Asha Patil", "recipientParticularsRequested": true,
                    "recipientAddress": counter_address() }),
        )
        .await;
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let (status, body) = post_paid_by(&f, &id, "upi", Some("UPI-9")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, document) = invoice(&f, &id).await;
        assert_eq!(document["regulatory"]["sellerSnapshotVersion"], 1);
        assert_eq!(document["regulatory"]["complianceSnapshotVersion"], 1);
        assert_eq!(document["recipient"]["snapshotVersion"], 1);
        assert_eq!(document["recipient"]["address"]["line1"], COUNTER_ADDRESS);
        assert_eq!(
            document["sellerSnapshot"]["legalName"],
            "Care Pharmacy Private Limited"
        );
        assert_eq!(document["regulatory"]["dynamicQrApplicability"], "required");
        assert_eq!(document["tender"][0]["referenceText"], "UPI-9");
    }

    #[tokio::test]
    async fn a4_the_dynamic_qr_answer_is_the_owners_to_record_validated_and_audited() {
        let f = fixture().await;
        let (_, profile) =
            request(f.pool.clone(), "GET", "/api/v1/store/profile", Value::Null).await;
        assert_eq!(profile["dynamicQrApplicability"], "not_required");
        let body = |dynamic_qr: &str, revision: i64| {
            json!({
                "expectedRevision": revision, "rule46sDeclarationApplicability": "not_applicable",
                "einvoiceApplicability": "not_required", "dynamicQrApplicability": dynamic_qr,
                "hsnTurnoverBand": "up_to_5_crore", "hsnTurnoverFinancialYear": "2026-27"
            })
        };
        let revision = profile["revision"].as_i64().unwrap();
        for invalid in ["applicable", "maybe", ""] {
            let (status, refused) = request(
                f.pool.clone(),
                "PUT",
                "/api/v1/store/invoice-compliance",
                body(invalid, revision),
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
            assert_eq!(issue_fields_of(&refused), vec!["dynamicQrApplicability"]);
        }
        let (status, _) = request_as(
            f.pool.clone(),
            "PUT",
            "/api/v1/store/invoice-compliance",
            body("required", revision),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, saved) = request(
            f.pool.clone(),
            "PUT",
            "/api/v1/store/invoice-compliance",
            body("required", revision),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(saved["dynamicQrApplicability"], "required");
        // Recorded as given; neither of the other two answers moved.
        assert_eq!(saved["rule46sDeclarationApplicability"], "not_applicable");
        assert_eq!(saved["einvoiceApplicability"], "not_required");
        let audited: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM master_change_events WHERE entity_type='store_tax_identity' \
             AND change_payload LIKE '%\"dynamicQrApplicability\":\"required\"%'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(audited, 1);
    }

    /// Posting races the owner withdrawing the answer. Either the Sale posted under the answer it
    /// read, or it was refused for the unknown answer — never a torn snapshot.
    #[tokio::test]
    async fn a4_a_sale_racing_the_dynamic_qr_answer_freezes_one_coherent_fact() {
        for attempt in 0..12 {
            let f = fixture().await;
            set_dynamic_qr(&f, "required").await;
            let id = b2c_draft(&f).await;
            let revision = revision_of(&f, &id).await;
            let pool = f.pool.clone();
            let editor = tokio::spawn(async move {
                sqlx::query("UPDATE store_identity SET dynamic_qr_applicability='unknown'")
                    .execute(&pool)
                    .await
                    .unwrap();
            });
            let key = Uuid::now_v7().to_string();
            let (posted, edited) =
                tokio::join!(post_sale_request(&f, &id, revision, &key, 8960), editor);
            edited.unwrap();
            match posted.0 {
                StatusCode::OK => assert_eq!(
                    dynamic_qr_snapshot(&f, &id).await,
                    (1, Some("required".to_owned())),
                    "attempt {attempt}"
                ),
                StatusCode::CONFLICT => {
                    assert_eq!(
                        issue_fields_of(&posted.1),
                        vec!["store.dynamicQrApplicability"],
                        "attempt {attempt}"
                    );
                    assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
                }
                other => panic!("attempt {attempt}: {other} {}", posted.1),
            }
        }
    }

    // -------------------------------------------------------------------------------------
    // Phase 1M-A — the Drugs Rules gate
    //
    // The attack these proofs defend against is a scheduled drug leaving the counter on an
    // ordinary bill: because nobody classified it, because somebody classified it and the
    // software sold it anyway, because a name looked harmless, or because the caller skipped the
    // browser entirely. Rule 65(9)(a) does not care which.
    // -------------------------------------------------------------------------------------

    /// Records one finding, exactly as an owner would through the API.
    async fn classify_scheme(
        f: &Fixture,
        scheme: &str,
        applies: bool,
        from: &str,
        to: Option<&str>,
    ) -> (StatusCode, Value) {
        request(
            f.pool.clone(),
            "POST",
            &format!(
                "/api/v1/products/{}/regulatory/classifications",
                f.product_id
            ),
            json!({
                "scheme": scheme,
                "applies": applies,
                "effectiveFrom": from,
                "effectiveTo": to,
                "sourceCitation": "G.S.R. 588(E) dated 30 August 2013",
            }),
        )
        .await
    }

    /// Ends the open finding for one scheme on the given date, as an owner records a change in law.
    async fn close_scheme(f: &Fixture, scheme: &str, effective_to: &str) {
        let (id, revision): (String, i64) = sqlx::query_as(
            "SELECT id,revision FROM product_regulatory_classifications \
             WHERE product_id=? AND scheme=? AND status='active' AND effective_to IS NULL",
        )
        .bind(&f.product_id)
        .bind(scheme)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        let (status, body) = request(
            f.pool.clone(),
            "POST",
            &format!(
                "/api/v1/products/{}/regulatory/classifications/{id}/close",
                f.product_id
            ),
            json!({
                "expectedRevision": revision,
                "effectiveTo": effective_to,
                "reason": "Schedule amended with effect from this date",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["effectiveTo"], effective_to);
    }

    /// Puts the product outside every gating schedule from the given date, so a sale may proceed.
    async fn clear_every_schedule(f: &Fixture, from: &str) {
        for scheme in crate::domain::regulatory::SALE_GATING_SCHEMES {
            let (status, body) = classify_scheme(f, scheme, false, from, None).await;
            assert_eq!(status, StatusCode::CREATED, "{scheme}: {body}");
        }
    }

    async fn line_snapshot(f: &Fixture, sale_id: &str) -> (i64, Option<String>, Option<String>) {
        sqlx::query_as(
            "SELECT regulatory_snapshot_version,regulatory_schemes_snapshot,manufacturer_name \
             FROM sale_lines WHERE sale_document_id=?",
        )
        .bind(sale_id)
        .fetch_one(&f.pool)
        .await
        .unwrap()
    }

    // --- 1-5. Nothing about a product's identity classifies it --------------------------------

    /// 1. A medicine nobody has classified cannot be sold. Not knowing is not a licence to guess.
    #[tokio::test]
    async fn an_unclassified_medicine_cannot_be_posted() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, body) = post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "regulatory_classification_unresolved");
    }

    /// 2-5. A name that says "H1", an HSN code, a dosage form and a manufacturer are all
    /// irrelevant: none of them is a finding, and the sale is refused exactly as before.
    #[tokio::test]
    async fn neither_name_nor_hsn_nor_dosage_form_nor_manufacturer_classifies_a_medicine() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        sqlx::query(
            "UPDATE products SET display_name='Schedule H1 Alprazolam Tramadol',\
             normalized_search_name='schedule h1 alprazolam tramadol' WHERE id=?",
        )
        .bind(&f.product_id)
        .execute(&f.pool)
        .await
        .unwrap();
        // An HSN code and a manufacturer, both recorded, neither of them a schedule finding.
        assign_hsn(&f.pool, &f.product_id, "30049099").await;
        let company = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO pharmaceutical_companies (id,display_name,normalized_search_name,\
             created_at_utc,updated_at_utc) VALUES (?,'Abbott India','abbott india',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&company)
        .execute(&f.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO product_company_roles (id,product_id,company_id,role,created_at_utc,\
             updated_at_utc) VALUES (?,?,?,'manufacturer',strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&f.product_id)
        .bind(&company)
        .execute(&f.pool)
        .await
        .unwrap();

        let (status, body) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/products/{}/regulatory", f.product_id),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        for answer in body["resolved"].as_array().unwrap() {
            assert_eq!(answer["answer"], "unknown", "{answer} was inferred");
        }
        assert_eq!(body["saleGate"], "unresolved");

        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::CONFLICT, "{posted}");
        assert_eq!(posted["code"], "regulatory_classification_unresolved");
    }

    // --- 6-8. Effective dating --------------------------------------------------------------

    /// 6-7. A finding that starts after this Sale's business date does not reach it; the same
    /// finding reaches a Sale dated on or after the day it commences. This is the Pregabalin and
    /// high-alcohol case: enacted, not yet commenced, and a Sale today is judged by today's law.
    #[tokio::test]
    async fn a_future_finding_governs_only_sales_dated_on_or_after_its_commencement() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        clear_every_schedule(&f, "2020-01-01").await;
        // The future finding closes the "outside Schedule H1" period and opens an "inside" one,
        // through the API an owner would use. The date is this test's, not a claim about any
        // notification's commencement.
        close_scheme(&f, "schedule_h1", "2027-01-09").await;
        let (status, body) = classify_scheme(&f, "schedule_h1", true, "2027-01-09", None).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");

        // TODAY is 2026-06-15: before commencement, so the sale proceeds.
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (version, snapshot, _) = line_snapshot(&f, &id).await;
        assert_eq!(version, 1);
        assert!(
            snapshot
                .as_deref()
                .unwrap()
                .contains("\"schedule_h1\":\"does_not_apply\""),
            "{snapshot:?}"
        );

        // A Sale dated on the day it commences sees the new finding and is refused.
        let (status, later) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/sales",
            json!({ "customerPartyId": Value::Null, "businessDate": "2027-01-09" }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{later}");
        let later_id = later["id"].as_str().unwrap().to_owned();
        let (status, withline) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{later_id}/lines"),
            line_body(1, &f, "pack", 1, 8000),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{withline}");
        let (status, refused) =
            post_sale_request(&f, &later_id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        // Since Phase 1M-C an H1 line whose NDPS purview nobody recorded (as here) is refused as an
        // unsupported NDPS-intersection workflow, with its own typed code.
        assert_eq!(refused["code"], "schedule_h1_ndps_workflow_not_available");
    }

    /// 8. A finding whose period has closed no longer governs, and the product falls back to
    /// unknown rather than to its old answer.
    #[tokio::test]
    async fn an_expired_finding_stops_governing_and_leaves_unknown_behind() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        let (status, body) =
            classify_scheme(&f, "schedule_h", false, "2020-01-01", Some("2026-01-01")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let (status, resolved) = request(
            f.pool.clone(),
            "GET",
            &format!(
                "/api/v1/products/{}/regulatory?asOf=2026-06-15",
                f.product_id
            ),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{resolved}");
        let schedule_h = resolved["resolved"]
            .as_array()
            .unwrap()
            .iter()
            .find(|answer| answer["scheme"] == "schedule_h")
            .unwrap();
        assert_eq!(schedule_h["answer"], "unknown");

        // And on a date inside the closed period it still answers what it answered then.
        let (_, historical) = request(
            f.pool.clone(),
            "GET",
            &format!(
                "/api/v1/products/{}/regulatory?asOf=2025-06-15",
                f.product_id
            ),
            Value::Null,
        )
        .await;
        let then = historical["resolved"]
            .as_array()
            .unwrap()
            .iter()
            .find(|answer| answer["scheme"] == "schedule_h")
            .unwrap();
        assert_eq!(then["answer"], "does_not_apply");
    }

    /// 9. Two findings cannot both govern the same product, scheme and day.
    #[tokio::test]
    async fn contradictory_overlapping_findings_are_refused() {
        let f = fixture().await;
        let (status, first) = classify_scheme(&f, "schedule_h", true, "2026-01-01", None).await;
        assert_eq!(status, StatusCode::CREATED, "{first}");
        let (status, second) = classify_scheme(&f, "schedule_h", false, "2026-06-01", None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{second}");
        assert_eq!(second["code"], "regulatory_period_overlaps");
        // A period that ends before the other begins is accepted, because it answers a different day.
        let (status, adjacent) =
            classify_scheme(&f, "schedule_h", false, "2025-01-01", Some("2026-01-01")).await;
        assert_eq!(status, StatusCode::CREATED, "{adjacent}");
    }

    // --- 10-12. Professionals are not application roles ---------------------------------------

    /// 10-11. An app role named "pharmacist" creates no professional record, and a professional
    /// record needs no login to be real.
    #[tokio::test]
    async fn an_application_role_is_not_a_professional_registration() {
        let f = fixture().await;
        insert_session(&f.pool, "pharmacist", "app-role-pharmacist-token").await;
        let (status, listed) = request(
            f.pool.clone(),
            "GET",
            "/api/v1/store/professionals",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{listed}");
        assert_eq!(
            listed.as_array().unwrap().len(),
            0,
            "an application role produced a professional record"
        );

        let (status, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/store/professionals",
            json!({
                "fullName": "Meera Iyer",
                "capacity": "registered_pharmacist",
                "registrationNumber": "MH-PH-44821",
                "registeringAuthority": "Maharashtra State Pharmacy Council",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["linkedUserId"], Value::Null);
        assert_eq!(created["registrationNumber"], "MH-PH-44821");
    }

    /// 12. A claim of registration without a registration number is refused.
    #[tokio::test]
    async fn a_registered_pharmacist_without_a_registration_number_is_refused() {
        let f = fixture().await;
        let (status, body) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/store/professionals",
            json!({ "fullName": "Anon", "capacity": "registered_pharmacist" }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["issues"][0]["field"], "registrationNumber");
        // A competent person needs none, and is accepted.
        let (status, competent) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/store/professionals",
            json!({ "fullName": "Store Manager", "capacity": "competent_person" }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{competent}");
    }

    // --- 13-16. Licences and elections ---------------------------------------------------------

    /// 13. Free text that happens to read "20F" does not become a typed Form 20F assertion.
    #[tokio::test]
    async fn a_free_text_licence_never_becomes_a_typed_form() {
        let f = fixture().await;
        sqlx::query(
            "INSERT INTO store_licences (id,store_id,licence_type,licence_number,\
             normalized_licence_number,created_at_utc,updated_at_utc) \
             VALUES (?,?,'Form 20F','MH-20F-99','MH20F99',strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&f.store_id)
        .execute(&f.pool)
        .await
        .unwrap();
        let (status, body) = request(
            f.pool.clone(),
            "GET",
            "/api/v1/store/drug-compliance",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["complianceLicences"].as_array().unwrap().len(),
            0,
            "a display licence was read as a statutory assertion"
        );
    }

    /// 14-15. A cashier may not assert a licence form or record an election.
    #[tokio::test]
    async fn a_cashier_cannot_assert_a_licence_form_or_an_election() {
        let f = fixture().await;
        let (status, licence) = request_as(
            f.pool.clone(),
            "POST",
            "/api/v1/store/compliance-licences",
            json!({ "licenceForm": "form_20f", "licenceNumber": "MH-20F-1" }),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{licence}");
        let (status, election) = request_as(
            f.pool.clone(),
            "POST",
            "/api/v1/store/record-elections",
            json!({
                "election": "rule_65_3_prescription_supply",
                "method": "prescription_register",
                "effectiveFrom": "2026-01-01",
            }),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{election}");
    }

    /// 16. The two elections are separate: recording one leaves the other unrecorded, and each
    /// accepts only the alternatives its own sub-rule offers.
    #[tokio::test]
    async fn the_two_rule_65_elections_stay_separate() {
        let f = fixture().await;
        let (status, first) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/store/record-elections",
            json!({
                "election": "rule_65_3_prescription_supply",
                "method": "prescription_register",
                "effectiveFrom": "2026-01-01",
                "evidenceReference": "Election filed with Licensing Authority, 12 Jan 2026",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{first}");

        let (status, body) = request(
            f.pool.clone(),
            "GET",
            "/api/v1/store/drug-compliance",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let elections = body["recordElections"].as_array().unwrap();
        assert_eq!(elections.len(), 1, "the other election was invented");
        assert_eq!(elections[0]["election"], "rule_65_3_prescription_supply");

        // Rule 65(4)(1) calls its register a register, not a prescription register.
        let (status, wrong) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/store/record-elections",
            json!({
                "election": "rule_65_4_non_prescription_schedule_c",
                "method": "prescription_register",
                "effectiveFrom": "2026-01-01",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{wrong}");
        let (status, right) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/store/record-elections",
            json!({
                "election": "rule_65_4_non_prescription_schedule_c",
                "method": "register",
                "effectiveFrom": "2026-01-01",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{right}");
    }

    // --- 17-20. A posted Sale is history ------------------------------------------------------

    /// 17-18. Changing today's manufacturer or today's classification cannot rewrite what a
    /// posted Sale froze.
    #[tokio::test]
    async fn todays_masters_cannot_rewrite_a_posted_sales_regulatory_facts() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        clear_every_schedule(&f, "2020-01-01").await;
        let company = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO pharmaceutical_companies (id,display_name,normalized_search_name,\
             created_at_utc,updated_at_utc) VALUES (?,'Micro Labs','micro labs',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&company)
        .execute(&f.pool)
        .await
        .unwrap();
        let role_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO product_company_roles (id,product_id,company_id,role,created_at_utc,\
             updated_at_utc) VALUES (?,?,?,'manufacturer',strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&role_id)
        .bind(&f.product_id)
        .bind(&company)
        .execute(&f.pool)
        .await
        .unwrap();

        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (version, snapshot, manufacturer) = line_snapshot(&f, &id).await;
        assert_eq!(version, 1);
        assert_eq!(manufacturer.as_deref(), Some("Micro Labs"));
        let frozen = snapshot.clone().unwrap();

        // The pharmacy changes its mind about both, after the fact.
        sqlx::query("UPDATE pharmaceutical_companies SET display_name='Renamed Labs' WHERE id=?")
            .bind(&company)
            .execute(&f.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE product_company_roles SET status='archived',archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='changed' WHERE id=?")
            .bind(&role_id)
            .execute(&f.pool)
            .await
            .unwrap();
        // The law changes later: close the old finding where the new one begins, then record it.
        close_scheme(&f, "schedule_h", "2026-06-20").await;
        let (status, reclassified) =
            classify_scheme(&f, "schedule_h", true, "2026-06-20", None).await;
        assert_eq!(status, StatusCode::CREATED, "{reclassified}");

        let (version, after, manufacturer_after) = line_snapshot(&f, &id).await;
        assert_eq!(version, 1);
        assert_eq!(after.unwrap(), frozen, "the frozen answer moved");
        assert_eq!(
            manufacturer_after.as_deref(),
            Some("Micro Labs"),
            "the frozen manufacturer moved"
        );
    }

    /// 19. A version-0 line is history, and history cannot be made after the fact.
    ///
    /// Migration 0021's populated test proves every line posted before this phase stays at version
    /// 0 with no snapshot. This proves the other half: after it, no route creates one. Posting
    /// always writes version 1; a document flipped to posted by hand is refused while any line is
    /// at 0; and a version-0 line written into an already-posted document is refused too. So a
    /// version-0 line can only ever mean "posted before regulatory facts were recorded".
    #[tokio::test]
    async fn no_route_after_this_phase_can_create_a_version_zero_posted_line() {
        let f = fixture().await;
        // Posting through the service writes version 1, even for an unclassified general item.
        let posted_id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, posted) =
            post_sale_request(&f, &posted_id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(line_snapshot(&f, &posted_id).await.0, 1);

        // A draft whose line was never snapshotted cannot be flipped to posted by hand.
        let draft_id = draft_with_line(&f, "pack", 1, 8000).await;
        let flipped = sqlx::query(
            "UPDATE sale_documents SET status='posted',document_number='INV/2627/000077',\
             sequence_value=77,series_code='INV',financial_year='2026-27',\
             tax_treatment='intra_state',posted_by_user_id=created_by_user_id,\
             posted_at_utc=updated_at_utc,posting_idempotency_key=? WHERE id=?",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&draft_id)
        .execute(&f.pool)
        .await;
        assert!(
            flipped.err().is_some_and(|error| error
                .to_string()
                .contains("regulatory_gate_refuses_posting")),
            "a version-0 line was posted by hand"
        );

        // Nor can a version-0 line be written into a document that is already posted.
        let smuggled = sqlx::query(
            "INSERT INTO sale_lines (id,sale_document_id,line_number,product_id,product_pack_id,\
             batch_id,quantity_basis,quantity_packs,quantity_atoms,selling_rate_paise,\
             regulatory_state_snapshot,created_at_utc,updated_at_utc) \
             SELECT ?,?,2,product_id,product_pack_id,batch_id,'pack',1,10,0,\
             '{\"premises_state_code\":\"27\",\"punjab_restricted_supply\":\"not_applicable\"}',created_at_utc,\
             updated_at_utc FROM sale_lines WHERE sale_document_id=? LIMIT 1",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&posted_id)
        .bind(&posted_id)
        .execute(&f.pool)
        .await;
        assert!(
            smuggled.err().is_some_and(|error| error
                .to_string()
                .contains("regulatory_gate_refuses_posting")),
            "a version-0 line was smuggled into a posted document"
        );
    }

    /// 20. Direct SQL cannot mutate a posted line's snapshot.
    #[tokio::test]
    async fn direct_sql_cannot_mutate_a_posted_regulatory_snapshot() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        clear_every_schedule(&f, "2020-01-01").await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let forged = sqlx::query(
            "UPDATE sale_lines SET regulatory_schemes_snapshot='{\"schedule_h\":\"applies\"}' \
             WHERE sale_document_id=?",
        )
        .bind(&id)
        .execute(&f.pool)
        .await;
        assert!(
            forged
                .err()
                .is_some_and(|error| error.to_string().contains("sale_document_is_posted")),
            "a posted snapshot was rewritable by direct SQL"
        );
    }

    // --- 21-23. The gate cannot be walked around ----------------------------------------------

    /// 21. A direct API caller gets the same refusal the browser would: there is no header, no
    /// flag and no field that skips it.
    #[tokio::test]
    async fn a_direct_api_caller_cannot_bypass_the_unresolved_block() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, body) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/post"),
            json!({
                "expectedRevision": 2,
                "idempotencyKey": Uuid::now_v7().to_string(),
                "tenders": [{ "method": "cash", "amountPaise": 8960 }],
                "regulatoryOverride": true,
                "skipCompliance": true,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "regulatory_classification_unresolved");
    }

    /// 22. Each of Schedule H, H1 and X refuses on its own, naming itself. Since Phase 1M-B a
    /// Schedule H line without its prescription is refused for exactly that; since Phase 1M-C so is
    /// a Schedule H1 line established outside NDPS purview; X stays refused because its own
    /// workflow does not exist yet.
    #[tokio::test]
    async fn every_prescription_schedule_refuses_until_its_workflow_exists() {
        for (scheme, code) in [
            ("schedule_h", "prescription_requirements_incomplete"),
            ("schedule_h1", "prescription_requirements_incomplete"),
            ("schedule_x", "schedule_x_workflow_not_available"),
        ] {
            let f = fixture().await;
            make_medicine(&f, &f.product_id).await;
            clear_every_schedule(&f, "2020-01-01").await;
            let (status, body) =
                classify_scheme(&f, "ndps_purview", false, "2020-01-01", None).await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
            // Since Phase 1M-B a Schedule H supply also needs its rule 65(3) basis; with it in place
            // the refusal below is for what this test is about.
            record_basis(&f, &f.product_id, Some("prescription_register")).await;
            // Close the "outside" finding and replace it with an "inside" one.
            sqlx::query(
                "UPDATE product_regulatory_classifications SET status='archived',\
                 archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='corrected' \
                 WHERE product_id=? AND scheme=?",
            )
            .bind(&f.product_id)
            .bind(scheme)
            .execute(&f.pool)
            .await
            .unwrap();
            let (status, body) = classify_scheme(&f, scheme, true, "2020-01-01", None).await;
            assert_eq!(status, StatusCode::CREATED, "{scheme}: {body}");

            let id = draft_with_line(&f, "pack", 1, 8000).await;
            let (status, refused) =
                post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
            assert_eq!(status, StatusCode::CONFLICT, "{scheme}: {refused}");
            assert_eq!(refused["code"], code, "{scheme}: {refused}");
            assert!(
                refused["issues"][0]["field"]
                    .as_str()
                    .unwrap()
                    .starts_with("lines.1"),
                "{scheme}: {refused}"
            );
            assert_eq!(detail(&f, &id).await["status"], "draft");
        }
    }

    /// 23. Schedule C refuses too: its rule 65(4)(1) record is not implemented either, and a
    /// correct classification must not become a knowingly incomplete sale.
    #[tokio::test]
    async fn schedule_c_refuses_until_its_record_exists() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        clear_every_schedule(&f, "2020-01-01").await;
        sqlx::query(
            "UPDATE product_regulatory_classifications SET status='archived',\
             archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='corrected' \
             WHERE product_id=? AND scheme='schedule_c'",
        )
        .bind(&f.product_id)
        .execute(&f.pool)
        .await
        .unwrap();
        let (status, body) = classify_scheme(&f, "schedule_c", true, "2020-01-01", None).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, refused) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "regulated_sale_workflow_not_available");
    }

    // --- 24-26. Scope: the ordinary counter keeps working --------------------------------------

    /// 24-25. A general item sells with no classification at all, and a medicine established to be
    /// outside every schedule sells and freezes that answer.
    #[tokio::test]
    async fn unclassified_general_items_and_cleared_medicines_both_sell() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (version, snapshot, _) = line_snapshot(&f, &id).await;
        assert_eq!(version, 1);
        // Its answers are honestly "unknown" — the general item was never classified — and that is
        // recorded rather than dressed up as "does not apply".
        assert!(
            snapshot
                .as_deref()
                .unwrap()
                .contains("\"schedule_h\":\"unknown\""),
            "{snapshot:?}"
        );

        let g = fixture().await;
        make_medicine(&g, &g.product_id).await;
        clear_every_schedule(&g, "2020-01-01").await;
        let cleared = draft_with_line(&g, "pack", 1, 8000).await;
        let (status, body) =
            post_sale_request(&g, &cleared, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, snapshot, _) = line_snapshot(&g, &cleared).await;
        assert!(
            snapshot
                .as_deref()
                .unwrap()
                .contains("\"schedule_h\":\"does_not_apply\""),
            "{snapshot:?}"
        );
    }

    /// 26. A device is not assumed to be outside Schedule C: classified into it, it is refused
    /// like any other line, because Schedule C lists sterile disposable single-use devices.
    #[tokio::test]
    async fn a_device_classified_into_schedule_c_is_refused() {
        let f = fixture().await;
        sqlx::query("UPDATE products SET product_kind='device' WHERE id=?")
            .bind(&f.product_id)
            .execute(&f.pool)
            .await
            .unwrap();
        // Unclassified, it still sells: an unknown does not block a device.
        let first = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, sold) =
            post_sale_request(&f, &first, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::OK, "{sold}");

        let (status, body) = classify_scheme(&f, "schedule_c", true, "2020-01-01", None).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let second = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, refused) =
            post_sale_request(&f, &second, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "regulated_sale_workflow_not_available");
    }

    // --- 27-28. Racing ------------------------------------------------------------------------

    /// 27. A classification committed while a posting is in flight yields one coherent snapshot:
    /// either the Sale was judged by the old finding and froze it, or it was refused by the new
    /// one. Never half of each.
    #[tokio::test]
    async fn a_classification_change_racing_a_posting_yields_one_coherent_answer() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        clear_every_schedule(&f, "2020-01-01").await;
        // With the rule 65(3)(2) election on record, a Sale the new finding catches is refused for
        // the one thing it lacks — its prescription — whichever way the race falls.
        elect(&f, "prescription_register", "2020-01-01", None).await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;

        let pool = f.pool.clone();
        let product_id = f.product_id.clone();
        let racer = tokio::spawn(async move {
            sqlx::query(
                "UPDATE product_regulatory_classifications SET applies=1 \
                 WHERE product_id=? AND scheme='schedule_h' AND status='active'",
            )
            .bind(&product_id)
            .execute(&pool)
            .await
        });
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        let _ = racer.await.unwrap();

        let (version, snapshot, _) = line_snapshot(&f, &id).await;
        match status {
            StatusCode::OK => {
                assert_eq!(version, 1);
                let frozen = snapshot.unwrap();
                assert!(
                    frozen.contains("\"schedule_h\":\"does_not_apply\""),
                    "a sale posted against a Schedule H finding: {frozen}"
                );
                assert_eq!(posted["status"], "posted");
            }
            StatusCode::CONFLICT => {
                // Since Phase 1M-B a Schedule H line is refused for lacking its prescription.
                assert_eq!(posted["code"], "prescription_requirements_incomplete");
                assert_eq!(
                    (version, snapshot),
                    (0, None),
                    "a refused sale froze a snapshot"
                );
            }
            other => panic!("{other}: {posted}"),
        }
    }

    /// 28. The same for the manufacturer: the frozen name is one of the two real answers, never a
    /// mixture, and never NULL beside a recorded company id.
    #[tokio::test]
    async fn a_manufacturer_change_racing_a_posting_yields_one_coherent_answer() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        clear_every_schedule(&f, "2020-01-01").await;
        let company = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO pharmaceutical_companies (id,display_name,normalized_search_name,\
             created_at_utc,updated_at_utc) VALUES (?,'Cipla','cipla',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&company)
        .execute(&f.pool)
        .await
        .unwrap();
        let role_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO product_company_roles (id,product_id,company_id,role,created_at_utc,\
             updated_at_utc) VALUES (?,?,?,'manufacturer',strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&role_id)
        .bind(&f.product_id)
        .bind(&company)
        .execute(&f.pool)
        .await
        .unwrap();
        let id = draft_with_line(&f, "pack", 1, 8000).await;

        let pool = f.pool.clone();
        let racing_role = role_id.clone();
        let racer = tokio::spawn(async move {
            sqlx::query(
                "UPDATE product_company_roles SET status='archived',\
                 archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='changed' \
                 WHERE id=?",
            )
            .bind(&racing_role)
            .execute(&pool)
            .await
        });
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        let _ = racer.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{posted}");

        let row: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT manufacturer_company_id,manufacturer_name FROM sale_lines \
             WHERE sale_document_id=?",
        )
        .bind(&id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(
            row.0.is_none(),
            row.1.is_none(),
            "half a manufacturer was frozen: {row:?}"
        );
        if let Some(name) = row.1 {
            assert_eq!(name, "Cipla");
        }
    }

    // --- 29-33. What a finding must carry -----------------------------------------------------

    /// 29-30. A finding needs an authority behind it; a blank or throwaway citation is refused.
    #[tokio::test]
    async fn a_finding_without_a_real_source_is_refused() {
        let f = fixture().await;
        for citation in ["", "   ", "x"] {
            let (status, body) = request(
                f.pool.clone(),
                "POST",
                &format!(
                    "/api/v1/products/{}/regulatory/classifications",
                    f.product_id
                ),
                json!({
                    "scheme": "schedule_h",
                    "applies": true,
                    "effectiveFrom": "2026-01-01",
                    "sourceCitation": citation,
                }),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{citation:?} was accepted: {body}"
            );
            assert_eq!(body["issues"][0]["field"], "sourceCitation");
        }
    }

    /// 31. A period that ends before it starts is refused.
    #[tokio::test]
    async fn an_impossible_effective_period_is_refused() {
        let f = fixture().await;
        let (status, body) =
            classify_scheme(&f, "schedule_h", true, "2026-06-01", Some("2026-01-01")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["issues"][0]["field"], "effectiveTo");
        let (status, same_day) =
            classify_scheme(&f, "schedule_h", true, "2026-06-01", Some("2026-06-01")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{same_day}");
    }

    /// 32. A scheme this software does not know is refused rather than stored as free text.
    #[tokio::test]
    async fn an_unknown_scheme_is_refused() {
        let f = fixture().await;
        for scheme in ["schedule_z", "SCHEDULE_H ", "narcotics", ""] {
            let (status, body) = classify_scheme(&f, scheme, true, "2026-01-01", None).await;
            if scheme.trim().eq_ignore_ascii_case("schedule_h") {
                assert_eq!(status, StatusCode::CREATED, "{scheme:?}: {body}");
            } else {
                assert_eq!(
                    status,
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "{scheme:?}: {body}"
                );
            }
        }
    }

    /// 33. "Unknown" is never reported as a finding of non-application, and the two are
    /// distinguishable everywhere they appear.
    #[tokio::test]
    async fn unknown_never_masquerades_as_a_finding_that_it_does_not_apply() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        let (status, before) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/products/{}/regulatory", f.product_id),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{before}");
        assert!(
            before["resolved"]
                .as_array()
                .unwrap()
                .iter()
                .all(|answer| answer["answer"] == "unknown")
        );
        assert_eq!(before["classifications"].as_array().unwrap().len(), 0);
        assert_eq!(before["saleGate"], "unresolved");

        let (status, created) = classify_scheme(&f, "schedule_h", false, "2020-01-01", None).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let (_, after) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/products/{}/regulatory", f.product_id),
            Value::Null,
        )
        .await;
        let schedule_h = after["resolved"]
            .as_array()
            .unwrap()
            .iter()
            .find(|answer| answer["scheme"] == "schedule_h")
            .unwrap();
        assert_eq!(schedule_h["answer"], "does_not_apply");
        // Saying "outside Schedule H" cost a row with a source; the others remain unknown.
        assert_eq!(after["classifications"].as_array().unwrap().len(), 1);
        assert_eq!(after["saleGate"], "unresolved");
    }

    // --- 34-35. Resolution by date ------------------------------------------------------------

    /// 34-35. The resolver answers for the date asked, in the past and in the future, without
    /// consulting the clock.
    #[tokio::test]
    async fn resolution_follows_the_business_date_backwards_and_forwards() {
        let f = fixture().await;
        let (status, body) =
            classify_scheme(&f, "schedule_h", true, "2026-04-01", Some("2026-07-01")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        for (as_of, expected) in [
            ("2026-03-31", "unknown"),
            ("2026-04-01", "applies"),
            ("2026-06-30", "applies"),
            ("2026-07-01", "unknown"),
            ("2030-01-01", "unknown"),
        ] {
            let (status, resolved) = request(
                f.pool.clone(),
                "GET",
                &format!("/api/v1/products/{}/regulatory?asOf={as_of}", f.product_id),
                Value::Null,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{as_of}: {resolved}");
            let answer = resolved["resolved"]
                .as_array()
                .unwrap()
                .iter()
                .find(|answer| answer["scheme"] == "schedule_h")
                .unwrap();
            assert_eq!(answer["answer"], expected, "on {as_of}");
        }
    }

    // --- 36-39. A refusal costs nothing -------------------------------------------------------

    /// 36-39. A refused Sale allocates no number, moves no stock, writes no tender and stays a
    /// draft the counter can fix.
    #[tokio::test]
    async fn a_refused_sale_leaves_the_series_stock_tenders_and_status_untouched() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let before_numbers: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sale_documents WHERE document_number IS NOT NULL",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        let before_sequence: Option<i64> =
            sqlx::query_scalar("SELECT MAX(next_value) FROM document_number_series")
                .fetch_one(&f.pool)
                .await
                .unwrap();

        let (status, body) = post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");

        assert_eq!(
            balance(&f, &f.batch_id).await,
            100,
            "a refused sale moved stock"
        );
        let tenders: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sale_tenders WHERE sale_document_id=?")
                .bind(&id)
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(tenders, 0, "a refused sale wrote a tender");
        let after_numbers: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sale_documents WHERE document_number IS NOT NULL",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(
            before_numbers, after_numbers,
            "a refused sale took a number"
        );
        let after_sequence: Option<i64> =
            sqlx::query_scalar("SELECT MAX(next_value) FROM document_number_series")
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(before_sequence, after_sequence, "the series advanced");
        let status_now: String = sqlx::query_scalar("SELECT status FROM sale_documents WHERE id=?")
            .bind(&id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(status_now, "draft");
    }

    /// 40. Every authorised change to a classification is on the audit log, with its actor.
    #[tokio::test]
    async fn an_authorised_classification_change_is_audited() {
        let f = fixture().await;
        let (status, created) = classify_scheme(&f, "schedule_h1", true, "2026-01-01", None).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let classification_id = created["id"].as_str().unwrap().to_owned();
        let (status, archived) = request(
            f.pool.clone(),
            "POST",
            &format!(
                "/api/v1/products/{}/regulatory/classifications/{classification_id}/archive",
                f.product_id
            ),
            json!({ "expectedRevision": 1, "reason": "Superseded by a later notification" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{archived}");

        let events: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT action,entity_id,actor_id,reason FROM master_change_events \
             WHERE entity_type='product_regulatory_classification' ORDER BY occurred_at_utc",
        )
        .fetch_all(&f.pool)
        .await
        .unwrap();
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0].0, "created");
        assert_eq!(events[1].0, "archived");
        for event in &events {
            assert_eq!(event.1, classification_id);
            assert_eq!(event.2.as_deref(), Some(f.owner_id.as_str()));
        }
        assert_eq!(
            events[1].3.as_deref(),
            Some("Superseded by a later notification")
        );
    }

    /// 41. The quote says what posting will do, before any money is taken.
    #[tokio::test]
    async fn the_quote_reports_the_same_gate_posting_will_apply() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, quote) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/sales/{id}/quote"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{quote}");
        assert_eq!(quote["lines"][0]["regulatoryGate"], "unresolved");
        assert_eq!(quote["lines"][0]["regulatoryGateScheme"], Value::Null);

        clear_every_schedule(&f, "2020-01-01").await;
        sqlx::query(
            "UPDATE product_regulatory_classifications SET applies=1 \
             WHERE product_id=? AND scheme='schedule_x' AND status='active'",
        )
        .bind(&f.product_id)
        .execute(&f.pool)
        .await
        .unwrap();
        let (_, quote) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/sales/{id}/quote"),
            Value::Null,
        )
        .await;
        assert_eq!(quote["lines"][0]["regulatoryGate"], "workflow_unavailable");
        assert_eq!(quote["lines"][0]["regulatoryGateScheme"], "schedule_x");
    }

    /// 42. A criterion attribute is a label fact, not a classification: recording 40% alcohol
    /// classifies nothing and unblocks nothing.
    #[tokio::test]
    async fn recording_an_alcohol_percentage_classifies_nothing() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        let (status, body) = request(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/products/{}/regulatory/attributes", f.product_id),
            json!({ "alcoholPercentVvHundredths": 4000 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["alcoholPercentVvHundredths"], 4000);
        assert_eq!(body["saleGate"], "unresolved");
        for answer in body["resolved"].as_array().unwrap() {
            assert_eq!(
                answer["answer"], "unknown",
                "{answer} was derived from a label fact"
            );
        }
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, refused) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "regulatory_classification_unresolved");
    }

    /// 43. A cashier cannot classify a product, and the refusal changes nothing.
    #[tokio::test]
    async fn a_cashier_cannot_classify_a_product() {
        let f = fixture().await;
        let (status, body) = request_as(
            f.pool.clone(),
            "POST",
            &format!(
                "/api/v1/products/{}/regulatory/classifications",
                f.product_id
            ),
            json!({
                "scheme": "schedule_h",
                "applies": false,
                "effectiveFrom": "2020-01-01",
                "sourceCitation": "I say so",
            }),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM product_regulatory_classifications")
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(count, 0);
    }

    /// 44. A returned line still points at the original Sale line, whose frozen regulatory facts
    /// are unchanged by the return.
    #[tokio::test]
    async fn a_return_leaves_the_original_lines_regulatory_facts_alone() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        clear_every_schedule(&f, "2020-01-01").await;
        let id = draft_with_line(&f, "pack", 2, 8000).await;
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 17920).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let before = line_snapshot(&f, &id).await;

        let sale_line_id: String =
            sqlx::query_scalar("SELECT id FROM sale_lines WHERE sale_document_id=?")
                .bind(&id)
                .fetch_one(&f.pool)
                .await
                .unwrap();
        let (status, draft) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/returns",
            json!({ "returnKind": "sales_return", "originalDocumentId": id, "businessDate": TODAY }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let return_id = draft["id"].as_str().unwrap().to_owned();
        let (status, withline) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{return_id}/lines"),
            json!({
                "expectedRevision": 1,
                "originalLineId": sale_line_id,
                "quantity": 1,
                "disposition": "quarantined",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{withline}");

        assert_eq!(
            line_snapshot(&f, &id).await,
            before,
            "a return rewrote the original line's regulatory facts"
        );
    }

    /// 45. A change in law is recorded by closing the old finding, and the close leaves every date
    /// it covered answering exactly as before. A close can only shorten a finding, never extend it.
    #[tokio::test]
    async fn closing_a_finding_keeps_its_past_answers_and_cannot_extend_it() {
        let f = fixture().await;
        let (status, body) = classify_scheme(&f, "schedule_h", false, "2020-01-01", None).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        close_scheme(&f, "schedule_h", "2026-07-01").await;

        let answer_on = |as_of: &'static str| {
            let pool = f.pool.clone();
            let product = f.product_id.clone();
            async move {
                let (_, resolved) = request(
                    pool,
                    "GET",
                    &format!("/api/v1/products/{product}/regulatory?asOf={as_of}"),
                    Value::Null,
                )
                .await;
                resolved["resolved"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|answer| answer["scheme"] == "schedule_h")
                    .unwrap()["answer"]
                    .clone()
            }
        };
        assert_eq!(
            answer_on("2025-06-15").await,
            "does_not_apply",
            "the past changed"
        );
        assert_eq!(answer_on("2026-06-30").await, "does_not_apply");
        assert_eq!(answer_on("2026-07-01").await, "unknown");

        let (id, revision): (String, i64) = sqlx::query_as(
            "SELECT id,revision FROM product_regulatory_classifications WHERE product_id=?",
        )
        .bind(&f.product_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        let (status, extended) = request(
            f.pool.clone(),
            "POST",
            &format!(
                "/api/v1/products/{}/regulatory/classifications/{id}/close",
                f.product_id
            ),
            json!({ "expectedRevision": revision, "effectiveTo": "2027-01-01", "reason": "extend" }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{extended}");
        assert_eq!(
            answer_on("2026-12-01").await,
            "unknown",
            "a close extended a finding"
        );
    }

    /// 46. A pack's volume is half of the Schedule H1 entry 52 test, so recording it is audited —
    /// and, like the alcohol percentage, it classifies nothing by itself.
    #[tokio::test]
    async fn recording_a_pack_volume_is_audited_and_classifies_nothing() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        let (status, body) = request(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/packs/{}/regulatory/attributes", f.pack_id),
            json!({ "netVolumeMillilitresHundredths": 10_000 }),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        let events: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT entity_type,action,actor_id FROM master_change_events \
             WHERE entity_type='product_pack_regulatory_attributes' AND entity_id=?",
        )
        .bind(&f.pack_id)
        .fetch_all(&f.pool)
        .await
        .unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].1, "created");
        assert_eq!(events[0].2.as_deref(), Some(f.owner_id.as_str()));

        let (status, cashier) = request_as(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/packs/{}/regulatory/attributes", f.pack_id),
            json!({ "expectedRevision": 1, "netVolumeMillilitresHundredths": 1 }),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{cashier}");

        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, refused) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "regulatory_classification_unresolved");
    }

    /// 47. Backdating cannot carry a drug scheduled since past the gate.
    ///
    /// A Schedule H1 finding commenced ten days ago. A draft dated twenty days ago resolves, on its
    /// own business date, to "does not apply" — and the frozen snapshot must still say so, because
    /// that is the law of the day the document records. But the goods are being handed over today,
    /// when the drug IS Schedule H1, so the sale is refused. A genuine late entry of an unrestricted
    /// medicine, dated the same day, still posts.
    #[tokio::test]
    async fn a_backdated_sale_cannot_carry_a_drug_scheduled_since() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        let (commenced, backdated): (String, String) = sqlx::query_as(
            "SELECT strftime('%Y-%m-%d','now','+5 hours','+30 minutes','-10 days'),\
             strftime('%Y-%m-%d','now','+5 hours','+30 minutes','-20 days')",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        clear_every_schedule(&f, "2020-01-01").await;
        // NDPS established not to apply, so what is judged below is the H1 date question alone.
        let (status, body) = classify_scheme(&f, "ndps_purview", false, "2020-01-01", None).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        record_basis(&f, &f.product_id, Some("prescription_register")).await;
        close_scheme(&f, "schedule_h1", &commenced).await;
        let (status, body) = classify_scheme(&f, "schedule_h1", true, &commenced, None).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");

        let (status, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/sales",
            json!({ "customerPartyId": Value::Null, "businessDate": backdated }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let id = created["id"].as_str().unwrap().to_owned();
        let (status, withline) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            line_body(1, &f, "pack", 1, 8000),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{withline}");

        // The quote tells the counter before any money is taken.
        let (_, quote) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/sales/{id}/quote"),
            Value::Null,
        )
        .await;
        // Since Phase 1M-C the stricter of the two days makes it an H1 line needing its
        // prescription — and its working entry could not truthfully record the business date.
        assert_eq!(quote["lines"][0]["regulatoryGate"], "prescription_required");
        assert_eq!(quote["lines"][0]["regulatoryGateScheme"], "schedule_h1");

        let (status, refused) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "prescription_requirements_incomplete");
        assert!(
            prescription_issue_codes(&refused)
                .contains(&"lines.1.schedule_h1_commenced_after_business_date".to_owned()),
            "{refused}"
        );
        assert_eq!(
            balance(&f, &f.batch_id).await,
            100,
            "a backdated sale moved stock"
        );
        assert_eq!(detail(&f, &id).await["status"], "draft");

        // A late entry of a medicine unrestricted on both days still posts, dated the same day,
        // and freezes the business-date answer.
        let g = fixture().await;
        make_medicine(&g, &g.product_id).await;
        clear_every_schedule(&g, "2020-01-01").await;
        let (_, late) = request(
            g.pool.clone(),
            "POST",
            "/api/v1/sales",
            json!({ "customerPartyId": Value::Null, "businessDate": backdated }),
        )
        .await;
        let late_id = late["id"].as_str().unwrap().to_owned();
        let (status, _) = request(
            g.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{late_id}/lines"),
            line_body(1, &g, "pack", 1, 8000),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, posted) =
            post_sale_request(&g, &late_id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (version, snapshot, _) = line_snapshot(&g, &late_id).await;
        assert_eq!(version, 1);
        assert!(
            snapshot
                .as_deref()
                .unwrap()
                .contains("\"schedule_h1\":\"does_not_apply\""),
            "{snapshot:?}"
        );
    }

    // -----------------------------------------------------------------------------------------
    // Phase 1M-B — prescription capture, dispensing and the Schedule H retail workflow
    //
    // Rule 65(9)(a) of the Drugs Rules, 1945: a Schedule H drug is sold by retail only on and in
    // accordance with a prescription. These prove that the one lawful path posts, that every
    // requirement missing from it refuses before anything is written, that H1, X and C stay
    // refused whatever paperwork is present, and that no caller — browser, direct API or direct
    // SQL — can get a Schedule H line past the counter another way.
    // -----------------------------------------------------------------------------------------

    const PHARMACIST_TOKEN: &str = "sale-pharmacist-session-token";
    const PATIENT_NAME: &str = "Sita Kulkarni";
    const PATIENT_ADDRESS: &str = "14 Lakshmi Road, Pune 411004";

    /// Records a finding for every gating scheme: `inside` applies, the rest do not.
    async fn schedule_product(f: &Fixture, product_id: &str, inside: &[&str]) {
        make_medicine(f, product_id).await;
        for scheme in crate::domain::regulatory::SALE_GATING_SCHEMES {
            sqlx::query(
                "INSERT INTO product_regulatory_classifications (id,product_id,scheme,applies,\
                 effective_from,source_citation,determined_by_user_id,revision,status,\
                 created_at_utc,updated_at_utc) VALUES (?,?,?,?,'2020-01-01',\
                 'Drugs Rules, 1945, Schedules as amended',?,1,'active',\
                 strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(product_id)
            .bind(scheme)
            .bind(i64::from(inside.contains(&scheme)))
            .bind(&f.owner_id)
            .execute(&f.pool)
            .await
            .unwrap();
        }
    }

    /// A second sellable product on its own lot, taxed like the first.
    async fn second_product(f: &Fixture, name: &str) -> (String, String, String) {
        let (product, pack) = insert_product(&f.pool, name, TABLET, 0, 10).await;
        enable_sale(&f.pool, &f.store_id, &product, &pack, 1, 0).await;
        classify(&f.pool, &product, &f.category_id).await;
        assign_hsn(&f.pool, &product, "30049099").await;
        let batch = insert_batch(&f.pool, &pack, "B-2", Some("2027-12-31"), Some(9550)).await;
        add_stock(
            &f.pool,
            &f.store_id,
            &product,
            &pack,
            &batch,
            100,
            &f.owner_id,
        )
        .await;
        (product, pack, batch)
    }

    async fn add_professional(f: &Fixture, body: Value) -> String {
        let (status, created) =
            request(f.pool.clone(), "POST", "/api/v1/store/professionals", body).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        created["id"].as_str().unwrap().to_owned()
    }

    /// A registered pharmacist on record, valid throughout.
    async fn pharmacist(f: &Fixture) -> String {
        add_professional(
            f,
            json!({
                "fullName": "Meera Iyer",
                "capacity": "registered_pharmacist",
                "registrationNumber": "MH-PH-44821",
                "registeringAuthority": "Maharashtra State Pharmacy Council",
                "validFrom": "2020-01-01",
            }),
        )
        .await
    }

    fn prescription_body(product_id: &str, atoms: i64, repeat: Value) -> Value {
        let mut body = json!({
            "prescribedOn": "2026-06-10",
            "prescriberName": "Dr. Anjali Rao",
            "prescriberAddress": "Rao Clinic, FC Road, Pune 411005",
            "prescriberRegistrationNumber": "MMC-2011-0457",
            "subjectKind": "human",
            "subjectName": PATIENT_NAME,
            "subjectAddress": PATIENT_ADDRESS,
            "repeatAuthority": "once",
            "writtenSignedDatedAttested": true,
            "items": [{
                "productId": product_id,
                "writtenDescription": "Tab. Crocin 500",
                "prescribedQuantityAtoms": atoms,
                "doseText": "1 tablet twice daily after food",
            }],
        });
        if let Value::Object(fields) = repeat {
            for (key, value) in fields {
                body[key] = value;
            }
        }
        body
    }

    async fn enter_prescription(f: &Fixture, body: Value) -> Value {
        let (status, created) =
            request(f.pool.clone(), "POST", "/api/v1/prescriptions", body).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        created
    }

    /// A once-only prescription for `atoms` of the fixture's product. Returns (prescription, item).
    async fn simple_prescription(f: &Fixture, atoms: i64) -> (String, String) {
        let created =
            enter_prescription(f, prescription_body(&f.product_id, atoms, json!({}))).await;
        (
            created["id"].as_str().unwrap().to_owned(),
            created["items"][0]["id"].as_str().unwrap().to_owned(),
        )
    }

    async fn first_line_id(f: &Fixture, sale: &str) -> String {
        detail(f, sale).await["lines"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn link_line(
        f: &Fixture,
        line_id: &str,
        sale: &str,
        item: Option<&str>,
        token: &str,
    ) -> (StatusCode, Value) {
        let revision = revision_of(f, sale).await;
        request_as(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/sale-lines/{line_id}/prescription"),
            json!({ "expectedRevision": revision, "prescriptionItemId": item }),
            Some(token),
        )
        .await
    }

    async fn set_supply(
        f: &Fixture,
        sale: &str,
        professional: Option<&str>,
        confirmed: bool,
        token: &str,
    ) -> (StatusCode, Value) {
        let revision = revision_of(f, sale).await;
        request_as(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/sales/{sale}/supply"),
            json!({
                "expectedRevision": revision,
                "supervisingProfessionalId": professional,
                "prescriptionEndorsementConfirmed": confirmed,
            }),
            Some(token),
        )
        .await
    }

    /// A draft of `packs` strips of the fixture's product, linked to `item`, supervised by
    /// `professional`, with the endorsement confirmed — everything the counter must do.
    async fn prepared_sale(f: &Fixture, item: &str, professional: &str, packs: i64) -> String {
        prepared_sale_on(f, item, professional, packs, TODAY).await
    }

    async fn prepared_sale_on(
        f: &Fixture,
        item: &str,
        professional: &str,
        packs: i64,
        business_date: &str,
    ) -> String {
        let (status, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/sales",
            json!({ "customerPartyId": Value::Null, "businessDate": business_date }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let id = created["id"].as_str().unwrap().to_owned();
        let line = add_line(f, &id, &f.product_id, &f.pack_id, &f.batch_id, packs, 8000).await;
        let (status, body) = link_line(f, &line, &id, Some(item), OWNER).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = set_supply(f, &id, Some(professional), true, OWNER).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        id
    }

    async fn dispensing_count(f: &Fixture) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM prescription_dispensings")
            .fetch_one(&f.pool)
            .await
            .unwrap()
    }

    fn prescription_issue_codes(body: &Value) -> Vec<String> {
        issue_fields(body)
    }

    /// A Schedule H fixture: the product is in Schedule H alone, a pharmacist is on record.
    async fn schedule_h_fixture() -> (Fixture, String) {
        let f = fixture().await;
        schedule_product(&f, &f.product_id, &["schedule_h"]).await;
        record_basis(&f, &f.product_id, Some("prescription_register")).await;
        let professional = pharmacist(&f).await;
        (f, professional)
    }

    /// What a lawful rule 65(3)(1) entry needs beyond the prescription: the product's manufacturer
    /// (particular (f)) and, unless `None`, the Store's rule 65(3)(2) election, from 2020.
    async fn record_basis(f: &Fixture, product_id: &str, election: Option<&str>) {
        manufacturer_for(f, product_id).await;
        if let Some(method) = election {
            elect(f, method, "2020-01-01", None).await;
        }
    }

    async fn manufacturer_for(f: &Fixture, product_id: &str) {
        let company = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO pharmaceutical_companies (id,display_name,normalized_search_name,\
             created_at_utc,updated_at_utc) VALUES (?,'Alkem Laboratories','alkem laboratories',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&company)
        .execute(&f.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO product_company_roles (id,product_id,company_id,role,created_at_utc,\
             updated_at_utc) VALUES (?,?,?,'manufacturer',strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(product_id)
        .bind(&company)
        .execute(&f.pool)
        .await
        .unwrap();
    }

    /// Records a rule 65(3)(2) election exactly as an owner does, through the API.
    async fn elect(f: &Fixture, method: &str, from: &str, to: Option<&str>) {
        let (status, body) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/store/record-elections",
            json!({
                "election": "rule_65_3_prescription_supply",
                "method": method,
                "effectiveFrom": from,
                "effectiveTo": to,
                "evidenceReference": "Election letter to the Licensing Authority",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    /// Prepares a Schedule H Sale's rule 65(3)(1) entries and confirms them, as a pharmacist does
    /// once the registered pharmacist has signed each physical entry and its serial is on the
    /// prescription. A refused preparation is left for the posting to report.
    /// Posts for the quoted total without touching the entry, for a Sale already confirmed.
    async fn post_confirmed(f: &Fixture, id: &str) -> (StatusCode, Value) {
        let revision = revision_of(f, id).await;
        let total = quote_of(f, id).await["grandTotalPaise"].as_i64().unwrap();
        post_sale_request(f, id, revision, &Uuid::now_v7().to_string(), total).await
    }

    async fn prepare_and_confirm(f: &Fixture, id: &str) -> Vec<String> {
        let revision = revision_of(f, id).await;
        let (status, _) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/prescription-records"),
            json!({ "expectedRevision": revision }),
        )
        .await;
        if status != StatusCode::OK {
            return Vec::new();
        }
        let prepared: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM prescription_supply_records WHERE sale_document_id=? AND status='prepared'",
        )
        .bind(id)
        .fetch_all(&f.pool)
        .await
        .unwrap();
        for record in &prepared {
            let (status, body) = request(
                f.pool.clone(),
                "POST",
                &format!("/api/v1/prescription-supply-records/{record}/confirm"),
                json!({ "manualSignatureConfirmed": true, "serialWrittenOnPrescription": true }),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
        }
        // Phase 1M-C: the H1 working entries, once their hard copy is placed in the separate
        // register and authenticated by the registered pharmacist.
        let h1_prepared: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM prescription_h1_register_entries \
             WHERE sale_document_id=? AND status='prepared'",
        )
        .bind(id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        if h1_prepared > 0 {
            let (status, body) = request(
                f.pool.clone(),
                "POST",
                &format!("/api/v1/sales/{id}/h1-register/confirm"),
                json!({ "hardCopyPlacedInRegister": true, "pharmacistAuthenticatedHardCopy": true }),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
        }
        prepared
    }

    // --- The lawful path ---------------------------------------------------------------------

    /// B-1. A pure Schedule H sale with a complete prescription, a registered pharmacist and a
    /// confirmed endorsement posts, and freezes every particular of the supply.
    #[tokio::test]
    async fn b01_a_complete_schedule_h_sale_posts_and_records_its_dispensing() {
        let (f, professional) = schedule_h_fixture().await;
        let (prescription, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;

        let quote = quote_of(&f, &id).await;
        assert_eq!(quote["lines"][0]["regulatoryGate"], "prescription_required");
        assert_eq!(quote["lines"][0]["prescription"]["required"], true);
        assert_eq!(quote["lines"][0]["prescription"]["issue"], Value::Null);
        assert_eq!(quote["lines"][0]["prescription"]["remainingAtoms"], 20);
        assert_eq!(quote["supply"]["supervisionRequired"], true);
        // Everything but the rule 65(3) entry is in place; the entry is the last step before posting.
        assert_eq!(
            quote["supply"]["issues"],
            json!(["prescription_record_not_prepared"])
        );

        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["status"], "posted");
        assert!(posted["documentNumber"].is_string());
        assert_eq!(balance(&f, &f.batch_id).await, 90);

        let row: (
            String,
            String,
            i64,
            String,
            String,
            String,
            Option<String>,
            String,
            String,
        ) = sqlx::query_as(
            "SELECT prescription_id,prescription_item_id,quantity_atoms,dispensed_on,\
                 supervising_professional_name,supervising_registration_number,\
                 supervising_registering_authority,endorsement_confirmed_by_user_id,\
                 product_id FROM prescription_dispensings WHERE sale_document_id=?",
        )
        .bind(&id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(row.0, prescription);
        assert_eq!(row.1, item);
        assert_eq!(row.2, 10);
        assert_eq!(
            row.3, TODAY,
            "the dispensing is dated the Sale's business date"
        );
        assert_eq!(row.4, "Meera Iyer");
        assert_eq!(row.5, "MH-PH-44821");
        assert_eq!(row.6.as_deref(), Some("Maharashtra State Pharmacy Council"));
        assert_eq!(row.7, f.owner_id, "the confirming user is recorded");
        assert_eq!(row.8, f.product_id);
        let (version, snapshot, _) = line_snapshot(&f, &id).await;
        assert_eq!(version, 1);
        assert!(
            snapshot
                .as_deref()
                .unwrap()
                .contains("\"schedule_h\":\"applies\""),
            "{snapshot:?}"
        );
        let linked: Option<String> = sqlx::query_scalar(
            "SELECT prescription_item_id FROM sale_lines WHERE sale_document_id=?",
        )
        .bind(&id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(linked.as_deref(), Some(item.as_str()));
    }

    /// B-2. The same Sale with nothing but the line is refused for every missing requirement at
    /// once, and nothing is written: no number, no stock, no tender, no dispensing.
    #[tokio::test]
    async fn b02_a_schedule_h_sale_without_its_paperwork_is_refused_and_writes_nothing() {
        let (f, _) = schedule_h_fixture().await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "prescription_requirements_incomplete");
        let fields = prescription_issue_codes(&refused);
        assert!(
            fields.contains(&"lines.1.prescription_missing".to_owned()),
            "{fields:?}"
        );
        assert!(
            fields.contains(&"supply.supervising_pharmacist_required".to_owned()),
            "{fields:?}"
        );
        assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
        assert_eq!(numbers_issued(&f).await, 0);
        assert_eq!(dispensing_count(&f).await, 0);
        assert_eq!(tender_rows(&f, &id).await, 0);
    }

    /// B-3. A prescription without a supervising pharmacist is not a lawful supply (rule 65(2)).
    #[tokio::test]
    async fn b03_a_supply_without_a_supervising_pharmacist_is_refused() {
        let (f, _) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let line = first_line_id(&f, &id).await;
        let (status, body) = link_line(&f, &line, &id, Some(&item), OWNER).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.supervising_pharmacist_required".to_owned()]
        );
        assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
    }

    /// B-4. The endorsement of rule 65(11)(c) is written on paper by a person. Until someone
    /// confirms they wrote it, the supply is refused; the software never claims to have written it.
    #[tokio::test]
    async fn b04_an_unconfirmed_endorsement_is_refused() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let line = first_line_id(&f, &id).await;
        link_line(&f, &line, &id, Some(&item), OWNER).await;
        let (status, body) = set_supply(&f, &id, Some(&professional), false, OWNER).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["supply"]["prescriptionEndorsementConfirmed"], false);
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.endorsement_not_confirmed".to_owned()]
        );
        assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
    }

    /// B-5. Changing what a line is dispensed against withdraws the endorsement confirmation:
    /// the note was written for the prescription that was linked when it was confirmed.
    #[tokio::test]
    async fn b05_relinking_a_line_withdraws_the_endorsement_confirmation() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let (_, other) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let line = first_line_id(&f, &id).await;
        let (status, body) = link_line(&f, &line, &id, Some(&other), OWNER).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["supply"]["prescriptionEndorsementConfirmed"], false);
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.endorsement_not_confirmed".to_owned()]
        );
    }

    // --- Who may supervise -------------------------------------------------------------------

    /// B-6. A competent person is not a registered pharmacist, whatever they are called.
    #[tokio::test]
    async fn b06_a_competent_person_cannot_supervise_a_schedule_h_supply() {
        let (f, _) = schedule_h_fixture().await;
        let competent = add_professional(
            &f,
            json!({ "fullName": "Store Manager", "capacity": "competent_person" }),
        )
        .await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &competent, 1).await;
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.supervising_pharmacist_invalid".to_owned()]
        );
        assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
    }

    /// B-7. A pharmacist whose record ended before the Sale's date, or starts after it, or who is
    /// archived, does not supervise it.
    #[tokio::test]
    async fn b07_a_pharmacist_outside_their_validity_or_archived_cannot_supervise() {
        for (label, body) in [
            (
                "expired",
                json!({ "validFrom": "2020-01-01", "validUpto": "2026-06-14" }),
            ),
            (
                "ended before the posting day",
                json!({ "validFrom": "2026-06-01", "validUpto": "2026-07-01" }),
            ),
            ("starts later", json!({ "validFrom": "2026-06-16" })),
        ] {
            let (f, _) = schedule_h_fixture().await;
            let mut professional = json!({
                "fullName": "Ravi Menon",
                "capacity": "registered_pharmacist",
                "registrationNumber": "MH-PH-10001",
            });
            for (key, value) in body.as_object().unwrap() {
                professional[key] = value.clone();
            }
            let professional = add_professional(&f, professional).await;
            let (_, item) = simple_prescription(&f, 20).await;
            let id = prepared_sale(&f, &item, &professional, 1).await;
            let (status, refused) = post_as_quoted(&f, &id).await;
            assert_eq!(status, StatusCode::CONFLICT, "{label}: {refused}");
            assert_eq!(
                prescription_issue_codes(&refused),
                vec!["supply.supervising_pharmacist_invalid".to_owned()],
                "{label}"
            );
        }

        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        sqlx::query(
            "UPDATE store_professionals SET status='archived',\
             archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='left' WHERE id=?",
        )
        .bind(&professional)
        .execute(&f.pool)
        .await
        .unwrap();
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.supervising_pharmacist_invalid".to_owned()]
        );
    }

    /// B-8. The application role `pharmacist` is a permission, not a registration: a user id is
    /// not a professional record, and naming one is refused.
    #[tokio::test]
    async fn b08_a_user_with_the_pharmacist_role_is_not_a_supervising_pharmacist() {
        let (f, _) = schedule_h_fixture().await;
        let user = insert_session(&f.pool, "pharmacist", PHARMACIST_TOKEN).await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let line = first_line_id(&f, &id).await;
        link_line(&f, &line, &id, Some(&item), PHARMACIST_TOKEN).await;
        let (status, refused) = set_supply(&f, &id, Some(&user), true, PHARMACIST_TOKEN).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.supervising_pharmacist_invalid".to_owned()]
        );
        let (status, still) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{still}");
        assert_eq!(dispensing_count(&f).await, 0);
    }

    /// B-9. A later change to the pharmacist's record never reaches a dispensing already made.
    #[tokio::test]
    async fn b09_the_supervising_pharmacist_is_frozen_onto_the_dispensing() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        sqlx::query(
            "UPDATE store_professionals SET full_name='Meera Iyer-Nair',registration_number='X-1' \
             WHERE id=?",
        )
        .bind(&professional)
        .execute(&f.pool)
        .await
        .unwrap();
        let (name, number): (String, String) = sqlx::query_as(
            "SELECT supervising_professional_name,supervising_registration_number \
             FROM prescription_dispensings WHERE sale_document_id=?",
        )
        .bind(&id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(
            (name.as_str(), number.as_str()),
            ("Meera Iyer", "MH-PH-44821")
        );
    }

    // --- In accordance with the prescription ------------------------------------------------

    /// B-10. Rule 65(11A): not another preparation, "whether containing the same substances or
    /// not". A prescription for one product cannot be linked to another, and a link forged by SQL
    /// is refused at posting.
    #[tokio::test]
    async fn b10_another_preparation_cannot_be_supplied_in_lieu() {
        let (f, professional) = schedule_h_fixture().await;
        let (generic, generic_pack, generic_batch) = second_product(&f, "Paracetamol 500").await;
        schedule_product(&f, &generic, &["schedule_h"]).await;
        manufacturer_for(&f, &generic).await;
        let (_, item) = simple_prescription(&f, 20).await;

        let id = open_sale(&f, draft_body(None)).await;
        let line = add_line(&f, &id, &generic, &generic_pack, &generic_batch, 1, 8000).await;
        let (status, refused) = link_line(&f, &line, &id, Some(&item), OWNER).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.prescription_substitution_not_permitted".to_owned()]
        );

        sqlx::query("UPDATE sale_lines SET prescription_item_id=? WHERE id=?")
            .bind(&item)
            .bind(&line)
            .execute(&f.pool)
            .await
            .unwrap();
        set_supply(&f, &id, Some(&professional), true, OWNER).await;
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["lines.1.prescription_substitution_not_permitted".to_owned()]
        );
        assert_nothing_posted(&f, &id, &generic_batch, 100).await;
    }

    /// B-11. Rule 65(10)(c): the total amount to be supplied. More than the prescription has left
    /// is refused.
    #[tokio::test]
    async fn b11_more_than_the_prescribed_quantity_is_refused() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 15).await;
        let id = prepared_sale(&f, &item, &professional, 2).await;
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["lines.1.prescription_quantity_exceeded".to_owned()]
        );
        assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
    }

    /// B-12. Two lines of one Sale cannot each take the same remaining quantity.
    #[tokio::test]
    async fn b12_two_lines_of_one_sale_share_one_prescribed_quantity() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 15).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let second = add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let (status, body) = link_line(&f, &second, &id, Some(&item), OWNER).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        set_supply(&f, &id, Some(&professional), true, OWNER).await;
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["lines.2.prescription_quantity_exceeded".to_owned()]
        );
    }

    /// B-13. Rule 65(11)(a): once, unless the prescriber stated otherwise. A partial first supply
    /// uses the one occasion; the balance cannot be collected later.
    #[tokio::test]
    async fn b13_a_once_only_prescription_cannot_be_dispensed_a_second_time() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 30).await;
        let first = prepared_sale(&f, &item, &professional, 1).await;
        let (status, posted) = post_as_quoted(&f, &first).await;
        assert_eq!(status, StatusCode::OK, "{posted}");

        let second = prepared_sale(&f, &item, &professional, 1).await;
        let quote = quote_of(&f, &second).await;
        assert_eq!(
            quote["lines"][0]["prescription"]["issue"],
            "prescription_repeat_not_authorised"
        );
        assert_eq!(quote["lines"][0]["prescription"]["remainingAtoms"], 20);
        let (status, refused) = post_as_quoted(&f, &second).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["lines.1.prescription_repeat_not_authorised".to_owned()]
        );
        assert_eq!(dispensing_count(&f).await, 1);
        assert_eq!(balance(&f, &f.batch_id).await, 90);
    }

    /// B-14. Rule 65(11)(b): a stated number of times, and not otherwise.
    #[tokio::test]
    async fn b14_a_stated_number_of_times_is_the_limit() {
        let (f, professional) = schedule_h_fixture().await;
        let created = enter_prescription(
            &f,
            prescription_body(
                &f.product_id,
                60,
                json!({ "repeatAuthority": "stated_times", "repeatTimes": 2 }),
            ),
        )
        .await;
        let item = created["items"][0]["id"].as_str().unwrap().to_owned();
        for _ in 0..2 {
            let id = prepared_sale(&f, &item, &professional, 1).await;
            let (status, posted) = post_as_quoted(&f, &id).await;
            assert_eq!(status, StatusCode::OK, "{posted}");
        }
        let third = prepared_sale(&f, &item, &professional, 1).await;
        let (status, refused) = post_as_quoted(&f, &third).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["lines.1.prescription_repeat_not_authorised".to_owned()]
        );
    }

    /// B-15. A repeat stated without a count is still bounded by the total quantity.
    #[tokio::test]
    async fn b15_a_repeat_without_a_count_is_bounded_by_the_total_quantity() {
        let (f, professional) = schedule_h_fixture().await;
        let created = enter_prescription(
            &f,
            prescription_body(
                &f.product_id,
                30,
                json!({ "repeatAuthority": "stated_without_count" }),
            ),
        )
        .await;
        let item = created["items"][0]["id"].as_str().unwrap().to_owned();
        for _ in 0..3 {
            let id = prepared_sale(&f, &item, &professional, 1).await;
            let (status, posted) = post_as_quoted(&f, &id).await;
            assert_eq!(status, StatusCode::OK, "{posted}");
        }
        let fourth = prepared_sale(&f, &item, &professional, 1).await;
        let (status, refused) = post_as_quoted(&f, &fourth).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["lines.1.prescription_quantity_exceeded".to_owned()]
        );
    }

    /// B-16. Rule 65(11)(b): at stated intervals, and not otherwise.
    #[tokio::test]
    async fn b16_a_stated_interval_must_pass_before_the_next_supply() {
        let (f, professional) = schedule_h_fixture().await;
        let created = enter_prescription(
            &f,
            prescription_body(
                &f.product_id,
                60,
                json!({
                    "repeatAuthority": "stated_times",
                    "repeatTimes": 3,
                    "repeatIntervalDays": 7,
                }),
            ),
        )
        .await;
        let item = created["items"][0]["id"].as_str().unwrap().to_owned();
        let first = prepared_sale_on(&f, &item, &professional, 1, "2026-06-11").await;
        let (status, posted) = post_as_quoted(&f, &first).await;
        assert_eq!(status, StatusCode::OK, "{posted}");

        let early = prepared_sale_on(&f, &item, &professional, 1, "2026-06-17").await;
        let (status, refused) = post_as_quoted(&f, &early).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["lines.1.prescription_repeat_too_soon".to_owned()]
        );

        let due = prepared_sale_on(&f, &item, &professional, 1, "2026-06-18").await;
        let (status, posted) = post_as_quoted(&f, &due).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
    }

    /// B-17. A prescription dated after the supply cannot authorise it.
    #[tokio::test]
    async fn b17_a_prescription_dated_after_the_supply_is_refused() {
        let (f, professional) = schedule_h_fixture().await;
        let mut body = prescription_body(&f.product_id, 20, json!({}));
        body["prescribedOn"] = json!("2026-06-20");
        let created = enter_prescription(&f, body).await;
        let item = created["items"][0]["id"].as_str().unwrap().to_owned();
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["lines.1.prescription_dated_after_supply".to_owned()]
        );
    }

    /// B-18. An archived prescription authorises nothing further: it cannot be linked, and a line
    /// linked before the archive is refused at posting.
    #[tokio::test]
    async fn b18_an_archived_prescription_cannot_be_dispensed() {
        let (f, professional) = schedule_h_fixture().await;
        let (prescription, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let (status, archived) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/prescriptions/{prescription}/archive"),
            json!({ "expectedRevision": 1, "reason": "entered against the wrong patient" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{archived}");
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["lines.1.prescription_archived".to_owned()]
        );

        let fresh = draft_with_line(&f, "pack", 1, 8000).await;
        let line = first_line_id(&f, &fresh).await;
        let (status, refused) = link_line(&f, &line, &fresh, Some(&item), OWNER).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.prescription_archived".to_owned()]
        );
    }

    /// B-19. An unknown or malformed prescription item is refused, not treated as a blank.
    #[tokio::test]
    async fn b19_an_unknown_prescription_item_is_refused() {
        let (f, _) = schedule_h_fixture().await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let line = first_line_id(&f, &id).await;
        let (status, refused) =
            link_line(&f, &line, &id, Some(&Uuid::now_v7().to_string()), OWNER).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.prescription_not_found".to_owned()]
        );
        let (status, refused) = link_line(&f, &line, &id, Some("RX-000001"), OWNER).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    }

    /// B-20. A prescription on a line that needs none is refused rather than silently turned into
    /// a dispensing nobody required.
    #[tokio::test]
    async fn b20_a_link_on_an_unscheduled_line_is_refused() {
        let f = fixture().await;
        make_lawful_medicine(&f, &f.product_id).await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let line = first_line_id(&f, &id).await;
        let (status, body) = link_line(&f, &line, &id, Some(&item), OWNER).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["lines.1.prescription_link_not_required".to_owned()]
        );
        // Removing the link lets the ordinary sale through, with no dispensing.
        link_line(&f, &line, &id, None, OWNER).await;
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(dispensing_count(&f).await, 0);
    }

    // --- Schedules still refused -------------------------------------------------------------

    /// B-21, revised in Phase 1M-C (C-04/C-05/C-64). A Schedule H1 line whose NDPS purview nobody has
    /// recorded is refused with its own typed error however complete the prescription is: an
    /// unsupported NDPS-intersection workflow, not a requirement of rule 65.
    #[tokio::test]
    async fn b21_schedule_h1_is_refused_even_with_a_complete_prescription() {
        let f = fixture().await;
        schedule_product(&f, &f.product_id, &["schedule_h", "schedule_h1"]).await;
        record_basis(&f, &f.product_id, Some("prescription_register")).await;
        let professional = pharmacist(&f).await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        // The quote names the NDPS-intersection refusal and reports the linked prescription without
        // calling it "a link nobody needs": an H1 drug does need one, and still cannot be sold here.
        let quote = quote_of(&f, &id).await;
        assert_eq!(quote["lines"][0]["regulatoryGate"], "workflow_unavailable");
        assert_eq!(quote["lines"][0]["regulatoryGateScheme"], "ndps_purview");
        assert_eq!(quote["lines"][0]["prescription"]["issue"], Value::Null);
        assert_eq!(
            quote["lines"][0]["prescription"]["prescriptionReference"],
            "RX-000001"
        );
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "schedule_h1_ndps_workflow_not_available");
        assert_eq!(
            refused["message"],
            "This Schedule H1 drug is, or may be, within the NDPS Act, and AUSHADHARTH does not support that combined workflow."
        );
        let text = refused.to_string().to_lowercase();
        assert!(
            text.contains("unsupported ndps-intersection workflow"),
            "{refused}"
        );
        assert!(
            !text.contains("prohibit") && !text.contains("banned"),
            "{refused}"
        );
        assert_eq!(issue_fields(&refused), vec!["lines.1".to_owned()]);
        assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
        assert_eq!(dispensing_count(&f).await, 0);
    }

    /// B-22. Schedule X is refused with its own typed error.
    #[tokio::test]
    async fn b22_schedule_x_is_refused_even_with_a_complete_prescription() {
        let f = fixture().await;
        schedule_product(&f, &f.product_id, &["schedule_x"]).await;
        record_basis(&f, &f.product_id, Some("prescription_register")).await;
        let professional = pharmacist(&f).await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "schedule_x_workflow_not_available");
        assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
    }

    /// B-23. A drug in Schedule H and Schedule C is held to both, and Schedule C's record does not
    /// exist yet, so it stays refused.
    #[tokio::test]
    async fn b23_schedule_h_with_schedule_c_stays_refused() {
        for scheme in ["schedule_c", "schedule_c1"] {
            let f = fixture().await;
            schedule_product(&f, &f.product_id, &["schedule_h", scheme]).await;
            record_basis(&f, &f.product_id, Some("prescription_register")).await;
            let professional = pharmacist(&f).await;
            let (_, item) = simple_prescription(&f, 20).await;
            let id = prepared_sale(&f, &item, &professional, 1).await;
            let (status, refused) = post_as_quoted(&f, &id).await;
            assert_eq!(status, StatusCode::CONFLICT, "{scheme}: {refused}");
            assert_eq!(
                refused["code"], "regulated_sale_workflow_not_available",
                "{scheme}"
            );
            assert_eq!(dispensing_count(&f).await, 0);
        }
    }

    /// B-24. A Schedule H drug whose Schedule C position is unknown is not a Schedule H sale: the
    /// unknown is resolved first.
    #[tokio::test]
    async fn b24_schedule_h_with_an_unknown_other_schedule_is_unresolved() {
        let f = fixture().await;
        schedule_product(&f, &f.product_id, &["schedule_h"]).await;
        sqlx::query(
            "DELETE FROM product_regulatory_classifications WHERE product_id=? AND scheme='schedule_c'",
        )
        .bind(&f.product_id)
        .execute(&f.pool)
        .await
        .unwrap();
        let professional = pharmacist(&f).await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "regulatory_classification_unresolved");
    }

    // --- Mixed baskets -----------------------------------------------------------------------

    /// B-25. Only the Schedule H line needs a prescription; the ordinary line beside it needs none.
    #[tokio::test]
    async fn b25_a_mixed_basket_needs_a_prescription_only_for_its_schedule_h_line() {
        let (f, professional) = schedule_h_fixture().await;
        let (ordinary, pack, batch) = second_product(&f, "Cotton roll").await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        add_line(&f, &id, &ordinary, &pack, &batch, 1, 8000).await;
        let quote = quote_of(&f, &id).await;
        assert_eq!(quote["lines"][1]["prescription"]["required"], false);
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(dispensing_count(&f).await, 1);
        assert_eq!(balance(&f, &batch).await, 90);
    }

    /// B-26. One Schedule H1 line blocks the whole basket, naming that line, however complete the
    /// Schedule H line beside it is.
    #[tokio::test]
    async fn b26_one_h1_line_blocks_a_complete_schedule_h_basket() {
        let (f, professional) = schedule_h_fixture().await;
        let (h1, pack, batch) = second_product(&f, "Alprax 0.25").await;
        schedule_product(&f, &h1, &["schedule_h", "schedule_h1"]).await;
        manufacturer_for(&f, &h1).await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        add_line(&f, &id, &h1, &pack, &batch, 1, 8000).await;
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        // NDPS purview unrecorded on the H1 line: an unsupported NDPS-intersection workflow.
        assert_eq!(refused["code"], "schedule_h1_ndps_workflow_not_available");
        assert_eq!(issue_fields(&refused), vec!["lines.2".to_owned()]);
        assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
        assert_eq!(balance(&f, &batch).await, 100);
        assert_eq!(dispensing_count(&f).await, 0);
    }

    /// B-27. An ordinary sale asks nothing about prescriptions, patients or pharmacists.
    #[tokio::test]
    async fn b27_an_ordinary_sale_is_untouched() {
        let f = fixture().await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let quote = quote_of(&f, &id).await;
        assert_eq!(quote["supply"]["supervisionRequired"], false);
        assert_eq!(quote["supply"]["issues"], json!([]));
        assert_eq!(quote["lines"][0]["prescription"]["required"], false);
        let (status, posted) =
            post_sale_request(&f, &id, 2, &Uuid::now_v7().to_string(), 8960).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["supply"]["supervisingProfessionalId"], Value::Null);
        assert_eq!(dispensing_count(&f).await, 0);
    }

    // --- Direct API adversaries --------------------------------------------------------------

    /// B-28. Invented override fields are ignored, not honoured.
    #[tokio::test]
    async fn b28_override_fields_cannot_bypass_the_prescription_gate() {
        let (f, _) = schedule_h_fixture().await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/post"),
            json!({
                "expectedRevision": 2,
                "idempotencyKey": Uuid::now_v7().to_string(),
                "tenders": [{ "method": "cash", "amountPaise": 8960 }],
                "prescriptionOverride": true,
                "prescriptionItemId": Uuid::now_v7().to_string(),
                "supervisingProfessionalId": Uuid::now_v7().to_string(),
                "prescriptionEndorsementConfirmed": true,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "prescription_requirements_incomplete");
        assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
    }

    /// B-29. A cashier cannot enter, list, read, or link prescriptions, nor name the supervising
    /// pharmacist. The till is not a route to patient records.
    #[tokio::test]
    async fn b29_a_cashier_cannot_touch_prescriptions() {
        let (f, professional) = schedule_h_fixture().await;
        let (prescription, item) = simple_prescription(&f, 20).await;
        for (method, uri, body) in [
            (
                "POST",
                "/api/v1/prescriptions".to_owned(),
                prescription_body(&f.product_id, 20, json!({})),
            ),
            ("GET", "/api/v1/prescriptions".to_owned(), Value::Null),
            (
                "GET",
                format!("/api/v1/prescriptions/{prescription}"),
                Value::Null,
            ),
            ("GET", "/api/v1/prescribers".to_owned(), Value::Null),
            (
                "POST",
                "/api/v1/prescribers".to_owned(),
                json!({ "fullName": "Dr X", "addressText": "Pune" }),
            ),
        ] {
            let (status, body) =
                request_as(f.pool.clone(), method, &uri, body, Some(CASHIER)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body}");
            assert!(!body.to_string().contains(PATIENT_NAME));
        }
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let line = first_line_id(&f, &id).await;
        let (status, _) = link_line(&f, &line, &id, Some(&item), CASHIER).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = set_supply(&f, &id, Some(&professional), true, CASHIER).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = request_as(
            f.pool.clone(),
            "GET",
            "/api/v1/prescriptions",
            Value::Null,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// B-30. Once the pharmacist has prepared the supply, a cashier may take the money, and the
    /// dispensing still records who confirmed the endorsement.
    #[tokio::test]
    async fn b30_a_cashier_may_post_a_supply_the_pharmacist_prepared() {
        let (f, professional) = schedule_h_fixture().await;
        let pharmacist_user = insert_session(&f.pool, "pharmacist", PHARMACIST_TOKEN).await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let line = first_line_id(&f, &id).await;
        let (status, body) = link_line(&f, &line, &id, Some(&item), PHARMACIST_TOKEN).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = set_supply(&f, &id, Some(&professional), true, PHARMACIST_TOKEN).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, posted) = post_as_quoted_by(&f, &id, CASHIER).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let confirmed_by: String = sqlx::query_scalar(
            "SELECT endorsement_confirmed_by_user_id FROM prescription_dispensings \
             WHERE sale_document_id=?",
        )
        .bind(&id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(confirmed_by, pharmacist_user);
    }

    /// B-31. A replayed posting returns the original Sale and records no second dispensing.
    #[tokio::test]
    async fn b31_a_replayed_posting_dispenses_once() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        prepare_and_confirm(&f, &id).await;
        let revision = revision_of(&f, &id).await;
        let key = Uuid::now_v7().to_string();
        let (status, first) = post_sale_request(&f, &id, revision, &key, 8960).await;
        assert_eq!(status, StatusCode::OK, "{first}");
        let (status, replay) = post_sale_request(&f, &id, revision, &key, 8960).await;
        assert_eq!(status, StatusCode::OK, "{replay}");
        assert_eq!(first["documentNumber"], replay["documentNumber"]);
        assert_eq!(dispensing_count(&f).await, 1);
    }

    /// B-32. Posted Sales keep their prescription link and supply facts: neither endpoint edits a
    /// posted Sale.
    #[tokio::test]
    async fn b32_a_posted_sales_prescription_facts_cannot_be_changed() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let (status, _) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK);
        let line = first_line_id(&f, &id).await;
        let (status, _) = link_line(&f, &line, &id, None, OWNER).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, _) = set_supply(&f, &id, None, false, OWNER).await;
        assert_eq!(status, StatusCode::CONFLICT);
        for statement in [
            "UPDATE sale_lines SET prescription_item_id=NULL WHERE sale_document_id=?",
            "UPDATE sale_documents SET supervising_professional_id=NULL WHERE id=?",
            "UPDATE sale_documents SET prescription_endorsement_confirmed=0 WHERE id=?",
        ] {
            let result = sqlx::query(statement).bind(&id).execute(&f.pool).await;
            assert!(result.is_err(), "{statement} changed a posted Sale");
        }
    }

    // --- Direct SQL adversaries --------------------------------------------------------------

    /// B-33. A dispensing is history: it cannot be edited or deleted. (Reversals: B-47.)
    #[tokio::test]
    async fn b33_dispensings_are_append_only() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let (status, _) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK);
        for statement in [
            "UPDATE prescription_dispensings SET quantity_atoms=1",
            "UPDATE prescription_dispensings SET supervising_professional_name='Someone else'",
            "UPDATE prescription_dispensings SET dispensed_on='2026-01-01'",
            "DELETE FROM prescription_dispensings",
        ] {
            let error = sqlx::query(statement)
                .execute(&f.pool)
                .await
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("prescription_dispensings_are_append_only"),
                "{statement}: {error}"
            );
        }
        assert_eq!(dispensing_count(&f).await, 1);
    }

    /// B-34. The database refuses a dispensing row written by hand: against a posted Sale, of
    /// another product, beyond the prescribed quantity, or on a second occasion of a once-only
    /// prescription.
    #[tokio::test]
    async fn b34_the_database_refuses_forged_dispensings() {
        let (f, professional) = schedule_h_fixture().await;
        let (prescription, item) = simple_prescription(&f, 20).await;
        let posted = prepared_sale(&f, &item, &professional, 1).await;
        let (status, _) = post_as_quoted(&f, &posted).await;
        assert_eq!(status, StatusCode::OK);
        let posted_line = first_line_id(&f, &posted).await;

        let forge = |prescription: String,
                     item: String,
                     sale: String,
                     line: String,
                     product: String,
                     atoms: i64| {
            let pool = f.pool.clone();
            let store = f.store_id.clone();
            let professional = professional.clone();
            let owner = f.owner_id.clone();
            async move {
                sqlx::query(
                    "INSERT INTO prescription_dispensings (id,store_id,prescription_id,                     prescription_item_id,sale_document_id,sale_line_id,product_id,quantity_atoms,                     dispensed_on,supervising_professional_id,supervising_professional_name,                     supervising_registration_number,                     endorsement_confirmed_by_user_id,endorsement_confirmed_at_utc,created_at_utc)                      VALUES (?,?,?,?,?,?,?,?,'2026-06-15',?,'Meera Iyer','MH-PH-44821',?,                     strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                )
                .bind(Uuid::now_v7().to_string())
                .bind(store)
                .bind(prescription)
                .bind(item)
                .bind(sale)
                .bind(line)
                .bind(product)
                .bind(atoms)
                .bind(professional)
                .bind(owner)
                .execute(&pool)
                .await
                .unwrap_err()
                .to_string()
            }
        };

        // Against a Sale already posted.
        let error = forge(
            prescription.clone(),
            item.clone(),
            posted.clone(),
            posted_line.clone(),
            f.product_id.clone(),
            10,
        )
        .await;
        assert!(
            error.contains("prescription_dispensing_incoherent"),
            "{error}"
        );

        // A draft line linked by hand to the once-only prescription: a second occasion, within
        // the quantity, so only the occasion is at fault.
        let draft = draft_with_line(&f, "pack", 1, 8000).await;
        let draft_line = first_line_id(&f, &draft).await;
        sqlx::query("UPDATE sale_lines SET prescription_item_id=? WHERE id=?")
            .bind(&item)
            .bind(&draft_line)
            .execute(&f.pool)
            .await
            .unwrap();
        let error = forge(
            prescription.clone(),
            item.clone(),
            draft.clone(),
            draft_line.clone(),
            f.product_id.clone(),
            10,
        )
        .await;
        assert!(
            error.contains("prescription_repeat_not_authorised"),
            "{error}"
        );

        // Against a repeatable prescription of 10 atoms: more than it holds, and another product.
        let draft = draft_with_line(&f, "pack", 2, 8000).await;
        let draft_line = first_line_id(&f, &draft).await;
        let repeatable = enter_prescription(
            &f,
            prescription_body(
                &f.product_id,
                10,
                json!({ "repeatAuthority": "stated_without_count" }),
            ),
        )
        .await;
        let repeatable_id = repeatable["id"].as_str().unwrap().to_owned();
        let repeatable_item = repeatable["items"][0]["id"].as_str().unwrap().to_owned();
        sqlx::query("UPDATE sale_lines SET prescription_item_id=? WHERE id=?")
            .bind(&repeatable_item)
            .bind(&draft_line)
            .execute(&f.pool)
            .await
            .unwrap();
        let error = forge(
            repeatable_id.clone(),
            repeatable_item.clone(),
            draft.clone(),
            draft_line.clone(),
            f.product_id.clone(),
            20,
        )
        .await;
        assert!(error.contains("prescription_quantity_exceeded"), "{error}");
        let (other, _, _) = second_product(&f, "Other").await;
        let error = forge(
            repeatable_id,
            repeatable_item,
            draft.clone(),
            draft_line.clone(),
            other,
            10,
        )
        .await;
        assert!(
            error.contains("prescription_dispensing_incoherent"),
            "{error}"
        );
        assert_eq!(dispensing_count(&f).await, 1);
    }

    /// Flips a draft to posted by hand, with every posting column filled, as a forger would. Since
    /// Phase 1M-C that includes each line's State snapshot where the test did not write its own.
    async fn flip_to_posted(f: &Fixture, id: &str) -> String {
        sqlx::query(
            "UPDATE sale_lines SET regulatory_state_snapshot=\
             '{\"premises_state_code\":\"27\",\"punjab_restricted_supply\":\"not_applicable\"}' \
             WHERE sale_document_id=? AND regulatory_state_snapshot IS NULL",
        )
        .bind(id)
        .execute(&f.pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE sale_documents SET status='posted',document_number='INV/2627/000077',             sequence_value=77,series_code='INV',financial_year='2026-27',             tax_treatment='intra_state',posted_by_user_id=created_by_user_id,             posted_at_utc=updated_at_utc,posting_idempotency_key=? WHERE id=?",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(id)
        .execute(&f.pool)
        .await
        .unwrap_err()
        .to_string()
    }

    /// B-35. A Schedule H Sale cannot be posted by direct SQL without a dispensing, and a line
    /// carrying a prescription link cannot be posted without being dispensed against it.
    #[tokio::test]
    async fn b35_the_database_refuses_to_post_schedule_h_without_a_dispensing() {
        let (f, _) = schedule_h_fixture().await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        sqlx::query(
            "UPDATE sale_lines SET regulatory_snapshot_version=1,regulatory_schemes_snapshot=             '{\"schedule_h\":\"applies\",\"schedule_h1\":\"does_not_apply\",             \"schedule_x\":\"does_not_apply\",\"schedule_c\":\"does_not_apply\",             \"schedule_c1\":\"does_not_apply\"}' WHERE sale_document_id=?",
        )
        .bind(&id)
        .execute(&f.pool)
        .await
        .unwrap();
        let error = flip_to_posted(&f, &id).await;
        assert!(error.contains("regulatory_gate_refuses_posting"), "{error}");
        assert_eq!(detail(&f, &id).await["status"], "draft");

        let g = fixture().await;
        make_lawful_medicine(&g, &g.product_id).await;
        let (_, item) = simple_prescription(&g, 20).await;
        let id = draft_with_line(&g, "pack", 1, 8000).await;
        sqlx::query(
            "UPDATE sale_lines SET prescription_item_id=?,regulatory_snapshot_version=1,             regulatory_schemes_snapshot=             '{\"schedule_h\":\"does_not_apply\",\"schedule_h1\":\"does_not_apply\",             \"schedule_x\":\"does_not_apply\",\"schedule_c\":\"does_not_apply\",             \"schedule_c1\":\"does_not_apply\"}' WHERE sale_document_id=?",
        )
        .bind(&item)
        .bind(&id)
        .execute(&g.pool)
        .await
        .unwrap();
        let error = flip_to_posted(&g, &id).await;
        assert!(
            error.contains("prescription_link_without_dispensing"),
            "{error}"
        );
    }

    /// B-36. A dispensed prescription's facts and items cannot be rewritten by direct SQL; only
    /// the archive is allowed.
    #[tokio::test]
    async fn b36_a_dispensed_prescription_cannot_be_rewritten_by_sql() {
        let (f, professional) = schedule_h_fixture().await;
        let (prescription, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        post_as_quoted(&f, &id).await;
        for statement in [
            "UPDATE prescriptions SET subject_name='Someone Else' WHERE id=?1",
            "UPDATE prescriptions SET repeat_authority='stated_without_count' WHERE id=?1",
            "UPDATE prescriptions SET prescribed_on='2026-01-01' WHERE id=?1",
            "UPDATE prescription_items SET prescribed_quantity_atoms=1000 WHERE prescription_id=?1",
            "DELETE FROM prescription_items WHERE prescription_id=?1",
        ] {
            let result = sqlx::query(statement)
                .bind(&prescription)
                .execute(&f.pool)
                .await;
            assert!(
                result.is_err(),
                "{statement} changed a dispensed prescription"
            );
        }
        let result = sqlx::query(
            "INSERT INTO prescription_items (id,prescription_id,line_number,product_id,\
             written_description,prescribed_quantity_atoms,dose_text,created_at_utc,updated_at_utc) \
             VALUES (?,?,9,?,'x',100,'x',strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&prescription)
        .bind(&f.product_id)
        .execute(&f.pool)
        .await;
        assert!(
            result.is_err(),
            "an item was added to a dispensed prescription"
        );
        let (status, archived) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/prescriptions/{prescription}/archive"),
            json!({ "expectedRevision": 1, "reason": "course completed" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{archived}");
    }

    // --- Correction policy -------------------------------------------------------------------

    /// B-37. Before any dispensing a prescription may be corrected, audited; after one, the API
    /// refuses and says to archive and re-enter.
    #[tokio::test]
    async fn b37_a_prescription_is_correctable_only_before_it_is_dispensed() {
        let (f, professional) = schedule_h_fixture().await;
        let (prescription, _) = simple_prescription(&f, 20).await;
        let mut corrected = prescription_body(&f.product_id, 30, json!({}));
        corrected["expectedRevision"] = json!(1);
        let (status, body) = request(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/prescriptions/{prescription}"),
            corrected.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["revision"], 2);
        assert_eq!(body["items"][0]["prescribedQuantityAtoms"], 30);
        let item = body["items"][0]["id"].as_str().unwrap().to_owned();
        let events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM master_change_events WHERE entity_type='prescription' \
             AND entity_id=? AND action='updated'",
        )
        .bind(&prescription)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(events, 1);

        // While a draft line points at it, correction is refused rather than orphaning the link.
        let id = prepared_sale(&f, &item, &professional, 1).await;
        corrected["expectedRevision"] = json!(2);
        let (status, refused) = request(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/prescriptions/{prescription}"),
            corrected.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");

        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (status, refused) = request(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/prescriptions/{prescription}"),
            corrected,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "prescription_is_dispensed");
    }

    /// B-38. A stale correction is refused rather than overwriting what someone else saved.
    #[tokio::test]
    async fn b38_a_stale_prescription_correction_is_refused() {
        let (f, _) = schedule_h_fixture().await;
        let (prescription, _) = simple_prescription(&f, 20).await;
        let mut corrected = prescription_body(&f.product_id, 30, json!({}));
        corrected["expectedRevision"] = json!(7);
        let (status, refused) = request(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/prescriptions/{prescription}"),
            corrected,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "revision_conflict");
    }

    // --- Entry validation --------------------------------------------------------------------

    /// B-39. Rule 65(10): an entry missing a required particular is refused, naming it.
    #[tokio::test]
    async fn b39_an_incomplete_prescription_is_refused_at_entry() {
        let f = fixture().await;
        for (label, change, field) in [
            (
                "unattested",
                json!({ "writtenSignedDatedAttested": false }),
                "writtenSignedDatedAttested",
            ),
            (
                "no patient name",
                json!({ "subjectName": "  " }),
                "subjectName",
            ),
            (
                "no patient address",
                json!({ "subjectAddress": "" }),
                "subjectAddress",
            ),
            (
                "no prescriber",
                json!({ "prescriberName": "" }),
                "prescriberName",
            ),
            (
                "no prescriber address",
                json!({ "prescriberAddress": " " }),
                "prescriberAddress",
            ),
            (
                "bad subject",
                json!({ "subjectKind": "company" }),
                "subjectKind",
            ),
            (
                "future date",
                json!({ "prescribedOn": "2099-01-01" }),
                "prescribedOn",
            ),
            (
                "impossible date",
                json!({ "prescribedOn": "2026-02-30" }),
                "prescribedOn",
            ),
            (
                "times without count",
                json!({ "repeatAuthority": "stated_times" }),
                "repeatAuthority",
            ),
            (
                "one time is not a repeat",
                json!({ "repeatAuthority": "stated_times", "repeatTimes": 1 }),
                "repeatAuthority",
            ),
            (
                "interval on once",
                json!({ "repeatIntervalDays": 7 }),
                "repeatIntervalDays",
            ),
            (
                "unknown authority",
                json!({ "repeatAuthority": "as_needed" }),
                "repeatAuthority",
            ),
            ("no items", json!({ "items": [] }), "items"),
        ] {
            let body = prescription_body(&f.product_id, 20, change);
            let (status, refused) =
                request(f.pool.clone(), "POST", "/api/v1/prescriptions", body).await;
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{label}: {refused}"
            );
            assert_eq!(refused["issues"][0]["field"], field, "{label}");
        }
        let mut body = prescription_body(&f.product_id, 0, json!({}));
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/prescriptions",
            body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        assert_eq!(
            refused["issues"][0]["field"],
            "items.0.prescribedQuantityAtoms"
        );
        body["items"][0]["prescribedQuantityAtoms"] = json!(10);
        body["items"][0]["doseText"] = json!("");
        let (status, refused) =
            request(f.pool.clone(), "POST", "/api/v1/prescriptions", body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        assert_eq!(refused["issues"][0]["field"], "items.0.doseText");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM prescriptions")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    /// B-40. Prescription references are their own series, allocated densely and distinct from
    /// invoice numbers, even when two counters enter prescriptions at the same moment.
    #[tokio::test]
    async fn b40_prescription_references_are_a_separate_dense_series() {
        let f = fixture().await;
        let body = prescription_body(&f.product_id, 20, json!({}));
        let (one, two) = tokio::join!(
            request(
                f.pool.clone(),
                "POST",
                "/api/v1/prescriptions",
                body.clone()
            ),
            request(
                f.pool.clone(),
                "POST",
                "/api/v1/prescriptions",
                body.clone()
            ),
        );
        let mut references = Vec::new();
        for (status, created) in [one, two] {
            if status == StatusCode::CREATED {
                references.push(created["reference"].as_str().unwrap().to_owned());
            } else {
                assert_eq!(created["code"], "service_busy", "{created}");
            }
        }
        let third = enter_prescription(&f, body).await;
        references.push(third["reference"].as_str().unwrap().to_owned());
        let mut sorted = references.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), references.len(), "{references:?}");
        let expected: Vec<String> = (1..=references.len())
            .map(|n| format!("RX-{n:06}"))
            .collect();
        assert_eq!(sorted, expected);
    }

    // --- Concurrency -------------------------------------------------------------------------

    /// B-41. Two counters dispensing the last of a prescription cannot both succeed.
    #[tokio::test]
    async fn b41_two_counters_cannot_both_dispense_the_last_quantity() {
        let (f, professional) = schedule_h_fixture().await;
        let created = enter_prescription(
            &f,
            prescription_body(
                &f.product_id,
                10,
                json!({ "repeatAuthority": "stated_without_count" }),
            ),
        )
        .await;
        let item = created["items"][0]["id"].as_str().unwrap().to_owned();
        let left = prepared_sale(&f, &item, &professional, 1).await;
        let right = prepared_sale(&f, &item, &professional, 1).await;
        let (one, two) = tokio::join!(post_as_quoted(&f, &left), post_as_quoted(&f, &right));
        let outcomes = [one, two];
        let succeeded = outcomes
            .iter()
            .filter(|(status, _)| *status == StatusCode::OK)
            .count();
        assert_eq!(succeeded, 1, "{outcomes:?}");
        for (status, body) in &outcomes {
            if *status != StatusCode::OK {
                assert!(
                    body["code"] == "prescription_requirements_incomplete"
                        || body["code"] == "service_busy",
                    "{body}"
                );
            }
        }
        let dispensed: i64 =
            sqlx::query_scalar("SELECT SUM(quantity_atoms) FROM prescription_dispensings")
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(dispensed, 10);
        assert_eq!(balance(&f, &f.batch_id).await, 90);
    }

    /// B-42. Two counters dispensing the last authorised repeat cannot both succeed.
    #[tokio::test]
    async fn b42_two_counters_cannot_both_take_the_last_repeat() {
        let (f, professional) = schedule_h_fixture().await;
        let created = enter_prescription(
            &f,
            prescription_body(
                &f.product_id,
                60,
                json!({ "repeatAuthority": "stated_times", "repeatTimes": 2 }),
            ),
        )
        .await;
        let item = created["items"][0]["id"].as_str().unwrap().to_owned();
        let first = prepared_sale(&f, &item, &professional, 1).await;
        let (status, _) = post_as_quoted(&f, &first).await;
        assert_eq!(status, StatusCode::OK);
        let left = prepared_sale(&f, &item, &professional, 1).await;
        let right = prepared_sale(&f, &item, &professional, 1).await;
        let (one, two) = tokio::join!(post_as_quoted(&f, &left), post_as_quoted(&f, &right));
        let succeeded = [&one, &two]
            .iter()
            .filter(|(status, _)| *status == StatusCode::OK)
            .count();
        assert_eq!(succeeded, 1, "{one:?} {two:?}");
        let occasions: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT sale_document_id) FROM prescription_dispensings",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(occasions, 2);
    }

    /// B-43. A pharmacist archived while a Sale posts yields one coherent answer: either the Sale
    /// posted under a pharmacist who was valid, or it was refused.
    #[tokio::test]
    async fn b43_a_pharmacist_archived_during_posting_yields_one_coherent_answer() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let (revision,): (i64,) =
            sqlx::query_as("SELECT revision FROM store_professionals WHERE id=?")
                .bind(&professional)
                .fetch_one(&f.pool)
                .await
                .unwrap();
        let archive_uri = format!("/api/v1/store/professionals/{professional}/archive");
        let (posted, archived) = tokio::join!(
            post_as_quoted(&f, &id),
            request(
                f.pool.clone(),
                "POST",
                &archive_uri,
                json!({ "expectedRevision": revision, "reason": "left the pharmacy" }),
            ),
        );
        let archived_at: Option<String> =
            sqlx::query_scalar("SELECT archived_at_utc FROM store_professionals WHERE id=?")
                .bind(&professional)
                .fetch_one(&f.pool)
                .await
                .unwrap();
        if posted.0 == StatusCode::OK {
            assert_eq!(dispensing_count(&f).await, 1);
            let posted_at: String =
                sqlx::query_scalar("SELECT posted_at_utc FROM sale_documents WHERE id=?")
                    .bind(&id)
                    .fetch_one(&f.pool)
                    .await
                    .unwrap();
            if let Some(archived_at) = archived_at {
                assert!(
                    posted_at <= archived_at,
                    "posted under an archived pharmacist"
                );
            }
        } else {
            assert_eq!(dispensing_count(&f).await, 0, "{posted:?} {archived:?}");
            assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
        }
    }

    /// B-44. A prescription archived while a Sale posts yields one coherent answer.
    #[tokio::test]
    async fn b44_a_prescription_archived_during_posting_yields_one_coherent_answer() {
        let (f, professional) = schedule_h_fixture().await;
        let (prescription, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let archive_uri = format!("/api/v1/prescriptions/{prescription}/archive");
        let (posted, archived) = tokio::join!(
            post_as_quoted(&f, &id),
            request(
                f.pool.clone(),
                "POST",
                &archive_uri,
                json!({ "expectedRevision": 1, "reason": "withdrawn by prescriber" }),
            ),
        );
        let dispensings = dispensing_count(&f).await;
        match posted.0 {
            StatusCode::OK => assert_eq!(dispensings, 1),
            _ => {
                assert_eq!(dispensings, 0, "{posted:?} {archived:?}");
                assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
            }
        }
    }

    // --- Returns -----------------------------------------------------------------------------

    async fn return_first_line(f: &Fixture, sale: &str, packs: i64) -> (StatusCode, Value) {
        let sale_line = first_line_id(f, sale).await;
        let (status, draft) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/returns",
            json!({ "returnKind": "sales_return", "originalDocumentId": sale, "businessDate": TODAY }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let return_id = draft["id"].as_str().unwrap().to_owned();
        let (status, withline) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{return_id}/lines"),
            json!({
                "expectedRevision": 1,
                "originalLineId": sale_line,
                "quantity": packs,
                "disposition": "quarantined",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{withline}");
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{return_id}/post"),
            json!({
                "expectedRevision": 2,
                "idempotencyKey": Uuid::now_v7().to_string(),
                "taxAdjustmentStatus": "commercial_only",
            }),
        )
        .await
    }

    /// B-45. A return reinstates the quantity as an append-only reversal; it does not give back
    /// the occasion, so a once-only prescription stays used.
    #[tokio::test]
    async fn b45_a_return_reinstates_quantity_but_not_the_occasion() {
        let (f, professional) = schedule_h_fixture().await;
        let (prescription, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 2).await;
        let (status, _) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK);
        let (status, returned) = return_first_line(&f, &id, 1).await;
        assert_eq!(status, StatusCode::OK, "{returned}");
        let reversed: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(quantity_atoms),0) FROM prescription_dispensing_reversals",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(reversed, 10);
        let (status, detail) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/prescriptions/{prescription}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{detail}");
        assert_eq!(detail["items"][0]["dispensedAtoms"], 20);
        assert_eq!(detail["items"][0]["reinstatedAtoms"], 10);
        assert_eq!(detail["occasionsUsed"], 1);
        assert_eq!(detail["occasionsAuthorised"], 1);

        let again = prepared_sale(&f, &item, &professional, 1).await;
        let (status, refused) = post_as_quoted(&f, &again).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["lines.1.prescription_repeat_not_authorised".to_owned()]
        );
    }

    /// B-46. Where repeats are authorised, returned quantity may be supplied again — and no more.
    #[tokio::test]
    async fn b46_a_returned_quantity_can_be_redispensed_within_the_authority() {
        let (f, professional) = schedule_h_fixture().await;
        let created = enter_prescription(
            &f,
            prescription_body(
                &f.product_id,
                20,
                json!({ "repeatAuthority": "stated_without_count" }),
            ),
        )
        .await;
        let item = created["items"][0]["id"].as_str().unwrap().to_owned();
        let id = prepared_sale(&f, &item, &professional, 2).await;
        post_as_quoted(&f, &id).await;
        let (status, returned) = return_first_line(&f, &id, 1).await;
        assert_eq!(status, StatusCode::OK, "{returned}");
        let over = prepared_sale(&f, &item, &professional, 2).await;
        let (status, refused) = post_as_quoted(&f, &over).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        let fits = prepared_sale(&f, &item, &professional, 1).await;
        let (status, posted) = post_as_quoted(&f, &fits).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
    }

    /// B-47. Return abuse: a reversal cannot be forged beyond what was dispensed, or against a
    /// return line of another Sale line, and cannot be edited once written.
    #[tokio::test]
    async fn b47_the_database_refuses_forged_or_excessive_reversals() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 2).await;
        post_as_quoted(&f, &id).await;
        let (status, _) = return_first_line(&f, &id, 1).await;
        assert_eq!(status, StatusCode::OK);
        let (dispensing, return_line): (String, String) = sqlx::query_as(
            "SELECT dispensing_id,return_line_id FROM prescription_dispensing_reversals",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        // The same return line twice.
        let result = sqlx::query(
            "INSERT INTO prescription_dispensing_reversals (id,dispensing_id,return_line_id,\
             quantity_atoms,created_at_utc) VALUES (?,?,?,1,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&dispensing)
        .bind(&return_line)
        .execute(&f.pool)
        .await;
        assert!(result.is_err(), "one return line reversed twice");
        for statement in [
            "UPDATE prescription_dispensing_reversals SET quantity_atoms=20",
            "DELETE FROM prescription_dispensing_reversals",
        ] {
            assert!(
                sqlx::query(statement).execute(&f.pool).await.is_err(),
                "{statement}"
            );
        }
        // A second, real return of the other strip is reinstated; a third cannot exceed it.
        let (status, second) = return_first_line(&f, &id, 1).await;
        assert_eq!(status, StatusCode::OK, "{second}");
        let (status, third) = return_first_line_expecting_refusal(&f, &id).await;
        assert_ne!(status, StatusCode::OK, "{third}");
        let reversed: i64 =
            sqlx::query_scalar("SELECT SUM(quantity_atoms) FROM prescription_dispensing_reversals")
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(reversed, 20);
    }

    async fn return_first_line_expecting_refusal(f: &Fixture, sale: &str) -> (StatusCode, Value) {
        let sale_line = first_line_id(f, sale).await;
        let (_, draft) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/returns",
            json!({ "returnKind": "sales_return", "originalDocumentId": sale, "businessDate": TODAY }),
        )
        .await;
        let return_id = draft["id"].as_str().unwrap_or_default().to_owned();
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{return_id}/lines"),
            json!({
                "expectedRevision": 1,
                "originalLineId": sale_line,
                "quantity": 1,
                "disposition": "quarantined",
            }),
        )
        .await
    }

    // --- Privacy and audit -------------------------------------------------------------------

    /// B-48. The list names references and prescribers, never the patient; the audit trail
    /// names identifiers, never the patient; the Sale and its quote never carry the patient.
    #[tokio::test]
    async fn b48_patient_particulars_appear_only_on_the_prescription_itself() {
        let (f, professional) = schedule_h_fixture().await;
        let (prescription, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let quote = quote_of(&f, &id).await;
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (_, listed) =
            request(f.pool.clone(), "GET", "/api/v1/prescriptions", Value::Null).await;
        let (_, invoice) = invoice(&f, &id).await;
        for (label, value) in [
            ("list", listed.to_string()),
            ("quote", quote.to_string()),
            ("sale", posted.to_string()),
            ("invoice", invoice.to_string()),
        ] {
            assert!(!value.contains(PATIENT_NAME), "{label} carries the patient");
            assert!(
                !value.contains(PATIENT_ADDRESS),
                "{label} carries the address"
            );
        }
        let payloads: Vec<String> = sqlx::query_scalar(
            "SELECT change_payload || COALESCE(reason,'') FROM master_change_events",
        )
        .fetch_all(&f.pool)
        .await
        .unwrap();
        for payload in &payloads {
            assert!(
                !payload.contains(PATIENT_NAME),
                "audit carries the patient: {payload}"
            );
            assert!(
                !payload.contains(PATIENT_ADDRESS),
                "audit carries the address: {payload}"
            );
        }
        let (_, detail) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/prescriptions/{prescription}"),
            Value::Null,
        )
        .await;
        assert_eq!(
            detail["subjectName"], PATIENT_NAME,
            "the record itself keeps it"
        );
        assert_eq!(
            detail["dispensings"][0]["supervisingProfessionalName"],
            "Meera Iyer"
        );
        let kinds: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT entity_type FROM master_change_events \
             WHERE entity_type LIKE 'prescription%' ORDER BY 1",
        )
        .fetch_all(&f.pool)
        .await
        .unwrap();
        assert_eq!(
            kinds,
            vec![
                "prescription",
                "prescription_dispensing",
                "prescription_supply_record"
            ]
        );
    }

    /// B-49. The prescriber master is a convenience: pharmacists add, only the owner corrects or
    /// archives, registration is optional, and a correction never reaches a prescription already
    /// written.
    #[tokio::test]
    async fn b49_the_prescriber_master_never_rewrites_a_written_prescription() {
        let f = fixture().await;
        insert_session(&f.pool, "pharmacist", PHARMACIST_TOKEN).await;
        let (status, prescriber) = request_as(
            f.pool.clone(),
            "POST",
            "/api/v1/prescribers",
            json!({ "fullName": "Dr. Anjali Rao", "addressText": "Rao Clinic, Pune" }),
            Some(PHARMACIST_TOKEN),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{prescriber}");
        assert_eq!(prescriber["registrationNumber"], Value::Null);
        let prescriber_id = prescriber["id"].as_str().unwrap().to_owned();

        let mut body = prescription_body(&f.product_id, 20, json!({}));
        body["prescriberId"] = json!(prescriber_id);
        let written = enter_prescription(&f, body).await;

        let update = json!({
            "expectedRevision": 1,
            "fullName": "Dr. Anjali Rao-Kulkarni",
            "addressText": "New Clinic, Pune",
        });
        let (status, _) = request_as(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/prescribers/{prescriber_id}"),
            update.clone(),
            Some(PHARMACIST_TOKEN),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, updated) = request(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/prescribers/{prescriber_id}"),
            update,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{updated}");
        assert_eq!(updated["fullName"], "Dr. Anjali Rao-Kulkarni");

        let (_, reread) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/prescriptions/{}", written["id"].as_str().unwrap()),
            Value::Null,
        )
        .await;
        assert_eq!(reread["prescriberName"], "Dr. Anjali Rao");
        assert_eq!(
            reread["prescriberAddress"],
            "Rao Clinic, FC Road, Pune 411005"
        );

        let (status, _) = request_as(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/prescribers/{prescriber_id}/archive"),
            json!({ "expectedRevision": 2, "reason": "retired" }),
            Some(PHARMACIST_TOKEN),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, archived) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/prescribers/{prescriber_id}/archive"),
            json!({ "expectedRevision": 2, "reason": "retired" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{archived}");
        assert_eq!(archived["status"], "archived");
    }

    /// B-50. An animal's prescription records the owner, as rule 65(10)(b) allows, and dispenses
    /// on the same terms.
    #[tokio::test]
    async fn b50_an_animal_prescription_records_the_owner_and_dispenses() {
        let (f, professional) = schedule_h_fixture().await;
        let mut body = prescription_body(&f.product_id, 20, json!({}));
        body["subjectKind"] = json!("animal");
        body["subjectName"] = json!("Owner: Arjun Patil (dog, Bruno)");
        let created = enter_prescription(&f, body).await;
        assert_eq!(created["subjectKind"], "animal");
        let item = created["items"][0]["id"].as_str().unwrap().to_owned();
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
    }

    /// B-51. Sales posted before this phase keep reporting what they were: no prescription, no
    /// pharmacist, no endorsement — and nothing is invented for them.
    #[tokio::test]
    async fn b51_an_earlier_sale_reports_no_supply_facts_it_never_had() {
        let f = fixture().await;
        let id = post_one(&f).await;
        let sale = detail(&f, &id).await;
        assert_eq!(sale["supply"]["supervisingProfessionalId"], Value::Null);
        assert_eq!(sale["supply"]["prescriptionEndorsementConfirmed"], false);
        assert_eq!(sale["lines"][0]["prescriptionItemId"], Value::Null);
        assert_eq!(dispensing_count(&f).await, 0);
    }

    /// B-52. The supply endpoint refuses a professional of another store or an unknown one, and a
    /// stale revision.
    #[tokio::test]
    async fn b52_the_supply_endpoint_validates_what_it_is_given() {
        let (f, professional) = schedule_h_fixture().await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, refused) =
            set_supply(&f, &id, Some(&Uuid::now_v7().to_string()), true, OWNER).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        let (status, refused) = request(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/sales/{id}/supply"),
            json!({
                "expectedRevision": 99,
                "supervisingProfessionalId": professional,
                "prescriptionEndorsementConfirmed": true,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "revision_conflict");
        let (status, set) = set_supply(&f, &id, Some(&professional), true, OWNER).await;
        assert_eq!(status, StatusCode::OK, "{set}");
        assert_eq!(set["supply"]["supervisingProfessionalId"], professional);
        assert_eq!(set["supply"]["prescriptionEndorsementConfirmed"], true);
    }

    /// B-53. A prescription of another store (only possible by forging rows past the single-store
    /// guard) is not this pharmacy's authority: refused at link and at posting.
    #[tokio::test]
    async fn b53_another_stores_prescription_is_refused() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let foreign_store = Uuid::now_v7().to_string();
        sqlx::query("DROP TRIGGER store_identity_single_store_insert")
            .execute(&f.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO store_identity (store_id,display_name,business_time_zone,created_at_utc) \
             VALUES (?,'Elsewhere','Asia/Kolkata',strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&foreign_store)
        .execute(&f.pool)
        .await
        .unwrap();
        let id = prepared_sale(&f, &item, &professional, 1).await;
        // Move the prescription to the other store after the link, as a forger would.
        sqlx::query(
            "UPDATE prescriptions SET store_id=? WHERE id=(SELECT prescription_id FROM \
             prescription_items WHERE id=?)",
        )
        .bind(&foreign_store)
        .bind(&item)
        .execute(&f.pool)
        .await
        .unwrap();
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["lines.1.prescription_not_found".to_owned()]
        );
        let fresh = draft_with_line(&f, "pack", 1, 8000).await;
        let line = first_line_id(&f, &fresh).await;
        let (status, refused) = link_line(&f, &line, &fresh, Some(&item), OWNER).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.prescription_not_found".to_owned()]
        );
        assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
    }

    /// B-54. Identifiers of the wrong kind are not prescription items: a prescription's own id, a
    /// dispensing's id, or a Sale line's id, pasted where an item id belongs; and a dispensing id
    /// smuggled into the posting request is ignored.
    #[tokio::test]
    async fn b54_forged_identifiers_are_not_prescription_items() {
        let (f, professional) = schedule_h_fixture().await;
        let (prescription, item) = simple_prescription(&f, 40).await;
        let first = prepared_sale(&f, &item, &professional, 1).await;
        let (status, _) = post_as_quoted(&f, &first).await;
        assert_eq!(status, StatusCode::OK);
        let dispensing: String = sqlx::query_scalar("SELECT id FROM prescription_dispensings")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        let sale_line = first_line_id(&f, &first).await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let line = first_line_id(&f, &id).await;
        for forged in [&prescription, &dispensing, &sale_line] {
            let (status, refused) = link_line(&f, &line, &id, Some(forged), OWNER).await;
            assert_eq!(status, StatusCode::CONFLICT, "{refused}");
            assert_eq!(
                prescription_issue_codes(&refused),
                vec!["supply.prescription_not_found".to_owned()]
            );
        }
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/post"),
            json!({
                "expectedRevision": revision_of(&f, &id).await,
                "idempotencyKey": Uuid::now_v7().to_string(),
                "tenders": [{ "method": "cash", "amountPaise": 8960 }],
                "dispensingId": dispensing,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "prescription_requirements_incomplete");
        assert_eq!(dispensing_count(&f).await, 1);
    }

    /// B-55. A reversal cannot pair one Sale's dispensing with a return line of another Sale line.
    #[tokio::test]
    async fn b55_a_reversal_against_the_wrong_sale_line_is_refused() {
        let (f, professional) = schedule_h_fixture().await;
        let created = enter_prescription(
            &f,
            prescription_body(
                &f.product_id,
                40,
                json!({ "repeatAuthority": "stated_without_count" }),
            ),
        )
        .await;
        let item = created["items"][0]["id"].as_str().unwrap().to_owned();
        let one = prepared_sale(&f, &item, &professional, 1).await;
        let (status, _) = post_as_quoted(&f, &one).await;
        assert_eq!(status, StatusCode::OK);
        let two = prepared_sale(&f, &item, &professional, 1).await;
        let (status, _) = post_as_quoted(&f, &two).await;
        assert_eq!(status, StatusCode::OK);

        // A return of Sale two, not yet reversed by anyone: build its return line without posting
        // the return, so the only question is whether it may reverse Sale one's dispensing.
        let sale_line = first_line_id(&f, &two).await;
        let (status, draft) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/returns",
            json!({ "returnKind": "sales_return", "originalDocumentId": two, "businessDate": TODAY }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let return_id = draft["id"].as_str().unwrap().to_owned();
        let (status, withline) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{return_id}/lines"),
            json!({
                "expectedRevision": 1,
                "originalLineId": sale_line,
                "quantity": 1,
                "disposition": "quarantined",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{withline}");
        let return_line: String =
            sqlx::query_scalar("SELECT id FROM return_lines WHERE original_sale_line_id=?")
                .bind(&sale_line)
                .fetch_one(&f.pool)
                .await
                .unwrap();
        let first_dispensing: String =
            sqlx::query_scalar("SELECT id FROM prescription_dispensings WHERE sale_document_id=?")
                .bind(&one)
                .fetch_one(&f.pool)
                .await
                .unwrap();
        let error = sqlx::query(
            "INSERT INTO prescription_dispensing_reversals (id,dispensing_id,return_line_id,\
             quantity_atoms,created_at_utc) VALUES (?,?,?,10,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&first_dispensing)
        .bind(&return_line)
        .execute(&f.pool)
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("prescription_reversal_incoherent"),
            "{error}"
        );
        let reversals: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM prescription_dispensing_reversals")
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(reversals, 0);
    }

    /// B-56. A return and a new dispensing racing each other never create authority the
    /// prescription did not give.
    #[tokio::test]
    async fn b56_a_return_racing_a_redispensing_never_exceeds_the_authority() {
        let (f, professional) = schedule_h_fixture().await;
        let created = enter_prescription(
            &f,
            prescription_body(
                &f.product_id,
                20,
                json!({ "repeatAuthority": "stated_without_count" }),
            ),
        )
        .await;
        let item = created["items"][0]["id"].as_str().unwrap().to_owned();
        let first = prepared_sale(&f, &item, &professional, 2).await;
        let (status, _) = post_as_quoted(&f, &first).await;
        assert_eq!(status, StatusCode::OK);
        let next = prepared_sale(&f, &item, &professional, 1).await;
        // The entry is prepared and confirmed first, so the race is the return against the posting.
        prepare_and_confirm(&f, &next).await;
        let (returned, redispensed) =
            tokio::join!(return_first_line(&f, &first, 1), post_confirmed(&f, &next));
        let (dispensed, reversed): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COALESCE(SUM(quantity_atoms),0) FROM prescription_dispensings),\
             (SELECT COALESCE(SUM(quantity_atoms),0) FROM prescription_dispensing_reversals)",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert!(
            dispensed - reversed <= 20,
            "net {dispensed}-{reversed} over authority: {returned:?} {redispensed:?}"
        );
        if redispensed.0 == StatusCode::OK {
            assert_eq!(returned.0, StatusCode::OK, "{returned:?}");
            assert_eq!((dispensed, reversed), (30, 10));
        } else {
            assert_eq!(dispensed, 20, "{redispensed:?}");
        }
    }

    /// B-57. Professional validity is inclusive at both ends and judged on the actual posting day.
    #[tokio::test]
    async fn b57_professional_validity_boundaries_are_exact() {
        let probe = fixture().await;
        let (today, yesterday): (String, String) = sqlx::query_as(
            "SELECT strftime('%Y-%m-%d','now','+5 hours','+30 minutes'),\
             strftime('%Y-%m-%d','now','+5 hours','+30 minutes','-1 day')",
        )
        .fetch_one(&probe.pool)
        .await
        .unwrap();
        for (label, from, upto, posts) in [
            ("valid from today", today.clone(), None, true),
            (
                "valid up to today",
                "2020-01-01".to_owned(),
                Some(today.clone()),
                true,
            ),
            (
                "ended yesterday",
                "2020-01-01".to_owned(),
                Some(yesterday.clone()),
                false,
            ),
        ] {
            let f = fixture().await;
            schedule_product(&f, &f.product_id, &["schedule_h"]).await;
            record_basis(&f, &f.product_id, Some("prescription_register")).await;
            let professional = add_professional(
                &f,
                json!({
                    "fullName": "Ravi Menon",
                    "capacity": "registered_pharmacist",
                    "registrationNumber": "MH-PH-10001",
                    "validFrom": from,
                    "validUpto": upto,
                }),
            )
            .await;
            let mut body = prescription_body(&f.product_id, 20, json!({}));
            body["prescribedOn"] = json!(yesterday);
            let created = enter_prescription(&f, body).await;
            let item = created["items"][0]["id"].as_str().unwrap().to_owned();
            let id = prepared_sale_on(&f, &item, &professional, 1, &today).await;
            let (status, body) = post_as_quoted(&f, &id).await;
            assert_eq!(status == StatusCode::OK, posts, "{label}: {body}");
            if !posts {
                assert_eq!(
                    prescription_issue_codes(&body),
                    vec!["supply.supervising_pharmacist_invalid".to_owned()],
                    "{label}"
                );
            }
        }
    }

    /// B-58. A backdated Sale cannot slip inside a stated interval by being dated before the
    /// earlier supply.
    #[tokio::test]
    async fn b58_backdating_cannot_defeat_a_repeat_interval() {
        let (f, professional) = schedule_h_fixture().await;
        let mut body = prescription_body(
            &f.product_id,
            60,
            json!({
                "repeatAuthority": "stated_times",
                "repeatTimes": 3,
                "repeatIntervalDays": 7,
            }),
        );
        body["prescribedOn"] = json!("2026-05-01");
        let created = enter_prescription(&f, body).await;
        let item = created["items"][0]["id"].as_str().unwrap().to_owned();
        let first = prepared_sale_on(&f, &item, &professional, 1, "2026-06-18").await;
        let (status, _) = post_as_quoted(&f, &first).await;
        assert_eq!(status, StatusCode::OK);
        let backdated = prepared_sale_on(&f, &item, &professional, 1, "2026-06-05").await;
        let (status, refused) = post_as_quoted(&f, &backdated).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["lines.1.prescription_repeat_too_soon".to_owned()]
        );
    }

    /// B-59. A drug placed in Schedule H since a backdated Sale's date still needs a prescription:
    /// the stricter of the two days governs, as in Phase 1M-A.
    #[tokio::test]
    async fn b59_a_backdated_sale_cannot_escape_a_schedule_h_listing_made_since() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        record_basis(&f, &f.product_id, Some("prescription_register")).await;
        let (commenced, backdated): (String, String) = sqlx::query_as(
            "SELECT strftime('%Y-%m-%d','now','+5 hours','+30 minutes','-10 days'),\
             strftime('%Y-%m-%d','now','+5 hours','+30 minutes','-20 days')",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        clear_every_schedule(&f, "2020-01-01").await;
        close_scheme(&f, "schedule_h", &commenced).await;
        let (status, body) = classify_scheme(&f, "schedule_h", true, &commenced, None).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let (_, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/sales",
            json!({ "customerPartyId": Value::Null, "businessDate": backdated }),
        )
        .await;
        let id = created["id"].as_str().unwrap().to_owned();
        add_line(&f, &id, &f.product_id, &f.pack_id, &f.batch_id, 1, 8000).await;
        let quote = quote_of(&f, &id).await;
        assert_eq!(quote["lines"][0]["regulatoryGate"], "prescription_required");
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "prescription_requirements_incomplete");
        assert_nothing_posted(&f, &id, &f.batch_id, 100).await;
    }

    // -----------------------------------------------------------------------------------------
    // Phase 1M-B corrective B-R2 — the rule 65(3)(1)(g) signature before the supply
    //
    // The entry is prepared and its serial allocated while nothing is sold; the registered
    // pharmacist signs the physical entry by hand and its serial goes on the prescription; a person
    // confirms both; only then may the Sale post, finalizing the entry in the same transaction.
    // -----------------------------------------------------------------------------------------

    #[derive(Debug, PartialEq, sqlx::FromRow)]
    struct EntryState {
        id: String,
        serial_number: String,
        record_method: String,
        status: String,
        manual_signature_confirmed: i64,
        serial_written_on_prescription: i64,
        prepared_by_user_id: String,
        confirmed_by_user_id: Option<String>,
        previous_record_id: Option<String>,
        subject_name: String,
        date_of_supply: String,
    }

    async fn entries(f: &Fixture, sale: &str) -> Vec<EntryState> {
        sqlx::query_as(
            "SELECT id,serial_number,record_method,status,manual_signature_confirmed,\
             serial_written_on_prescription,prepared_by_user_id,confirmed_by_user_id,\
             previous_record_id,subject_name,date_of_supply FROM prescription_supply_records \
             WHERE sale_document_id=? ORDER BY serial_value",
        )
        .bind(sale)
        .fetch_all(&f.pool)
        .await
        .unwrap()
    }

    async fn entry_count(f: &Fixture) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM prescription_supply_records")
            .fetch_one(&f.pool)
            .await
            .unwrap()
    }

    async fn prepare_as(f: &Fixture, sale: &str, token: &str) -> (StatusCode, Value) {
        let revision = revision_of(f, sale).await;
        request_as(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{sale}/prescription-records"),
            json!({ "expectedRevision": revision }),
            Some(token),
        )
        .await
    }

    async fn confirm_as(
        f: &Fixture,
        record: &str,
        signature: bool,
        serial: bool,
        token: &str,
    ) -> (StatusCode, Value) {
        request_as(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/prescription-supply-records/{record}/confirm"),
            json!({ "manualSignatureConfirmed": signature, "serialWrittenOnPrescription": serial }),
            Some(token),
        )
        .await
    }

    async fn void_as(f: &Fixture, record: &str, reason: &str, token: &str) -> (StatusCode, Value) {
        request_as(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/prescription-supply-records/{record}/void"),
            json!({ "reason": reason }),
            Some(token),
        )
        .await
    }

    /// Nothing about a prepared-but-unposted supply has been sold.
    async fn assert_nothing_sold(f: &Fixture, sale: &str) {
        assert_nothing_posted(f, sale, &f.batch_id, 100).await;
        assert_eq!(dispensing_count(f).await, 0, "a dispensing was written");
        assert_eq!(tender_rows(f, sale).await, 0, "a tender was written");
        assert_eq!(numbers_issued(f).await, 0, "an invoice number was issued");
    }

    /// S-01. An entry that is prepared but not confirmed cannot produce a posted Sale.
    #[tokio::test]
    async fn s01_an_unsigned_entry_cannot_produce_a_posted_sale() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let (status, prepared) = prepare_as(&f, &id, OWNER).await;
        assert_eq!(status, StatusCode::OK, "{prepared}");
        let (status, refused) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.prescription_record_not_confirmed".to_owned()]
        );
        assert_nothing_sold(&f, &id).await;
        assert_eq!(entries(&f, &id).await[0].status, "prepared");
    }

    /// S-02. Preparing allocates the serial while nothing is sold: no stock, no dispensing, no
    /// tender, no number, and the Sale is still a draft showing its prepared entry.
    #[tokio::test]
    async fn s02_the_serial_exists_before_the_manual_acts_and_nothing_is_sold() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        assert_eq!(
            quote_of(&f, &id).await["supply"]["issues"],
            json!(["prescription_record_not_prepared"])
        );
        let (status, sale) = prepare_as(&f, &id, OWNER).await;
        assert_eq!(status, StatusCode::OK, "{sale}");
        assert_eq!(sale["status"], "draft");
        assert_eq!(sale["prescriptionRecords"][0]["serialNumber"], "PR-000001");
        assert_eq!(sale["prescriptionRecords"][0]["status"], "prepared");
        let entry = entries(&f, &id).await.remove(0);
        assert_eq!(
            (
                entry.status.as_str(),
                entry.manual_signature_confirmed,
                entry.serial_written_on_prescription
            ),
            ("prepared", 0, 0)
        );
        assert_eq!(entry.date_of_supply, TODAY);
        assert_eq!(entry.subject_name, PATIENT_NAME);
        assert_nothing_sold(&f, &id).await;
        assert_eq!(
            quote_of(&f, &id).await["supply"]["issues"],
            json!(["prescription_record_not_confirmed"])
        );
    }

    /// S-03, S-04, S-05. The serial on the prescription and the manual signature are each required,
    /// neither is enough alone, and the database will not hold one without the other.
    #[tokio::test]
    async fn s03_both_manual_acts_are_required_and_neither_suffices_alone() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        prepare_as(&f, &id, OWNER).await;
        let record = entries(&f, &id).await.remove(0).id;
        for (signature, serial, field) in [
            (true, false, "serialWrittenOnPrescription"),
            (false, true, "manualSignatureConfirmed"),
            (false, false, "manualSignatureConfirmed"),
        ] {
            let (status, refused) = confirm_as(&f, &record, signature, serial, OWNER).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
            assert_eq!(refused["issues"][0]["field"], field);
        }
        assert_eq!(entries(&f, &id).await[0].status, "prepared");
        for statement in [
            "UPDATE prescription_supply_records SET manual_signature_confirmed=1",
            "UPDATE prescription_supply_records SET serial_written_on_prescription=1",
            "UPDATE prescription_supply_records SET status='confirmed'",
        ] {
            assert!(
                sqlx::query(statement).execute(&f.pool).await.is_err(),
                "{statement}"
            );
        }
        let (status, refused) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_nothing_sold(&f, &id).await;
    }

    /// S-06. With both acts confirmed the Sale posts, and the entry is finalized in the same
    /// transaction, every line linked to the dispensing it records.
    #[tokio::test]
    async fn s06_both_confirmations_permit_finalization() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        prepare_as(&f, &id, OWNER).await;
        let record = entries(&f, &id).await.remove(0).id;
        let (status, confirmed) = confirm_as(&f, &record, true, true, OWNER).await;
        assert_eq!(status, StatusCode::OK, "{confirmed}");
        assert_eq!(confirmed["status"], "confirmed");
        assert_eq!(quote_of(&f, &id).await["supply"]["issues"], json!([]));
        let (status, posted) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["prescriptionRecords"][0]["status"], "finalized");
        let linked: (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*),COUNT(dispensing_id) FROM prescription_supply_record_lines WHERE record_id=?",
        )
        .bind(&record)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(linked, (1, 1));
        assert_eq!(dispensing_count(&f).await, 1);
        assert_eq!(balance(&f, &f.batch_id).await, 90);
    }

    /// S-07. No direct API call skips the confirmation: flags smuggled into the posting are ignored,
    /// and a cashier cannot prepare, confirm or void.
    #[tokio::test]
    async fn s07_the_api_cannot_be_used_to_skip_the_manual_acts() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let (status, _) = prepare_as(&f, &id, CASHIER).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        prepare_as(&f, &id, OWNER).await;
        let record = entries(&f, &id).await.remove(0).id;
        let (status, _) = confirm_as(&f, &record, true, true, CASHIER).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = void_as(&f, &record, "cashier trying", CASHIER).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let revision = revision_of(&f, &id).await;
        let (status, refused) = request_as(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/post"),
            json!({
                "expectedRevision": revision,
                "idempotencyKey": Uuid::now_v7().to_string(),
                "tenders": [{ "method": "cash", "amountPaise": 8960 }],
                "manualSignatureConfirmed": true,
                "serialWrittenOnPrescription": true,
                "recordStatus": "confirmed",
            }),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_nothing_sold(&f, &id).await;
    }

    /// S-08. Direct SQL cannot post a Schedule H Sale whose entry is unsigned: not with no entry,
    /// not with a prepared or confirmed one, and a status cannot be forged forward.
    #[tokio::test]
    async fn s08_direct_sql_cannot_post_schedule_h_on_an_unsigned_entry() {
        let (f, professional) = schedule_h_fixture().await;
        let (prescription, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        prepare_as(&f, &id, OWNER).await;
        let record = entries(&f, &id).await.remove(0).id;
        // Forged forward, straight to finalized: refused.
        let error = sqlx::query(
            "UPDATE prescription_supply_records SET status='finalized',\
             finalized_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?",
        )
        .bind(&record)
        .execute(&f.pool)
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("CHECK") || error.contains("immutable"),
            "{error}"
        );
        // A hand-written dispensing and snapshot, then a hand-flipped posting: refused, because the
        // entry is only prepared.
        let line = first_line_id(&f, &id).await;
        sqlx::query(
            "UPDATE sale_lines SET regulatory_snapshot_version=1,regulatory_schemes_snapshot=\
             '{\"schedule_h\":\"applies\",\"schedule_h1\":\"does_not_apply\",\
             \"schedule_x\":\"does_not_apply\",\"schedule_c\":\"does_not_apply\",\
             \"schedule_c1\":\"does_not_apply\"}' WHERE id=?",
        )
        .bind(&line)
        .execute(&f.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO prescription_dispensings (id,store_id,prescription_id,prescription_item_id,\
             sale_document_id,sale_line_id,product_id,quantity_atoms,dispensed_on,\
             supervising_professional_id,supervising_professional_name,\
             supervising_registration_number,endorsement_confirmed_by_user_id,\
             endorsement_confirmed_at_utc,created_at_utc) VALUES (?,?,?,?,?,?,?,10,?,?,'Meera Iyer',\
             'MH-PH-44821',?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&f.store_id)
        .bind(&prescription)
        .bind(&item)
        .bind(&id)
        .bind(&line)
        .bind(&f.product_id)
        .bind(TODAY)
        .bind(&professional)
        .bind(&f.owner_id)
        .execute(&f.pool)
        .await
        .unwrap();
        let error = flip_to_posted(&f, &id).await;
        assert!(
            error.contains("prescription_supply_record_missing"),
            "{error}"
        );
        // Confirmed but never finalized: still refused.
        confirm_as(&f, &record, true, true, OWNER).await;
        let error = flip_to_posted(&f, &id).await;
        assert!(
            error.contains("prescription_supply_record_missing"),
            "{error}"
        );
        // Finalizing by hand with the line unlinked: refused.
        let error = sqlx::query(
            "UPDATE prescription_supply_records SET status='finalized',\
             finalized_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?",
        )
        .bind(&record)
        .execute(&f.pool)
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("prescription_supply_record_immutable"),
            "{error}"
        );
        assert_eq!(detail(&f, &id).await["status"], "draft");
    }

    /// S-09, S-10. A cancelled entry is voided, never deleted; its serial is kept and never reused;
    /// a void entry supports no posting; and a finalized entry cannot be voided.
    #[tokio::test]
    async fn s09_a_cancelled_serial_is_voided_kept_and_never_reused() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        prepare_as(&f, &id, OWNER).await;
        let first = entries(&f, &id).await.remove(0);
        confirm_as(&f, &first.id, true, true, OWNER).await;
        let (status, refused) = void_as(&f, &first.id, "  ", OWNER).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        let (status, voided) = void_as(&f, &first.id, "customer left before paying", OWNER).await;
        assert_eq!(status, StatusCode::OK, "{voided}");
        assert_eq!(voided["status"], "void");
        assert_eq!(voided["voidReason"], "customer left before paying");
        // The confirmation it had is kept: the serial may be on paper.
        assert_eq!(voided["manualSignatureConfirmed"], true);
        let (status, refused) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.prescription_record_not_prepared".to_owned()]
        );
        // The next entry takes the next serial; PR-000001 is never issued again.
        prepare_as(&f, &id, OWNER).await;
        let serials: Vec<String> = entries(&f, &id)
            .await
            .into_iter()
            .map(|entry| format!("{}:{}", entry.serial_number, entry.status))
            .collect();
        assert_eq!(serials, vec!["PR-000001:void", "PR-000002:prepared"]);
        for statement in [
            "UPDATE prescription_supply_records SET status='prepared',voided_by_user_id=NULL,\
             voided_at_utc=NULL,void_reason=NULL WHERE serial_value=1",
            "DELETE FROM prescription_supply_records WHERE serial_value=1",
        ] {
            assert!(
                sqlx::query(statement).execute(&f.pool).await.is_err(),
                "{statement}"
            );
        }
        let second = entries(&f, &id).await.remove(1);
        confirm_as(&f, &second.id, true, true, OWNER).await;
        let (status, posted) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (status, refused) = void_as(&f, &second.id, "too late", OWNER).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "prescription_record_state_conflict");
    }

    /// S-11. A posting that fails after the entry was confirmed leaves the entry confirmed — it may
    /// be signed on paper — and nothing sold; retried, it posts under the same serial.
    #[tokio::test]
    async fn s11_a_failure_after_confirmation_keeps_the_entry_truthful_and_recoverable() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let confirmed = prepare_and_confirm(&f, &id).await;
        assert_eq!(confirmed.len(), 1);
        sqlx::query(
            "CREATE TRIGGER test_fail_tender BEFORE INSERT ON sale_tenders \
             BEGIN SELECT RAISE(ABORT, 'injected_failure'); END",
        )
        .execute(&f.pool)
        .await
        .unwrap();
        let (status, failed) = post_confirmed(&f, &id).await;
        assert_ne!(status, StatusCode::OK, "{failed}");
        let entry = entries(&f, &id).await.remove(0);
        assert_eq!(
            (entry.serial_number.as_str(), entry.status.as_str()),
            ("PR-000001", "confirmed")
        );
        assert_nothing_sold(&f, &id).await;
        let unlinked: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM prescription_supply_record_lines WHERE dispensing_id IS NOT NULL",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(unlinked, 0);
        sqlx::query("DROP TRIGGER test_fail_tender")
            .execute(&f.pool)
            .await
            .unwrap();
        let (status, posted) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(
            posted["prescriptionRecords"][0]["serialNumber"],
            "PR-000001"
        );
        assert_eq!(posted["prescriptionRecords"][0]["status"], "finalized");
    }

    /// S-12. Two counters preparing at once get distinct serials; the same Sale prepared twice at
    /// once gets one entry.
    #[tokio::test]
    async fn s12_concurrent_preparation_never_shares_or_duplicates_a_serial() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, left_item) = simple_prescription(&f, 20).await;
        let (_, right_item) = simple_prescription(&f, 20).await;
        let left = prepared_sale(&f, &left_item, &professional, 1).await;
        let right = prepared_sale(&f, &right_item, &professional, 1).await;
        let (one, two) = tokio::join!(prepare_as(&f, &left, OWNER), prepare_as(&f, &right, OWNER));
        for (status, body) in [&one, &two] {
            assert!(
                *status == StatusCode::OK || body["code"] == "service_busy",
                "{body}"
            );
        }
        let (dup_one, dup_two) =
            tokio::join!(prepare_as(&f, &left, OWNER), prepare_as(&f, &left, OWNER));
        for (status, body) in [&dup_one, &dup_two] {
            assert!(
                *status == StatusCode::OK
                    || body["code"] == "service_busy"
                    || body["code"] == "revision_conflict",
                "{body}"
            );
        }
        let serials: Vec<i64> = sqlx::query_scalar(
            "SELECT serial_value FROM prescription_supply_records ORDER BY serial_value",
        )
        .fetch_all(&f.pool)
        .await
        .unwrap();
        let expected: Vec<i64> = (1..=serials.len() as i64).collect();
        assert_eq!(
            serials, expected,
            "serials must be distinct and consecutive"
        );
        let live_left: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM prescription_supply_records WHERE sale_document_id=? AND status<>'void'",
        )
        .bind(&left)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert!(live_left <= 1, "one Sale holds two live entries");
    }

    /// S-13. A pharmacist archived after the entry is confirmed stops the posting; the entry stays
    /// confirmed; the counter recovers by voiding it and preparing a new one under a valid pharmacist.
    #[tokio::test]
    async fn s13_a_pharmacist_archived_before_finalization_stops_the_posting() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let record = prepare_and_confirm(&f, &id).await.remove(0);
        sqlx::query(
            "UPDATE store_professionals SET status='archived',\
             archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='left' WHERE id=?",
        )
        .bind(&professional)
        .execute(&f.pool)
        .await
        .unwrap();
        let (status, refused) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.supervising_pharmacist_invalid".to_owned()]
        );
        assert_eq!(entries(&f, &id).await[0].status, "confirmed");
        assert_nothing_sold(&f, &id).await;
        // The Sale is held until the entry is voided.
        let (status, held) = set_supply(&f, &id, None, false, OWNER).await;
        assert_eq!(status, StatusCode::CONFLICT, "{held}");
        assert_eq!(held["code"], "prescription_record_prepared");
        void_as(
            &f,
            &record,
            "supervising pharmacist no longer on record",
            OWNER,
        )
        .await;
        let other = add_professional(
            &f,
            json!({
                "fullName": "Ravi Menon",
                "capacity": "registered_pharmacist",
                "registrationNumber": "MH-PH-10001",
            }),
        )
        .await;
        set_supply(&f, &id, Some(&other), true, OWNER).await;
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let serials: Vec<String> = entries(&f, &id)
            .await
            .into_iter()
            .map(|entry| format!("{}:{}", entry.serial_number, entry.status))
            .collect();
        assert_eq!(serials, vec!["PR-000001:void", "PR-000002:finalized"]);
    }

    /// S-14. An election that changes after the entry is confirmed makes it stale: the posting is
    /// refused rather than finalized under an election no longer in force.
    #[tokio::test]
    async fn s14_an_election_changed_before_finalization_makes_the_entry_stale() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        prepare_and_confirm(&f, &id).await;
        sqlx::query(
            "UPDATE store_record_elections SET status='archived',\
             archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='re-elected'",
        )
        .execute(&f.pool)
        .await
        .unwrap();
        elect(&f, "prescription_register", "2020-01-01", None).await;
        let (status, refused) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.prescription_record_stale".to_owned()]
        );
        assert_nothing_sold(&f, &id).await;
        // With no election at all, the refusal says so.
        sqlx::query(
            "UPDATE store_record_elections SET status='archived',\
             archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='withdrawn' \
             WHERE status='active'",
        )
        .execute(&f.pool)
        .await
        .unwrap();
        let (status, refused) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "prescription_record_election_unresolved");
    }

    /// S-15. A prescription archived, or consumed by another Sale, after the entry is confirmed stops
    /// the posting: the posting revalidates the authority for itself.
    #[tokio::test]
    async fn s15_a_prescription_unavailable_before_finalization_stops_the_posting() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let left = prepared_sale(&f, &item, &professional, 1).await;
        let right = prepared_sale(&f, &item, &professional, 1).await;
        prepare_and_confirm(&f, &left).await;
        prepare_and_confirm(&f, &right).await;
        let (status, posted) = post_confirmed(&f, &left).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (status, refused) = post_confirmed(&f, &right).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["lines.1.prescription_repeat_not_authorised".to_owned()]
        );
        assert_eq!(entries(&f, &right).await[0].status, "confirmed");

        let (other_prescription, other_item) = simple_prescription(&f, 20).await;
        let third = prepared_sale(&f, &other_item, &professional, 1).await;
        prepare_and_confirm(&f, &third).await;
        sqlx::query(
            "UPDATE prescriptions SET status='archived',\
             archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='withdrawn' WHERE id=?",
        )
        .bind(&other_prescription)
        .execute(&f.pool)
        .await
        .unwrap();
        let (status, refused) = post_confirmed(&f, &third).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["lines.1.prescription_archived".to_owned()]
        );
    }

    /// Phase 1M-D1-A, §26. Receipt provenance changed nothing about supplying a Schedule X drug:
    /// a line the owner has placed in Schedule X still meets the unsupported-workflow boundary,
    /// with nothing sold and no register implied. The receipt side may now record where stock came
    /// from; that is a record of a purchase, not an authority to dispense.
    #[tokio::test]
    async fn d1a_schedule_x_remains_unsupported_after_purchase_provenance() {
        let f = fixture().await;
        schedule_product(&f, &f.product_id, &["schedule_x"]).await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        let (status, refused) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "schedule_x_workflow_not_available");
        assert_nothing_sold(&f, &id).await;
        let text = refused.to_string().to_lowercase();
        for word in ["banned", "prohibit", "illegal"] {
            assert!(!text.contains(word), "{word}: {refused}");
        }
    }

    /// S-16, S-17, S-18. Schedule H1, Schedule X and Schedule H with C cannot even be prepared:
    /// preparing an entry is never a way round the scheme gate.
    #[tokio::test]
    async fn s16_h1_x_and_c_cannot_be_prepared_or_posted() {
        for (inside, code) in [
            (
                &["schedule_h", "schedule_h1"][..],
                "schedule_h1_ndps_workflow_not_available",
            ),
            (&["schedule_x"][..], "schedule_x_workflow_not_available"),
            (
                &["schedule_h", "schedule_c"][..],
                "regulated_sale_workflow_not_available",
            ),
            (
                &["schedule_h", "schedule_c1"][..],
                "regulated_sale_workflow_not_available",
            ),
        ] {
            let f = fixture().await;
            schedule_product(&f, &f.product_id, inside).await;
            record_basis(&f, &f.product_id, Some("prescription_register")).await;
            let professional = pharmacist(&f).await;
            let (_, item) = simple_prescription(&f, 20).await;
            let id = prepared_sale(&f, &item, &professional, 1).await;
            let (status, refused) = prepare_as(&f, &id, OWNER).await;
            assert_eq!(status, StatusCode::CONFLICT, "{inside:?}: {refused}");
            assert_eq!(refused["code"], code, "{inside:?}");
            let (status, refused) = post_confirmed(&f, &id).await;
            assert_eq!(status, StatusCode::CONFLICT, "{inside:?}: {refused}");
            assert_eq!(refused["code"], code, "{inside:?}");
            assert_eq!(entry_count(&f).await, 0, "{inside:?}");
        }
    }

    /// S-19. An ordinary Sale is untouched while another Sale's entry awaits its signature, and
    /// there is no entry to prepare for it.
    #[tokio::test]
    async fn s19_an_ordinary_sale_is_not_held_by_another_sales_entry() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let controlled = prepared_sale(&f, &item, &professional, 1).await;
        prepare_as(&f, &controlled, OWNER).await;
        let (general, pack, batch) = second_product(&f, "Cotton roll").await;
        let ordinary = open_sale(&f, draft_body(None)).await;
        add_line(&f, &ordinary, &general, &pack, &batch, 1, 8000).await;
        let (status, refused) = prepare_as(&f, &ordinary, OWNER).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        let (status, posted) = post_confirmed(&f, &ordinary).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["prescriptionRecords"], json!([]));
        assert_eq!(entries(&f, &controlled).await[0].status, "prepared");
    }

    /// S-20. The software never claims a signature: a prepared entry says it is unsigned, a
    /// confirmed one names the person who confirmed the manual acts, and no response says an entry
    /// was signed by AUSHADHARTH or electronically.
    #[tokio::test]
    async fn s20_no_response_claims_a_software_signature() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        prepare_as(&f, &id, OWNER).await;
        let record = entries(&f, &id).await.remove(0).id;
        let (_, prepared) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/prescription-supply-records/{record}"),
            Value::Null,
        )
        .await;
        assert_eq!(prepared["manualSignatureConfirmed"], false);
        assert_eq!(prepared["confirmedByDisplayName"], Value::Null);
        assert_eq!(prepared["supervisingProfessionalName"], "Meera Iyer");
        let (_, confirmed) = confirm_as(&f, &record, true, true, OWNER).await;
        assert_eq!(confirmed["confirmedByDisplayName"], "owner_admin sale user");
        let (_, posted) = post_confirmed(&f, &id).await;
        for body in [
            prepared.to_string(),
            confirmed.to_string(),
            posted.to_string(),
        ] {
            let lower = body.to_lowercase();
            for claim in ["signed by", "digitally", "electronically", "e-sign"] {
                assert!(!lower.contains(claim), "a response claims {claim}: {body}");
            }
        }
    }

    /// S-21. A Sale with a live entry is held as signed for: lines, links, supply and header are all
    /// refused until the entry is voided.
    #[tokio::test]
    async fn s21_a_sale_with_a_live_entry_cannot_be_changed_until_it_is_voided() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        prepare_as(&f, &id, OWNER).await;
        let line = first_line_id(&f, &id).await;
        let revision = revision_of(&f, &id).await;
        for (method, uri, body) in [
            (
                "POST",
                format!("/api/v1/sales/{id}/lines"),
                line_body(revision, &f, "pack", 1, 8000),
            ),
            (
                "DELETE",
                format!("/api/v1/sale-lines/{line}"),
                json!({ "expectedRevision": revision }),
            ),
            (
                "PUT",
                format!("/api/v1/sale-lines/{line}/prescription"),
                json!({ "expectedRevision": revision, "prescriptionItemId": Value::Null }),
            ),
            (
                "PUT",
                format!("/api/v1/sales/{id}"),
                json!({ "expectedRevision": revision, "customerPartyId": Value::Null, "businessDate": TODAY }),
            ),
        ] {
            let (status, refused) = request(f.pool.clone(), method, &uri, body).await;
            assert_eq!(status, StatusCode::CONFLICT, "{method} {uri}: {refused}");
            assert_eq!(refused["code"], "prescription_record_prepared");
        }
        let record = entries(&f, &id).await.remove(0).id;
        void_as(&f, &record, "wrong quantity prepared", OWNER).await;
        let (status, body) = link_line(&f, &line, &id, None, OWNER).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// S-22. Three identities, kept apart: the pharmacist who prepared and confirmed, the registered
    /// pharmacist named in (g), and the cashier who operated the till.
    #[tokio::test]
    async fn s22_operator_supervisor_and_confirmer_stay_distinct() {
        let (f, professional) = schedule_h_fixture().await;
        let pharmacist_user = insert_session(&f.pool, "pharmacist", PHARMACIST_TOKEN).await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let (status, _) = prepare_as(&f, &id, PHARMACIST_TOKEN).await;
        assert_eq!(status, StatusCode::OK);
        let record = entries(&f, &id).await.remove(0).id;
        let (status, _) = confirm_as(&f, &record, true, true, PHARMACIST_TOKEN).await;
        assert_eq!(status, StatusCode::OK);
        let revision = revision_of(&f, &id).await;
        let (status, posted) = request_as(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/post"),
            json!({
                "expectedRevision": revision,
                "idempotencyKey": Uuid::now_v7().to_string(),
                "tenders": [{ "method": "cash", "amountPaise": 8960 }],
            }),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let entry = entries(&f, &id).await.remove(0);
        assert_eq!(entry.prepared_by_user_id, pharmacist_user);
        assert_eq!(
            entry.confirmed_by_user_id.as_deref(),
            Some(pharmacist_user.as_str())
        );
        let (poster, supervisor): (String, String) = sqlx::query_as(
            "SELECT document.posted_by_user_id,record.supervising_professional_name \
             FROM sale_documents document JOIN prescription_supply_records record \
             ON record.sale_document_id=document.id WHERE document.id=?",
        )
        .bind(&id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_ne!(poster, pharmacist_user);
        assert_eq!(supervisor, "Meera Iyer");
    }

    /// S-23. The book is the election's: none recorded, nothing prepared; the memo book only with
    /// its attestation, in its own PM series.
    #[tokio::test]
    async fn s23_the_book_follows_the_election_and_its_proviso() {
        let f = fixture().await;
        schedule_product(&f, &f.product_id, &["schedule_h"]).await;
        record_basis(&f, &f.product_id, None).await;
        let professional = pharmacist(&f).await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let (status, refused) = prepare_as(&f, &id, OWNER).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "prescription_record_election_unresolved");
        elect(&f, "cash_or_credit_memo_book", "2020-01-01", None).await;
        let (status, refused) = prepare_as(&f, &id, OWNER).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.prescription_memo_path_ineligible".to_owned()]
        );
        let revision = revision_of(&f, &id).await;
        request(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/sales/{id}/supply"),
            json!({
                "expectedRevision": revision,
                "supervisingProfessionalId": professional,
                "prescriptionEndorsementConfirmed": true,
                "prescriptionOriginalContainerConfirmed": true,
            }),
        )
        .await;
        let (status, prepared) = prepare_as(&f, &id, OWNER).await;
        assert_eq!(status, StatusCode::OK, "{prepared}");
        assert_eq!(
            prepared["prescriptionRecords"][0]["serialNumber"],
            "PM-000001"
        );
        assert_eq!(
            prepared["prescriptionRecords"][0]["recordMethod"],
            "cash_or_credit_memo_book"
        );
        // The attestation is not a signature: the memo entry still needs both manual acts.
        let (status, refused) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.prescription_record_not_confirmed".to_owned()]
        );
    }

    /// S-24. A return leaves the finalized entry exactly as it was.
    #[tokio::test]
    async fn s24_a_return_never_rewrites_the_entry() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 2).await;
        let (status, _) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK);
        let before = entries(&f, &id).await;
        let (status, returned) = return_first_line(&f, &id, 1).await;
        assert_eq!(status, StatusCode::OK, "{returned}");
        assert_eq!(entries(&f, &id).await, before);
        let (_, detail) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/prescription-supply-records/{}", before[0].id),
            Value::Null,
        )
        .await;
        assert_eq!(detail["lines"][0]["quantityAtoms"], 20);
        assert_eq!(detail["lines"][0]["returnedAtoms"], 10);
    }

    /// S-25. A repeat supply is a new entry with its own serial, referring to the finalized entry
    /// of the previous supply.
    #[tokio::test]
    async fn s25_a_repeat_supply_refers_to_the_previous_finalized_entry() {
        let (f, professional) = schedule_h_fixture().await;
        let created = enter_prescription(
            &f,
            prescription_body(
                &f.product_id,
                40,
                json!({ "repeatAuthority": "stated_times", "repeatTimes": 2 }),
            ),
        )
        .await;
        let item = created["items"][0]["id"].as_str().unwrap().to_owned();
        let first = prepared_sale(&f, &item, &professional, 1).await;
        post_as_quoted(&f, &first).await;
        let second = prepared_sale(&f, &item, &professional, 1).await;
        let (status, _) = post_as_quoted(&f, &second).await;
        assert_eq!(status, StatusCode::OK);
        let earlier = entries(&f, &first).await.remove(0);
        let later = entries(&f, &second).await.remove(0);
        assert_eq!(later.serial_number, "PR-000002");
        assert_eq!(
            later.previous_record_id.as_deref(),
            Some(earlier.id.as_str())
        );
    }

    /// S-26. A Schedule H product with no manufacturer on record cannot be entered truthfully
    /// (rule 65(3)(1)(f)), so nothing is prepared.
    #[tokio::test]
    async fn s26_no_entry_is_prepared_without_a_manufacturer() {
        let f = fixture().await;
        schedule_product(&f, &f.product_id, &["schedule_h"]).await;
        elect(&f, "prescription_register", "2020-01-01", None).await;
        let professional = pharmacist(&f).await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let (status, refused) = prepare_as(&f, &id, OWNER).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["lines.1.manufacturer_not_recorded".to_owned()]
        );
        assert_eq!(entry_count(&f).await, 0);
    }

    /// S-27. A catalogue change after confirmation makes the entry stale: the Sale is not posted
    /// against particulars the pharmacist did not sign.
    #[tokio::test]
    async fn s27_particulars_changed_after_signature_make_the_entry_stale() {
        let (f, professional) = schedule_h_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let record = prepare_and_confirm(&f, &id).await.remove(0);
        sqlx::query("UPDATE product_batches SET expires_on='2027-06-30' WHERE id=?")
            .bind(&f.batch_id)
            .execute(&f.pool)
            .await
            .unwrap();
        let (status, refused) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.prescription_record_stale".to_owned()]
        );
        assert_nothing_sold(&f, &id).await;
        void_as(&f, &record, "batch expiry corrected after signing", OWNER).await;
        let (status, posted) = post_as_quoted(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(
            posted["prescriptionRecords"][1]["serialNumber"],
            "PR-000002"
        );
    }

    // -----------------------------------------------------------------------------------------
    // Phase 1M-C — the Schedule H1 working record and the Punjab unsupported-workflow boundary
    //
    // Two layers for every H1 supply: the 1M-B rule 65(3)(1) entry, and the separate H1 working
    // entry whose hard copy is placed in the pharmacy's physical H1 register and authenticated by
    // the registered pharmacist (the 48th DCC recommendation, followed conservatively). Each proof
    // names the C-numbers of the approved test plan it covers.
    // -----------------------------------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
    struct H1State {
        id: String,
        reference: String,
        status: String,
        sale_line_id: String,
        supply_record_id: String,
        patient_name: String,
        prescriber_name: String,
        prescriber_address: String,
        drug_name: String,
        quantity_atoms: i64,
        hard_copy_placed_in_register: i64,
        pharmacist_authenticated_hard_copy: i64,
        confirmed_by_user_id: Option<String>,
        dispensing_id: Option<String>,
        void_reason: Option<String>,
    }

    async fn h1_entries(f: &Fixture, sale: &str) -> Vec<H1State> {
        sqlx::query_as(
            "SELECT id,reference,status,sale_line_id,supply_record_id,patient_name,prescriber_name,\
             prescriber_address,drug_name,quantity_atoms,hard_copy_placed_in_register,\
             pharmacist_authenticated_hard_copy,confirmed_by_user_id,dispensing_id,void_reason \
             FROM prescription_h1_register_entries WHERE sale_document_id=? ORDER BY reference_value",
        )
        .bind(sale)
        .fetch_all(&f.pool)
        .await
        .unwrap()
    }

    /// Records one finding straight into the classification table, as `schedule_product` does.
    async fn add_finding(f: &Fixture, product_id: &str, scheme: &str, applies: bool, from: &str) {
        sqlx::query(
            "INSERT INTO product_regulatory_classifications (id,product_id,scheme,applies,\
             effective_from,source_citation,determined_by_user_id,revision,status,\
             created_at_utc,updated_at_utc) VALUES (?,?,?,?,?,'Owner-recorded finding',?,1,'active',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(product_id)
        .bind(scheme)
        .bind(i64::from(applies))
        .bind(from)
        .bind(&f.owner_id)
        .execute(&f.pool)
        .await
        .unwrap();
    }

    /// Schedule H1 alone (not Schedule H), NDPS purview established not to apply, the rule
    /// 65(3)(1) basis recorded, and a registered pharmacist on record.
    async fn h1_fixture() -> (Fixture, String) {
        let f = fixture().await;
        schedule_product(&f, &f.product_id, &["schedule_h1"]).await;
        add_finding(&f, &f.product_id, "ndps_purview", false, "2020-01-01").await;
        record_basis(&f, &f.product_id, Some("prescription_register")).await;
        let professional = pharmacist(&f).await;
        (f, professional)
    }

    async fn confirm_h1(
        f: &Fixture,
        sale: &str,
        placed: bool,
        authenticated: bool,
        token: &str,
    ) -> (StatusCode, Value) {
        request_as(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{sale}/h1-register/confirm"),
            json!({
                "hardCopyPlacedInRegister": placed,
                "pharmacistAuthenticatedHardCopy": authenticated,
            }),
            Some(token),
        )
        .await
    }

    async fn confirm_records(f: &Fixture, sale: &str) {
        for entry in entries(f, sale).await {
            if entry.status == "prepared" {
                let (status, body) = confirm_as(f, &entry.id, true, true, OWNER).await;
                assert_eq!(status, StatusCode::OK, "{body}");
            }
        }
    }

    async fn move_store_to(f: &Fixture, state_id: Option<&str>) {
        sqlx::query("UPDATE store_addresses SET state_id=? WHERE store_id=?")
            .bind(state_id)
            .bind(&f.store_id)
            .execute(&f.pool)
            .await
            .unwrap();
    }

    async fn today_ist(f: &Fixture, offset_days: i64) -> String {
        sqlx::query_scalar(&format!(
            "SELECT strftime('%Y-%m-%d','now','+5 hours','+30 minutes','{offset_days:+} days')"
        ))
        .fetch_one(&f.pool)
        .await
        .unwrap()
    }

    /// C-68, C-01, C-02, C-58, C-59, C-60, C-11, C-26, C-27. The supported ordinary H1 supply: an
    /// H1-only product needs the prescription layer and the separate H1 layer; neither the
    /// electronic working entry nor either physical act alone completes it; a cashier can neither
    /// prepare nor confirm; a pharmacist confirms both acts; the cashier then posts; both layers
    /// finalize with the Sale; stock, dispensing, number and tender are each written exactly once.
    #[tokio::test]
    async fn c68_a_supported_h1_supply_needs_both_layers_and_completes() {
        let (f, professional) = h1_fixture().await;
        let pharmacist_user = insert_session(&f.pool, "pharmacist", PHARMACIST_TOKEN).await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;

        let quote = quote_of(&f, &id).await;
        assert_eq!(quote["lines"][0]["regulatoryGate"], "prescription_required");
        assert_eq!(quote["lines"][0]["regulatoryGateScheme"], "schedule_h1");
        assert_eq!(
            quote["supply"]["issues"],
            json!([
                "prescription_record_not_prepared",
                "schedule_h1_register_not_prepared"
            ])
        );

        // A cashier cannot prepare either layer.
        let (status, _) = prepare_as(&f, &id, CASHIER).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, prepared) = prepare_as(&f, &id, OWNER).await;
        assert_eq!(status, StatusCode::OK, "{prepared}");
        assert_eq!(prepared["status"], "draft");
        assert_eq!(prepared["h1RegisterEntries"][0]["reference"], "AH1-000001");
        assert_eq!(prepared["h1RegisterEntries"][0]["status"], "prepared");
        assert_nothing_sold(&f, &id).await;

        // The first layer confirmed alone is not enough: the electronic H1 entry is prepared but
        // its physical acts are not confirmed.
        confirm_records(&f, &id).await;
        let (status, refused) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.schedule_h1_register_not_confirmed".to_owned()]
        );
        assert_nothing_sold(&f, &id).await;

        // Neither physical act alone confirms; a cashier cannot confirm at all.
        let (status, _) = confirm_h1(&f, &id, true, true, CASHIER).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, refused) = confirm_h1(&f, &id, true, false, OWNER).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        assert_eq!(
            refused["issues"][0]["field"],
            "pharmacistAuthenticatedHardCopy"
        );
        let (status, refused) = confirm_h1(&f, &id, false, true, OWNER).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        assert_eq!(refused["issues"][0]["field"], "hardCopyPlacedInRegister");
        assert_eq!(h1_entries(&f, &id).await[0].status, "prepared");

        let (status, sheet) = confirm_h1(&f, &id, true, true, PHARMACIST_TOKEN).await;
        assert_eq!(status, StatusCode::OK, "{sheet}");
        assert_eq!(sheet["entries"][0]["status"], "confirmed");
        assert_nothing_sold(&f, &id).await;

        // The cashier posts the confirmed supply.
        let revision = revision_of(&f, &id).await;
        let (status, posted) = request_as(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/post"),
            json!({
                "expectedRevision": revision,
                "idempotencyKey": Uuid::now_v7().to_string(),
                "tenders": [{ "method": "cash", "amountPaise": 8960 }],
            }),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["prescriptionRecords"][0]["status"], "finalized");
        assert_eq!(posted["h1RegisterEntries"][0]["status"], "finalized");
        let entry = h1_entries(&f, &id).await.remove(0);
        assert_eq!(
            entry.confirmed_by_user_id.as_deref(),
            Some(pharmacist_user.as_str())
        );
        let dispensing: String =
            sqlx::query_scalar("SELECT id FROM prescription_dispensings WHERE sale_document_id=?")
                .bind(&id)
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(entry.dispensing_id.as_deref(), Some(dispensing.as_str()));
        assert_eq!(dispensing_count(&f).await, 1);
        assert_eq!(balance(&f, &f.batch_id).await, 90);
        assert_eq!(numbers_issued(&f).await, 1);
        assert_eq!(tender_rows(&f, &id).await, 1);
    }

    /// C-07, C-08, C-09, C-10. The entry carries exactly the clause (h) particulars — the
    /// prescriber's name and address, the patient's NAME (no patient address is stored), the
    /// drug and the quantity — frozen from the prescription and the line. Its identifier is an
    /// internal AUSHADHARTH Reference, never presented as a register serial. A blank prescriber
    /// address cannot be recorded.
    #[tokio::test]
    async fn c07_the_entry_holds_exactly_the_clause_h_particulars() {
        let (f, professional) = h1_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        prepare_as(&f, &id, OWNER).await;
        let entry = h1_entries(&f, &id).await.remove(0);
        assert_eq!(entry.prescriber_name, "Dr. Anjali Rao");
        assert_eq!(entry.prescriber_address, "Rao Clinic, FC Road, Pune 411005");
        assert_eq!(entry.patient_name, PATIENT_NAME);
        assert_eq!(entry.quantity_atoms, 10);
        assert_eq!(entry.reference, "AH1-000001");
        let columns: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info('prescription_h1_register_entries')",
        )
        .fetch_all(&f.pool)
        .await
        .unwrap();
        for absent in [
            "patient_address",
            "subject_address",
            "serial_number",
            "page_number",
        ] {
            assert!(!columns.iter().any(|column| column == absent), "{absent}");
        }
        let (status, sheet) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/sales/{id}/h1-register"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{sheet}");
        let text = sheet.to_string();
        assert!(
            !text.contains(PATIENT_ADDRESS),
            "the hard copy carries the patient's address"
        );
        assert!(!text.contains("registerSerial") && !text.contains("pageNumber"));
        assert_eq!(sheet["entries"][0]["reference"], "AH1-000001");
        // The database will not hold a blank prescriber address.
        let error = sqlx::query(
            "UPDATE prescription_h1_register_entries SET prescriber_address='' WHERE id=?",
        )
        .bind(&entry.id)
        .execute(&f.pool)
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("CHECK") || error.contains("prescription_h1_register_entry_immutable"),
            "{error}"
        );
    }

    /// C-12, C-50, C-51, C-52. Direct SQL cannot forge an H1 supply: no entry without its rule
    /// 65(3)(1) entry, no entry across Sales, no hand-finalized entry, no edited or deleted
    /// particular, and no posting with the H1 entry left unfinalized even when every other layer
    /// was completed by hand.
    #[tokio::test]
    async fn c50_direct_sql_cannot_forge_an_h1_supply() {
        let (f, professional) = h1_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let other = prepared_sale(&f, &item, &professional, 1).await;
        prepare_as(&f, &id, OWNER).await;
        let entry = h1_entries(&f, &id).await.remove(0);
        let other_line = first_line_id(&f, &other).await;

        // An entry whose line belongs to another Sale, or with no rule 65(3)(1) entry beside it.
        for (sale, line, record) in [
            (
                id.as_str(),
                other_line.as_str(),
                entry.supply_record_id.as_str(),
            ),
            (
                other.as_str(),
                other_line.as_str(),
                entry.supply_record_id.as_str(),
            ),
        ] {
            let error = sqlx::query(
                "INSERT INTO prescription_h1_register_entries (id,store_id,sale_document_id,\
                 sale_line_id,supply_record_id,prescription_id,prescription_item_id,product_id,\
                 reference_value,reference,date_of_supply,prescriber_name,prescriber_address,\
                 patient_name,drug_name,quantity_atoms,supervising_professional_id,\
                 supervising_professional_name,supervising_registration_number,\
                 prepared_by_user_id,prepared_at_utc) \
                 SELECT ?,store_id,?,?,?,prescription_id,prescription_item_id,product_id,2,\
                 'AH1-000002',date_of_supply,prescriber_name,prescriber_address,patient_name,\
                 drug_name,quantity_atoms,supervising_professional_id,\
                 supervising_professional_name,supervising_registration_number,\
                 prepared_by_user_id,prepared_at_utc FROM prescription_h1_register_entries WHERE id=?",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(sale)
            .bind(line)
            .bind(record)
            .bind(&entry.id)
            .execute(&f.pool)
            .await
            .unwrap_err()
            .to_string();
            assert!(
                error.contains("prescription_h1_register_entry_incoherent"),
                "{error}"
            );
        }

        // Forged transitions and edits.
        for statement in [
            "UPDATE prescription_h1_register_entries SET status='finalized',\
             finalized_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now')",
            "UPDATE prescription_h1_register_entries SET patient_name='Someone Else'",
            "UPDATE prescription_h1_register_entries SET quantity_atoms=20",
            "UPDATE prescription_h1_register_entries SET status='confirmed'",
            "DELETE FROM prescription_h1_register_entries",
        ] {
            assert!(
                sqlx::query(statement).execute(&f.pool).await.is_err(),
                "{statement}"
            );
        }
        // A cashier's id cannot be written in as the confirmer.
        let cashier: String = sqlx::query_scalar("SELECT id FROM users WHERE role='cashier'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        let error = sqlx::query(
            "UPDATE prescription_h1_register_entries SET status='confirmed',\
             hard_copy_placed_in_register=1,pharmacist_authenticated_hard_copy=1,\
             confirmed_by_user_id=?,confirmed_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?",
        )
        .bind(&cashier)
        .bind(&entry.id)
        .execute(&f.pool)
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("prescription_h1_register_entry_immutable"),
            "{error}"
        );

        // Every other layer completed by hand; the H1 entry confirmed but left unfinalized.
        confirm_records(&f, &id).await;
        confirm_h1(&f, &id, true, true, OWNER).await;
        let line = first_line_id(&f, &id).await;
        let (prescription, record): (String, String) = sqlx::query_as(
            "SELECT prescription_id,supply_record_id FROM prescription_h1_register_entries WHERE id=?",
        )
        .bind(&entry.id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        // Every snapshot the posting would freeze, written by hand from the prescription entry, so
        // the 1M-B link and the State gate are satisfied and only the H1 guard is left to refuse.
        sqlx::query(
            "UPDATE sale_lines SET regulatory_snapshot_version=1,regulatory_schemes_snapshot=\
             '{\"schedule_h\":\"does_not_apply\",\"schedule_h1\":\"applies\",\
             \"schedule_x\":\"does_not_apply\",\"schedule_c\":\"does_not_apply\",\
             \"schedule_c1\":\"does_not_apply\",\"ndps_purview\":\"does_not_apply\"}',\
             regulatory_state_snapshot=\
             '{\"premises_state_code\":\"27\",\"punjab_restricted_supply\":\"not_applicable\"}',\
             product_display_name=(SELECT drug_name FROM prescription_supply_record_lines \
                 WHERE sale_line_id=sale_lines.id),\
             manufacturer_name=(SELECT manufacturer_name FROM prescription_supply_record_lines \
                 WHERE sale_line_id=sale_lines.id),\
             manufacturer_company_id=(SELECT company_id FROM product_company_roles \
                 WHERE product_id=sale_lines.product_id AND role='manufacturer' LIMIT 1),\
             batch_number=(SELECT batch_number FROM prescription_supply_record_lines \
                 WHERE sale_line_id=sale_lines.id),\
             batch_expires_on=(SELECT batch_expires_on FROM prescription_supply_record_lines \
                 WHERE sale_line_id=sale_lines.id),\
             base_unit_label=(SELECT quantity_unit_label FROM prescription_supply_record_lines \
                 WHERE sale_line_id=sale_lines.id) WHERE id=?",
        )
        .bind(&line)
        .execute(&f.pool)
        .await
        .unwrap();
        let dispensing = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO prescription_dispensings (id,store_id,prescription_id,prescription_item_id,\
             sale_document_id,sale_line_id,product_id,quantity_atoms,dispensed_on,\
             supervising_professional_id,supervising_professional_name,\
             supervising_registration_number,endorsement_confirmed_by_user_id,\
             endorsement_confirmed_at_utc,created_at_utc) VALUES (?,?,?,?,?,?,?,10,?,?,'Meera Iyer',\
             'MH-PH-44821',?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&dispensing)
        .bind(&f.store_id)
        .bind(&prescription)
        .bind(&item)
        .bind(&id)
        .bind(&line)
        .bind(&f.product_id)
        .bind(TODAY)
        .bind(&professional)
        .bind(&f.owner_id)
        .execute(&f.pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE prescription_supply_record_lines SET dispensing_id=? WHERE record_id=?",
        )
        .bind(&dispensing)
        .bind(&record)
        .execute(&f.pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE prescription_supply_records SET status='finalized',\
             finalized_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?",
        )
        .bind(&record)
        .execute(&f.pool)
        .await
        .unwrap();
        let error = flip_to_posted(&f, &id).await;
        assert!(
            error.contains("prescription_h1_register_entry_missing"),
            "{error}"
        );
        assert_eq!(detail(&f, &id).await["status"], "draft");
    }

    /// C-16, C-17, C-18, C-21. The prepared entry sells nothing; posting waits for BOTH layers, in
    /// either order; voiding the rule 65(3)(1) entry voids the H1 entry beside it, keeping both
    /// with the reason, and the next preparation takes the next reference; a finalized entry
    /// can never be voided.
    #[tokio::test]
    async fn c18_voiding_keeps_both_layers_and_never_reuses_a_reference() {
        let (f, professional) = h1_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        prepare_as(&f, &id, OWNER).await;
        assert_nothing_sold(&f, &id).await;
        // H1 confirmed first, the prescription entry not: still refused.
        let (status, _) = confirm_h1(&f, &id, true, true, OWNER).await;
        assert_eq!(status, StatusCode::OK);
        let (status, refused) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.prescription_record_not_confirmed".to_owned()]
        );
        let record = entries(&f, &id).await.remove(0).id;
        let (status, voided) = void_as(&f, &record, "customer left before paying", OWNER).await;
        assert_eq!(status, StatusCode::OK, "{voided}");
        let voided = h1_entries(&f, &id).await.remove(0);
        assert_eq!(voided.status, "void");
        assert_eq!(
            voided.void_reason.as_deref(),
            Some("customer left before paying")
        );
        // The H1 entry records that its hard copy was placed: the confirmation stays with it.
        assert_eq!(voided.hard_copy_placed_in_register, 1);

        prepare_as(&f, &id, OWNER).await;
        let references: Vec<String> = h1_entries(&f, &id)
            .await
            .into_iter()
            .map(|entry| format!("{}:{}", entry.reference, entry.status))
            .collect();
        assert_eq!(references, vec!["AH1-000001:void", "AH1-000002:prepared"]);
        prepare_and_confirm(&f, &id).await;
        let (status, posted) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        for statement in [
            "UPDATE prescription_h1_register_entries SET status='void',\
             voided_by_user_id=confirmed_by_user_id,\
             voided_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),void_reason='too late' \
             WHERE status='finalized'",
            "DELETE FROM prescription_h1_register_entries WHERE status='void'",
        ] {
            assert!(
                sqlx::query(statement).execute(&f.pool).await.is_err(),
                "{statement}"
            );
        }
    }

    /// C-19, C-20. A posting that fails after both layers were confirmed leaves both confirmed —
    /// the hard copy may already be in the register — and nothing sold; the retry finalizes both.
    #[tokio::test]
    async fn c19_a_failed_posting_keeps_both_layers_confirmed_and_the_retry_finalizes() {
        let (f, professional) = h1_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        prepare_and_confirm(&f, &id).await;
        assert_eq!(h1_entries(&f, &id).await[0].status, "confirmed");
        sqlx::query(
            "CREATE TRIGGER test_fail_tender BEFORE INSERT ON sale_tenders \
             BEGIN SELECT RAISE(ABORT, 'injected_failure'); END",
        )
        .execute(&f.pool)
        .await
        .unwrap();
        let (status, failed) = post_confirmed(&f, &id).await;
        assert_ne!(status, StatusCode::OK, "{failed}");
        assert_nothing_sold(&f, &id).await;
        let entry = h1_entries(&f, &id).await.remove(0);
        assert_eq!(
            (entry.status.as_str(), entry.dispensing_id.as_deref()),
            ("confirmed", None)
        );
        assert_eq!(entries(&f, &id).await[0].status, "confirmed");
        sqlx::query("DROP TRIGGER test_fail_tender")
            .execute(&f.pool)
            .await
            .unwrap();
        let (status, posted) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["h1RegisterEntries"][0]["reference"], "AH1-000001");
        assert_eq!(posted["h1RegisterEntries"][0]["status"], "finalized");
    }

    /// C-22, C-23, C-24, C-25. The patient never leaves the dispensing roles: the quote, the Sale
    /// detail, refusals and audit payloads carry no patient or prescriber particular; the register
    /// is read by date and reference only; a cashier cannot read the register or a Sale's hard copy
    /// or append to it, directly by API.
    #[tokio::test]
    async fn c25_the_register_stays_with_the_dispensing_roles() {
        let (f, professional) = h1_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        prepare_and_confirm(&f, &id).await;
        let (status, posted) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let entry = h1_entries(&f, &id).await.remove(0);
        let quote = quote_of(&f, &id).await;
        let sale_detail = detail(&f, &id).await;
        let audit: Vec<String> = sqlx::query_scalar(
            "SELECT change_payload FROM master_change_events \
             WHERE entity_type IN ('prescription_h1_register_entry','prescription_h1_register_annotation')",
        )
        .fetch_all(&f.pool)
        .await
        .unwrap();
        assert!(!audit.is_empty());
        let mut texts = vec![
            quote.to_string(),
            sale_detail.to_string(),
            posted.to_string(),
        ];
        texts.extend(audit);
        for text in texts {
            for private in [
                PATIENT_NAME,
                PATIENT_ADDRESS,
                "Dr. Anjali Rao",
                "Rao Clinic",
            ] {
                assert!(!text.contains(private), "{private} leaked: {text}");
            }
        }
        for (method, uri, body) in [
            (
                "GET",
                "/api/v1/h1-register?from=2026-06-01&to=2026-06-30".to_owned(),
                Value::Null,
            ),
            (
                "GET",
                format!("/api/v1/sales/{id}/h1-register"),
                Value::Null,
            ),
            (
                "POST",
                format!("/api/v1/h1-register/{}/annotations", entry.id),
                json!({ "note": "cashier note" }),
            ),
            (
                "POST",
                format!("/api/v1/sales/{id}/h1-register/confirm"),
                json!({ "hardCopyPlacedInRegister": true, "pharmacistAuthenticatedHardCopy": true }),
            ),
        ] {
            let (status, denied) =
                request_as(f.pool.clone(), method, &uri, body, Some(CASHIER)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {denied}");
            assert!(!denied.to_string().contains(PATIENT_NAME));
        }
        // The dispensing roles read it by date.
        let (status, register) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/h1-register?from={TODAY}&to={TODAY}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{register}");
        assert_eq!(register["entries"][0]["patientName"], PATIENT_NAME);
        assert_eq!(
            register["store"]["legalName"],
            "Care Pharmacy Private Limited"
        );
        let (status, empty) = request(
            f.pool.clone(),
            "GET",
            "/api/v1/h1-register?from=2026-07-01&to=2026-07-31",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(empty["entries"], json!([]));
    }

    /// C-28. A login's role is not a registration: the H1 entry names a registered pharmacist from
    /// the professional record, and the database refuses one who is not.
    #[tokio::test]
    async fn c28_the_responsible_pharmacist_is_a_registration_not_a_login() {
        let (f, professional) = h1_fixture().await;
        let competent = add_professional(
            &f,
            json!({ "fullName": "Store Manager", "capacity": "competent_person" }),
        )
        .await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &competent, 1).await;
        let (status, refused) = prepare_as(&f, &id, OWNER).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.supervising_pharmacist_invalid".to_owned()]
        );
        assert!(h1_entries(&f, &id).await.is_empty());
        // Archived after preparation, before confirmation: the database refuses the confirmation.
        let good = prepared_sale(&f, &item, &professional, 1).await;
        prepare_as(&f, &good, OWNER).await;
        sqlx::query(
            "UPDATE store_professionals SET status='archived',\
             archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='left' WHERE id=?",
        )
        .bind(&professional)
        .execute(&f.pool)
        .await
        .unwrap();
        let (status, refused) = confirm_h1(&f, &good, true, true, OWNER).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "prescription_record_state_conflict");
        assert_eq!(h1_entries(&f, &good).await[0].status, "prepared");
    }

    /// C-29, C-49, C-46, C-47, C-48. After supply the entry never changes. A return is linked
    /// through the dispensing reversal and shown beside it; the occasion is not given back; a
    /// dispensing user may append a note, which is itself append-only; a cashier may not.
    #[tokio::test]
    async fn c46_returns_and_notes_never_rewrite_the_entry() {
        let (f, professional) = h1_fixture().await;
        let (prescription, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 2).await;
        prepare_and_confirm(&f, &id).await;
        let (status, _) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::OK);
        let before = h1_entries(&f, &id).await;
        // Not before posting, and not by a cashier.
        let (status, noted) = request_as(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/h1-register/{}/annotations", before[0].id),
            json!({ "note": "Pharmacist initials clarified on the pasted copy." }),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{noted}");
        let (status, noted) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/h1-register/{}/annotations", before[0].id),
            json!({ "note": "Pharmacist initials clarified on the pasted copy." }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{noted}");
        for statement in [
            "UPDATE prescription_h1_register_annotations SET note='rewritten'",
            "DELETE FROM prescription_h1_register_annotations",
        ] {
            assert!(
                sqlx::query(statement).execute(&f.pool).await.is_err(),
                "{statement}"
            );
        }

        let (status, returned) = return_first_line(&f, &id, 1).await;
        assert_eq!(status, StatusCode::OK, "{returned}");
        assert_eq!(h1_entries(&f, &id).await, before);
        let (status, register) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/h1-register?from={TODAY}&to={TODAY}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{register}");
        assert_eq!(register["entries"][0]["quantityAtoms"], 20);
        assert_eq!(register["entries"][0]["returnedAtoms"], 10);
        assert_eq!(register["annotations"][0]["entryId"], before[0].id);
        // Beyond what remains returnable is refused, and the entry is still untouched.
        let sale_line = first_line_id(&f, &id).await;
        let (status, draft) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/returns",
            json!({ "returnKind": "sales_return", "originalDocumentId": id, "businessDate": TODAY }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let return_id = draft["id"].as_str().unwrap().to_owned();
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{return_id}/lines"),
            json!({
                "expectedRevision": 1,
                "originalLineId": sale_line,
                "quantity": 2,
                "disposition": "quarantined",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "over_return");
        assert_eq!(h1_entries(&f, &id).await, before);
        let (_, rx) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/prescriptions/{prescription}"),
            Value::Null,
        )
        .await;
        assert_eq!(rx["occasionsUsed"], 1, "a return gave back the occasion");
    }

    /// Records every central finding for `product_id` as an owner would, with Schedule H1 applying
    /// from `from` (and not applying before it), and NDPS purview established not to apply.
    async fn h1_from(f: &Fixture, product_id: &str, from: &str) {
        make_medicine(f, product_id).await;
        for scheme in [
            "schedule_h",
            "schedule_x",
            "schedule_c",
            "schedule_c1",
            "ndps_purview",
        ] {
            add_finding(f, product_id, scheme, false, "2020-01-01").await;
        }
        sqlx::query(
            "INSERT INTO product_regulatory_classifications (id,product_id,scheme,applies,\
             effective_from,effective_to,source_citation,determined_by_user_id,revision,status,\
             created_at_utc,updated_at_utc) VALUES (?,?,'schedule_h1',0,'2020-01-01',?,\
             'Owner-recorded finding',?,1,'active',strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(product_id)
        .bind(from)
        .bind(&f.owner_id)
        .execute(&f.pool)
        .await
        .unwrap();
        add_finding(f, product_id, "schedule_h1", true, from).await;
    }

    /// C-30, C-31, C-32, C-34, C-36. Effective dating, from owner-recorded findings only: a
    /// finding in force from the Sale's own date reaches it (both layers are required and
    /// complete); a finding already stored but commencing later does not (an ordinary sale, no
    /// entry); and a posted Sale keeps what it froze when the findings later change. (C-32, after
    /// commencement, is every other H1 proof here: their findings run from 2020.)
    #[tokio::test]
    async fn c30_h1_follows_the_recorded_findings_dates() {
        let f = fixture().await;
        let today = today_ist(&f, 0).await;
        let later = today_ist(&f, 10).await;
        h1_from(&f, &f.product_id, &today).await;
        record_basis(&f, &f.product_id, Some("prescription_register")).await;
        let professional = pharmacist(&f).await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale_on(&f, &item, &professional, 1, &today).await;
        assert_eq!(
            quote_of(&f, &id).await["lines"][0]["regulatoryGateScheme"],
            "schedule_h1"
        );
        prepare_and_confirm(&f, &id).await;
        let (status, posted) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["h1RegisterEntries"][0]["status"], "finalized");

        let (future, pack, batch) = second_product(&f, "Future H1 Tablet").await;
        h1_from(&f, &future, &later).await;
        let (status, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/sales",
            json!({ "customerPartyId": Value::Null, "businessDate": today }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let ordinary = created["id"].as_str().unwrap().to_owned();
        add_line(&f, &ordinary, &future, &pack, &batch, 1, 8000).await;
        assert_eq!(
            quote_of(&f, &ordinary).await["lines"][0]["regulatoryGate"],
            "clear"
        );
        let (status, posted) = post_confirmed(&f, &ordinary).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert!(h1_entries(&f, &ordinary).await.is_empty());

        // The findings change afterwards; neither posted Sale is reinterpreted.
        sqlx::query("UPDATE product_regulatory_classifications SET applies=1 WHERE product_id=?")
            .bind(&future)
            .execute(&f.pool)
            .await
            .unwrap();
        let snapshot: String = sqlx::query_scalar(
            "SELECT regulatory_schemes_snapshot FROM sale_lines WHERE sale_document_id=?",
        )
        .bind(&ordinary)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert!(
            snapshot.contains("\"schedule_h1\":\"does_not_apply\""),
            "{snapshot}"
        );
        assert_eq!(h1_entries(&f, &id).await[0].status, "finalized");
    }

    /// C-35. A reclassification after preparation is caught at posting: the working entry no
    /// longer matches a Schedule H1 line, and nothing is sold.
    #[tokio::test]
    async fn c35_a_reclassification_after_preparation_is_caught_at_posting() {
        let (f, professional) = h1_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        prepare_and_confirm(&f, &id).await;
        // The owner records, as of 2020, that NDPS purview does apply after all.
        sqlx::query(
            "UPDATE product_regulatory_classifications SET applies=1 \
             WHERE product_id=? AND scheme='ndps_purview'",
        )
        .bind(&f.product_id)
        .execute(&f.pool)
        .await
        .unwrap();
        let (status, refused) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "schedule_h1_ndps_workflow_not_available");
        assert_nothing_sold(&f, &id).await;
        assert_eq!(h1_entries(&f, &id).await[0].status, "confirmed");
    }

    /// C-37, C-38, C-39, C-40. Races, resolved by the posting lock: two counters for the same
    /// remaining quantity post once; the same Sale prepared twice at once yields one live entry
    /// per line; two Sales prepared at once never share a reference.
    #[tokio::test]
    async fn c37_concurrent_h1_supplies_never_double_dispense_or_share_a_reference() {
        let (f, professional) = h1_fixture().await;
        let (_, item) = simple_prescription(&f, 10).await;
        let left = prepared_sale(&f, &item, &professional, 1).await;
        let right = prepared_sale(&f, &item, &professional, 1).await;
        let (one, two) = tokio::join!(prepare_as(&f, &left, OWNER), prepare_as(&f, &right, OWNER));
        for (status, body) in [&one, &two] {
            assert!(
                *status == StatusCode::OK || body["code"] == "service_busy",
                "{body}"
            );
        }
        let (a, b) = tokio::join!(prepare_as(&f, &left, OWNER), prepare_as(&f, &left, OWNER));
        for (status, body) in [&a, &b] {
            assert!(
                *status == StatusCode::OK
                    || body["code"] == "service_busy"
                    || body["code"] == "revision_conflict",
                "{body}"
            );
        }
        let references: Vec<i64> = sqlx::query_scalar(
            "SELECT reference_value FROM prescription_h1_register_entries ORDER BY reference_value",
        )
        .fetch_all(&f.pool)
        .await
        .unwrap();
        let expected: Vec<i64> = (1..=references.len() as i64).collect();
        assert_eq!(
            references, expected,
            "references must be distinct and consecutive"
        );
        for sale in [&left, &right] {
            let live = h1_entries(&f, sale)
                .await
                .into_iter()
                .filter(|entry| entry.status != "void")
                .count();
            assert!(live <= 1, "one line holds two live entries");
        }
        // Both prepared and confirmed; the prescription covers one supply only.
        for sale in [&left, &right] {
            if h1_entries(&f, sale).await.is_empty() {
                prepare_as(&f, sale, OWNER).await;
            }
            prepare_and_confirm(&f, sale).await;
        }
        let (first, second) = tokio::join!(post_confirmed(&f, &left), post_confirmed(&f, &right));
        let posted = [&first, &second]
            .iter()
            .filter(|(status, _)| *status == StatusCode::OK)
            .count();
        assert_eq!(posted, 1, "{first:?} {second:?}");
        assert_eq!(dispensing_count(&f).await, 1);
        let finalized: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM prescription_h1_register_entries WHERE status='finalized'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(finalized, 1);
    }

    /// C-41, C-42, C-43, C-44, C-45. Changes racing, or landing between confirmation and posting,
    /// yield one coherent answer: a void racing the posting either wins (nothing sold) or loses
    /// (posted, void refused) — never both; an election withdrawn, a pharmacist archived, or the
    /// prescription archived stops the posting with both layers left confirmed.
    #[tokio::test]
    async fn c45_changes_between_confirmation_and_posting_yield_one_answer() {
        let (f, professional) = h1_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        prepare_and_confirm(&f, &id).await;
        let record = entries(&f, &id).await.remove(0).id;
        let (void, post) =
            tokio::join!(void_as(&f, &record, "race", OWNER), post_confirmed(&f, &id));
        let entry = h1_entries(&f, &id).await.remove(0);
        match (void.0, post.0) {
            (StatusCode::OK, status) => {
                assert_ne!(status, StatusCode::OK, "{post:?}");
                assert_eq!(entry.status, "void");
                assert_nothing_sold(&f, &id).await;
            }
            (_, StatusCode::OK) => assert_eq!(entry.status, "finalized"),
            other => panic!("{other:?}: {void:?} {post:?}"),
        }

        for change in ["election", "pharmacist", "prescription"] {
            let (f, professional) = h1_fixture().await;
            let (prescription, item) = simple_prescription(&f, 20).await;
            let id = prepared_sale(&f, &item, &professional, 1).await;
            prepare_and_confirm(&f, &id).await;
            let (statement, bound) = match change {
                "election" => (
                    "UPDATE store_record_elections SET status='archived',\
                     archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='x' \
                     WHERE id<>?",
                    String::new(),
                ),
                "pharmacist" => (
                    "UPDATE store_professionals SET status='archived',\
                     archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='x' \
                     WHERE id=?",
                    professional.clone(),
                ),
                _ => (
                    "UPDATE prescriptions SET status='archived',\
                     archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='x' \
                     WHERE id=?",
                    prescription.clone(),
                ),
            };
            sqlx::query(statement)
                .bind(&bound)
                .execute(&f.pool)
                .await
                .unwrap();
            let (status, refused) = post_confirmed(&f, &id).await;
            assert_eq!(status, StatusCode::CONFLICT, "{change}: {refused}");
            assert_nothing_sold(&f, &id).await;
            assert_eq!(h1_entries(&f, &id).await[0].status, "confirmed", "{change}");
        }
    }

    /// C-13, C-14, C-15. The memo-book election still needs the separate H1 entry; the prescription
    /// layer still refuses a substitute or an unauthorised repeat for an H1 drug.
    #[tokio::test]
    async fn c13_the_h1_layer_is_separate_from_whichever_book_was_elected() {
        let f = fixture().await;
        schedule_product(&f, &f.product_id, &["schedule_h1"]).await;
        add_finding(&f, &f.product_id, "ndps_purview", false, "2020-01-01").await;
        record_basis(&f, &f.product_id, Some("cash_or_credit_memo_book")).await;
        let professional = pharmacist(&f).await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let revision = revision_of(&f, &id).await;
        request(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/sales/{id}/supply"),
            json!({
                "expectedRevision": revision,
                "supervisingProfessionalId": professional,
                "prescriptionEndorsementConfirmed": true,
                "prescriptionOriginalContainerConfirmed": true,
            }),
        )
        .await;
        let (status, prepared) = prepare_as(&f, &id, OWNER).await;
        assert_eq!(status, StatusCode::OK, "{prepared}");
        assert_eq!(
            prepared["prescriptionRecords"][0]["serialNumber"],
            "PM-000001"
        );
        assert_eq!(prepared["h1RegisterEntries"][0]["reference"], "AH1-000001");
        prepare_and_confirm(&f, &id).await;
        let (status, posted) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");

        // A once-only prescription is not dispensed again, and another product cannot stand in.
        let again = prepared_sale(&f, &item, &professional, 1).await;
        let (status, refused) = post_as_quoted(&f, &again).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert!(
            prescription_issue_codes(&refused)
                .contains(&"lines.1.prescription_repeat_not_authorised".to_owned()),
            "{refused}"
        );
        let (other, pack, batch) = second_product(&f, "Cefixime 200").await;
        schedule_product(&f, &other, &["schedule_h1"]).await;
        add_finding(&f, &other, "ndps_purview", false, "2020-01-01").await;
        manufacturer_for(&f, &other).await;
        let substitute = open_sale(&f, draft_body(None)).await;
        let line = add_line(&f, &substitute, &other, &pack, &batch, 1, 8000).await;
        let (_, fresh) = simple_prescription(&f, 20).await;
        // Rule 65(11A): the link itself is refused, naming the substitution.
        let (status, body) = link_line(&f, &line, &substitute, Some(&fresh), OWNER).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(
            prescription_issue_codes(&body),
            vec!["supply.prescription_substitution_not_permitted".to_owned()]
        );
        let quote = quote_of(&f, &substitute).await;
        assert_eq!(
            quote["lines"][0]["prescription"]["issue"],
            "prescription_missing"
        );
    }

    /// C-56, C-65. A veterinary Schedule H1 supply is an unresolved veterinary-H1 workflow: refused
    /// at preparation and at posting, with no entry made — and not described as prohibited.
    #[tokio::test]
    async fn c65_a_veterinary_h1_supply_is_an_unresolved_workflow() {
        let (f, professional) = h1_fixture().await;
        let created = enter_prescription(
            &f,
            prescription_body(
                &f.product_id,
                20,
                json!({ "subjectKind": "animal", "subjectName": "Owner of the dog" }),
            ),
        )
        .await;
        let item = created["items"][0]["id"].as_str().unwrap().to_owned();
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let quote = quote_of(&f, &id).await;
        assert_eq!(
            quote["lines"][0]["prescription"]["issue"],
            "schedule_h1_veterinary_workflow_unresolved"
        );
        let (status, refused) = prepare_as(&f, &id, OWNER).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        let (status, refused) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["lines.1.schedule_h1_veterinary_workflow_unresolved".to_owned()]
        );
        let text = refused.to_string().to_lowercase();
        assert!(text.contains("unresolved"), "{refused}");
        assert!(!text.contains("prohibit"), "{refused}");
        assert!(h1_entries(&f, &id).await.is_empty());
        assert_eq!(entry_count(&f).await, 0);
    }

    /// C-03, C-06. An unknown H1 position, or H1 with Schedule X, C or C(1), is refused before any
    /// entry exists.
    #[tokio::test]
    async fn c06_unknown_or_unsupported_h1_combinations_prepare_nothing() {
        for (inside, unknown_h1, code) in [
            (
                &["schedule_h"][..],
                true,
                "regulatory_classification_unresolved",
            ),
            (
                &["schedule_h1", "schedule_x"][..],
                false,
                "schedule_x_workflow_not_available",
            ),
            (
                &["schedule_h1", "schedule_c"][..],
                false,
                "regulated_sale_workflow_not_available",
            ),
            (
                &["schedule_h1", "schedule_c1"][..],
                false,
                "regulated_sale_workflow_not_available",
            ),
        ] {
            let f = fixture().await;
            schedule_product(&f, &f.product_id, inside).await;
            add_finding(&f, &f.product_id, "ndps_purview", false, "2020-01-01").await;
            if unknown_h1 {
                sqlx::query(
                    "DELETE FROM product_regulatory_classifications WHERE scheme='schedule_h1'",
                )
                .execute(&f.pool)
                .await
                .unwrap();
            }
            record_basis(&f, &f.product_id, Some("prescription_register")).await;
            let professional = pharmacist(&f).await;
            let (_, item) = simple_prescription(&f, 20).await;
            let id = prepared_sale(&f, &item, &professional, 1).await;
            let (status, refused) = prepare_as(&f, &id, OWNER).await;
            assert_eq!(status, StatusCode::CONFLICT, "{inside:?}: {refused}");
            assert_eq!(refused["code"], code, "{inside:?}");
            let (status, refused) = post_confirmed(&f, &id).await;
            assert_eq!(status, StatusCode::CONFLICT, "{inside:?}: {refused}");
            assert_eq!(refused["code"], code, "{inside:?}");
            assert!(h1_entries(&f, &id).await.is_empty(), "{inside:?}");
        }
    }

    /// C-66, C-69, C-70, C-71, C-72. The Punjab boundary. At a store whose premises are in Punjab:
    /// a medicine the owner recorded within the Punjab restriction is refused with the typed
    /// unsupported-workflow error and neutral words; one with no Punjab finding fails closed; one
    /// recorded outside it proceeds to the ordinary H1 workflow and completes. The same "applies"
    /// finding is ignored at a store elsewhere. A general item is untouched.
    #[tokio::test]
    async fn c69_the_punjab_boundary_refuses_only_where_it_governs() {
        let (f, professional) = h1_fixture().await;
        move_store_to(&f, Some(PUNJAB)).await;
        let (_, item) = simple_prescription(&f, 20).await;

        // No Punjab finding: unresolved.
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let quote = quote_of(&f, &id).await;
        assert_eq!(quote["lines"][0]["regulatoryGate"], "unresolved");
        assert_eq!(
            quote["lines"][0]["regulatoryGateScheme"],
            "punjab_restricted_supply"
        );
        let (status, refused) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "state_regulatory_position_unresolved");

        // Recorded as within the restriction, through the owner's API: refused as unsupported.
        let (status, body) =
            classify_scheme(&f, "punjab_restricted_supply", true, "2021-03-25", None).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let quote = quote_of(&f, &id).await;
        assert_eq!(quote["lines"][0]["regulatoryGate"], "workflow_unavailable");
        assert_eq!(
            quote["lines"][0]["regulatoryGateScheme"],
            "punjab_restricted_supply"
        );
        for (status, refused) in [
            prepare_as(&f, &id, OWNER).await,
            post_confirmed(&f, &id).await,
        ] {
            assert_eq!(status, StatusCode::CONFLICT, "{refused}");
            assert_eq!(
                refused["code"],
                "state_restricted_drug_workflow_not_available"
            );
            assert_eq!(
                refused["message"],
                "This drug is subject to an additional Punjab drug-control workflow that AUSHADHARTH does not yet support."
            );
            let text = refused.to_string().to_lowercase();
            for word in ["banned", "prohibit", "illegal"] {
                assert!(!text.contains(word), "{word}: {refused}");
            }
        }
        assert!(h1_entries(&f, &id).await.is_empty());
        assert_nothing_sold(&f, &id).await;

        // The same finding at a store elsewhere is not a Punjab store's boundary.
        move_store_to(&f, Some(MAHARASHTRA)).await;
        assert_eq!(
            quote_of(&f, &id).await["lines"][0]["regulatoryGate"],
            "prescription_required"
        );

        // Back in Punjab, recorded outside the restriction: the ordinary H1 workflow completes.
        move_store_to(&f, Some(PUNJAB)).await;
        sqlx::query(
            "UPDATE product_regulatory_classifications SET applies=0 \
             WHERE product_id=? AND scheme='punjab_restricted_supply'",
        )
        .bind(&f.product_id)
        .execute(&f.pool)
        .await
        .unwrap();
        prepare_and_confirm(&f, &id).await;
        let (status, posted) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["h1RegisterEntries"][0]["status"], "finalized");

        // A general item in the Punjab store, with no finding of any kind, still sells.
        let (general, pack, batch) = second_product(&f, "Cotton roll").await;
        let ordinary = open_sale(&f, draft_body(None)).await;
        add_line(&f, &ordinary, &general, &pack, &batch, 1, 8000).await;
        let (status, posted) = post_confirmed(&f, &ordinary).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
    }

    /// C-76, C-77, C-78. The posting re-judges the State boundary under its own lock: a Punjab
    /// finding recorded after both layers were confirmed, or the store's premises moved into
    /// Punjab, stops the posting with nothing sold. A posted line freezes the premises State and
    /// the Punjab answer in their own column; the central version-1 snapshot is unchanged in shape.
    #[tokio::test]
    async fn c76_the_posting_rechecks_the_state_boundary_and_freezes_it_apart() {
        for change in ["finding", "premises"] {
            let (f, professional) = h1_fixture().await;
            if change == "finding" {
                move_store_to(&f, Some(PUNJAB)).await;
                add_finding(
                    &f,
                    &f.product_id,
                    "punjab_restricted_supply",
                    false,
                    "2021-03-25",
                )
                .await;
            } else {
                add_finding(
                    &f,
                    &f.product_id,
                    "punjab_restricted_supply",
                    true,
                    "2021-03-25",
                )
                .await;
            }
            let (_, item) = simple_prescription(&f, 20).await;
            let id = prepared_sale(&f, &item, &professional, 1).await;
            prepare_and_confirm(&f, &id).await;
            if change == "finding" {
                sqlx::query(
                    "UPDATE product_regulatory_classifications SET applies=1 \
                     WHERE product_id=? AND scheme='punjab_restricted_supply'",
                )
                .bind(&f.product_id)
                .execute(&f.pool)
                .await
                .unwrap();
            } else {
                move_store_to(&f, Some(PUNJAB)).await;
            }
            let (status, refused) = post_confirmed(&f, &id).await;
            assert_eq!(status, StatusCode::CONFLICT, "{change}: {refused}");
            assert_eq!(
                refused["code"], "state_restricted_drug_workflow_not_available",
                "{change}"
            );
            assert_nothing_sold(&f, &id).await;
        }

        // What a posted Punjab line freezes.
        let (f, professional) = h1_fixture().await;
        move_store_to(&f, Some(PUNJAB)).await;
        add_finding(
            &f,
            &f.product_id,
            "punjab_restricted_supply",
            false,
            "2021-03-25",
        )
        .await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        prepare_and_confirm(&f, &id).await;
        let (status, posted) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (version, schemes, state): (i64, String, String) = sqlx::query_as(
            "SELECT regulatory_snapshot_version,regulatory_schemes_snapshot,\
             regulatory_state_snapshot FROM sale_lines WHERE sale_document_id=?",
        )
        .bind(&id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(version, 1);
        assert!(!schemes.contains("punjab"), "{schemes}");
        assert_eq!(
            state,
            "{\"premises_state_code\":\"03\",\"punjab_restricted_supply\":\"does_not_apply\"}"
        );
        // A premises State that is not recorded fails closed for a medicine.
        let (f, professional) = h1_fixture().await;
        move_store_to(&f, None).await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        let (status, refused) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "state_regulatory_position_unresolved");
    }

    /// C-51 (State snapshot). The database refuses a hand-forged Punjab answer, an "applies" or
    /// "unknown" Punjab medicine line, and a snapshot naming the wrong premises State.
    #[tokio::test]
    async fn c51_the_database_refuses_a_forged_state_snapshot() {
        let f = fixture().await;
        make_medicine(&f, &f.product_id).await;
        clear_every_schedule(&f, "2020-01-01").await;
        move_store_to(&f, Some(PUNJAB)).await;
        add_finding(
            &f,
            &f.product_id,
            "punjab_restricted_supply",
            true,
            "2021-03-25",
        )
        .await;
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        sqlx::query(
            "UPDATE sale_lines SET regulatory_snapshot_version=1,regulatory_schemes_snapshot=\
             '{\"schedule_h\":\"does_not_apply\",\"schedule_h1\":\"does_not_apply\",\
             \"schedule_x\":\"does_not_apply\",\"schedule_c\":\"does_not_apply\",\
             \"schedule_c1\":\"does_not_apply\",\"ndps_purview\":\"does_not_apply\"}' \
             WHERE sale_document_id=?",
        )
        .bind(&id)
        .execute(&f.pool)
        .await
        .unwrap();
        for snapshot in [
            // Claims the finding says "does not apply": it says "applies".
            "{\"premises_state_code\":\"03\",\"punjab_restricted_supply\":\"does_not_apply\"}",
            "{\"premises_state_code\":\"03\",\"punjab_restricted_supply\":\"applies\"}",
            // Claims the store is elsewhere.
            "{\"premises_state_code\":\"27\",\"punjab_restricted_supply\":\"not_applicable\"}",
        ] {
            sqlx::query(
                "UPDATE sale_lines SET regulatory_state_snapshot=? WHERE sale_document_id=?",
            )
            .bind(snapshot)
            .bind(&id)
            .execute(&f.pool)
            .await
            .unwrap();
            let error = flip_to_posted(&f, &id).await;
            assert!(
                error.contains("state_regulatory_boundary_refuses_posting"),
                "{snapshot}: {error}"
            );
        }
        // An incoherent shape never gets in at all.
        let error = sqlx::query(
            "UPDATE sale_lines SET regulatory_state_snapshot=\
             '{\"premises_state_code\":\"03\",\"punjab_restricted_supply\":\"not_applicable\"}' \
             WHERE sale_document_id=?",
        )
        .bind(&id)
        .execute(&f.pool)
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("regulatory_snapshot_incoherent"), "{error}");
    }

    /// C-61, C-73, C-74. Nothing is classified from a name or a fact: a medicine named after
    /// hydroxychloroquine, pregabalin or tramadol with no findings is simply unresolved (never
    /// "Schedule H1 listed" and never Punjab-restricted by inference); alcohol and pack-volume facts
    /// that would meet entry 52 change nothing; no finding exists that the owner did not record.
    #[tokio::test]
    async fn c73_nothing_is_classified_from_a_name_or_a_criterion() {
        let f = fixture().await;
        let seeded: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM product_regulatory_classifications")
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(seeded, 0, "a finding was seeded");
        move_store_to(&f, Some(PUNJAB)).await;
        for name in [
            "Hydroxychloroquine 200 mg Tablet",
            "Pregabalin 75 mg Capsule",
            "Tramadol 50 mg Tablet",
        ] {
            let (product, _, _) = second_product(&f, name).await;
            make_medicine(&f, &product).await;
            let (status, position) = request(
                f.pool.clone(),
                "GET",
                &format!("/api/v1/products/{product}/regulatory"),
                Value::Null,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{position}");
            assert_eq!(position["saleGate"], "unresolved", "{name}");
            for answer in position["resolved"].as_array().unwrap() {
                assert_eq!(answer["answer"], "unknown", "{name}: {answer}");
            }
        }
        // Entry 52's facts, recorded, compute nothing: the gate follows findings only.
        make_medicine(&f, &f.product_id).await;
        clear_every_schedule(&f, "2020-01-01").await;
        add_finding(
            &f,
            &f.product_id,
            "punjab_restricted_supply",
            false,
            "2021-03-25",
        )
        .await;
        sqlx::query(
            "INSERT INTO product_regulatory_attributes (product_id,alcohol_percent_vv_hundredths,\
             recorded_by_user_id,created_at_utc,updated_at_utc) VALUES (?,1300,?,\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&f.product_id)
        .bind(&f.owner_id)
        .execute(&f.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO product_pack_regulatory_attributes (product_pack_id,\
             net_volume_millilitres_hundredths,recorded_by_user_id,created_at_utc,updated_at_utc) \
             VALUES (?,10000,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&f.pack_id)
        .bind(&f.owner_id)
        .execute(&f.pool)
        .await
        .unwrap();
        let id = draft_with_line(&f, "pack", 1, 8000).await;
        assert_eq!(
            quote_of(&f, &id).await["lines"][0]["regulatoryGate"],
            "clear"
        );
        let (status, posted) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert!(h1_entries(&f, &id).await.is_empty());
    }

    /// C-67, C-75, C-57. There is no batching path: no endpoint confirms or posts H1 entries by
    /// day, and a confirmation covers only its own Sale. No ESI exemption can be claimed through
    /// the API: an ESI flag is ignored and the H1 entry is still required. No response calls the
    /// DCC recommendation law or says rule 65(3)(1)(h) requires a signature.
    #[tokio::test]
    async fn c67_there_is_no_batching_and_no_esi_path() {
        let (f, professional) = h1_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let first = prepared_sale(&f, &item, &professional, 1).await;
        let second = prepared_sale(&f, &item, &professional, 1).await;
        prepare_as(&f, &first, OWNER).await;
        prepare_as(&f, &second, OWNER).await;
        for uri in [
            "/api/v1/h1-register/confirm",
            "/api/v1/h1-register/confirm-day",
            "/api/v1/h1-register/end-of-day",
        ] {
            let (status, _) = request(
                f.pool.clone(),
                "POST",
                uri,
                json!({ "hardCopyPlacedInRegister": true, "pharmacistAuthenticatedHardCopy": true,
                        "date": TODAY }),
            )
            .await;
            assert!(
                status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED,
                "{uri}: {status}"
            );
        }
        confirm_h1(&f, &first, true, true, OWNER).await;
        assert_eq!(h1_entries(&f, &first).await[0].status, "confirmed");
        assert_eq!(h1_entries(&f, &second).await[0].status, "prepared");

        // An ESI claim changes nothing: the second supply still needs its own H1 confirmation.
        let revision = revision_of(&f, &second).await;
        let (_, supply) = request(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/sales/{second}/supply"),
            json!({
                "expectedRevision": revision,
                "supervisingProfessionalId": professional,
                "prescriptionEndorsementConfirmed": true,
                "esiPrescription": true,
                "esiScheme": true,
            }),
        )
        .await;
        assert_eq!(supply["code"], "prescription_record_prepared", "{supply}");
        confirm_records(&f, &second).await;
        let (status, refused) = post_confirmed(&f, &second).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["supply.schedule_h1_register_not_confirmed".to_owned()]
        );
        let text = refused.to_string().to_lowercase();
        for claim in [
            "esi",
            "dcc",
            "gazette",
            "rule 65(3)(1)(h) requires",
            "e-sign",
            "digitally",
        ] {
            assert!(!text.contains(claim), "{claim}: {refused}");
        }
    }

    // -----------------------------------------------------------------------------------------
    // Phase 1M-C pre-freeze corrective C-R1: the two race cases of the approved plan that had no
    // dedicated proof, and the State-snapshot architecture that was implemented beside the
    // version-1 central snapshot instead of inside it.
    // -----------------------------------------------------------------------------------------

    /// Stages a return of `packs` from `sale` up to the point of posting, so a race can begin at
    /// the posting itself rather than at the paperwork before it.
    async fn staged_return(f: &Fixture, sale: &str, packs: i64) -> String {
        let sale_line = first_line_id(f, sale).await;
        let (status, draft) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/returns",
            json!({ "returnKind": "sales_return", "originalDocumentId": sale, "businessDate": TODAY }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let return_id = draft["id"].as_str().unwrap().to_owned();
        let (status, withline) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{return_id}/lines"),
            json!({
                "expectedRevision": 1,
                "originalLineId": sale_line,
                "quantity": packs,
                "disposition": "quarantined",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{withline}");
        return_id
    }

    async fn post_return(f: &Fixture, return_id: &str) -> (StatusCode, Value) {
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{return_id}/post"),
            json!({
                "expectedRevision": 2,
                "idempotencyKey": Uuid::now_v7().to_string(),
                "taxAdjustmentStatus": "commercial_only",
            }),
        )
        .await
    }

    /// A prescription for `atoms` that may be supplied on `times` occasions in all.
    async fn repeatable_prescription(f: &Fixture, atoms: i64, times: i64) -> (String, String) {
        let created = enter_prescription(
            f,
            prescription_body(
                &f.product_id,
                atoms,
                json!({ "repeatAuthority": "stated_times", "repeatTimes": times }),
            ),
        )
        .await;
        (
            created["id"].as_str().unwrap().to_owned(),
            created["items"][0]["id"].as_str().unwrap().to_owned(),
        )
    }

    async fn occasions_used(f: &Fixture, prescription: &str) -> i64 {
        let (status, body) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/prescriptions/{prescription}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["occasionsUsed"].as_i64().expect("occasionsUsed")
    }

    /// What the whole prescription-supply ledger says after every transaction has settled:
    /// dispensings, the atoms they moved, the atoms given back, and the H1 entries that finalized.
    #[derive(Debug, PartialEq, Eq)]
    struct SupplyLedger {
        dispensings: i64,
        dispensed_atoms: i64,
        reversals: i64,
        reversed_atoms: i64,
        finalized_entries: i64,
    }

    async fn supply_ledger(f: &Fixture) -> SupplyLedger {
        let (dispensings, dispensed_atoms): (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*),COALESCE(SUM(quantity_atoms),0) FROM prescription_dispensings",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        let (reversals, reversed_atoms): (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*),COALESCE(SUM(quantity_atoms),0) FROM prescription_dispensing_reversals",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        let finalized_entries: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM prescription_h1_register_entries WHERE status='finalized'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        SupplyLedger {
            dispensings,
            dispensed_atoms,
            reversals,
            reversed_atoms,
            finalized_entries,
        }
    }

    /// The ledger invariants that must hold whichever way a race settles: nothing is dispensed
    /// beyond the prescription, nothing is given back that was never dispensed, no finalized H1
    /// entry is an orphan, and no dispensing carries two of them.
    async fn assert_supply_ledger_coherent(f: &Fixture, prescribed_atoms: i64) {
        let ledger = supply_ledger(f).await;
        assert!(
            ledger.dispensed_atoms - ledger.reversed_atoms >= 0,
            "negative net supply: {ledger:?}"
        );
        assert!(
            ledger.dispensed_atoms - ledger.reversed_atoms <= prescribed_atoms,
            "over-dispensed: {ledger:?}"
        );
        assert!(
            ledger.reversed_atoms <= ledger.dispensed_atoms,
            "gave back more than was supplied: {ledger:?}"
        );
        assert_eq!(
            ledger.dispensings, ledger.finalized_entries,
            "a finalized H1 entry without its dispensing, or the other way about: {ledger:?}"
        );
        let orphans: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM prescription_h1_register_entries entry \
             WHERE entry.status='finalized' AND (entry.dispensing_id IS NULL OR NOT EXISTS \
             (SELECT 1 FROM prescription_dispensings dispensing \
              WHERE dispensing.id=entry.dispensing_id AND dispensing.sale_line_id=entry.sale_line_id))",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(orphans, 0, "orphan H1 entries");
        let shared: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM (SELECT dispensing_id FROM prescription_h1_register_entries \
             WHERE dispensing_id IS NOT NULL GROUP BY dispensing_id HAVING COUNT(*)>1)",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(shared, 0, "two H1 entries on one dispensing");
        let over_returned: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM prescription_dispensings dispensing WHERE \
             (SELECT COALESCE(SUM(quantity_atoms),0) FROM prescription_dispensing_reversals \
              WHERE dispensing_id=dispensing.id) > dispensing.quantity_atoms",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(over_returned, 0, "a reversal beyond its dispensing");
    }

    /// A refused posting of `sale` left everything as it was: still a draft, no number of its own, no
    /// tender, and the batch holding exactly `stock` — what the supplies before it left behind.
    async fn assert_this_sale_sold_nothing(f: &Fixture, sale: &str, stock: i64) {
        assert_nothing_posted(f, sale, &f.batch_id, stock).await;
        assert_eq!(tender_rows(f, sale).await, 0, "a tender was written");
    }

    /// Two Schedule H1 supplies of one prescription, both prepared and confirmed while its whole
    /// quantity is still outstanding: the first takes two strips of it, the second one strip. Once
    /// the first is posted the second is waiting on exactly what a return would give back.
    async fn two_confirmed_supplies(
        f: &Fixture,
        item: &str,
        professional: &str,
    ) -> (String, String) {
        let first = prepared_sale(f, item, professional, 2).await;
        let second = prepared_sale(f, item, professional, 1).await;
        prepare_and_confirm(f, &first).await;
        prepare_and_confirm(f, &second).await;
        for sale in [&first, &second] {
            assert_eq!(h1_entries(f, sale).await[0].status, "confirmed");
        }
        (first, second)
    }

    /// C-R1 Race A. A return of a finalized Schedule H1 supply racing a fresh dispensing of the
    /// same prescription. The repository's semantics are that a return gives the QUANTITY back
    /// and never the OCCASION, so the two transactions conflict only over quantity and are
    /// serialized by the posting lock. Proved in both fixed orders — where the outcome is exact —
    /// and then concurrently, where either order may win but the ledger must settle coherently
    /// and the finalized H1 entry must be untouched either way.
    #[tokio::test]
    async fn cr1_a_return_racing_a_new_dispensing_settles_once() {
        // Order 1: the return commits first, so the quantity it gave back is there to be supplied.
        let (f, professional) = h1_fixture().await;
        let (prescription, item) = repeatable_prescription(&f, 20, 2).await;
        let (first, second) = two_confirmed_supplies(&f, &item, &professional).await;
        let (status, posted) = post_confirmed(&f, &first).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let untouched = h1_entries(&f, &first).await;
        let (status, returned) = post_return(&f, &staged_return(&f, &first, 1).await).await;
        assert_eq!(status, StatusCode::OK, "{returned}");
        let (status, posted) = post_confirmed(&f, &second).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(
            h1_entries(&f, &first).await,
            untouched,
            "the return rewrote a finalized entry"
        );
        assert_eq!(h1_entries(&f, &second).await[0].status, "finalized");
        assert_eq!(
            occasions_used(&f, &prescription).await,
            2,
            "a return gave an occasion back"
        );
        assert_supply_ledger_coherent(&f, 20).await;
        assert_eq!(
            supply_ledger(&f).await,
            SupplyLedger {
                dispensings: 2,
                dispensed_atoms: 30,
                reversals: 1,
                reversed_atoms: 10,
                finalized_entries: 2,
            }
        );

        // Order 2: the fresh dispensing goes first, and the quantity is not there yet. Once the
        // return has committed the same Sale posts, and the second occasion is consumed once.
        let (f, professional) = h1_fixture().await;
        let (prescription, item) = repeatable_prescription(&f, 20, 2).await;
        let (first, second) = two_confirmed_supplies(&f, &item, &professional).await;
        let (status, posted) = post_confirmed(&f, &first).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let untouched = h1_entries(&f, &first).await;
        let staged = staged_return(&f, &first, 1).await;
        let (status, refused) = post_confirmed(&f, &second).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(
            prescription_issue_codes(&refused),
            vec!["lines.1.prescription_quantity_exceeded".to_owned()]
        );
        assert_this_sale_sold_nothing(&f, &second, 80).await;
        assert_eq!(h1_entries(&f, &second).await[0].status, "confirmed");
        assert_eq!(occasions_used(&f, &prescription).await, 1);
        let (status, returned) = post_return(&f, &staged).await;
        assert_eq!(status, StatusCode::OK, "{returned}");
        let (status, posted) = post_confirmed(&f, &second).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(h1_entries(&f, &first).await, untouched);
        assert_eq!(occasions_used(&f, &prescription).await, 2);
        assert_supply_ledger_coherent(&f, 20).await;

        // Concurrently: both transactions are offered at once. Whichever the posting lock takes
        // first, exactly one coherent ledger emerges — never a supply beyond the prescription,
        // never a second consumption of the same occasion, never a changed finalized entry.
        let (f, professional) = h1_fixture().await;
        let (prescription, item) = repeatable_prescription(&f, 20, 2).await;
        let (first, second) = two_confirmed_supplies(&f, &item, &professional).await;
        let (status, posted) = post_confirmed(&f, &first).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let untouched = h1_entries(&f, &first).await;
        let staged = staged_return(&f, &first, 1).await;
        let (returned, dispensed) =
            tokio::join!(post_return(&f, &staged), post_confirmed(&f, &second));
        let entry = h1_entries(&f, &second).await.remove(0);
        let ledger = supply_ledger(&f).await;
        match (returned.0, dispensed.0) {
            (StatusCode::OK, StatusCode::OK) => {
                // The return was serialized first: the strip it gave back paid for the new supply.
                assert_eq!(entry.status, "finalized");
                assert_eq!(occasions_used(&f, &prescription).await, 2);
                assert_eq!(ledger.dispensings, 2);
                assert_eq!(ledger.reversals, 1);
            }
            (StatusCode::OK, _) => {
                // The new supply was judged first, against the quantity still outstanding, and
                // refused. The return then committed on its own.
                assert_eq!(entry.status, "confirmed");
                assert_eq!(occasions_used(&f, &prescription).await, 1);
                assert_eq!(ledger.reversals, 1);
                assert_this_sale_sold_nothing(&f, &second, 90).await;
            }
            (_, StatusCode::OK) => panic!(
                "a supply was made out of quantity that no committed return ever gave back: \
                 {returned:?} {dispensed:?}"
            ),
            _ => {
                // Neither settled: nothing moved at all.
                assert_eq!(entry.status, "confirmed");
                assert_eq!(occasions_used(&f, &prescription).await, 1);
                assert_eq!(ledger.reversals, 0);
                assert_this_sale_sold_nothing(&f, &second, 80).await;
            }
        }
        // Whichever way it settled, the refusal named the quantity and nothing was left half-done.
        if dispensed.0 != StatusCode::OK {
            assert!(
                prescription_issue_codes(&dispensed.1)
                    .contains(&"lines.1.prescription_quantity_exceeded".to_owned())
                    || dispensed.1["code"] == "service_busy",
                "{dispensed:?}"
            );
        }
        assert_eq!(h1_entries(&f, &first).await, untouched);
        assert_supply_ledger_coherent(&f, 20).await;
        assert!(ledger.reversals <= 1, "the return was applied twice");
    }

    /// C-R1 Race B. An append-only annotation racing an authorized reading of the register. The
    /// annotation is a single committed insert, so a reader sees the register either without it or
    /// with the whole of it, never half of it; the entry itself is never touched; the note appears
    /// exactly once with its author and time; and neither racer changes who may read.
    #[tokio::test]
    async fn cr1_b_an_annotation_racing_a_register_read_is_atomic() {
        let (f, professional) = h1_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let id = prepared_sale(&f, &item, &professional, 1).await;
        prepare_and_confirm(&f, &id).await;
        let (status, posted) = post_confirmed(&f, &id).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let before = h1_entries(&f, &id).await;
        let entry = before[0].clone();
        const NOTE: &str = "The pharmacist's initials on the pasted hard copy were clarified in ink on the same day.";

        let read = |token: &'static str| {
            let pool = f.pool.clone();
            async move {
                request_as(
                    pool,
                    "GET",
                    &format!("/api/v1/h1-register?from={TODAY}&to={TODAY}"),
                    Value::Null,
                    Some(token),
                )
                .await
            }
        };
        let annotate = |note: &'static str| {
            let pool = f.pool.clone();
            let entry_id = entry.id.clone();
            async move {
                request(
                    pool,
                    "POST",
                    &format!("/api/v1/h1-register/{entry_id}/annotations"),
                    json!({ "note": note }),
                )
                .await
            }
        };

        // The read is offered at the same moment as the append, in both orders.
        for _ in 0..2 {
            let ((status, sheet), (created, body)) = tokio::join!(read(OWNER), annotate(NOTE));
            assert_eq!(created, StatusCode::CREATED, "{body}");
            assert_eq!(status, StatusCode::OK, "{sheet}");
            let seen = sheet["annotations"].as_array().expect("annotations").len();
            assert!(seen <= 2, "{sheet}");
            for annotation in sheet["annotations"].as_array().unwrap() {
                // Nothing half-written is ever visible: the note, its author and its time are
                // either all there or the annotation is not there at all.
                assert_eq!(annotation["note"], NOTE, "{sheet}");
                assert_eq!(annotation["entryId"], entry.id, "{sheet}");
                assert!(
                    annotation["createdByDisplayName"].is_string()
                        && annotation["createdAtUtc"].is_string(),
                    "{sheet}"
                );
            }
            // The entry beside them is the one that was supplied, unchanged.
            assert_eq!(sheet["entries"][0]["reference"], entry.reference, "{sheet}");
            assert_eq!(sheet["entries"][0]["status"], "finalized", "{sheet}");
            assert_eq!(sheet["entries"][0]["quantityAtoms"], entry.quantity_atoms);
        }

        // Two appends racing each other: each lands exactly once, and neither overwrites the other.
        let (left, right) = tokio::join!(annotate("Left-hand note."), annotate("Right-hand note."));
        for (status, body) in [&left, &right] {
            assert_eq!(*status, StatusCode::CREATED, "{body}");
        }
        let notes: Vec<String> = sqlx::query_scalar(
            "SELECT note FROM prescription_h1_register_annotations ORDER BY created_at_utc,note",
        )
        .fetch_all(&f.pool)
        .await
        .unwrap();
        assert_eq!(notes.iter().filter(|note| *note == NOTE).count(), 2);
        assert_eq!(
            notes
                .iter()
                .filter(|note| *note == "Left-hand note.")
                .count(),
            1
        );
        assert_eq!(
            notes
                .iter()
                .filter(|note| *note == "Right-hand note.")
                .count(),
            1
        );
        let (authors, times): (i64, i64) = sqlx::query_as(
            "SELECT COUNT(DISTINCT created_by_user_id),COUNT(created_at_utc) \
             FROM prescription_h1_register_annotations",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(authors, 1);
        assert_eq!(times, notes.len() as i64, "an annotation without its time");

        // Annotating never reaches the entry, and an entry can never be moved to another store.
        assert_eq!(h1_entries(&f, &id).await, before);
        for statement in [
            "UPDATE prescription_h1_register_entries SET store_id='elsewhere'",
            "UPDATE prescription_h1_register_annotations SET created_by_user_id='someone-else'",
            "DELETE FROM prescription_h1_register_annotations",
        ] {
            assert!(
                sqlx::query(statement).execute(&f.pool).await.is_err(),
                "{statement}"
            );
        }

        // Neither racer changed who may read: a cashier still cannot read the register or append.
        let (status, refused) = read(CASHIER).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
        let (status, refused) = request_as(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/h1-register/{}/annotations", entry.id),
            json!({ "note": "Cashier's note." }),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
        // The register is this store's alone: the read is filtered by the store that owns the
        // entry, an entry's store can never be rewritten (above), and a posted Sale can never be
        // moved to another store either.
        let (status, sheet) = read(OWNER).await;
        assert_eq!(status, StatusCode::OK, "{sheet}");
        assert_eq!(sheet["entries"].as_array().unwrap().len(), 1, "{sheet}");
        assert_eq!(sheet["entries"][0]["reference"], entry.reference, "{sheet}");
        let error = sqlx::query("UPDATE sale_documents SET store_id='elsewhere' WHERE id=?")
            .bind(&id)
            .execute(&f.pool)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("sale_document_is_posted"), "{error}");
        assert_eq!(h1_entries(&f, &id).await, before);
    }

    /// C-R1 snapshot architecture. The State answer is frozen in its own column beside the
    /// unchanged version-1 central snapshot. A Sale that has been posted is never re-judged when
    /// the premises or the owner's findings change afterwards; a Sale posted now freezes both
    /// answers; neither can be edited after posting, by any hand; and the two answers are
    /// independent of one another. (That migration 0023 writes nothing into the lines of Sales
    /// posted before it is proved by the migration_0023 tests.)
    #[tokio::test]
    async fn cr1_c_the_state_snapshot_is_frozen_beside_the_central_one() {
        let (f, professional) = h1_fixture().await;
        let (_, item) = simple_prescription(&f, 20).await;
        let historical = prepared_sale(&f, &item, &professional, 1).await;
        prepare_and_confirm(&f, &historical).await;
        let (status, posted) = post_confirmed(&f, &historical).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let frozen: (i64, String, Option<String>) = sqlx::query_as(
            "SELECT regulatory_snapshot_version,regulatory_schemes_snapshot,\
             regulatory_state_snapshot FROM sale_lines WHERE sale_document_id=?",
        )
        .bind(&historical)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(frozen.0, 1, "the central snapshot changed version");
        assert_eq!(
            frozen.2.as_deref(),
            Some(
                "{\"premises_state_code\":\"27\",\"punjab_restricted_supply\":\"not_applicable\"}"
            ),
            "a Sale outside Punjab must freeze that the axis did not reach it"
        );

        // The premises move into Punjab and the owner records a restriction that would refuse this
        // very drug today. Nothing reaches back into what was already supplied.
        move_store_to(&f, Some(PUNJAB)).await;
        add_finding(
            &f,
            &f.product_id,
            "punjab_restricted_supply",
            true,
            "2021-03-25",
        )
        .await;
        let after: (i64, String, Option<String>) = sqlx::query_as(
            "SELECT regulatory_snapshot_version,regulatory_schemes_snapshot,\
             regulatory_state_snapshot FROM sale_lines WHERE sale_document_id=?",
        )
        .bind(&historical)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(after, frozen, "a finalized Sale was reinterpreted");
        let (status, document) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/sales/{historical}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{document}");
        assert_eq!(document["status"], "posted");
        assert_eq!(h1_entries(&f, &historical).await[0].status, "finalized");

        // A Sale posted now, at the Punjab premises, is judged under the posting lock against the
        // findings as they stand then — not against the draft's — and freezes that answer.
        sqlx::query(
            "UPDATE product_regulatory_classifications SET applies=0 \
             WHERE product_id=? AND scheme='punjab_restricted_supply'",
        )
        .bind(&f.product_id)
        .execute(&f.pool)
        .await
        .unwrap();
        let (_, item) = simple_prescription(&f, 20).await;
        let fresh = prepared_sale(&f, &item, &professional, 1).await;
        prepare_and_confirm(&f, &fresh).await;
        let (status, posted) = post_confirmed(&f, &fresh).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let (version, central, state): (i64, String, String) = sqlx::query_as(
            "SELECT regulatory_snapshot_version,regulatory_schemes_snapshot,\
             regulatory_state_snapshot FROM sale_lines WHERE sale_document_id=?",
        )
        .bind(&fresh)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(version, 1, "the central snapshot changed version");
        assert!(!central.contains("punjab"), "{central}");
        assert!(!central.contains("premises"), "{central}");
        assert_eq!(
            state,
            "{\"premises_state_code\":\"03\",\"punjab_restricted_supply\":\"does_not_apply\"}"
        );
        // The two answers are independent: the central one says this drug is in Schedule H1, the
        // State one says the Punjab restriction does not reach it. Two Sales of the same product
        // at the same store hold different State answers, because the premises differed.
        assert!(central.contains("\"schedule_h1\":\"applies\""), "{central}");
        assert_ne!(Some(state.as_str()), frozen.2.as_deref());
        assert_eq!(
            central, frozen.1,
            "the central answer moved with the State one"
        );

        // Neither frozen answer can be edited after posting, by any hand. A rewritten State answer
        // meets the coherence guard first — it would no longer match the premises and the findings
        // — and everything else meets the posted-document guard.
        for (statement, guard) in [
            (
                "UPDATE sale_lines SET regulatory_state_snapshot=\
                 '{\"premises_state_code\":\"03\",\"punjab_restricted_supply\":\"not_applicable\"}' \
                 WHERE sale_document_id=?",
                "regulatory_snapshot_incoherent",
            ),
            (
                "UPDATE sale_lines SET regulatory_state_snapshot=NULL WHERE sale_document_id=?",
                "sale_document_is_posted",
            ),
            (
                "UPDATE sale_lines SET regulatory_schemes_snapshot=\
                 '{\"schedule_h1\":\"does_not_apply\"}' WHERE sale_document_id=?",
                "sale_document_is_posted",
            ),
            (
                "UPDATE sale_lines SET regulatory_snapshot_version=2 WHERE sale_document_id=?",
                "sale_document_is_posted",
            ),
        ] {
            for sale in [&fresh, &historical] {
                let error = sqlx::query(statement)
                    .bind(sale)
                    .execute(&f.pool)
                    .await
                    .unwrap_err()
                    .to_string();
                assert!(error.contains(guard), "{statement}: {error}");
            }
        }
        let still: (i64, String, String) = sqlx::query_as(
            "SELECT regulatory_snapshot_version,regulatory_schemes_snapshot,\
             regulatory_state_snapshot FROM sale_lines WHERE sale_document_id=?",
        )
        .bind(&fresh)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(still, (version, central, state));

        // Independence the other way about: a product the owner has placed within the Punjab
        // restriction, with no central Schedule H1 finding of any kind, is a State matter alone.
        let (other, _, _) = second_product(&f, "Punjab-restricted general item").await;
        make_medicine(&f, &other).await;
        add_finding(&f, &other, "punjab_restricted_supply", true, "2021-03-25").await;
        let (status, view) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/products/{other}/regulatory"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        let text = view.to_string();
        assert!(text.contains("punjab_restricted_supply"), "{view}");
        assert!(!text.contains("\"schedule_h1\":\"applies\""), "{view}");
    }
}
