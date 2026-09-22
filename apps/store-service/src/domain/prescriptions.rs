//! Phase 1M-B — what a prescription still authorises.
//!
//! Pure rules, with no database, so every one of them can be proved in isolation and the posting
//! transaction only has to gather the facts and ask. The facts are always recomputed from the
//! append-only dispensing ledger: there is no stored "remaining" number anywhere to trust.
//!
//! The rules are the Drugs Rules, 1945, and nothing more:
//!
//! * rule 65(10)(c) — the prescription states the total amount to be supplied, so no dispensing,
//!   alone or with the ones before it, may exceed it;
//! * rule 65(11)(a) — "the prescription must not be dispensed more than once unless the prescriber
//!   has stated thereon that it may be dispensed more than once". A second supply is a second
//!   dispensing, however small the first one was;
//! * rule 65(11)(b) — where the prescriber stated a number of times or an interval, "it must not be
//!   dispensed otherwise than in accordance with the directions";
//! * rule 65(11A) — no other preparation "in lieu thereof, whether containing the same substances or
//!   not". The product dispensed must be the product prescribed; a shared salt, generic, HSN or
//!   dosage form is not authority.

use time::{Date, Duration, macros::format_description};

/// What the prescriber stated about repeats. A blank on the paper can only ever mean `Once`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatAuthority {
    /// Not to be dispensed more than once — the rule's default.
    Once,
    /// A stated number of dispensings in total, the first one included.
    StatedTimes(i64),
    /// Stated that it may be dispensed more than once, with no number. Still bounded by the total
    /// quantity the prescription states.
    StatedWithoutCount,
}

impl RepeatAuthority {
    pub fn parse(kind: &str, times: Option<i64>) -> Option<Self> {
        match (kind, times) {
            ("once", None) => Some(Self::Once),
            ("stated_times", Some(count)) if count >= 2 => Some(Self::StatedTimes(count)),
            ("stated_without_count", None) => Some(Self::StatedWithoutCount),
            _ => None,
        }
    }

    /// How many separate dispensings the prescriber authorised, where a number exists.
    pub fn authorised_occasions(self) -> Option<i64> {
        match self {
            Self::Once => Some(1),
            Self::StatedTimes(count) => Some(count),
            Self::StatedWithoutCount => None,
        }
    }
}

/// Everything a posting knows about one prescription item at the moment it asks.
#[derive(Debug, Clone)]
pub struct ItemAuthority<'a> {
    pub prescribed_product_id: &'a str,
    pub prescribed_quantity_atoms: i64,
    /// Every earlier dispensing of this item, net of its reversals.
    pub net_dispensed_atoms: i64,
    /// Distinct earlier Sales that dispensed anything from this prescription. Reversals do not
    /// reduce this: an occasion that happened was endorsed on the paper.
    pub earlier_occasions: i64,
    pub last_dispensed_on: Option<&'a str>,
    pub repeat: RepeatAuthority,
    pub repeat_interval_days: Option<i64>,
    pub prescribed_on: &'a str,
}

/// The request: this Sale line, on these dates.
#[derive(Debug, Clone)]
pub struct DispenseRequest<'a> {
    pub product_id: &'a str,
    pub quantity_atoms: i64,
    /// The Sale's business date — the date of supply the document records.
    pub business_date: &'a str,
    /// The store's actual posting day(s). A prescription written after either cannot authorise it.
    pub posting_days: &'a [String],
}

/// Why a dispensing cannot happen, named for the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispenseRefusal {
    /// Rule 65(11A): a different product from the one prescribed.
    SubstitutionNotPermitted,
    /// Rule 65(10)(c): more than the prescription's total, counting what was already supplied.
    QuantityExceeded,
    /// Rule 65(11)(a)/(b): one more dispensing than the prescriber authorised.
    RepeatNotAuthorised,
    /// Rule 65(11)(b): sooner than the stated interval allows.
    RepeatTooSoon,
    /// The Sale is dated before the prescription was written, or the prescription is dated after
    /// the day it is being dispensed — backdating cannot make a later prescription earlier.
    DatedBeforePrescription,
    /// A date that is not a calendar date reached here.
    InvalidDate,
}

impl DispenseRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::SubstitutionNotPermitted => "prescription_substitution_not_permitted",
            Self::QuantityExceeded => "prescription_quantity_exceeded",
            Self::RepeatNotAuthorised => "prescription_repeat_not_authorised",
            Self::RepeatTooSoon => "prescription_repeat_too_soon",
            Self::DatedBeforePrescription => "prescription_dated_after_supply",
            Self::InvalidDate => "prescription_date_invalid",
        }
    }
}

fn parse(value: &str) -> Result<Date, DispenseRefusal> {
    Date::parse(value, format_description!("[year]-[month]-[day]"))
        .map_err(|_| DispenseRefusal::InvalidDate)
}

