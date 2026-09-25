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

/// The schemes whose statutory record this software cannot yet produce.
///
/// Schedule X needs the duplicate prescription and the rule 65(21) register; Schedule C and C(1)
/// need the rule 65(4)(1) register or memo particulars. None of them is satisfied by a prescription
/// alone, so a sale that needs any of them is refused rather than posted without it. Schedule H is
/// not here since Phase 1M-B, and Schedule H1 is not here since Phase 1M-C: both are gated on their
/// prescription workflow, and H1 additionally on its separate rule 65(3)(1)(h) working entry.
pub const WORKFLOW_PENDING_SCHEMES: [&str; 3] = ["schedule_x", "schedule_c", "schedule_c1"];

/// Phase 1M-C — State-level regulatory axes, recorded like any other finding but kept apart from
/// `SCHEMES` on purpose: they are not central schedules, they are not frozen into the version-1
/// central snapshot, and they gate only where the store's premises are in that State.
///
/// `punjab_restricted_supply` is a product's position under Punjab notification No.
/// 9/16/21-3H6/1039 dated 25-03-2021 (eight habit-forming drugs whose stocking and sale need a
/// departmental permission and monthly statements). It is not Schedule H1, not NDPS purview and not
/// an external-order H1 condition. AUSHADHARTH does not implement that State workflow; it refuses.
pub const STATE_SCHEMES: [&str; 1] = ["punjab_restricted_supply"];

/// Every scheme an owner may record a finding under.
pub const CLASSIFIABLE_SCHEMES: [&str; 7] = [
    "schedule_h",
    "schedule_h1",
    "schedule_x",
    "schedule_c",
    "schedule_c1",
    "ndps_purview",
    "punjab_restricted_supply",
];

