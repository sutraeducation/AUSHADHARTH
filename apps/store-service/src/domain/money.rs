//! Phase 1G commercial and tax arithmetic. The single authoritative implementation of ADR-016.
//!
//! Every figure a purchase posts is computed here and nowhere else, in checked integer arithmetic.
//! No `f32` or `f64` appears in this module or anywhere in the money path.
//!
//! The arithmetic is arranged so exactly one operation can produce a remainder — the tax component
//! division — because every additional rounding point is another place two systems disagree by a
//! paise.

/// Basis points are hundredths of a percent: `10_000` is 100.00%.
pub const BASIS_POINT_SCALE: i64 = 10_000;

/// The largest purchase rate accepted, matching the frozen MRP bound.
pub const MAX_RATE_PER_PACK_PAISE: i64 = 100_000_000_000;

/// The largest pack count one line may carry.
pub const MAX_QUANTITY_PACKS: i64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoneyError {
    /// An intermediate exceeded `i64`. Refused rather than wrapped.
    Overflow,
    InvalidQuantity,
    InvalidRate,
}

/// How a document's tax is split, decided by comparing the two places of supply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaxTreatment {
    IntraState,
    InterState,
}

impl TaxTreatment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IntraState => "intra_state",
            Self::InterState => "inter_state",
        }
    }

    /// Same State is an intra-state supply; anything else is inter-state.
    ///
    /// Compared on the two-character State codes rather than the row identifiers, because the code
    /// is the thing GST actually reasons about and is what a posted document snapshots.
    pub fn from_state_codes(store_state_code: &str, supplier_state_code: &str) -> Self {
        if store_state_code == supplier_state_code {
            Self::IntraState
        } else {
            Self::InterState
        }
    }
}

/// The four basis-point components of a resolved rate version, before treatment is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RateComponents {
    pub cgst_basis_points: i64,
    pub sgst_basis_points: i64,
    pub igst_basis_points: i64,
    pub cess_basis_points: i64,
}

/// One line's computed money. Every field is exact integer paise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineAmounts {
    pub taxable_value_paise: i64,
    pub cgst_paise: i64,
    pub sgst_paise: i64,
    pub igst_paise: i64,
    pub cess_paise: i64,
    pub line_total_paise: i64,
}

/// Integer division rounding halves away from zero, per ADR-016 §2.
///
/// `0.5 → 1`, `1.5 → 2`, `−0.5 → −1`. This is the only rounding in the system. The direction
/// follows the one rounding direction Indian GST law states — section 170 rounds a part of a rupee
/// of fifty paise or more upward — applied here at the paise level as a documented project
/// decision, not as a claim that statute mandates paise-level rounding.
///
/// The comparison is `2·|remainder| >= |divisor|`, which never multiplies the dividend and so
/// cannot overflow for any inputs the caller has already bounded.
pub fn divide_rounding_half_away_from_zero(dividend: i64, divisor: i64) -> Result<i64, MoneyError> {
    debug_assert!(divisor > 0, "divisor is always a positive scale");
    let quotient = dividend.checked_div(divisor).ok_or(MoneyError::Overflow)?;
    let remainder = dividend.checked_rem(divisor).ok_or(MoneyError::Overflow)?;
    if remainder == 0 {
        return Ok(quotient);
    }
    let doubled = remainder.checked_abs().ok_or(MoneyError::Overflow)?;
    let rounds_away = doubled.checked_mul(2).ok_or(MoneyError::Overflow)? >= divisor;
    if !rounds_away {
        return Ok(quotient);
    }
    if dividend < 0 {
        quotient.checked_sub(1).ok_or(MoneyError::Overflow)
    } else {
        quotient.checked_add(1).ok_or(MoneyError::Overflow)
    }
}

/// One tax component: `taxable × basis_points / 10_000`, rounded once.
pub fn tax_component_paise(taxable_value_paise: i64, basis_points: i64) -> Result<i64, MoneyError> {
    if basis_points == 0 {
        return Ok(0);
    }
    let product = taxable_value_paise
        .checked_mul(basis_points)
        .ok_or(MoneyError::Overflow)?;
    divide_rounding_half_away_from_zero(product, BASIS_POINT_SCALE)
}

