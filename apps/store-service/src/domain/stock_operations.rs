//! Phase 1J stock-operation rules.
//!
//! Two things live here, and they are here rather than in the handler because both must be
//! provable on their own.
//!
//! 1. **What each kind of operation is allowed to be.** Operator intent is a typed value, and the
//!    rule table below is what turns that label into an invariant: an `expiry` document cannot
//!    quietly contain a theft, and a `removal` cannot take goods off the shelf. The database
//!    enforces the same table in a trigger, so neither layer is the only thing standing between a
//!    mistake and the ledger.
//! 2. **How a counted quantity becomes a variance.** The operator supplies what they counted; the
//!    server supplies what it believes is there. Nothing in between is allowed to be the browser's
//!    opinion.

/// Why a stock operation could not be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StockOperationError {
    /// The counted quantity is negative, or beyond the representable range.
    InvalidCountedQuantity,
    /// The requested quantity is not a positive integer, or beyond the representable range.
    InvalidQuantity,
    /// The balance the count is measured against is not representable.
    InvalidBalance,
    /// The arithmetic could not be represented.
    Overflow,
}

/// The largest quantity any line may name, matching the schema's own bound.
pub const MAX_OPERATION_ATOMS: i64 = 9_000_000_000_000_000;

/// Which way a line moves quantity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// A counted quantity; the server works out the sign.
    Count,
    /// Quantity appears at one status.
    Increase,
    /// Quantity leaves one status.
    Decrease,
    /// Quantity moves from one status to another, unchanged in total.
    Transfer,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Count => "count",
            Direction::Increase => "increase",
            Direction::Decrease => "decrease",
            Direction::Transfer => "transfer",
        }
    }
}

/// What one kind of stock operation is permitted to contain.
///
/// `sources` is the status a line may act on. `targets` is where a transfer may send the quantity,
/// and is empty for the kinds that do not transfer. `reasons` is the closed set of typed causes.
#[derive(Debug, Clone, Copy)]
pub struct KindRules {
    pub kind: &'static str,
    pub directions: &'static [Direction],
    pub sources: &'static [&'static str],
    pub targets: &'static [&'static str],
    pub reasons: &'static [&'static str],
    /// The movement type a line of this kind produces, where it produces one directly.
    pub movement_type: &'static str,
}

/// Every kind this phase implements.
///
/// The shape of this table is the architecture: a pharmacist never means "change this number", they
/// mean "these were broken" or "I counted the shelf", and those two sentences have different
/// authorities, different destinations and different consequences for a future report.
pub const KINDS: &[KindRules] = &[
    KindRules {
        kind: "physical_count",
        directions: &[Direction::Count],
        sources: &["sellable", "quarantined", "non_sellable"],
        targets: &[],
        reasons: &["physical_count_gain", "physical_count_loss"],
        movement_type: "stock_count",
    },
    KindRules {
        // Phase 1D's generic correction, finally reachable from a screen. Kept deliberately narrow:
        // a correction that could reach any status in any direction for any reason would be a way
        // around every other rule in this table.
        kind: "adjustment",
        directions: &[Direction::Increase, Direction::Decrease],
        sources: &["sellable", "quarantined", "non_sellable"],
        targets: &[],
        reasons: &["theft_or_loss", "data_correction"],
        movement_type: "adjustment",
    },
    KindRules {
        kind: "damage",
        directions: &[Direction::Transfer],
        sources: &["sellable"],
        targets: &["quarantined", "non_sellable"],
        reasons: &["damage", "breakage"],
        movement_type: "disposition_transfer",
    },
    KindRules {
        kind: "expiry",
        directions: &[Direction::Transfer],
        sources: &["sellable"],
        targets: &["non_sellable"],
        reasons: &["expiry"],
        movement_type: "disposition_transfer",
    },
    KindRules {
        kind: "quarantine",
        directions: &[Direction::Transfer],
        sources: &["sellable"],
        targets: &["quarantined"],
        reasons: &["quality_hold", "damage"],
        movement_type: "disposition_transfer",
    },
    KindRules {
        // Custody ending. Only goods already written off can leave this way: removing sellable
        // stock would be a sale nobody recorded, and removing quarantined stock would pre-empt the
        // decision quarantine exists to wait for.
        kind: "removal",
        directions: &[Direction::Decrease],
        sources: &["non_sellable"],
        targets: &[],
        reasons: &["disposal"],
        movement_type: "stock_removal",
    },
];

