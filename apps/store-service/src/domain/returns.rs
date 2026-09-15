//! Phase 1I return arithmetic.
//!
//! A return reverses part of an immutable original document. Two rules shape everything here:
//!
//! 1. **The original is the only source of value.** A return never resolves a current rate, price or
//!    master record. It reproduces a proportion of figures that were frozen when the original was
//!    posted, so a reversal stays equal and opposite even if a rate version changed in between.
//! 2. **Proportions are taken cumulatively, never independently.** ADR-016 rounds once per tax
//!    component per line, so rounding each partial return on its own lets three partials sum to a
//!    paisa more or less than the original. Every amount here is therefore the *difference between
//!    two cumulative totals*, which makes the arithmetic self-correcting: the residual lands on the
//!    last return by construction, and a fully returned line reverses the original exactly.

/// Why a reversal could not be computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnMoneyError {
    /// The original line's quantity was not a positive integer.
    InvalidOriginalQuantity,
    /// The requested quantity is not positive, or exceeds what is left to return.
    InvalidReturnQuantity,
    /// An original amount was negative, which the schema forbids.
    InvalidOriginalAmount,
    /// The arithmetic could not be represented.
    Overflow,
}

/// The frozen money on one original line, as the posted document recorded it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OriginalLineAmounts {
    pub quantity_atoms: i64,
    pub taxable_value_paise: i64,
    pub cgst_paise: i64,
    pub sgst_paise: i64,
    pub igst_paise: i64,
    pub cess_paise: i64,
}

/// What one return line reverses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReversedAmounts {
    pub taxable_value_paise: i64,
    pub cgst_paise: i64,
    pub sgst_paise: i64,
    pub igst_paise: i64,
    pub cess_paise: i64,
    pub line_total_paise: i64,
}

/// How much of an original line is still returnable.
///
/// `already_returned_atoms` is the sum over **posted** returns against that line. A draft return
/// reserves nothing: two operators may both draft a return for the last strip, and the posting
/// transaction decides which one gets it.
pub fn returnable_atoms(
    original_atoms: i64,
    already_returned_atoms: i64,
) -> Result<i64, ReturnMoneyError> {
    if original_atoms < 1 {
        return Err(ReturnMoneyError::InvalidOriginalQuantity);
    }
    if already_returned_atoms < 0 || already_returned_atoms > original_atoms {
        return Err(ReturnMoneyError::InvalidReturnQuantity);
    }
    Ok(original_atoms - already_returned_atoms)
}

/// The cumulative share of one frozen amount after `returned_atoms` of `original_atoms` are back.
///
/// `round(original × returned / original_atoms)`, half away from zero, evaluated in `i128`: the
/// numerator reaches `10^15 × 9×10^15 = 9×10^30` for schema-legal inputs, which `i64` cannot hold.
/// There is no division until the final rounding step, so nothing is lost on the way.
fn amount_to_date(
    original_amount_paise: i64,
    returned_atoms: i64,
    original_atoms: i64,
) -> Result<i64, ReturnMoneyError> {
    if original_atoms < 1 {
        return Err(ReturnMoneyError::InvalidOriginalQuantity);
    }
    if !(0..=original_atoms).contains(&returned_atoms) {
        return Err(ReturnMoneyError::InvalidReturnQuantity);
    }
    if original_amount_paise < 0 {
        return Err(ReturnMoneyError::InvalidOriginalAmount);
    }
    let quantity = i128::from(original_atoms);
    let numerator = i128::from(original_amount_paise)
        .checked_mul(i128::from(returned_atoms))
        .ok_or(ReturnMoneyError::Overflow)?;
    // Both operands are non-negative, so flooring `(2n + q) / 2q` rounds half away from zero.
    let rounded = numerator
        .checked_mul(2)
        .ok_or(ReturnMoneyError::Overflow)?
        .checked_add(quantity)
        .ok_or(ReturnMoneyError::Overflow)?
        / (quantity * 2);
    i64::try_from(rounded).map_err(|_| ReturnMoneyError::Overflow)
}