/// Exact taxable value: an integer count of Packs times an integer paise rate for one Pack.
///
/// No rounding occurs here, which is precisely why the quantity is a Pack count and the rate is per
/// Pack. A per-base-unit rate or a fractional quantity would introduce a second rounding point.
pub fn taxable_value_paise(
    quantity_packs: i64,
    rate_per_pack_paise: i64,
) -> Result<i64, MoneyError> {
    if !(1..=MAX_QUANTITY_PACKS).contains(&quantity_packs) {
        return Err(MoneyError::InvalidQuantity);
    }
    if !(0..=MAX_RATE_PER_PACK_PAISE).contains(&rate_per_pack_paise) {
        return Err(MoneyError::InvalidRate);
    }
    quantity_packs
        .checked_mul(rate_per_pack_paise)
        .ok_or(MoneyError::Overflow)
}

/// Exact inventory quantity in base-unit atoms, from the frozen Pack conversion.
pub fn quantity_atoms(quantity_packs: i64, base_quantity_atoms: i64) -> Result<i64, MoneyError> {
    if !(1..=MAX_QUANTITY_PACKS).contains(&quantity_packs) {
        return Err(MoneyError::InvalidQuantity);
    }
    quantity_packs
        .checked_mul(base_quantity_atoms)
        .ok_or(MoneyError::Overflow)
}

/// Computes one line, charging only the components the treatment allows.
///
/// Intra-state charges CGST and SGST and leaves IGST at zero; inter-state charges IGST and leaves
/// CGST and SGST at zero. Cess applies under either. A zero-rate category — exempt, nil-rated, or
/// non-GST — simply carries zero basis points and therefore zero tax, which is a positive assertion
/// by the classification rather than an absence of data.
pub fn compute_line(
    quantity_packs: i64,
    rate_per_pack_paise: i64,
    treatment: TaxTreatment,
    rate: RateComponents,
) -> Result<LineAmounts, MoneyError> {
    let taxable = taxable_value_paise(quantity_packs, rate_per_pack_paise)?;
    let (cgst, sgst, igst) = match treatment {
        TaxTreatment::IntraState => (
            tax_component_paise(taxable, rate.cgst_basis_points)?,
            tax_component_paise(taxable, rate.sgst_basis_points)?,
            0,
        ),
        TaxTreatment::InterState => (0, 0, tax_component_paise(taxable, rate.igst_basis_points)?),
    };
    let cess = tax_component_paise(taxable, rate.cess_basis_points)?;
    let total = [taxable, cgst, sgst, igst, cess]
        .into_iter()
        .try_fold(0_i64, |sum, part| sum.checked_add(part))
        .ok_or(MoneyError::Overflow)?;
    Ok(LineAmounts {
        taxable_value_paise: taxable,
        cgst_paise: cgst,
        sgst_paise: sgst,
        igst_paise: igst,
        cess_paise: cess,
        line_total_paise: total,
    })
}