/// What is left of the item's total after everything already supplied.
pub fn remaining_atoms(authority: &ItemAuthority<'_>) -> i64 {
    (authority.prescribed_quantity_atoms - authority.net_dispensed_atoms).max(0)
}

/// Whether this Sale line may be dispensed against this item. The first refusal found is returned,
/// in the order an operator can act on: the wrong product first, then the dates, then the amounts.
pub fn check_dispense(
    authority: &ItemAuthority<'_>,
    request: &DispenseRequest<'_>,
) -> Result<(), DispenseRefusal> {
    if request.product_id != authority.prescribed_product_id {
        return Err(DispenseRefusal::SubstitutionNotPermitted);
    }

    let prescribed_on = parse(authority.prescribed_on)?;
    if parse(request.business_date)? < prescribed_on {
        return Err(DispenseRefusal::DatedBeforePrescription);
    }
    for day in request.posting_days {
        if parse(day)? < prescribed_on {
            return Err(DispenseRefusal::DatedBeforePrescription);
        }
    }

    if let Some(authorised) = authority.repeat.authorised_occasions()
        && authority.earlier_occasions + 1 > authorised
    {
        return Err(DispenseRefusal::RepeatNotAuthorised);
    }

    if authority.earlier_occasions > 0
        && let (Some(interval), Some(last)) =
            (authority.repeat_interval_days, authority.last_dispensed_on)
    {
        let earliest = parse(last)? + Duration::days(interval);
        if parse(request.business_date)? < earliest {
            return Err(DispenseRefusal::RepeatTooSoon);
        }
    }

    if request.quantity_atoms > remaining_atoms(authority) {
        return Err(DispenseRefusal::QuantityExceeded);
    }
    Ok(())
}