/// Reverses `this_return_atoms` of an original line that already has `already_returned_atoms` back.
///
/// Each tax component is reversed by cumulative difference — `share(before + now) − share(before)` —
/// independently, exactly as ADR-016 rounds each component independently on the original.
///
/// The line total is then the **sum of the reversed components**, not a sixth independent
/// proportion. A document whose total does not equal its own parts is unreadable, and a paisa of
/// proportional drift is a far smaller sin than a bill that does not add up. Because each component
/// reverses to exactly its original figure once the line is fully returned, the summed total also
/// reverses exactly.
pub fn reverse_line(
    original: &OriginalLineAmounts,
    already_returned_atoms: i64,
    this_return_atoms: i64,
) -> Result<ReversedAmounts, ReturnMoneyError> {
    let quantity = original.quantity_atoms;
    if quantity < 1 {
        return Err(ReturnMoneyError::InvalidOriginalQuantity);
    }
    if this_return_atoms < 1 {
        return Err(ReturnMoneyError::InvalidReturnQuantity);
    }
    let after = already_returned_atoms
        .checked_add(this_return_atoms)
        .ok_or(ReturnMoneyError::Overflow)?;
    if after > quantity {
        return Err(ReturnMoneyError::InvalidReturnQuantity);
    }

    let component = |amount: i64| -> Result<i64, ReturnMoneyError> {
        let to_date = amount_to_date(amount, after, quantity)?;
        let before = amount_to_date(amount, already_returned_atoms, quantity)?;
        Ok(to_date - before)
    };

    let taxable_value_paise = component(original.taxable_value_paise)?;
    let cgst_paise = component(original.cgst_paise)?;
    let sgst_paise = component(original.sgst_paise)?;
    let igst_paise = component(original.igst_paise)?;
    let cess_paise = component(original.cess_paise)?;

    let line_total_paise = [
        taxable_value_paise,
        cgst_paise,
        sgst_paise,
        igst_paise,
        cess_paise,
    ]
    .into_iter()
    .try_fold(0_i64, |total, part| total.checked_add(part))
    .ok_or(ReturnMoneyError::Overflow)?;

    Ok(ReversedAmounts {
        taxable_value_paise,
        cgst_paise,
        sgst_paise,
        igst_paise,
        cess_paise,
        line_total_paise,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two strips at 80.00 with 6% + 6%: 160.00 taxable, 9.60 each side, 179.20 in all — the
    /// worked example from the Phase 1H tests, so the two phases agree on the same line.
    fn original(quantity_atoms: i64) -> OriginalLineAmounts {
        OriginalLineAmounts {
            quantity_atoms,
            taxable_value_paise: 16_000,
            cgst_paise: 960,
            sgst_paise: 960,
            igst_paise: 0,
            cess_paise: 0,
        }
    }

    #[test]
    fn returnable_quantity_is_the_original_less_what_is_already_back() {
        assert_eq!(returnable_atoms(20, 0), Ok(20));
        assert_eq!(returnable_atoms(20, 7), Ok(13));
        assert_eq!(returnable_atoms(20, 20), Ok(0));
        // Nothing may claim more has come back than ever went out.
        assert_eq!(
            returnable_atoms(20, 21),
            Err(ReturnMoneyError::InvalidReturnQuantity)
        );
        assert_eq!(
            returnable_atoms(0, 0),
            Err(ReturnMoneyError::InvalidOriginalQuantity)
        );
    }

    #[test]
    fn a_full_return_reverses_the_original_line_exactly() {
        let line = original(20);
        let reversed = reverse_line(&line, 0, 20).unwrap();
        assert_eq!(reversed.taxable_value_paise, 16_000);
        assert_eq!(reversed.cgst_paise, 960);
        assert_eq!(reversed.sgst_paise, 960);
        assert_eq!(reversed.line_total_paise, 17_920);
    }

    #[test]
    fn a_half_return_reverses_half_of_each_component() {
        let reversed = reverse_line(&original(20), 0, 10).unwrap();
        assert_eq!(reversed.taxable_value_paise, 8_000);
        assert_eq!(reversed.cgst_paise, 480);
        assert_eq!(reversed.sgst_paise, 480);
        assert_eq!(reversed.line_total_paise, 8_960);
    }

    /// The property that makes cumulative difference necessary: several partial returns must sum to
    /// the original exactly, for every component, with no drift and no residual left behind.
    #[test]
    fn partial_returns_sum_to_the_original_however_they_are_split() {
        // A quantity and amounts chosen so the per-unit share is not a whole paisa: 17 atoms of a
        // 1000-paise line is 58.8235… paise each.
        let line = OriginalLineAmounts {
            quantity_atoms: 17,
            taxable_value_paise: 1_000,
            cgst_paise: 63,
            sgst_paise: 63,
            igst_paise: 0,
            cess_paise: 7,
        };

        for split in [
            vec![17],
            vec![1, 16],
            vec![8, 9],
            vec![5, 5, 7],
            vec![1, 1, 1, 14],
            vec![3, 3, 3, 3, 5],
            vec![1; 17],
        ] {
            let mut returned = 0;
            let mut totals = (0_i64, 0_i64, 0_i64, 0_i64, 0_i64, 0_i64);
            for part in &split {
                let reversed = reverse_line(&line, returned, *part).unwrap();
                totals.0 += reversed.taxable_value_paise;
                totals.1 += reversed.cgst_paise;
                totals.2 += reversed.sgst_paise;
                totals.3 += reversed.igst_paise;
                totals.4 += reversed.cess_paise;
                totals.5 += reversed.line_total_paise;
                returned += part;
            }
            assert_eq!(returned, line.quantity_atoms, "{split:?}");
            assert_eq!(totals.0, line.taxable_value_paise, "taxable: {split:?}");
            assert_eq!(totals.1, line.cgst_paise, "cgst: {split:?}");
            assert_eq!(totals.2, line.sgst_paise, "sgst: {split:?}");
            assert_eq!(totals.3, line.igst_paise, "igst: {split:?}");
            assert_eq!(totals.4, line.cess_paise, "cess: {split:?}");
            // And the reversed totals add up to the original total, which is what a reader checks.
            let original_total = line.taxable_value_paise
                + line.cgst_paise
                + line.sgst_paise
                + line.igst_paise
                + line.cess_paise;
            assert_eq!(totals.5, original_total, "total: {split:?}");
        }
    }

    /// Independent per-return rounding would drift here; cumulative difference does not.
    ///
    /// Three atoms of a 10-paise line: an independently rounded third is 3 paise each, summing to 9
    /// and losing a paisa. The cumulative differences are 3, 4, 3 and sum to 10.
    #[test]
    fn the_residual_lands_on_the_last_return_rather_than_being_lost() {
        let line = OriginalLineAmounts {
            quantity_atoms: 3,
            taxable_value_paise: 10,
            cgst_paise: 0,
            sgst_paise: 0,
            igst_paise: 0,
            cess_paise: 0,
        };
        let first = reverse_line(&line, 0, 1).unwrap().taxable_value_paise;
        let second = reverse_line(&line, 1, 1).unwrap().taxable_value_paise;
        let third = reverse_line(&line, 2, 1).unwrap().taxable_value_paise;
        assert_eq!((first, second, third), (3, 4, 3));
        assert_eq!(first + second + third, 10);
    }

    #[test]
    fn a_return_can_never_exceed_what_is_left() {
        let line = original(20);
        assert_eq!(
            reverse_line(&line, 0, 21),
            Err(ReturnMoneyError::InvalidReturnQuantity)
        );
        assert_eq!(
            reverse_line(&line, 15, 6),
            Err(ReturnMoneyError::InvalidReturnQuantity)
        );
        assert_eq!(
            reverse_line(&line, 0, 0),
            Err(ReturnMoneyError::InvalidReturnQuantity)
        );
        // Exactly the remainder is fine.
        assert!(reverse_line(&line, 15, 5).is_ok());
    }

    #[test]
    fn rounding_is_half_away_from_zero_on_the_cumulative_share() {
        // 1 of 2 atoms of a 5-paise line: 2.5 rounds away from zero to 3.
        let line = OriginalLineAmounts {
            quantity_atoms: 2,
            taxable_value_paise: 5,
            cgst_paise: 0,
            sgst_paise: 0,
            igst_paise: 0,
            cess_paise: 0,
        };
        assert_eq!(reverse_line(&line, 0, 1).unwrap().taxable_value_paise, 3);
        // And the second half takes the remaining 2, so the pair still sums to 5.
        assert_eq!(reverse_line(&line, 1, 1).unwrap().taxable_value_paise, 2);
    }

    /// The numerator reaches 9×10^30 at the schema's own bounds, which only `i128` can hold.
    #[test]
    fn schema_legal_extremes_do_not_overflow() {
        let line = OriginalLineAmounts {
            quantity_atoms: 9_000_000_000_000_000,
            taxable_value_paise: 1_000_000_000_000_000,
            cgst_paise: 0,
            sgst_paise: 0,
            igst_paise: 0,
            cess_paise: 0,
        };
        let half = reverse_line(&line, 0, 4_500_000_000_000_000).unwrap();
        assert_eq!(half.taxable_value_paise, 500_000_000_000_000);
        let rest = reverse_line(&line, 4_500_000_000_000_000, 4_500_000_000_000_000).unwrap();
        assert_eq!(
            half.taxable_value_paise + rest.taxable_value_paise,
            line.taxable_value_paise
        );
    }

    #[test]
    fn a_zero_rated_line_reverses_to_zero_tax_without_complaint() {
        let line = OriginalLineAmounts {
            quantity_atoms: 10,
            taxable_value_paise: 5_000,
            cgst_paise: 0,
            sgst_paise: 0,
            igst_paise: 0,
            cess_paise: 0,
        };
        let reversed = reverse_line(&line, 0, 4).unwrap();
        assert_eq!(reversed.taxable_value_paise, 2_000);
        assert_eq!(reversed.cgst_paise, 0);
        assert_eq!(reversed.line_total_paise, 2_000);
    }
}