/// Document totals are the sum of the already-rounded line values.
///
/// A total is never produced by applying a rate to an aggregate. That is what makes the printed
/// total always equal the sum of its parts, and it is why no reconciliation line is needed.
pub fn sum_lines(lines: &[LineAmounts]) -> Result<LineAmounts, MoneyError> {
    lines
        .iter()
        .try_fold(LineAmounts::default(), |mut total, line| {
            total.taxable_value_paise = total
                .taxable_value_paise
                .checked_add(line.taxable_value_paise)?;
            total.cgst_paise = total.cgst_paise.checked_add(line.cgst_paise)?;
            total.sgst_paise = total.sgst_paise.checked_add(line.sgst_paise)?;
            total.igst_paise = total.igst_paise.checked_add(line.igst_paise)?;
            total.cess_paise = total.cess_paise.checked_add(line.cess_paise)?;
            total.line_total_paise = total.line_total_paise.checked_add(line.line_total_paise)?;
            Some(total)
        })
        .ok_or(MoneyError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every vector from the ADR-016 table, asserted exactly as published.
    #[test]
    fn adr_016_rounding_vectors_hold() {
        let vectors: [(i64, i64, i64); 9] = [
            (10_000, 600, 600),             // 1  exact
            (1, 250, 0),                    // 2  below half
            (1, 5_000, 1),                  // 3  exactly half
            (3, 5_000, 2),                  // 4  exactly half, odd quotient
            (1, 7_500, 1),                  // 5  above half
            (12_345, 250, 309),             // 6  odd basis points
            (123_456_789, 900, 11_111_111), // 7  high value
            (7, 250, 0),                    // 8  low value
            (50_000, 0, 0),                 // 9  nil rate
        ];
        for (taxable, basis_points, expected) in vectors {
            assert_eq!(
                tax_component_paise(taxable, basis_points).unwrap(),
                expected,
                "taxable {taxable} at {basis_points} bp"
            );
        }
    }

    /// Vector 10: the negative case exists for the reversal arithmetic a later phase will need.
    #[test]
    fn halves_round_away_from_zero_in_both_directions() {
        assert_eq!(
            divide_rounding_half_away_from_zero(5_000, 10_000).unwrap(),
            1
        );
        assert_eq!(
            divide_rounding_half_away_from_zero(-5_000, 10_000).unwrap(),
            -1
        );
        assert_eq!(
            divide_rounding_half_away_from_zero(15_000, 10_000).unwrap(),
            2
        );
        assert_eq!(
            divide_rounding_half_away_from_zero(-15_000, 10_000).unwrap(),
            -2
        );
        // Just below half stays put, in both directions.
        assert_eq!(
            divide_rounding_half_away_from_zero(4_999, 10_000).unwrap(),
            0
        );
        assert_eq!(
            divide_rounding_half_away_from_zero(-4_999, 10_000).unwrap(),
            0
        );
    }

    /// The case where per-line rounding is observable, and where rounding an aggregate would diverge.
    #[test]
    fn document_totals_sum_rounded_lines_rather_than_rounding_the_aggregate() {
        let rate = RateComponents {
            cgst_basis_points: 5_000,
            ..RateComponents::default()
        };
        // Two lines of one paise each, at 50%: each rounds to 1, so the document shows 2.
        let a = compute_line(1, 1, TaxTreatment::IntraState, rate).unwrap();
        let b = compute_line(1, 1, TaxTreatment::IntraState, rate).unwrap();
        assert_eq!(a.cgst_paise, 1);
        assert_eq!(b.cgst_paise, 1);

        let total = sum_lines(&[a, b]).unwrap();
        assert_eq!(total.taxable_value_paise, 2);
        assert_eq!(total.cgst_paise, 2, "sum of rounded lines");

        // Rounding the aggregate instead would give 1, which is exactly the divergence ADR-016 §5
        // exists to prevent.
        assert_eq!(tax_component_paise(2, 5_000).unwrap(), 1);
    }

    #[test]
    fn intra_state_never_charges_igst_and_inter_state_never_charges_cgst_or_sgst() {
        let rate = RateComponents {
            cgst_basis_points: 600,
            sgst_basis_points: 600,
            igst_basis_points: 1_200,
            cess_basis_points: 100,
        };
        // 5 packs at 80.00 each is 400.00 taxable.
        let intra = compute_line(5, 8_000, TaxTreatment::IntraState, rate).unwrap();
        assert_eq!(intra.taxable_value_paise, 40_000);
        assert_eq!(intra.cgst_paise, 2_400);
        assert_eq!(intra.sgst_paise, 2_400);
        assert_eq!(intra.igst_paise, 0, "intra-state must not charge IGST");
        assert_eq!(intra.cess_paise, 400);
        assert_eq!(intra.line_total_paise, 40_000 + 2_400 + 2_400 + 400);

        let inter = compute_line(5, 8_000, TaxTreatment::InterState, rate).unwrap();
        assert_eq!(inter.igst_paise, 4_800);
        assert_eq!(inter.cgst_paise, 0, "inter-state must not charge CGST");
        assert_eq!(inter.sgst_paise, 0, "inter-state must not charge SGST");
        assert_eq!(inter.cess_paise, 400, "cess applies either way");
        // The two treatments collect the same total tax; only the split differs.
        assert_eq!(inter.line_total_paise, intra.line_total_paise);
    }

    #[test]
    fn a_zero_rate_classification_yields_zero_tax_without_any_special_case() {
        let zero = RateComponents::default();
        let line = compute_line(10, 12_345, TaxTreatment::IntraState, zero).unwrap();
        assert_eq!(line.taxable_value_paise, 123_450);
        assert_eq!(line.cgst_paise, 0);
        assert_eq!(line.sgst_paise, 0);
        assert_eq!(line.igst_paise, 0);
        assert_eq!(line.cess_paise, 0);
        // A zero-rated line still has a total: the goods were still bought.
        assert_eq!(line.line_total_paise, 123_450);
    }

    #[test]
    fn quantity_and_atoms_are_exact_and_bounded() {
        assert_eq!(taxable_value_paise(5, 8_000).unwrap(), 40_000);
        assert_eq!(quantity_atoms(5, 10).unwrap(), 50);
        // A strip of ten, bought five times, is fifty tablets — the ledger's unit.
        assert_eq!(quantity_atoms(1, 1).unwrap(), 1);

        assert_eq!(
            taxable_value_paise(0, 100),
            Err(MoneyError::InvalidQuantity)
        );
        assert_eq!(
            taxable_value_paise(-1, 100),
            Err(MoneyError::InvalidQuantity)
        );
        assert_eq!(
            taxable_value_paise(MAX_QUANTITY_PACKS + 1, 100),
            Err(MoneyError::InvalidQuantity)
        );
        assert_eq!(taxable_value_paise(1, -1), Err(MoneyError::InvalidRate));
        assert_eq!(
            taxable_value_paise(1, MAX_RATE_PER_PACK_PAISE + 1),
            Err(MoneyError::InvalidRate)
        );
        // A free line is legitimate: zero rate is a price, not a missing value.
        assert_eq!(taxable_value_paise(5, 0).unwrap(), 0);
    }

    #[test]
    fn overflow_is_refused_rather_than_wrapped() {
        assert_eq!(
            quantity_atoms(MAX_QUANTITY_PACKS, i64::MAX),
            Err(MoneyError::Overflow)
        );
        assert_eq!(
            tax_component_paise(i64::MAX, 10_000),
            Err(MoneyError::Overflow)
        );
        let huge = LineAmounts {
            taxable_value_paise: i64::MAX,
            line_total_paise: i64::MAX,
            ..LineAmounts::default()
        };
        assert_eq!(sum_lines(&[huge, huge]), Err(MoneyError::Overflow));
    }

    #[test]
    fn treatment_is_decided_by_comparing_state_codes() {
        assert_eq!(
            TaxTreatment::from_state_codes("27", "27"),
            TaxTreatment::IntraState
        );
        assert_eq!(
            TaxTreatment::from_state_codes("27", "29"),
            TaxTreatment::InterState
        );
        assert_eq!(TaxTreatment::IntraState.as_str(), "intra_state");
        assert_eq!(TaxTreatment::InterState.as_str(), "inter_state");
    }

    /// The widest intermediate ADR-016 §7 bounds: a maximal taxable value at a maximal rate.
    #[test]
    fn the_documented_widest_intermediate_stays_inside_i64() {
        let taxable = taxable_value_paise(1, MAX_RATE_PER_PACK_PAISE).unwrap();
        let component = tax_component_paise(taxable, BASIS_POINT_SCALE).unwrap();
        assert_eq!(component, MAX_RATE_PER_PACK_PAISE, "100% of the maximum");
        assert!(taxable.checked_mul(BASIS_POINT_SCALE).is_some());
    }
}
