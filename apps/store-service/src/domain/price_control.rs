//! Phase 1H-0 medicine price-control resolution.
//!
//! The single place effective-date logic for notified ceiling prices lives, mirroring
//! [`crate::domain::taxation`]. A Product asserts whether it is price-controlled and, if so, which
//! controlled formulation it maps to; the ceiling that applies on a given date is always resolved
//! from `price_control_versions` and never stored on the Product.
//!
//! Three prices are kept deliberately distinct and are never merged:
//!
//! | fact | grain | tax basis |
//! |---|---|---|
//! | Batch MRP | one lot | **inclusive** of GST (Legal Metrology) |
//! | Controlled ceiling | one formulation | **exclusive** of GST (DPCO: MRP = ceiling + GST) |
//! | Selling rate | one sale line | **exclusive** of GST |
//!
//! Because the first two sit on different tax bases, they are never compared with each other and
//! `min(mrp, ceiling)` is meaningless. A future Sale enforces *both* constraints, each against the
//! figure that shares its basis, which yields the stricter effective maximum without ever placing
//! the two numbers on one axis.

use sqlx::{FromRow, SqliteExecutor};
use time::{Date, macros::format_description};

/// The largest ceiling the schema admits, mirroring `price_control_versions.ceiling_price_paise`.
pub const MAX_CEILING_PRICE_PAISE: i64 = 100_000_000_000;

/// What a ceiling price is quoted per.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CeilingBasis {
    /// Per one base unit of the product — per tablet, per ml. The shape DPCO notifications take.
    PerBaseUnit,
    /// Per a pack presentation. Recorded truthfully, but not comparable without knowing which pack,
    /// and dividing it by an arbitrary pack count would invent a legal fact.
    PerPack,
}

impl CeilingBasis {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PerBaseUnit => "per_base_unit",
            Self::PerPack => "per_pack",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "per_base_unit" => Some(Self::PerBaseUnit),
            "per_pack" => Some(Self::PerPack),
            _ => None,
        }
    }
}

/// Whether anyone has assessed this Product for price control, and what they concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceControlStatus {
    /// Nobody has assessed it yet. A real state, never to be read as "not controlled".
    Unknown,
    /// Positively asserted as outside price control.
    NotApplicable,
    /// Under price control; a controlled formulation is required.
    Controlled,
}

impl PriceControlStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::NotApplicable => "not_applicable",
            Self::Controlled => "controlled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "unknown" => Some(Self::Unknown),
            "not_applicable" => Some(Self::NotApplicable),
            "controlled" => Some(Self::Controlled),
            _ => None,
        }
    }
}

/// One resolved ceiling version, reported exactly as stored.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct ResolvedPriceCeiling {
    pub id: String,
    pub effective_from: String,
    pub effective_to: Option<String>,
    pub ceiling_price_paise: i64,
    pub ceiling_basis: String,
    pub ceiling_basis_unit_id: Option<String>,
    pub notification_reference: Option<String>,
}

#[derive(Debug)]
pub enum PriceControlError {
    InvalidDate,
    Database(sqlx::Error),
}

impl From<sqlx::Error> for PriceControlError {
    fn from(value: sqlx::Error) -> Self {
        Self::Database(value)
    }
}

/// Resolves the ceiling in force for a controlled formulation on a calendar date.
///
/// Periods are half-open `[effective_from, effective_to)` exactly as the frozen migration states and
/// as the no-overlap triggers assume, so on the day equal to `effective_to` the *next* version
/// applies, or none. At most one active version can match, because those triggers forbid overlapping
/// active periods for one formulation.
///
/// Only `status = 'active'` versions resolve: an archived version is history, not a current ceiling.
/// The formulation's own status is deliberately **not** filtered — a Product that already references
/// a since-archived formulation must still resolve its ceiling for historical display, exactly as
/// `resolve_tax_rate` reasons about archived Tax Categories.
///
/// This function resolves **by date only**. It makes no comparison and reaches no verdict; deciding
/// whether a ceiling is comparable with a selling rate is [`compare_basis`], and deciding what to do
/// about it belongs to the caller.
pub async fn resolve_price_ceiling<'e, E>(
    executor: E,
    controlled_formulation_id: &str,
    on_date: &str,
) -> Result<Option<ResolvedPriceCeiling>, PriceControlError>
where
    E: SqliteExecutor<'e>,
{
    let date = validate_calendar_date(on_date)?;
    sqlx::query_as::<_, ResolvedPriceCeiling>(
        "SELECT id,effective_from,effective_to,ceiling_price_paise,ceiling_basis,\
         ceiling_basis_unit_id,notification_reference \
         FROM price_control_versions \
         WHERE controlled_formulation_id = ?1 AND status = 'active' \
           AND effective_from <= ?2 AND (effective_to IS NULL OR effective_to > ?2) \
         LIMIT 1",
    )
    .bind(controlled_formulation_id)
    .bind(&date)
    .fetch_optional(executor)
    .await
    .map_err(PriceControlError::Database)
}

