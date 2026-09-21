//! Phase 1M-A — what the Drugs Rules say about a product, on a given day.
//!
//! One resolver, used by the quote and by posting, so the counter's warning and the refusal that
//! stops the sale can never disagree. It answers from `product_regulatory_classifications` and from
//! nothing else: not from the product's name, its brand, its dosage form, its HSN code, its tax
//! category, its manufacturer or its barcode. None of those is evidence of a schedule, and a
//! pharmacy system that guessed from them would be inventing law.
//!
//! Three answers, never two. `Applies` and `DoesNotApply` are both findings, recorded by a person
//! with a citation. `Unknown` is the absence of a finding, and it is not a quiet "no": a medicine
//! nobody has classified is a medicine nobody has established to be outside Schedule H.

use std::fmt;

use sqlx::{Sqlite, pool::PoolConnection};

/// The schemes a retail sale turns on, plus the axis that crosses two of them.
///
/// `NdpsPurview` is not a sixth schedule. Rule 97(1)(c) and (f) label a Schedule H or Schedule H1
/// substance differently when it comes within the purview of the Narcotic Drugs and Psychotropic
/// Substances Act, 1985, which is why it is an independent fact and not a value that would exclude
/// the others.
pub const SCHEMES: [&str; 6] = [
    "schedule_h",
    "schedule_h1",
    "schedule_x",
    "schedule_c",
    "schedule_c1",
    "ndps_purview",
];

/// The schemes whose answer a `medicine` must have before it may be sold at all.
///
/// NDPS purview is deliberately absent. It attaches through Schedule H or H1 labelling and no rule
/// 65 retail obligation turns on it by itself, so not knowing it does not stop a counter sale of a
/// drug established to be outside every schedule.
pub const SALE_GATING_SCHEMES: [&str; 5] = [
    "schedule_h",
    "schedule_h1",
    "schedule_x",
    "schedule_c",
    "schedule_c1",
];

/// The schemes whose statutory record this phase cannot yet produce.
///
/// Schedule H, H1 and X need the rule 65(9) prescription; Schedule C and C(1) need the rule
/// 65(4)(1) register or memo particulars. Both arrive in later phases, and until they do a sale
/// that would need them is refused rather than posted without them.
pub const WORKFLOW_PENDING_SCHEMES: [&str; 5] = [
    "schedule_h",
    "schedule_h1",
    "schedule_x",
    "schedule_c",
    "schedule_c1",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Applies,
    DoesNotApply,
    Unknown,
}

impl Resolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Resolution::Applies => "applies",
            Resolution::DoesNotApply => "does_not_apply",
            Resolution::Unknown => "unknown",
        }
    }
}

impl fmt::Display for Resolution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One product's position under every scheme, as it stood on one business date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedRegulatory {
    answers: [Resolution; SCHEMES.len()],
}

impl ResolvedRegulatory {
    pub fn unknown() -> Self {
        Self {
            answers: [Resolution::Unknown; SCHEMES.len()],
        }
    }

    pub fn answer(&self, scheme: &str) -> Resolution {
        SCHEMES
            .iter()
            .position(|known| *known == scheme)
            .map(|index| self.answers[index])
            .unwrap_or(Resolution::Unknown)
    }

    fn set(&mut self, scheme: &str, resolution: Resolution) {
        if let Some(index) = SCHEMES.iter().position(|known| *known == scheme) {
            self.answers[index] = resolution;
        }
    }

    /// The exact JSON a posted line freezes. Written in `SCHEMES` order so two snapshots of the
    /// same position are byte-identical and a reader can diff them.
    pub fn to_snapshot_json(&self) -> String {
        let body = SCHEMES
            .iter()
            .map(|scheme| format!("\"{scheme}\":\"{}\"", self.answer(scheme)))
            .collect::<Vec<_>>()
            .join(",");
        format!("{{{body}}}")
    }

    /// Every scheme this product is positively within.
    pub fn applied_schemes(&self) -> Vec<&'static str> {
        SCHEMES
            .iter()
            .copied()
            .filter(|scheme| self.answer(scheme) == Resolution::Applies)
            .collect()
    }
}

/// What a Sale line may do about a resolved position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaleGate {
    /// Nothing in these Rules stops this line being sold by this software today.
    Clear,
    /// A medicine whose schedule position nobody has established. Not sellable until it is.
    Unresolved,
    /// Lawfully sellable, but only with a statutory record this phase does not implement.
    WorkflowUnavailable { scheme: &'static str },
}