/// Whether a professional's recorded validity covers every day in `days`. An open end on either
/// side is open; a recorded date is inclusive. A record with no dates at all covers every day —
/// the Store recorded no limit — but a record that has expired covers none after its end.
pub fn professional_valid_on(
    valid_from: Option<&str>,
    valid_upto: Option<&str>,
    days: &[&str],
) -> Result<bool, DispenseRefusal> {
    for day in days {
        let day = parse(day)?;
        if let Some(from) = valid_from
            && day < parse(from)?
        {
            return Ok(false);
        }
        if let Some(upto) = valid_upto
            && day > parse(upto)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRODUCT: &str = "product-a";
    const OTHER: &str = "product-b";

    fn authority(repeat: RepeatAuthority) -> ItemAuthority<'static> {
        ItemAuthority {
            prescribed_product_id: PRODUCT,
            prescribed_quantity_atoms: 30,
            net_dispensed_atoms: 0,
            earlier_occasions: 0,
            last_dispensed_on: None,
            repeat,
            repeat_interval_days: None,
            prescribed_on: "2026-09-01",
        }
    }

    fn today() -> Vec<String> {
        vec!["2026-09-21".to_owned()]
    }

    fn request<'a>(quantity: i64, days: &'a [String]) -> DispenseRequest<'a> {
        DispenseRequest {
            product_id: PRODUCT,
            quantity_atoms: quantity,
            business_date: "2026-09-21",
            posting_days: days,
        }
    }

    #[test]
    fn a_blank_on_the_paper_parses_only_as_once() {
        assert_eq!(
            RepeatAuthority::parse("once", None),
            Some(RepeatAuthority::Once)
        );
        assert_eq!(RepeatAuthority::parse("stated_times", None), None);
        assert_eq!(RepeatAuthority::parse("stated_times", Some(1)), None);
        assert_eq!(RepeatAuthority::parse("once", Some(3)), None);
        assert_eq!(RepeatAuthority::parse("unlimited", None), None);
        assert_eq!(RepeatAuthority::Once.authorised_occasions(), Some(1));
        assert_eq!(
            RepeatAuthority::StatedTimes(3).authorised_occasions(),
            Some(3)
        );
        assert_eq!(
            RepeatAuthority::StatedWithoutCount.authorised_occasions(),
            None
        );
    }

    #[test]
    fn a_first_dispensing_within_the_total_is_allowed() {
        let days = today();
        assert_eq!(
            check_dispense(&authority(RepeatAuthority::Once), &request(30, &days)),
            Ok(())
        );
        assert_eq!(
            check_dispense(&authority(RepeatAuthority::Once), &request(10, &days)),
            Ok(())
        );
    }

    #[test]
    fn more_than_the_prescribed_total_is_refused() {
        let days = today();
        assert_eq!(
            check_dispense(&authority(RepeatAuthority::Once), &request(31, &days)),
            Err(DispenseRefusal::QuantityExceeded)
        );
    }

    /// Rule 65(11)(a): partial supply on a one-time prescription uses the one occasion up.
    #[test]
    fn a_one_time_prescription_cannot_be_dispensed_twice_even_with_quantity_left() {
        let days = today();
        let mut used = authority(RepeatAuthority::Once);
        used.net_dispensed_atoms = 10;
        used.earlier_occasions = 1;
        assert_eq!(remaining_atoms(&used), 20);
        assert_eq!(
            check_dispense(&used, &request(10, &days)),
            Err(DispenseRefusal::RepeatNotAuthorised)
        );
    }

    /// Partial dispensing where repeats are stated: 30 prescribed, 10 then 20, then nothing.
    #[test]
    fn stated_repeats_allow_partial_supply_up_to_the_total_and_the_count() {
        let days = today();
        let mut item = authority(RepeatAuthority::StatedTimes(2));
        item.net_dispensed_atoms = 10;
        item.earlier_occasions = 1;
        item.last_dispensed_on = Some("2026-09-10");
        assert_eq!(check_dispense(&item, &request(20, &days)), Ok(()));
        assert_eq!(
            check_dispense(&item, &request(21, &days)),
            Err(DispenseRefusal::QuantityExceeded)
        );
        item.net_dispensed_atoms = 30;
        item.earlier_occasions = 2;
        assert_eq!(
            check_dispense(&item, &request(1, &days)),
            Err(DispenseRefusal::RepeatNotAuthorised)
        );
    }

    #[test]
    fn repeats_without_a_count_are_still_bounded_by_the_total() {
        let days = today();
        let mut item = authority(RepeatAuthority::StatedWithoutCount);
        item.net_dispensed_atoms = 29;
        item.earlier_occasions = 7;
        assert_eq!(check_dispense(&item, &request(1, &days)), Ok(()));
        assert_eq!(
            check_dispense(&item, &request(2, &days)),
            Err(DispenseRefusal::QuantityExceeded)
        );
    }

    /// Rule 65(11)(b): a stated interval holds the next dispensing back until it has passed.
    #[test]
    fn a_stated_interval_is_enforced_from_the_last_dispensing() {
        let days = today();
        let mut item = authority(RepeatAuthority::StatedTimes(3));
        item.net_dispensed_atoms = 10;
        item.earlier_occasions = 1;
        item.last_dispensed_on = Some("2026-09-15");
        item.repeat_interval_days = Some(7);
        assert_eq!(
            check_dispense(&item, &request(10, &days)),
            Err(DispenseRefusal::RepeatTooSoon)
        );
        item.last_dispensed_on = Some("2026-09-14");
        assert_eq!(check_dispense(&item, &request(10, &days)), Ok(()));
    }

    /// Rule 65(11A): the same salt, the same generic — none of it is the prescribed product.
    #[test]
    fn another_product_is_never_the_prescribed_preparation() {
        let days = today();
        let mut other = request(1, &days);
        other.product_id = OTHER;
        assert_eq!(
            check_dispense(&authority(RepeatAuthority::StatedWithoutCount), &other),
            Err(DispenseRefusal::SubstitutionNotPermitted)
        );
    }

    /// Backdating cannot make a prescription earlier than it was written, in either direction.
    #[test]
    fn a_supply_dated_before_the_prescription_is_refused() {
        let days = today();
        let mut early = request(1, &days);
        early.business_date = "2026-08-31";
        assert_eq!(
            check_dispense(&authority(RepeatAuthority::Once), &early),
            Err(DispenseRefusal::DatedBeforePrescription)
        );
        let past = vec!["2026-08-20".to_owned()];
        let mut future_prescription = authority(RepeatAuthority::Once);
        future_prescription.prescribed_on = "2026-08-25";
        let mut backdated = request(1, &past);
        backdated.business_date = "2026-08-25";
        assert_eq!(
            check_dispense(&future_prescription, &backdated),
            Err(DispenseRefusal::DatedBeforePrescription)
        );
    }

    #[test]
    fn a_professional_is_valid_only_inside_the_recorded_dates() {
        assert_eq!(professional_valid_on(None, None, &["2026-09-21"]), Ok(true));
        assert_eq!(
            professional_valid_on(Some("2026-09-21"), Some("2026-09-21"), &["2026-09-21"]),
            Ok(true)
        );
        assert_eq!(
            professional_valid_on(Some("2026-09-22"), None, &["2026-09-21"]),
            Ok(false)
        );
        assert_eq!(
            professional_valid_on(None, Some("2026-09-20"), &["2026-09-21"]),
            Ok(false)
        );
        // Every day asked about must be covered: valid on the business date is not enough.
        assert_eq!(
            professional_valid_on(None, Some("2026-09-20"), &["2026-09-19", "2026-09-21"]),
            Ok(false)
        );
    }

    #[test]
    fn a_malformed_date_refuses_rather_than_passes() {
        let bad = vec!["2026-13-01".to_owned()];
        assert_eq!(
            check_dispense(&authority(RepeatAuthority::Once), &request(1, &bad)),
            Err(DispenseRefusal::InvalidDate)
        );
        assert_eq!(
            professional_valid_on(Some("x"), None, &["2026-09-21"]),
            Err(DispenseRefusal::InvalidDate)
        );
    }
}