/// Dates are compared as `YYYY-MM-DD` strings, which orders correctly only for real calendar dates,
/// so a malformed value is refused rather than silently mis-comparing.
pub fn validate_calendar_date(value: &str) -> Result<String, PriceControlError> {
    Date::parse(value, format_description!("[year]-[month]-[day]"))
        .map(|date| date.to_string())
        .map_err(|_| PriceControlError::InvalidDate)
}

/// Whether a resolved ceiling can be compared with a Product's selling rate at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparability {
    /// The ceiling is quoted per base unit, in the Product's own base unit.
    Comparable,
    /// Quoted per pack: which pack is not stated, and guessing one would invent a legal fact.
    IncomparablePackBasis,
    /// Quoted per base unit but in a different unit than the Product's. No conversion is attempted:
    /// turning milligrams into millilitres, or tablets into grams, is not something this foundation
    /// can do safely.
    IncomparableUnit,
}

/// Decides comparability without converting anything.
pub fn compare_basis(ceiling: &ResolvedPriceCeiling, product_base_unit_id: &str) -> Comparability {
    match CeilingBasis::parse(&ceiling.ceiling_basis) {
        Some(CeilingBasis::PerPack) | None => Comparability::IncomparablePackBasis,
        Some(CeilingBasis::PerBaseUnit) => match ceiling.ceiling_basis_unit_id.as_deref() {
            Some(unit) if unit == product_base_unit_id => Comparability::Comparable,
            _ => Comparability::IncomparableUnit,
        },
    }
}

/// `10^scale`, the number of atoms in one base unit.
///
/// `products.quantity_scale` is 0–6, so one base unit is **not** always one atom: at scale 6 a
/// single base unit is 1 000 000 atoms. Every comparison below multiplies by this rather than
/// dividing a ceiling into a per-atom figure, which would round a legal maximum.
pub fn atoms_per_base_unit(quantity_scale: i64) -> Option<i64> {
    (0..=6)
        .contains(&quantity_scale)
        .then(|| 10_i64.pow(quantity_scale as u32))
}

#[derive(Debug)]
pub enum CeilingCheckError {
    /// An operand was outside the schema's own bounds, or the arithmetic could not be represented.
    Overflow,
    /// The scale was outside the 0..=6 the schema admits.
    InvalidScale,
}

/// Is a **per-atom** selling rate within a per-base-unit ceiling?
///
/// `rate_per_atom × 10^scale ≤ ceiling` — the price of one whole base unit implied by this per-atom
/// rate may not exceed the ceiling quoted for one base unit. Exact: one multiplication, no division,
/// no rounding, no invented per-atom ceiling.
///
/// Evaluated in `i128` for the same reason the Phase 1H MRP comparison is: the operands are
/// schema-legal up to `10^11 × 10^6`, and a future bound change must not be able to wrap silently.
pub fn per_atom_rate_within_ceiling(
    rate_per_atom_paise: i64,
    quantity_scale: i64,
    ceiling_price_paise: i64,
) -> Result<bool, CeilingCheckError> {
    let atoms = atoms_per_base_unit(quantity_scale).ok_or(CeilingCheckError::InvalidScale)?;
    let implied = i128::from(rate_per_atom_paise)
        .checked_mul(i128::from(atoms))
        .ok_or(CeilingCheckError::Overflow)?;
    Ok(implied <= i128::from(ceiling_price_paise))
}

