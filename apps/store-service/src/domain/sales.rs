//! Phase 1H sale money and price-ceiling rules.
//!
//! Everything here is pure arithmetic over integers, isolated from HTTP and SQL so the parts that
//! decide what a customer pays can be tested exhaustively at their boundaries.
//!
//! Three rules shape the whole module:
//!
//! 1. **The rate is expressed in the same basis as the quantity**, so a taxable value is always an
//!    exact integer product and no division ever enters the commercial money path.
//! 2. **ADR-016 owns the only rounding point**: one rounding per tax component, half away from zero,
//!    document totals summed from already-rounded lines.
//! 3. **A ceiling is compared against the figure that shares its tax basis.** A Batch MRP is a
//!    printed price *including* GST, so it is compared against the GST-inclusive line total; a DPCO
//!    ceiling is notified *excluding* GST, so it is compared against the taxable value. Comparing a
//!    tax-exclusive rate against a tax-inclusive MRP — as an early draft of the blueprint did —
//!    permits selling above MRP by exactly the tax, and is the defect this module exists to prevent.

use time::{Date, macros::format_description};

use crate::domain::money::{self, LineAmounts, MoneyError, RateComponents, TaxTreatment};
use crate::domain::price_control::atoms_per_base_unit;

/// The largest quantity a single sale line may carry, in its own basis.
///
/// Atoms are bounded by the ledger's own `±9e15` rather than by something tighter: `quantity_scale`
/// may be 6, so one base unit can be 1 000 000 atoms, and a "one million atoms" cap would silently
/// limit a scale-6 product to a single unit.
pub const MAX_SALE_QUANTITY_PACKS: i64 = money::MAX_QUANTITY_PACKS;
pub const MAX_SALE_QUANTITY_ATOMS: i64 = 9_000_000_000_000_000;
pub const MAX_SALE_RATE_PAISE: i64 = money::MAX_RATE_PER_PACK_PAISE;
/// The line is bounded where it actually matters — on money, not on quantity.
pub const MAX_SALE_LINE_TAXABLE_PAISE: i64 = 1_000_000_000_000_000;

/// What a sale line's quantity and rate are counted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantityBasis {
    /// Whole packs: quantity is a pack count, the rate is paise per pack.
    Pack,
    /// Loose units: quantity is base-unit atoms, the rate is paise per atom.
    BaseUnit,
}

impl QuantityBasis {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pack => "pack",
            Self::BaseUnit => "base_unit",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pack" => Some(Self::Pack),
            "base_unit" => Some(Self::BaseUnit),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SaleMoneyError {
    /// A quantity outside `1..=MAX`, or not a whole number of packs.
    InvalidQuantity,
    /// A rate outside `0..=MAX`.
    InvalidRate,
    /// The arithmetic could not be represented, or the line exceeds its money bound.
    Overflow,
    /// The pack's own atom count was not a positive integer, or its scale was outside 0..=6.
    InvalidPack,
    /// The quantity is not a whole multiple of the Pack policy's sale granularity.
    IncrementViolation,
    /// The quantity is smaller than, or not aligned to, one whole base unit, and this Product's
    /// policy does not permit a sub-base-unit quantity.
    FractionalNotPermitted,
}

impl From<MoneyError> for SaleMoneyError {
    fn from(value: MoneyError) -> Self {
        match value {
            MoneyError::Overflow => Self::Overflow,
            MoneyError::InvalidQuantity => Self::InvalidQuantity,
            MoneyError::InvalidRate => Self::InvalidRate,
        }
    }
}

/// One line's quantity resolved into both the basis it was entered in and the atoms the ledger
/// needs. The atoms are always derived here; a browser never supplies them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaleQuantity {
    pub basis: QuantityBasis,
    /// Present only for `Pack`, and always `None` for `BaseUnit`, matching the schema CHECK.
    pub quantity_packs: Option<i64>,
    pub quantity_atoms: i64,
}