/// The gate, from the resolved position and the product's kind.
///
/// Kind matters in one direction only. A `medicine` with an unknown position is refused, because a
/// drug of unknown schedule is not a drug anyone may hand over. A `device` or a
/// `general_pharmacy_item` is not refused for an unknown, so the ordinary counter keeps working —
/// but if a device has been positively classified into Schedule C, which lists "Sterile Disposable
/// Devices for single use only", the first branch catches it like anything else.
pub fn gate(product_kind: &str, resolved: &ResolvedRegulatory) -> SaleGate {
    for scheme in WORKFLOW_PENDING_SCHEMES {
        if resolved.answer(scheme) == Resolution::Applies {
            return SaleGate::WorkflowUnavailable { scheme };
        }
    }
    if product_kind == "medicine"
        && SALE_GATING_SCHEMES
            .iter()
            .any(|scheme| resolved.answer(scheme) == Resolution::Unknown)
    {
        return SaleGate::Unresolved;
    }
    SaleGate::Clear
}

#[derive(Debug, sqlx::FromRow)]
struct ClassificationRow {
    scheme: String,
    applies: i64,
}

/// The position of one product on one business date.
///
/// `business_date` is the Sale's own date, never today's clock. Reproducing what was resolved for a
/// Sale posted last year must not depend on when the question is asked, and a Sale dated into a
/// future the law has already been amended for must see that amendment.
pub async fn resolve_for_product(
    connection: &mut PoolConnection<Sqlite>,
    product_id: &str,
    business_date: &str,
) -> Result<ResolvedRegulatory, sqlx::Error> {
    let rows: Vec<ClassificationRow> = sqlx::query_as(
        "SELECT scheme,applies FROM product_regulatory_classifications \
         WHERE product_id=? AND status='active' AND effective_from<=? \
           AND (effective_to IS NULL OR effective_to>?)",
    )
    .bind(product_id)
    .bind(business_date)
    .bind(business_date)
    .fetch_all(&mut **connection)
    .await?;

    let mut resolved = ResolvedRegulatory::unknown();
    for row in rows {
        resolved.set(
            &row.scheme,
            if row.applies == 1 {
                Resolution::Applies
            } else {
                Resolution::DoesNotApply
            },
        );
    }
    Ok(resolved)
}