/// Is a **per-pack** selling rate within a per-base-unit ceiling?
///
/// A pack holds `base_quantity_atoms` atoms, which is `base_quantity_atoms / 10^scale` base units.
/// Rather than divide — which would round the ceiling — both sides are cross-multiplied:
///
/// ```text
/// rate_per_pack × 10^scale  ≤  ceiling × base_quantity_atoms
/// ```
///
/// Exact, with no division and no rounding. The right-hand side reaches `10^11 × 9×10^15 = 9×10^26`
/// for schema-legal inputs, which exceeds `i64`, so the comparison is evaluated in `i128`.
pub fn per_pack_rate_within_ceiling(
    rate_per_pack_paise: i64,
    quantity_scale: i64,
    base_quantity_atoms: i64,
    ceiling_price_paise: i64,
) -> Result<bool, CeilingCheckError> {
    let atoms = atoms_per_base_unit(quantity_scale).ok_or(CeilingCheckError::InvalidScale)?;
    let left = i128::from(rate_per_pack_paise)
        .checked_mul(i128::from(atoms))
        .ok_or(CeilingCheckError::Overflow)?;
    let right = i128::from(ceiling_price_paise)
        .checked_mul(i128::from(base_quantity_atoms))
        .ok_or(CeilingCheckError::Overflow)?;
    Ok(left <= right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_basis_round_trips_through_its_stored_text() {
        for basis in [CeilingBasis::PerBaseUnit, CeilingBasis::PerPack] {
            assert_eq!(CeilingBasis::parse(basis.as_str()), Some(basis));
        }
        assert_eq!(CeilingBasis::parse("per_strip"), None);
    }

    #[test]
    fn a_status_round_trips_and_refuses_anything_else() {
        for status in [
            PriceControlStatus::Unknown,
            PriceControlStatus::NotApplicable,
            PriceControlStatus::Controlled,
        ] {
            assert_eq!(PriceControlStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(PriceControlStatus::parse("scheduled"), None);
    }

    #[test]
    fn one_base_unit_is_ten_to_the_scale_atoms() {
        assert_eq!(atoms_per_base_unit(0), Some(1));
        assert_eq!(atoms_per_base_unit(1), Some(10));
        assert_eq!(atoms_per_base_unit(6), Some(1_000_000));
        assert_eq!(atoms_per_base_unit(7), None);
        assert_eq!(atoms_per_base_unit(-1), None);
    }

    fn ceiling(basis: &str, unit: Option<&str>) -> ResolvedPriceCeiling {
        ResolvedPriceCeiling {
            id: "01997a00-0000-7000-8000-000000000001".to_owned(),
            effective_from: "2026-01-01".to_owned(),
            effective_to: None,
            ceiling_price_paise: 109,
            ceiling_basis: basis.to_owned(),
            ceiling_basis_unit_id: unit.map(str::to_owned),
            notification_reference: None,
        }
    }

    #[test]
    fn a_per_pack_ceiling_is_never_silently_compared() {
        assert_eq!(
            compare_basis(&ceiling("per_pack", None), "tablet"),
            Comparability::IncomparablePackBasis
        );
    }

    #[test]
    fn a_ceiling_in_another_unit_is_refused_rather_than_converted() {
        assert_eq!(
            compare_basis(&ceiling("per_base_unit", Some("millilitre")), "tablet"),
            Comparability::IncomparableUnit
        );
        assert_eq!(
            compare_basis(&ceiling("per_base_unit", None), "tablet"),
            Comparability::IncomparableUnit
        );
        assert_eq!(
            compare_basis(&ceiling("per_base_unit", Some("tablet")), "tablet"),
            Comparability::Comparable
        );
    }

    #[test]
    fn a_per_atom_rate_is_compared_against_one_whole_base_unit() {
        // Scale 0: one atom is one tablet, so the rate is compared directly.
        assert!(per_atom_rate_within_ceiling(109, 0, 109).unwrap());
        assert!(!per_atom_rate_within_ceiling(110, 0, 109).unwrap());
        // Scale 1: one base unit is ten atoms, so a per-atom rate of 10 implies 100 for the unit.
        assert!(per_atom_rate_within_ceiling(10, 1, 100).unwrap());
        assert!(!per_atom_rate_within_ceiling(11, 1, 100).unwrap());
    }

    #[test]
    fn a_per_pack_rate_is_compared_without_dividing_the_ceiling() {
        // A strip of 15 tablets, ceiling 109 paise per tablet: the pack may reach 1635 paise.
        assert!(per_pack_rate_within_ceiling(1635, 0, 15, 109).unwrap());
        assert!(!per_pack_rate_within_ceiling(1636, 0, 15, 109).unwrap());
    }

    /// The reason the ceiling is never divided into a per-atom figure first.
    #[test]
    fn dividing_the_ceiling_first_would_move_the_legal_maximum() {
        // 9550 paise across 15 tablets is 636.67 per tablet. A rounded 637 would permit a pack price
        // of 9555, five paise above the notified ceiling; rounding down to 636 would forbid 9550,
        // which is lawful. The exact comparison does neither.
        assert!(
            per_pack_rate_within_ceiling(9550, 0, 1, 9550).unwrap(),
            "the exact ceiling itself must be permitted"
        );
        let rounded_up_per_atom = 9550_i64.div_euclid(15) + 1;
        assert_eq!(rounded_up_per_atom, 637);
        assert!(
            rounded_up_per_atom * 15 > 9550,
            "which is why a rounded per-atom ceiling is not used"
        );
    }

    #[test]
    fn schema_legal_operands_do_not_overflow_the_comparison() {
        // The largest the schema admits: ceiling 10^11 against a pack of 9x10^15 atoms.
        assert!(
            per_pack_rate_within_ceiling(
                MAX_CEILING_PRICE_PAISE,
                6,
                9_000_000_000_000_000,
                MAX_CEILING_PRICE_PAISE
            )
            .unwrap()
        );
        // And an out-of-range scale is refused rather than shifting by an absurd power.
        assert!(matches!(
            per_pack_rate_within_ceiling(1, 9, 1, 1),
            Err(CeilingCheckError::InvalidScale)
        ));
    }
}