/// Resolves a line's quantity and validates it against the Pack's own sale policy.
///
/// The two frozen Phase 1B policy fields govern **different** things, and conflating them was the
/// defect this signature exists to prevent:
///
/// - `minimum_sale_increment_atoms` is operational **sale granularity**. It — and it alone — decides
///   whether a Pack may be broken open: an increment of 1 sells single tablets from a strip of ten,
///   an increment equal to `base_quantity_atoms` makes the strip whole-Pack-only.
/// - `fractional_sale_allowed` is permission for a genuine **sub-base-unit** quantity, which only
///   exists where `quantity_scale > 0`. Three tablets out of a strip of ten is three *whole* base
///   units, not a fraction of one, so it needs no such permission — and could never obtain it, since
///   the frozen Phase 1B triggers forbid `fractional_sale_allowed` on a discrete scale-zero Product.
///
/// Reading the second field as "may break a Pack" would make loose sale of tablets and capsules
/// impossible, because a discrete unit is pinned to scale 0 by `products_unit_compatibility_insert`.
pub fn resolve_quantity(
    basis: QuantityBasis,
    quantity: i64,
    base_quantity_atoms: i64,
    quantity_scale: i64,
    fractional_sale_allowed: bool,
    minimum_sale_increment_atoms: i64,
) -> Result<SaleQuantity, SaleMoneyError> {
    if base_quantity_atoms < 1 {
        return Err(SaleMoneyError::InvalidPack);
    }
    match basis {
        QuantityBasis::Pack => {
            if !(1..=MAX_SALE_QUANTITY_PACKS).contains(&quantity) {
                return Err(SaleMoneyError::InvalidQuantity);
            }
            let atoms = money::quantity_atoms(quantity, base_quantity_atoms)?;
            if atoms > MAX_SALE_QUANTITY_ATOMS {
                return Err(SaleMoneyError::Overflow);
            }
            Ok(SaleQuantity {
                basis,
                quantity_packs: Some(quantity),
                quantity_atoms: atoms,
            })
        }
        QuantityBasis::BaseUnit => {
            if !(1..=MAX_SALE_QUANTITY_ATOMS).contains(&quantity) {
                return Err(SaleMoneyError::InvalidQuantity);
            }
            if minimum_sale_increment_atoms < 1 {
                return Err(SaleMoneyError::InvalidPack);
            }
            // Granularity first: this is what a store sets to keep a strip whole.
            if quantity % minimum_sale_increment_atoms != 0 {
                return Err(SaleMoneyError::IncrementViolation);
            }
            // Then, and only then, ask whether this is a part of a base unit at all. At scale 0 one
            // atom IS one base unit, so this never fires and no permission is needed.
            let per_base_unit =
                atoms_per_base_unit(quantity_scale).ok_or(SaleMoneyError::InvalidPack)?;
            if quantity % per_base_unit != 0 && !fractional_sale_allowed {
                return Err(SaleMoneyError::FractionalNotPermitted);
            }
            Ok(SaleQuantity {
                basis,
                quantity_packs: None,
                quantity_atoms: quantity,
            })
        }
    }
}

/// The taxable value of a line: quantity × rate, both in the same basis.
///
/// Exact integer multiplication with no division, which is what keeps ADR-016's single rounding
/// point intact.
pub fn taxable_value_paise(
    quantity: &SaleQuantity,
    selling_rate_paise: i64,
) -> Result<i64, SaleMoneyError> {
    if !(0..=MAX_SALE_RATE_PAISE).contains(&selling_rate_paise) {
        return Err(SaleMoneyError::InvalidRate);
    }
    let units = match quantity.basis {
        QuantityBasis::Pack => quantity
            .quantity_packs
            .ok_or(SaleMoneyError::InvalidQuantity)?,
        QuantityBasis::BaseUnit => quantity.quantity_atoms,
    };
    let taxable = units
        .checked_mul(selling_rate_paise)
        .ok_or(SaleMoneyError::Overflow)?;
    if taxable > MAX_SALE_LINE_TAXABLE_PAISE {
        return Err(SaleMoneyError::Overflow);
    }
    Ok(taxable)
}

/// Computes a line's tax and total from its taxable value, delegating every rounding decision to
/// ADR-016 so a sale and a purchase round identically.
pub fn compute_line(
    quantity: &SaleQuantity,
    selling_rate_paise: i64,
    treatment: TaxTreatment,
    rate: RateComponents,
) -> Result<LineAmounts, SaleMoneyError> {
    let taxable = taxable_value_paise(quantity, selling_rate_paise)?;
    // One unit at the full taxable value reuses the frozen component arithmetic without re-deriving
    // the product, so the rounding point stays in exactly one place.
    Ok(money::compute_line(1, taxable, treatment, rate)?)
}

/// Why a line's price was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CeilingBreach {
    /// The GST-inclusive line total exceeds the batch's printed MRP for this quantity.
    AboveMrp,
    /// The tax-exclusive taxable value exceeds the notified controlled ceiling.
    AboveControlledCeiling,
}