/// The manufacturer in force for a product on a business date, as `(company_id, display_name)`.
///
/// Only the `manufacturer` role. Rule 65(3)(1)(f), 65(4)(1)(e) and 65(21)(b)(v) ask for the
/// manufacturer of the drug; a marketer, a brand owner and an importer are different parties, and
/// no rule permits one to stand in for the maker. An unrecorded manufacturer resolves to `None`,
/// which is the truth, and the phases that need one will refuse on it rather than substitute.
pub async fn resolve_manufacturer(
    connection: &mut PoolConnection<Sqlite>,
    product_id: &str,
    business_date: &str,
) -> Result<Option<(String, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT company.id,company.display_name \
         FROM product_company_roles role \
         JOIN pharmaceutical_companies company ON company.id=role.company_id \
         WHERE role.product_id=? AND role.role='manufacturer' AND role.status='active' \
           AND (role.effective_from IS NULL OR role.effective_from<=?) \
           AND (role.effective_to IS NULL OR role.effective_to>?) \
         ORDER BY COALESCE(role.effective_from,'') DESC, role.id DESC LIMIT 1",
    )
    .bind(product_id)
    .bind(business_date)
    .bind(business_date)
    .fetch_optional(&mut **connection)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(pairs: &[(&str, Resolution)]) -> ResolvedRegulatory {
        let mut resolved = ResolvedRegulatory::unknown();
        for (scheme, answer) in pairs {
            resolved.set(scheme, *answer);
        }
        resolved
    }

    fn all(answer: Resolution) -> ResolvedRegulatory {
        let mut resolved = ResolvedRegulatory::unknown();
        for scheme in SCHEMES {
            resolved.set(scheme, answer);
        }
        resolved
    }

    #[test]
    fn nothing_recorded_is_unknown_and_never_a_quiet_no() {
        let resolved = ResolvedRegulatory::unknown();
        for scheme in SCHEMES {
            assert_eq!(resolved.answer(scheme), Resolution::Unknown);
            assert_ne!(resolved.answer(scheme), Resolution::DoesNotApply);
        }
        assert_eq!(gate("medicine", &resolved), SaleGate::Unresolved);
    }

    #[test]
    fn an_unclassified_medicine_is_refused_and_an_unclassified_general_item_is_not() {
        let unknown = ResolvedRegulatory::unknown();
        assert_eq!(gate("medicine", &unknown), SaleGate::Unresolved);
        assert_eq!(gate("general_pharmacy_item", &unknown), SaleGate::Clear);
        assert_eq!(gate("device", &unknown), SaleGate::Clear);
    }

    #[test]
    fn a_medicine_established_outside_every_schedule_sells() {
        assert_eq!(
            gate("medicine", &all(Resolution::DoesNotApply)),
            SaleGate::Clear
        );
    }

    /// Each scheme whose record does not exist yet stops the sale on its own.
    #[test]
    fn each_pending_workflow_refuses_by_itself() {
        for scheme in WORKFLOW_PENDING_SCHEMES {
            let mut resolved = all(Resolution::DoesNotApply);
            resolved.set(scheme, Resolution::Applies);
            assert_eq!(
                gate("medicine", &resolved),
                SaleGate::WorkflowUnavailable { scheme },
                "{scheme} did not refuse"
            );
        }
    }

    /// Schedule C lists "Sterile Disposable Devices for single use only", so a device is not
    /// assumed to be outside it.
    #[test]
    fn a_device_classified_into_schedule_c_is_refused_like_any_other_line() {
        let resolved = with(&[("schedule_c", Resolution::Applies)]);
        assert_eq!(
            gate("device", &resolved),
            SaleGate::WorkflowUnavailable {
                scheme: "schedule_c"
            }
        );
    }

    /// NDPS purview is an axis over Schedule H and H1, not a schedule of its own: it neither sells
    /// a drug nor blocks one by itself.
    #[test]
    fn ndps_purview_alone_neither_blocks_nor_is_required() {
        let mut resolved = all(Resolution::DoesNotApply);
        resolved.set("ndps_purview", Resolution::Applies);
        assert_eq!(gate("medicine", &resolved), SaleGate::Clear);

        let mut unknown_ndps = all(Resolution::DoesNotApply);
        unknown_ndps.set("ndps_purview", Resolution::Unknown);
        assert_eq!(gate("medicine", &unknown_ndps), SaleGate::Clear);
    }

    /// An NDPS-purview Schedule H drug is refused for being Schedule H, not for being NDPS.
    #[test]
    fn an_ndps_schedule_h_drug_is_refused_on_the_schedule() {
        let resolved = with(&[
            ("schedule_h", Resolution::Applies),
            ("ndps_purview", Resolution::Applies),
        ]);
        assert_eq!(
            gate("medicine", &resolved),
            SaleGate::WorkflowUnavailable {
                scheme: "schedule_h"
            }
        );
    }

    /// The schemes are independent: being inside H1 says nothing about H, and the model must not
    /// encode an exclusivity that no rule states.
    #[test]
    fn schemes_are_independent_of_one_another() {
        let resolved = with(&[
            ("schedule_h1", Resolution::Applies),
            ("schedule_h", Resolution::Unknown),
        ]);
        assert_eq!(resolved.answer("schedule_h"), Resolution::Unknown);
        assert_eq!(resolved.answer("schedule_h1"), Resolution::Applies);
        let both = with(&[
            ("schedule_h", Resolution::Applies),
            ("schedule_c", Resolution::Applies),
        ]);
        assert_eq!(both.applied_schemes(), vec!["schedule_h", "schedule_c"]);
    }

    #[test]
    fn the_snapshot_names_every_scheme_in_a_stable_order() {
        let resolved = with(&[("schedule_h1", Resolution::Applies)]);
        assert_eq!(
            resolved.to_snapshot_json(),
            "{\"schedule_h\":\"unknown\",\"schedule_h1\":\"applies\",\"schedule_x\":\"unknown\",\
             \"schedule_c\":\"unknown\",\"schedule_c1\":\"unknown\",\"ndps_purview\":\"unknown\"}"
        );
        assert_eq!(resolved.to_snapshot_json(), resolved.to_snapshot_json());
    }

    #[test]
    fn an_unrecognised_scheme_answers_unknown_rather_than_guessing() {
        assert_eq!(
            ResolvedRegulatory::unknown().answer("schedule_z"),
            Resolution::Unknown
        );
    }
}