/// The rules for a kind, or `None` if the kind is not one this phase implements.
pub fn rules_for(kind: &str) -> Option<&'static KindRules> {
    KINDS.iter().find(|rules| rules.kind == kind)
}

/// Whether a line's shape is one its document's kind allows.
///
/// `theft_or_loss` is the one reason with a rule of its own: goods that were stolen or lost are
/// already gone, so the quantity can only fall. An "increase by theft" is not a thing that happens.
pub fn line_is_permitted(
    kind: &str,
    direction: Direction,
    source_status: &str,
    target_status: Option<&str>,
    reason_code: &str,
) -> bool {
    let Some(rules) = rules_for(kind) else {
        return false;
    };
    if !rules.directions.contains(&direction) {
        return false;
    }
    if !rules.sources.contains(&source_status) {
        return false;
    }
    if !rules.reasons.contains(&reason_code) {
        return false;
    }
    if reason_code == "theft_or_loss" && direction != Direction::Decrease {
        return false;
    }
    match (direction, target_status) {
        (Direction::Transfer, Some(target)) => {
            rules.targets.contains(&target) && target != source_status
        }
        (Direction::Transfer, None) => false,
        (_, Some(_)) => false,
        (_, None) => true,
    }
}

/// The signed variance a count implies: what was counted, less what the ledger says is there.
///
/// The balance is the server's, read under the posting transaction's write lock. A zero variance is
/// a real and useful answer — "I counted it and it was right" is an audit fact — so it is returned
/// rather than refused, and the caller decides that no movement is needed.
pub fn count_variance(counted_atoms: i64, balance_atoms: i64) -> Result<i64, StockOperationError> {
    if !(0..=MAX_OPERATION_ATOMS).contains(&counted_atoms) {
        return Err(StockOperationError::InvalidCountedQuantity);
    }
    if !(-MAX_OPERATION_ATOMS..=MAX_OPERATION_ATOMS).contains(&balance_atoms) {
        return Err(StockOperationError::InvalidBalance);
    }
    let variance = counted_atoms
        .checked_sub(balance_atoms)
        .ok_or(StockOperationError::Overflow)?;
    if !(-MAX_OPERATION_ATOMS..=MAX_OPERATION_ATOMS).contains(&variance) {
        return Err(StockOperationError::Overflow);
    }
    Ok(variance)
}

/// Which typed reason a count variance carries, given its sign.
///
/// The operator does not choose this. They counted a shelf; whether that is a gain or a loss is a
/// fact about the difference, and letting it be typed in would let the record disagree with the
/// arithmetic.
pub fn count_reason(variance_atoms: i64) -> &'static str {
    if variance_atoms >= 0 {
        "physical_count_gain"
    } else {
        "physical_count_loss"
    }
}