/// The GST state code of Punjab in the frozen `state_codes` reference table.
pub const PUNJAB_STATE_CODE: &str = "03";

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
    /// Schedule H or H1: sellable only on a prescription, under a registered pharmacist's
    /// supervision, with every rule 65 fact present — and for H1 the separate working entry too.
    /// Whether they are is the posting's question, not this one's.
    PrescriptionRequired,
    /// Phase 1M-C — a combination this software does not support, although each part alone may be:
    /// `schedule_h1_ndps` is a Schedule H1 drug whose NDPS purview applies or is unknown. This is
    /// AUSHADHARTH's own unsupported-workflow boundary, not a requirement rule 65 states.
    UnsupportedIntersection { reason: &'static str },
    /// Phase 1M-C — a State workflow this software does not implement applies to the product at a
    /// store in that State.
    StateWorkflowUnavailable { scheme: &'static str },
    /// Phase 1M-C — the product's position under a State axis that governs this store is not
    /// recorded, or the store's premises State itself is not recorded.
    StateUnresolved { scheme: &'static str },
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
    // Phase 1M-C: Schedule H1 is sellable only where its NDPS purview is established NOT to apply.
    // An NDPS-purview H1 drug, or one whose purview nobody has recorded, is an unsupported
    // NDPS-intersection workflow.
    if resolved.answer("schedule_h1") == Resolution::Applies
        && resolved.answer("ndps_purview") != Resolution::DoesNotApply
    {
        return SaleGate::UnsupportedIntersection {
            reason: "schedule_h1_ndps",
        };
    }
    // Last, so an unsupported scheme or an unknown position always wins: a line that is both
    // Schedule H and Schedule C is held to Schedule C as well, and a supported H requirement never
    // cancels an unsupported one.
    if resolved.answer("schedule_h") == Resolution::Applies
        || resolved.answer("schedule_h1") == Resolution::Applies
    {
        return SaleGate::PrescriptionRequired;
    }
    SaleGate::Clear
}

/// Phase 1M-C — a line's position under the State axes, as the posting freezes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateAnswer {
    /// The GST state code of the store's recorded premises, or `None` where it is not recorded.
    pub premises_state_code: Option<String>,
    pub punjab: StateAxisAnswer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateAxisAnswer {
    /// The premises are in the State: the recorded finding, or `Unknown`.
    Resolved(Resolution),
    /// The premises are elsewhere; the axis does not govern this store.
    NotApplicable,
    /// The premises State is not recorded, so whether the axis governs cannot be said.
    Undetermined,
}

impl StateAnswer {
    /// The exact JSON a posted line freezes in `regulatory_state_snapshot`.
    pub fn to_snapshot_json(&self) -> String {
        let premises = match &self.premises_state_code {
            Some(code) => format!("\"{code}\""),
            None => "null".to_owned(),
        };
        let punjab = match self.punjab {
            StateAxisAnswer::Resolved(answer) => answer.as_str(),
            StateAxisAnswer::NotApplicable => "not_applicable",
            StateAxisAnswer::Undetermined => "undetermined",
        };
        format!("{{\"premises_state_code\":{premises},\"punjab_restricted_supply\":\"{punjab}\"}}")
    }
}

/// The State gate, where one governs. `None` means no State axis stands in the way.
///
/// Kind matters as for the central gate: an unknown or undetermined State position stops a
/// medicine, never a device or a general item; a positive finding stops anything.
pub fn state_gate(product_kind: &str, state: &StateAnswer) -> Option<SaleGate> {
    match state.punjab {
        StateAxisAnswer::Resolved(Resolution::Applies) => {
            Some(SaleGate::StateWorkflowUnavailable {
                scheme: "punjab_restricted_supply",
            })
        }
        StateAxisAnswer::Resolved(Resolution::Unknown) | StateAxisAnswer::Undetermined
            if product_kind == "medicine" =>
        {
            Some(SaleGate::StateUnresolved {
                scheme: "punjab_restricted_supply",
            })
        }
        _ => None,
    }
}

/// The full gate, in the order the posting judges it: the central classification and any central
/// intersection this software does not support first, then the State boundary, then the
/// prescription requirement.
pub fn combined_gate(
    product_kind: &str,
    resolved: &ResolvedRegulatory,
    state: &StateAnswer,
) -> SaleGate {
    let central = gate(product_kind, resolved);
    match central {
        SaleGate::WorkflowUnavailable { .. }
        | SaleGate::Unresolved
        | SaleGate::UnsupportedIntersection { .. } => central,
        _ => state_gate(product_kind, state).unwrap_or(central),
    }
}

/// The GST state code of the store's recorded premises, or `None` where no premises address, no
/// State, or no Indian State is recorded. Never the GSTIN prefix or the place of supply.
pub async fn resolve_premises_state(
    connection: &mut PoolConnection<Sqlite>,
    store_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    let code: Option<Option<String>> = sqlx::query_scalar(
        "SELECT CASE WHEN address.country_code='IN' AND code.jurisdiction='IN' \
         THEN code.state_code END FROM store_addresses address \
         LEFT JOIN state_codes code ON code.id=address.state_id WHERE address.store_id=?",
    )
    .bind(store_id)
    .fetch_optional(&mut **connection)
    .await?;
    Ok(code.flatten())
}

/// A product's State position on one date, for a store whose premises are `premises`.
pub async fn resolve_state_for_product(
    connection: &mut PoolConnection<Sqlite>,
    product_id: &str,
    date: &str,
    premises: Option<&str>,
) -> Result<StateAnswer, sqlx::Error> {
    let punjab = match premises {
        None => StateAxisAnswer::Undetermined,
        Some(code) if code != PUNJAB_STATE_CODE => StateAxisAnswer::NotApplicable,
        Some(_) => {
            let applies: Option<i64> = sqlx::query_scalar(
                "SELECT applies FROM product_regulatory_classifications \
                 WHERE product_id=? AND scheme='punjab_restricted_supply' AND status='active' \
                   AND effective_from<=? AND (effective_to IS NULL OR effective_to>?)",
            )
            .bind(product_id)
            .bind(date)
            .bind(date)
            .fetch_optional(&mut **connection)
            .await?;
            StateAxisAnswer::Resolved(match applies {
                Some(1) => Resolution::Applies,
                Some(_) => Resolution::DoesNotApply,
                None => Resolution::Unknown,
            })
        }
    };
    Ok(StateAnswer {
        premises_state_code: premises.map(str::to_owned),
        punjab,
    })
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

/// Every manufacturer in force for a product on a date, as `(company_id, display_name)`.
///
/// `resolve_manufacturer` answers "which one does a Sale record", and takes the latest when a
/// product has more than one. A receipt cannot do that: rule 65(21)(b)(v) asks who made the goods
/// that actually arrived, and picking the most recent of two would be a guess about a physical
/// carton. This returns them all, so the caller can record the only one, ask the operator which it
/// was, or record that it is not known.
pub async fn resolve_manufacturer_candidates(
    connection: &mut PoolConnection<Sqlite>,
    product_id: &str,
    business_date: &str,
) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT company.id,company.display_name \
         FROM product_company_roles role \
         JOIN pharmaceutical_companies company ON company.id=role.company_id \
         WHERE role.product_id=? AND role.role='manufacturer' AND role.status='active' \
           AND (role.effective_from IS NULL OR role.effective_from<=?) \
           AND (role.effective_to IS NULL OR role.effective_to>?) \
         ORDER BY company.display_name, company.id",
    )
    .bind(product_id)
    .bind(business_date)
    .bind(business_date)
    .fetch_all(&mut **connection)
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
    fn an_ndps_schedule_h_drug_is_gated_on_the_schedule_not_the_axis() {
        let mut resolved = all(Resolution::DoesNotApply);
        resolved.set("schedule_h", Resolution::Applies);
        resolved.set("ndps_purview", Resolution::Applies);
        assert_eq!(gate("medicine", &resolved), SaleGate::PrescriptionRequired);
    }

    /// Phase 1M-B: Schedule H alone needs a prescription, not a missing workflow.
    #[test]
    fn schedule_h_alone_requires_a_prescription() {
        let mut resolved = all(Resolution::DoesNotApply);
        resolved.set("schedule_h", Resolution::Applies);
        assert_eq!(gate("medicine", &resolved), SaleGate::PrescriptionRequired);
    }

    /// A supported Schedule H requirement never cancels an unsupported one: H with C, H1 or X is
    /// held to the unsupported scheme.
    #[test]
    fn schedule_h_with_an_unsupported_scheme_stays_blocked_on_that_scheme() {
        for other in ["schedule_x", "schedule_c", "schedule_c1"] {
            let mut resolved = all(Resolution::DoesNotApply);
            resolved.set("schedule_h", Resolution::Applies);
            resolved.set(other, Resolution::Applies);
            assert_eq!(
                gate("medicine", &resolved),
                SaleGate::WorkflowUnavailable { scheme: other },
                "{other}"
            );
        }
    }

    /// Schedule H with any other schedule still unknown is unresolved, not merely prescription-bound.
    #[test]
    fn schedule_h_with_an_unknown_schedule_is_unresolved() {
        let mut resolved = all(Resolution::DoesNotApply);
        resolved.set("schedule_h", Resolution::Applies);
        resolved.set("schedule_c", Resolution::Unknown);
        assert_eq!(gate("medicine", &resolved), SaleGate::Unresolved);
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

    /// Phase 1M-C: Schedule H1 whose NDPS purview is established not to apply needs a
    /// prescription (and, at posting, its separate working entry) — it is no longer a missing
    /// workflow.
    #[test]
    fn schedule_h1_outside_ndps_requires_a_prescription() {
        let mut resolved = all(Resolution::DoesNotApply);
        resolved.set("schedule_h1", Resolution::Applies);
        assert_eq!(gate("medicine", &resolved), SaleGate::PrescriptionRequired);
        resolved.set("schedule_h", Resolution::Applies);
        assert_eq!(gate("medicine", &resolved), SaleGate::PrescriptionRequired);
    }

    /// C-04/C-05/C-64: NDPS applies or unknown on a Schedule H1 drug is an unsupported
    /// NDPS-intersection workflow — not a verified H1 requirement, and not Schedule H's rule.
    #[test]
    fn schedule_h1_with_ndps_applying_or_unknown_is_an_unsupported_intersection() {
        for ndps in [Resolution::Applies, Resolution::Unknown] {
            let mut resolved = all(Resolution::DoesNotApply);
            resolved.set("schedule_h1", Resolution::Applies);
            resolved.set("ndps_purview", ndps);
            assert_eq!(
                gate("medicine", &resolved),
                SaleGate::UnsupportedIntersection {
                    reason: "schedule_h1_ndps"
                }
            );
        }
        // Schedule H alone keeps its 1M-B behaviour: NDPS does not gate it.
        let mut h = all(Resolution::DoesNotApply);
        h.set("schedule_h", Resolution::Applies);
        h.set("ndps_purview", Resolution::Unknown);
        assert_eq!(gate("medicine", &h), SaleGate::PrescriptionRequired);
    }

    /// Schedule X, C and C(1) still outrank H1.
    #[test]
    fn schedule_h1_with_x_or_c_stays_blocked_on_that_scheme() {
        for other in WORKFLOW_PENDING_SCHEMES {
            let mut resolved = all(Resolution::DoesNotApply);
            resolved.set("schedule_h1", Resolution::Applies);
            resolved.set(other, Resolution::Applies);
            assert_eq!(
                gate("medicine", &resolved),
                SaleGate::WorkflowUnavailable { scheme: other }
            );
        }
    }

    fn punjab(answer: StateAxisAnswer) -> StateAnswer {
        StateAnswer {
            premises_state_code: Some(PUNJAB_STATE_CODE.to_owned()),
            punjab: answer,
        }
    }

    /// C-69/C-71: in a Punjab store the axis refuses when it applies, and fails closed for a
    /// medicine when it is unknown or the premises State is undetermined; elsewhere it is ignored.
    #[test]
    fn the_punjab_axis_gates_only_where_it_governs() {
        let clear = all(Resolution::DoesNotApply);
        assert_eq!(
            combined_gate(
                "medicine",
                &clear,
                &punjab(StateAxisAnswer::Resolved(Resolution::Applies))
            ),
            SaleGate::StateWorkflowUnavailable {
                scheme: "punjab_restricted_supply"
            }
        );
        assert_eq!(
            combined_gate(
                "medicine",
                &clear,
                &punjab(StateAxisAnswer::Resolved(Resolution::Unknown))
            ),
            SaleGate::StateUnresolved {
                scheme: "punjab_restricted_supply"
            }
        );
        assert_eq!(
            combined_gate(
                "medicine",
                &clear,
                &punjab(StateAxisAnswer::Resolved(Resolution::DoesNotApply))
            ),
            SaleGate::Clear
        );
        let undetermined = StateAnswer {
            premises_state_code: None,
            punjab: StateAxisAnswer::Undetermined,
        };
        assert_eq!(
            combined_gate("medicine", &clear, &undetermined),
            SaleGate::StateUnresolved {
                scheme: "punjab_restricted_supply"
            }
        );
        // A general item is never stopped by an unknown, only by a positive finding.
        assert_eq!(
            combined_gate(
                "general_pharmacy_item",
                &ResolvedRegulatory::unknown(),
                &punjab(StateAxisAnswer::Resolved(Resolution::Unknown))
            ),
            SaleGate::Clear
        );
        let elsewhere = StateAnswer {
            premises_state_code: Some("27".to_owned()),
            punjab: StateAxisAnswer::NotApplicable,
        };
        assert_eq!(
            combined_gate("medicine", &clear, &elsewhere),
            SaleGate::Clear
        );
    }

    /// C-72: the central H1 answer and the Punjab answer are independent both ways, and the central
    /// refusals outrank the State one.
    #[test]
    fn the_central_and_state_axes_stay_independent() {
        let mut h1 = all(Resolution::DoesNotApply);
        h1.set("schedule_h1", Resolution::Applies);
        assert_eq!(
            combined_gate(
                "medicine",
                &h1,
                &punjab(StateAxisAnswer::Resolved(Resolution::DoesNotApply))
            ),
            SaleGate::PrescriptionRequired
        );
        assert_eq!(
            combined_gate(
                "medicine",
                &h1,
                &punjab(StateAxisAnswer::Resolved(Resolution::Applies))
            ),
            SaleGate::StateWorkflowUnavailable {
                scheme: "punjab_restricted_supply"
            }
        );
        // The central snapshot never carries the State axis.
        assert!(!h1.to_snapshot_json().contains("punjab"));
        let mut x = all(Resolution::DoesNotApply);
        x.set("schedule_x", Resolution::Applies);
        assert_eq!(
            combined_gate(
                "medicine",
                &x,
                &punjab(StateAxisAnswer::Resolved(Resolution::Applies))
            ),
            SaleGate::WorkflowUnavailable {
                scheme: "schedule_x"
            }
        );
    }

    #[test]
    fn the_state_snapshot_is_exact() {
        assert_eq!(
            punjab(StateAxisAnswer::Resolved(Resolution::DoesNotApply)).to_snapshot_json(),
            "{\"premises_state_code\":\"03\",\"punjab_restricted_supply\":\"does_not_apply\"}"
        );
        assert_eq!(
            StateAnswer {
                premises_state_code: None,
                punjab: StateAxisAnswer::Undetermined
            }
            .to_snapshot_json(),
            "{\"premises_state_code\":null,\"punjab_restricted_supply\":\"undetermined\"}"
        );
    }

    #[test]
    fn an_unrecognised_scheme_answers_unknown_rather_than_guessing() {
        assert_eq!(
            ResolvedRegulatory::unknown().answer("schedule_z"),
            Resolution::Unknown
        );
    }
}