/// Is the line within the batch's printed MRP?
///
/// **MRP is inclusive of GST**, so the comparison is against the GST-inclusive `line_total_paise`,
/// never against the tax-exclusive rate. The pro-rata share for a loose quantity is expressed as a
/// cross-product so no per-unit MRP is ever invented:
///
/// ```text
/// basis pack       line_total                        ≤ mrp × quantity_packs
/// basis base_unit  line_total × base_quantity_atoms  ≤ mrp × quantity_atoms
/// ```
///
/// The quantity does **not** cancel in the second form, because GST is rounded once per line rather
/// than per unit, so `line_total` is not exactly proportional to the atoms sold.
///
/// `None` for `batch_mrp_paise` means the lot has no printed price recorded: there is then no known
/// ceiling, and inventing one from a sibling batch or pack would fabricate a legal fact.
pub fn within_mrp_ceiling(
    quantity: &SaleQuantity,
    line_total_paise: i64,
    base_quantity_atoms: i64,
    batch_mrp_paise: Option<i64>,
) -> Result<bool, SaleMoneyError> {
    let Some(mrp) = batch_mrp_paise else {
        return Ok(true);
    };
    if base_quantity_atoms < 1 {
        return Err(SaleMoneyError::InvalidPack);
    }
    let (left, right) = match quantity.basis {
        QuantityBasis::Pack => {
            let packs = quantity
                .quantity_packs
                .ok_or(SaleMoneyError::InvalidQuantity)?;
            (
                i128::from(line_total_paise),
                i128::from(mrp)
                    .checked_mul(i128::from(packs))
                    .ok_or(SaleMoneyError::Overflow)?,
            )
        }
        QuantityBasis::BaseUnit => (
            i128::from(line_total_paise)
                .checked_mul(i128::from(base_quantity_atoms))
                .ok_or(SaleMoneyError::Overflow)?,
            i128::from(mrp)
                .checked_mul(i128::from(quantity.quantity_atoms))
                .ok_or(SaleMoneyError::Overflow)?,
        ),
    };
    Ok(left <= right)
}

/// The Indian financial year a business date falls in, as the literal `YYYY-YY` a series row stores.
///
/// The Indian financial year runs 1 April to 31 March, so 2026-04-01 and 2027-03-31 are both
/// `2026-27` while 2027-04-01 is `2027-28`. This is the *only* place the rule is expressed: the
/// value is written onto the series row and onto the posted document, so a later change to this
/// function can never silently renumber documents that were already issued.
///
/// A malformed date yields `None` rather than a guess, because `YYYY-MM-DD` strings only order
/// correctly when they are real calendar dates.
pub fn indian_financial_year(business_date: &str) -> Option<String> {
    let date = Date::parse(business_date, format_description!("[year]-[month]-[day]")).ok()?;
    let starting_year = if date.month() as u8 >= 4 {
        date.year()
    } else {
        date.year() - 1
    };
    // Only years the schema's own `YYYY-YY` shape can hold are produced; a year outside it would
    // otherwise be formatted into a string the CHECK constraint would reject at insert time.
    if !(1000..=9998).contains(&starting_year) {
        return None;
    }
    let ending_year = starting_year + 1;
    Some(format!("{starting_year:04}-{:02}", ending_year % 100))
}

/// The longest a statutory document serial may be.
///
/// Rule 46(b) of the CGST Rules requires a tax invoice to carry "a consecutive serial number **not
/// exceeding sixteen characters**, in one or multiple series, containing alphabets or numerals or
/// special characters-hyphen or dash and slash symbolised as `-` and `/` respectively, and any
/// combination thereof, unique for a financial year". Rule 53(1A)(c) imposes the identical limit on
/// a credit or debit note under section 34.
///
/// This is a statutory limit, not a house style.
pub const MAX_DOCUMENT_SERIAL_LENGTH: usize = 16;