/// The signed effect a non-count line has on the status it acts on.
pub fn applied_delta(
    direction: Direction,
    quantity_atoms: i64,
) -> Result<i64, StockOperationError> {
    if !(1..=MAX_OPERATION_ATOMS).contains(&quantity_atoms) {
        return Err(StockOperationError::InvalidQuantity);
    }
    Ok(match direction {
        Direction::Increase => quantity_atoms,
        Direction::Decrease | Direction::Transfer => -quantity_atoms,
        // A count's delta comes from `count_variance`, never from a requested quantity.
        Direction::Count => return Err(StockOperationError::InvalidQuantity),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_count_that_matches_the_ledger_is_a_zero_variance_not_a_refusal() {
        assert_eq!(count_variance(120, 120), Ok(0));
        // And it is still a countable fact: the caller records the line, not a movement.
        assert_eq!(count_reason(0), "physical_count_gain");
    }

    #[test]
    fn a_count_reports_the_direction_the_arithmetic_found() {
        assert_eq!(count_variance(130, 120), Ok(10));
        assert_eq!(count_reason(10), "physical_count_gain");
        assert_eq!(count_variance(110, 120), Ok(-10));
        assert_eq!(count_reason(-10), "physical_count_loss");
    }

    #[test]
    fn a_count_can_find_a_shelf_empty() {
        assert_eq!(count_variance(0, 40), Ok(-40));
        assert_eq!(count_reason(-40), "physical_count_loss");
    }

    #[test]
    fn a_count_cannot_be_negative_and_cannot_overflow() {
        assert_eq!(
            count_variance(-1, 0),
            Err(StockOperationError::InvalidCountedQuantity)
        );
        assert_eq!(
            count_variance(MAX_OPERATION_ATOMS + 1, 0),
            Err(StockOperationError::InvalidCountedQuantity)
        );
        assert_eq!(
            count_variance(MAX_OPERATION_ATOMS, -MAX_OPERATION_ATOMS),
            Err(StockOperationError::Overflow)
        );
    }

    /// Every kind's table is reachable and closed. A kind the service does not know is not a kind.
    #[test]
    fn only_the_six_implemented_kinds_exist() {
        let kinds: Vec<&str> = KINDS.iter().map(|rules| rules.kind).collect();
        assert_eq!(
            kinds,
            vec![
                "physical_count",
                "adjustment",
                "damage",
                "expiry",
                "quarantine",
                "removal"
            ]
        );
        assert!(rules_for("stock_transfer").is_none());
        assert!(rules_for("").is_none());
    }

    #[test]
    fn a_damage_document_cannot_contain_an_expiry_write_off() {
        assert!(line_is_permitted(
            "damage",
            Direction::Transfer,
            "sellable",
            Some("non_sellable"),
            "damage"
        ));
        // Same shape, wrong cause.
        assert!(!line_is_permitted(
            "damage",
            Direction::Transfer,
            "sellable",
            Some("non_sellable"),
            "expiry"
        ));
    }

    #[test]
    fn damage_may_hold_goods_back_or_write_them_off_and_nothing_else() {
        for target in ["quarantined", "non_sellable"] {
            assert!(line_is_permitted(
                "damage",
                Direction::Transfer,
                "sellable",
                Some(target),
                "breakage"
            ));
        }
        // Damaged goods never become sellable by being damaged.
        assert!(!line_is_permitted(
            "damage",
            Direction::Transfer,
            "quarantined",
            Some("sellable"),
            "breakage"
        ));
    }

    /// The rule that keeps `stock_removal` from becoming an unrecorded sale.
    #[test]
    fn custody_can_only_end_for_goods_already_written_off() {
        assert!(line_is_permitted(
            "removal",
            Direction::Decrease,
            "non_sellable",
            None,
            "disposal"
        ));
        for status in ["sellable", "quarantined"] {
            assert!(
                !line_is_permitted("removal", Direction::Decrease, status, None, "disposal"),
                "removal accepted {status} stock"
            );
        }
    }

    #[test]
    fn goods_already_gone_cannot_be_recorded_as_arriving() {
        assert!(line_is_permitted(
            "adjustment",
            Direction::Decrease,
            "sellable",
            None,
            "theft_or_loss"
        ));
        // Theft that increased the stock is not a thing.
        assert!(!line_is_permitted(
            "adjustment",
            Direction::Increase,
            "sellable",
            None,
            "theft_or_loss"
        ));
        // A data correction may go either way, because a typing mistake can go either way.
        assert!(line_is_permitted(
            "adjustment",
            Direction::Increase,
            "sellable",
            None,
            "data_correction"
        ));
    }

    #[test]
    fn a_transfer_must_name_a_destination_and_a_non_transfer_must_not() {
        assert!(!line_is_permitted(
            "expiry",
            Direction::Transfer,
            "sellable",
            None,
            "expiry"
        ));
        assert!(!line_is_permitted(
            "adjustment",
            Direction::Decrease,
            "sellable",
            Some("non_sellable"),
            "data_correction"
        ));
        // And it must actually move somewhere else.
        assert!(!line_is_permitted(
            "quarantine",
            Direction::Transfer,
            "quarantined",
            Some("quarantined"),
            "quality_hold"
        ));
    }

    #[test]
    fn expiry_writes_off_and_never_merely_holds() {
        assert!(line_is_permitted(
            "expiry",
            Direction::Transfer,
            "sellable",
            Some("non_sellable"),
            "expiry"
        ));
        // An expired lot is never quarantined: no later assessment could make it sellable.
        assert!(!line_is_permitted(
            "expiry",
            Direction::Transfer,
            "sellable",
            Some("quarantined"),
            "expiry"
        ));
    }

    #[test]
    fn a_counted_line_carries_no_requested_quantity() {
        assert_eq!(
            applied_delta(Direction::Count, 10),
            Err(StockOperationError::InvalidQuantity)
        );
        assert_eq!(applied_delta(Direction::Increase, 10), Ok(10));
        assert_eq!(applied_delta(Direction::Decrease, 10), Ok(-10));
        assert_eq!(applied_delta(Direction::Transfer, 10), Ok(-10));
        assert_eq!(
            applied_delta(Direction::Increase, 0),
            Err(StockOperationError::InvalidQuantity)
        );
    }
}