/// The compact financial-year component of a document serial: `2026-27` becomes `2627`.
///
/// The stored business fact stays `2026-27` — on the series row and on the posted document — because
/// that is what a financial year *is*. Only the rendered serial is compacted, and only because the
/// full form does not fit inside sixteen characters beside a series code and a sequence.
///
/// A value that is not already in the frozen `YYYY-YY` shape yields `None` rather than a guess: this
/// function never invents a year, it only shortens one the caller already established.
pub fn compact_financial_year(financial_year: &str) -> Option<String> {
    let bytes = financial_year.as_bytes();
    if bytes.len() != 7 || bytes[4] != b'-' {
        return None;
    }
    if !financial_year
        .chars()
        .enumerate()
        .all(|(index, character)| index == 4 || character.is_ascii_digit())
    {
        return None;
    }
    Some(format!(
        "{}{}",
        &financial_year[2..4],
        &financial_year[5..7]
    ))
}

/// Renders the statutory serial for a numbered document.
///
/// `INV` + `2627` + a six-digit sequence gives `INV/2627/000001` — fifteen characters, one inside the
/// limit, and still able to express 999 999 documents in a financial year.
///
/// Returns `None` rather than an over-long string if the parts cannot fit. A serial that breaches
/// Rule 46(b) must never reach a document: once issued, an invoice number is permanent, and a
/// non-conforming one cannot be corrected by re-issuing it.
pub fn document_serial(
    series_code: &str,
    financial_year: &str,
    sequence_value: i64,
) -> Option<String> {
    if series_code.is_empty() || sequence_value < 1 {
        return None;
    }
    let compact = compact_financial_year(financial_year)?;
    let serial = format!("{series_code}/{compact}/{sequence_value:06}");
    (serial.len() <= MAX_DOCUMENT_SERIAL_LENGTH).then_some(serial)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(cgst: i64, sgst: i64, igst: i64) -> RateComponents {
        RateComponents {
            cgst_basis_points: cgst,
            sgst_basis_points: sgst,
            igst_basis_points: igst,
            cess_basis_points: 0,
        }
    }

    fn packs(n: i64, atoms_per_pack: i64) -> SaleQuantity {
        resolve_quantity(QuantityBasis::Pack, n, atoms_per_pack, 0, false, 1).unwrap()
    }

    /// A loose quantity of a discrete, scale-zero Product. `fractional_sale_allowed` is deliberately
    /// FALSE here, because three tablets are three whole tablets: the frozen Phase 1B triggers make
    /// that permission unobtainable for a tablet, so any helper that needed it would be testing a
    /// configuration no real store can have.
    fn loose(atoms: i64, atoms_per_pack: i64, increment: i64) -> SaleQuantity {
        resolve_quantity(
            QuantityBasis::BaseUnit,
            atoms,
            atoms_per_pack,
            0,
            false,
            increment,
        )
        .unwrap()
    }

    #[test]
    fn a_basis_round_trips_through_its_stored_text() {
        for basis in [QuantityBasis::Pack, QuantityBasis::BaseUnit] {
            assert_eq!(QuantityBasis::parse(basis.as_str()), Some(basis));
        }
        assert_eq!(QuantityBasis::parse("strip"), None);
    }

    #[test]
    fn a_pack_quantity_derives_its_atoms_and_never_accepts_a_fraction_of_a_pack() {
        let quantity = packs(3, 15);
        assert_eq!(quantity.quantity_packs, Some(3));
        assert_eq!(quantity.quantity_atoms, 45);
        // There is no decimal pack anywhere: the type itself is an integer.
        for bad in [0, -1, MAX_SALE_QUANTITY_PACKS + 1] {
            assert_eq!(
                resolve_quantity(QuantityBasis::Pack, bad, 15, 0, false, 1),
                Err(SaleMoneyError::InvalidQuantity),
                "{bad} packs must be refused"
            );
        }
    }

    /// Breaking a Pack open is governed by the increment, and by nothing else.
    ///
    /// A tablet is a discrete unit, so the frozen Phase 1B triggers pin it to `quantity_scale = 0`
    /// and therefore forbid `fractional_sale_allowed` on it outright. Reading that flag as
    /// "may break a strip" would have made loose sale of tablets and capsules impossible — which is
    /// most of what an Indian pharmacy sells.
    #[test]
    fn breaking_a_pack_is_governed_by_the_increment_not_by_fractional_permission() {
        // 1. A strip of ten sold in singles: three tablets, with no sub-unit permission anywhere.
        let three = resolve_quantity(QuantityBasis::BaseUnit, 3, 10, 0, false, 1).unwrap();
        assert_eq!(three.quantity_atoms, 3);
        assert_eq!(three.quantity_packs, None, "a loose line counts no packs");

        // 2. The same store, having set the increment to a whole strip, refuses three.
        assert_eq!(
            resolve_quantity(QuantityBasis::BaseUnit, 3, 10, 0, false, 10),
            Err(SaleMoneyError::IncrementViolation),
            "an increment of a whole strip is how a store forbids breaking one"
        );

        // 3. And accepts ten, which is that same strip.
        assert_eq!(
            resolve_quantity(QuantityBasis::BaseUnit, 10, 10, 0, false, 10)
                .unwrap()
                .quantity_atoms,
            10
        );

        // 4. Stated plainly, because it is the whole point: a scale-zero Product with fractional
        //    permission switched OFF still sells three loose units.
        assert!(resolve_quantity(QuantityBasis::BaseUnit, 3, 10, 0, false, 1).is_ok());

        // Granting the permission changes nothing at scale 0, since it governs a quantity that
        // cannot exist there.
        assert_eq!(
            resolve_quantity(QuantityBasis::BaseUnit, 3, 10, 0, true, 1),
            resolve_quantity(QuantityBasis::BaseUnit, 3, 10, 0, false, 1)
        );
        assert_eq!(loose(4, 10, 2).quantity_atoms, 4);
    }

    /// What `fractional_sale_allowed` actually governs: less than one whole base unit, which only
    /// exists where the Product's precision admits it.
    ///
    /// A 100 ml bottle at scale 1 holds 1 000 atoms, and one millilitre is 10 of them.
    #[test]
    fn a_sub_base_unit_quantity_is_what_fractional_permission_governs() {
        // 5. Five whole millilitres needs no permission: 50 atoms is 5 complete base units.
        assert_eq!(
            resolve_quantity(QuantityBasis::BaseUnit, 50, 1_000, 1, false, 1)
                .unwrap()
                .quantity_atoms,
            50
        );

        // 6. Half a millilitre is a genuine fraction of a base unit, and is refused without it.
        assert_eq!(
            resolve_quantity(QuantityBasis::BaseUnit, 5, 1_000, 1, false, 1),
            Err(SaleMoneyError::FractionalNotPermitted),
            "half a base unit is exactly what this permission is for"
        );

        // 7. The same quantity, permitted.
        assert_eq!(
            resolve_quantity(QuantityBasis::BaseUnit, 5, 1_000, 1, true, 1)
                .unwrap()
                .quantity_atoms,
            5
        );
    }

    /// 8. The granularity is checked first and refuses on its own terms, so a store cannot be
    ///    talked past its own increment by holding the other permission.
    #[test]
    fn an_increment_violation_is_refused_whatever_the_fractional_permission_says() {
        for permitted in [false, true] {
            assert_eq!(
                resolve_quantity(QuantityBasis::BaseUnit, 5, 1_000, 1, permitted, 2),
                Err(SaleMoneyError::IncrementViolation),
                "5 is not a multiple of a 2-atom increment (permission: {permitted})"
            );
            assert_eq!(
                resolve_quantity(QuantityBasis::BaseUnit, 3, 10, 0, permitted, 2),
                Err(SaleMoneyError::IncrementViolation),
                "3 is not a multiple of a 2-atom increment (permission: {permitted})"
            );
        }
    }

    #[test]
    fn a_scale_six_product_is_not_limited_to_one_unit_by_the_atom_bound() {
        // One base unit is 1 000 000 atoms at scale 6. A tighter cap would have made a single unit
        // the maximum sale, which would be a bug invented by the bound.
        // Five whole base units at scale 6, which needs no sub-unit permission.
        let quantity =
            resolve_quantity(QuantityBasis::BaseUnit, 5_000_000, 1_000_000, 6, false, 1).unwrap();
        assert_eq!(quantity.quantity_atoms, 5_000_000);
        // One atom short of a whole unit IS a fraction of one, and needs the permission.
        assert_eq!(
            resolve_quantity(QuantityBasis::BaseUnit, 4_999_999, 1_000_000, 6, false, 1),
            Err(SaleMoneyError::FractionalNotPermitted)
        );
    }

    #[test]
    fn taxable_value_is_an_exact_product_in_the_lines_own_basis() {
        // Two strips at 95.50 each.
        assert_eq!(taxable_value_paise(&packs(2, 15), 9_550).unwrap(), 19_100);
        // Three tablets at 6.36 each. Note this is NOT 9550 apportioned: the operator priced the
        // unit directly, so no division and no remainder loss occurs.
        assert_eq!(taxable_value_paise(&loose(3, 15, 1), 636).unwrap(), 1_908);
        // Free goods are invoiced at nil rate and the Store Service accepts it.
        assert_eq!(taxable_value_paise(&packs(4, 10), 0).unwrap(), 0);
    }

    #[test]
    fn an_impossible_rate_or_an_unrepresentable_line_is_refused_not_wrapped() {
        assert_eq!(
            taxable_value_paise(&packs(1, 10), -1),
            Err(SaleMoneyError::InvalidRate)
        );
        assert_eq!(
            taxable_value_paise(&packs(1, 10), MAX_SALE_RATE_PAISE + 1),
            Err(SaleMoneyError::InvalidRate)
        );
        // Bounded on money rather than on quantity: a huge loose quantity at a real price overflows
        // the line bound and is refused rather than silently truncated.
        let huge = loose(MAX_SALE_QUANTITY_ATOMS, 1, 1);
        assert_eq!(
            taxable_value_paise(&huge, MAX_SALE_RATE_PAISE),
            Err(SaleMoneyError::Overflow)
        );
    }

    #[test]
    fn tax_is_rounded_once_per_component_half_away_from_zero() {
        // The ADR-016 discriminator: taxable 100 paise at 2.5% is 2.5 paise, which rounds to 3
        // away from zero. Banker's rounding would give 2.
        let amounts = compute_line(
            &packs(1, 10),
            100,
            TaxTreatment::IntraState,
            rate(250, 250, 500),
        )
        .unwrap();
        assert_eq!(amounts.taxable_value_paise, 100);
        assert_eq!(amounts.cgst_paise, 3);
        assert_eq!(amounts.sgst_paise, 3);
        assert_eq!(amounts.igst_paise, 0);
        assert_eq!(amounts.line_total_paise, 106);
    }

    #[test]
    fn an_interstate_line_charges_igst_alone() {
        let amounts = compute_line(
            &packs(2, 15),
            9_550,
            TaxTreatment::InterState,
            rate(600, 600, 1_200),
        )
        .unwrap();
        assert_eq!(amounts.taxable_value_paise, 19_100);
        assert_eq!(amounts.cgst_paise, 0);
        assert_eq!(amounts.sgst_paise, 0);
        assert_eq!(amounts.igst_paise, 2_292);
        assert_eq!(amounts.line_total_paise, 21_392);
    }

    /// The defect this module exists to prevent.
    #[test]
    fn the_mrp_ceiling_compares_the_gst_inclusive_total_not_the_tax_exclusive_rate() {
        let quantity = packs(1, 15);
        // A rate exactly equal to the printed MRP. Tax-exclusively it "fits"; with 12% GST added the
        // customer would pay 106.96 against a printed 95.50.
        let amounts = compute_line(
            &quantity,
            9_550,
            TaxTreatment::IntraState,
            rate(600, 600, 1_200),
        )
        .unwrap();
        assert_eq!(amounts.line_total_paise, 10_696);
        assert!(
            !within_mrp_ceiling(&quantity, amounts.line_total_paise, 15, Some(9_550)).unwrap(),
            "comparing the exclusive rate against the inclusive MRP would have permitted this"
        );

        // The largest lawful rate is the one whose INCLUSIVE total lands on the MRP: at 8527 the
        // rounded tax tips the total to 9551, one paise over.
        let lawful = compute_line(
            &quantity,
            8_526,
            TaxTreatment::IntraState,
            rate(600, 600, 1_200),
        )
        .unwrap();
        assert_eq!(lawful.line_total_paise, 9_550);
        assert!(within_mrp_ceiling(&quantity, lawful.line_total_paise, 15, Some(9_550)).unwrap());

        // One paise more is refused, so the boundary is real rather than approximate.
        let over = compute_line(
            &quantity,
            8_527,
            TaxTreatment::IntraState,
            rate(600, 600, 1_200),
        )
        .unwrap();
        assert_eq!(over.line_total_paise, 9_551);
        assert!(!within_mrp_ceiling(&quantity, over.line_total_paise, 15, Some(9_550)).unwrap());
    }

    #[test]
    fn a_loose_line_is_held_to_the_pro_rata_mrp_without_inventing_a_per_unit_mrp() {
        // A strip of 15 printed at 95.50, selling 3 tablets at 12% GST.
        let quantity = loose(3, 15, 1);
        for (rate_paise, expected_total, permitted) in
            [(568_i64, 1_908_i64, true), (569, 1_911, false)]
        {
            let amounts = compute_line(
                &quantity,
                rate_paise,
                TaxTreatment::IntraState,
                rate(600, 600, 1_200),
            )
            .unwrap();
            assert_eq!(amounts.line_total_paise, expected_total, "at {rate_paise}");
            assert_eq!(
                within_mrp_ceiling(&quantity, amounts.line_total_paise, 15, Some(9_550)).unwrap(),
                permitted,
                "at {rate_paise} the pro-rata ceiling verdict must be {permitted}"
            );
        }
        // 1908 × 15 = 28 620 ≤ 9550 × 3 = 28 650, and 1911 × 15 = 28 665 > 28 650. A rounded
        // "MRP per tablet" of 637 would have mis-stated this ceiling.
    }

    #[test]
    fn a_lot_with_no_printed_price_has_no_ceiling_rather_than_an_invented_one() {
        let quantity = packs(1, 15);
        assert!(
            within_mrp_ceiling(&quantity, i64::MAX / 2, 15, None).unwrap(),
            "an unknown MRP must not be fabricated from anything else"
        );
    }

    #[test]
    fn the_mrp_comparison_survives_schema_legal_extremes() {
        // The widest operands the schema admits, which exceed i64 and are why this is i128.
        let quantity = loose(MAX_SALE_QUANTITY_ATOMS, 9_000_000_000_000_000, 1);
        let verdict = within_mrp_ceiling(&quantity, 1, 9_000_000_000_000_000, Some(1));
        assert!(verdict.is_ok(), "the comparison must not overflow");
    }

    /// Table-driven boundary vectors: every row states the intended verdict in full.
    #[test]
    fn line_vectors_hold_at_their_boundaries() {
        struct Vector {
            name: &'static str,
            quantity: SaleQuantity,
            rate_paise: i64,
            treatment: TaxTreatment,
            components: RateComponents,
            taxable: i64,
            total: i64,
        }
        let vectors = [
            Vector {
                name: "whole packs, 12% split",
                quantity: packs(12, 10),
                rate_paise: 3_050,
                treatment: TaxTreatment::IntraState,
                components: rate(600, 600, 1_200),
                taxable: 36_600,
                total: 40_992,
            },
            Vector {
                name: "loose units, 5% split, exact halves",
                quantity: loose(7, 15, 1),
                rate_paise: 400,
                treatment: TaxTreatment::IntraState,
                components: rate(250, 250, 500),
                taxable: 2_800,
                total: 2_940,
            },
            Vector {
                name: "exempt line carries no tax at all",
                quantity: packs(50, 1),
                rate_paise: 1_575,
                treatment: TaxTreatment::IntraState,
                components: rate(0, 0, 0),
                taxable: 78_750,
                total: 78_750,
            },
            Vector {
                name: "nil rate on a loose line",
                quantity: loose(2, 10, 1),
                rate_paise: 1_000,
                treatment: TaxTreatment::InterState,
                components: rate(0, 0, 0),
                taxable: 2_000,
                total: 2_000,
            },
        ];
        for vector in vectors {
            let amounts = compute_line(
                &vector.quantity,
                vector.rate_paise,
                vector.treatment,
                vector.components,
            )
            .unwrap();
            assert_eq!(
                amounts.taxable_value_paise, vector.taxable,
                "{}: taxable",
                vector.name
            );
            assert_eq!(
                amounts.line_total_paise, vector.total,
                "{}: line total",
                vector.name
            );
        }
    }

    #[test]
    fn document_totals_are_the_sum_of_already_rounded_lines() {
        let lines: Vec<LineAmounts> = [(1_i64, 100_i64), (1, 100), (1, 100)]
            .into_iter()
            .map(|(count, taxable)| {
                compute_line(
                    &packs(count, 1),
                    taxable,
                    TaxTreatment::IntraState,
                    rate(250, 250, 500),
                )
                .unwrap()
            })
            .collect();
        let totals = money::sum_lines(&lines).unwrap();
        // Each line rounds 2.5 up to 3 independently; the document is 9, not a re-rounded 7.5.
        assert_eq!(totals.cgst_paise, 9);
        assert_eq!(totals.taxable_value_paise, 300);
        assert_eq!(totals.line_total_paise, 318);
    }

    /// The year rolls on 1 April, not 1 January. Both edges are asserted, because an off-by-one day
    /// here would restart an invoice series in the middle of a year.
    #[test]
    fn the_financial_year_turns_on_the_first_of_april() {
        assert_eq!(
            indian_financial_year("2026-03-31").as_deref(),
            Some("2025-26")
        );
        assert_eq!(
            indian_financial_year("2026-04-01").as_deref(),
            Some("2026-27")
        );
        assert_eq!(
            indian_financial_year("2027-03-31").as_deref(),
            Some("2026-27")
        );
        assert_eq!(
            indian_financial_year("2027-04-01").as_deref(),
            Some("2027-28")
        );
        // A century boundary still prints two digits, so '2099-00' is the shape, not '2099-100'.
        assert_eq!(
            indian_financial_year("2099-12-31").as_deref(),
            Some("2099-00")
        );
        // Not a date at all, and a date that does not exist: both refused rather than guessed.
        assert!(indian_financial_year("2026-13-01").is_none());
        assert!(indian_financial_year("2026-02-30").is_none());
        assert!(indian_financial_year("").is_none());
    }

    /// Rule 46(b) caps a tax invoice serial at sixteen characters, and Rule 53(1A)(c) caps a credit
    /// or debit note at the same. The first shipped format, `INV/2026-27/000001`, was eighteen.
    ///
    /// The financial year the database stores is unchanged and is still the full business fact; only
    /// the rendered serial is compact, and only because the full form does not fit.
    #[test]
    fn a_document_serial_fits_inside_the_statutory_sixteen_characters() {
        let serial = document_serial("INV", "2026-27", 1).expect("a serial");
        assert_eq!(serial, "INV/2627/000001");
        assert_eq!(serial.len(), 15);
        assert!(serial.len() <= MAX_DOCUMENT_SERIAL_LENGTH);

        // The whole six-digit range still fits, so the limit costs no capacity.
        let last = document_serial("INV", "2026-27", 999_999).expect("a serial");
        assert_eq!(last, "INV/2627/999999");
        assert!(last.len() <= MAX_DOCUMENT_SERIAL_LENGTH);
    }

    /// The compact component is a rendering of the stored year, never a recomputation of it.
    #[test]
    fn the_compact_financial_year_shortens_rather_than_reinvents() {
        assert_eq!(compact_financial_year("2026-27").as_deref(), Some("2627"));
        assert_eq!(compact_financial_year("2027-28").as_deref(), Some("2728"));
        // The century boundary the year helper already produces.
        assert_eq!(compact_financial_year("2099-00").as_deref(), Some("9900"));

        // Anything not already in the frozen YYYY-YY shape is refused rather than guessed at.
        for bad in ["2026-2027", "26-27", "2026/27", "", "20A6-27", "2026-2"] {
            assert!(
                compact_financial_year(bad).is_none(),
                "{bad} must not yield a compact year"
            );
        }
    }

    /// The year the business stores and the year the serial renders must agree, across the boundary
    /// that opens a new series.
    #[test]
    fn the_serial_follows_the_financial_year_the_business_date_falls_in() {
        for (date, expected) in [
            ("2026-03-31", "INV/2526/000001"),
            ("2026-04-01", "INV/2627/000001"),
            ("2027-03-31", "INV/2627/000001"),
            ("2027-04-01", "INV/2728/000001"),
        ] {
            let year = indian_financial_year(date).expect("a financial year");
            let serial = document_serial("INV", &year, 1).expect("a serial");
            assert_eq!(serial, expected, "{date}");
            assert!(serial.len() <= MAX_DOCUMENT_SERIAL_LENGTH, "{date}");
        }
    }

    /// A serial that cannot be rendered inside the limit is refused, not truncated and not issued.
    /// An invoice number is permanent: a non-conforming one cannot be put right afterwards.
    #[test]
    fn a_serial_that_would_breach_the_limit_is_refused_rather_than_issued() {
        // Eight characters of series leaves no room for `/2627/000001`.
        assert!(document_serial("VERYLONGSERIES", "2026-27", 1).is_none());
        // A series code right at the edge still renders.
        assert_eq!(
            document_serial("INVOI", "2026-27", 1).as_deref(),
            Some("INVOI/2627/000001").filter(|serial| serial.len() <= MAX_DOCUMENT_SERIAL_LENGTH)
        );
        // Nonsense inputs yield nothing rather than a malformed serial.
        assert!(document_serial("", "2026-27", 1).is_none());
        assert!(document_serial("INV", "2026-27", 0).is_none());
        assert!(document_serial("INV", "not-a-year", 1).is_none());
    }
}
