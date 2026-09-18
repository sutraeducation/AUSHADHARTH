//! Phase 1L-A2 — which recipient particulars a posted Sale must carry, and whether it has them.
//!
//! Three rules, taken from Rule 46 of the CGST Rules and nothing added to them:
//!
//! * **(d)** a REGISTERED recipient: name, address and GSTIN, at any value;
//! * **(e)** an UNREGISTERED recipient where the TAXABLE value is ₹50,000 or more: name, address,
//!   address of delivery, and the State with its code;
//! * **(f)** the same particulars below ₹50,000 when the recipient asks for them.
//!
//! "Unregistered" here means anything the Party master does not positively record as `registered` —
//! `unregistered` or `unknown`, and every walk-in. That is the same line the frozen 1L-A document
//! classification already draws, so a Sale is never a B2B invoice for one purpose and a B2C one for
//! another.
//!
//! The threshold is judged on the value of the TAXABLE supply alone: the sum of `taxable_value_paise`
//! over lines whose frozen treatment is `taxable`. That qualifier matters in this repository —
//! `compute_line` records the full value of an exempt, nil-rated or non-GST line in that line's
//! `taxable_value_paise` too, so the header total of that column is the value of the whole basket,
//! not of its taxable part. Neither untaxed goods nor the tax itself count, so a basket whose payable
//! total crosses ₹50,000 only because of GST or untaxed lines does not trigger (e).
//!
//! Everything here is pure. The Store Service reads the facts inside the posting transaction and
//! hands them in; the same function answers the counter's quote, so what the screen asks for and
//! what posting refuses cannot drift apart.

use crate::domain::parties::normalize_gstin;

/// ₹50,000 in paise: the Rule 46(e) line, compared against taxable value with `>=`.
pub const RECIPIENT_PARTICULARS_THRESHOLD_PAISE: i64 = 5_000_000;

/// Why this Sale's invoice must show recipient particulars. More than one can apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequirementReason {
    /// Rule 46(d).
    RegisteredRecipient,
    /// Rule 46(e).
    TaxableValueThreshold,
    /// Rule 46(f).
    RecipientRequested,
}

impl RequirementReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RegisteredRecipient => "registered_recipient",
            Self::TaxableValueThreshold => "taxable_value_threshold",
            Self::RecipientRequested => "recipient_requested",
        }
    }
}

/// One particular the invoice needs and the Sale does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingRecipientFact {
    /// A walk-in with no name typed for the bill.
    Name,
    /// A party recorded as registered whose GSTIN is absent or fails its check digit.
    Gstin,
    /// The customer's record has no active billing address.
    PartyAddress,
    /// The customer's record has several active billing addresses and none is marked primary.
    PartyAddressAmbiguous,
    /// The customer's billing address has no State.
    PartyAddressState,
    CounterAddressLine1,
    CounterAddressState,
    DeliveryAddressLine1,
    DeliveryAddressState,
}

impl MissingRecipientFact {
    pub fn field(self) -> &'static str {
        match self {
            Self::Name => "customerNameText",
            Self::Gstin => "customer.gstin",
            Self::PartyAddress | Self::PartyAddressAmbiguous => "customer.billingAddress",
            Self::PartyAddressState => "customer.billingAddress.stateId",
            Self::CounterAddressLine1 => "recipientAddress.line1",
            Self::CounterAddressState => "recipientAddress.stateId",
            Self::DeliveryAddressLine1 => "deliveryAddress.line1",
            Self::DeliveryAddressState => "deliveryAddress.stateId",
        }
    }

    /// Said to the person at the counter, not to a lawyer.
    pub fn message(self) -> &'static str {
        match self {
            Self::Name => "Enter the customer's name for the invoice.",
            Self::Gstin => {
                "This customer is recorded as GST-registered without a valid GSTIN. Correct it in Parties."
            }
            Self::PartyAddress => "Add a billing address to this customer in Parties.",
            Self::PartyAddressAmbiguous => {
                "This customer has several billing addresses. Mark one as primary in Parties."
            }
            Self::PartyAddressState => {
                "Add the State to this customer's billing address in Parties."
            }
            Self::CounterAddressLine1 => "Enter the customer's address.",
            Self::CounterAddressState => "Choose the State of the customer's address.",
            Self::DeliveryAddressLine1 => "Enter the delivery address.",
            Self::DeliveryAddressState => "Choose the State of the delivery address.",
        }
    }
}

/// An address as it would be frozen: text exactly as recorded, and the State already resolved to
/// its printed name and code.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddressFacts {
    pub line1: Option<String>,
    pub line2: Option<String>,
    pub city: Option<String>,
    pub postal_code: Option<String>,
    pub state_id: Option<String>,
    pub state_name: Option<String>,
    pub state_code: Option<String>,
}

impl AddressFacts {
    fn has_line1(&self) -> bool {
        present(self.line1.as_deref())
    }

    /// A State counts only when it resolved to both a name and a code.
    fn has_state(&self) -> bool {
        self.state_id.is_some()
            && present(self.state_name.as_deref())
            && present(self.state_code.as_deref())
    }
}

/// What the customer's own record says about their billing address, at the moment it was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartyBilling {
    None,
    /// The primary active billing address, or the only active one.
    One(AddressFacts),
    /// Several active billing addresses, none primary. Choosing one would be a guess.
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartyRecipient {
    pub display_name: String,
    pub gst_registration_status: String,
    pub normalized_gstin: Option<String>,
    pub billing: PartyBilling,
}

/// Everything the rule needs, read from the Sale and — for a named customer — from the Party master
/// in the same transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientSource {
    /// `None` is a walk-in.
    pub party: Option<PartyRecipient>,
    pub counter_name: Option<String>,
    pub counter_address: AddressFacts,
    pub particulars_requested: bool,
    pub delivery_same_as_recipient: bool,
    pub delivery_address: AddressFacts,
    /// Sum of `taxable_value_paise` over lines treated as `taxable` only. See the module note.
    pub taxable_supply_value_paise: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressSource {
    Party,
    Counter,
}

impl AddressSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Party => "party",
            Self::Counter => "counter",
        }
    }
}

/// What a version-1 Sale freezes about its recipient beyond the Phase 1H name, status and GSTIN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientSnapshot {
    pub particulars_requested: bool,
    pub address: Option<(AddressSource, AddressFacts)>,
    /// `None` when no delivery particular was required — Rule 46(d) names none, and a Sale needing
    /// no particulars asserts nothing about where the goods went.
    pub delivery_same_as_recipient: Option<bool>,
    pub delivery_address: Option<AddressFacts>,
}

/// Whether particulars are required, why, and what is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub reasons: Vec<RequirementReason>,
    pub missing: Vec<MissingRecipientFact>,
}

impl Requirement {
    pub fn required(&self) -> bool {
        !self.reasons.is_empty()
    }
}

fn present(value: Option<&str>) -> bool {
    value.map(str::trim).is_some_and(|value| !value.is_empty())
}

fn is_registered(source: &RecipientSource) -> bool {
    source
        .party
        .as_ref()
        .is_some_and(|party| party.gst_registration_status == "registered")
}

/// The Rule 46 reasons that apply to this Sale, in a fixed order.
pub fn reasons(source: &RecipientSource) -> Vec<RequirementReason> {
    let mut reasons = Vec::new();
    if is_registered(source) {
        reasons.push(RequirementReason::RegisteredRecipient);
    } else if source.taxable_supply_value_paise >= RECIPIENT_PARTICULARS_THRESHOLD_PAISE {
        reasons.push(RequirementReason::TaxableValueThreshold);
    }
    // Recorded for a registered recipient too — the customer did ask — but it adds no particular
    // beyond what (d) already demands.
    if source.particulars_requested {
        reasons.push(RequirementReason::RecipientRequested);
    }
    reasons
}

/// What this Sale must carry, and which of it is missing.
pub fn requirement(source: &RecipientSource) -> Requirement {
    let reasons = reasons(source);
    if reasons.is_empty() {
        return Requirement {
            reasons,
            missing: Vec::new(),
        };
    }
    let registered = is_registered(source);
    let mut missing = Vec::new();

    match &source.party {
        Some(party) => {
            // A party's display name is NOT NULL in the master, so it cannot be missing here.
            if registered
                && !party.normalized_gstin.as_deref().is_some_and(|gstin| {
                    normalize_gstin(gstin).is_ok_and(|(_, normal)| normal == gstin)
                })
            {
                missing.push(MissingRecipientFact::Gstin);
            }
            match &party.billing {
                PartyBilling::None => missing.push(MissingRecipientFact::PartyAddress),
                PartyBilling::Ambiguous => {
                    missing.push(MissingRecipientFact::PartyAddressAmbiguous)
                }
                PartyBilling::One(address) => {
                    if !address.has_line1() {
                        missing.push(MissingRecipientFact::PartyAddress);
                    } else if !registered && !address.has_state() {
                        missing.push(MissingRecipientFact::PartyAddressState);
                    }
                }
            }
        }
        None => {
            if !present(source.counter_name.as_deref()) {
                missing.push(MissingRecipientFact::Name);
            }
            if !source.counter_address.has_line1() {
                missing.push(MissingRecipientFact::CounterAddressLine1);
            }
            if !source.counter_address.has_state() {
                missing.push(MissingRecipientFact::CounterAddressState);
            }
        }
    }

    // Address of delivery belongs to the Rule 46(e)/(f) path only. "Delivered elsewhere" is a
    // statement the operator made, and a document that said so without saying where would
    // contradict itself.
    //
    // These addresses are DOCUMENTARY particulars, and nothing here reads them as where the goods
    // travelled. The only transaction this POS models is a physical handover at the pharmacy
    // counter, whose tax treatment is fixed by that handover (see `post_within_transaction`), not by
    // the State printed in anybody's address. A customer or delivery address in another State is
    // therefore recorded as given and never compared with the Store's State. Supplier-arranged
    // delivery is a different transaction, unsupported here, and must model movement explicitly
    // rather than infer it from a postal address.
    if !registered && !source.delivery_same_as_recipient {
        if !source.delivery_address.has_line1() {
            missing.push(MissingRecipientFact::DeliveryAddressLine1);
        }
        if !source.delivery_address.has_state() {
            missing.push(MissingRecipientFact::DeliveryAddressState);
        }
    }

    Requirement { reasons, missing }
}

/// The snapshot to freeze at posting, or the particulars that stop it.
///
/// When no rule applies, the invoice carries no recipient address and no delivery address, whatever
/// was typed on the draft: Rule 46(f) is the customer asking, and the operator records that with the
/// request flag rather than by the side effect of filling in a form.
pub fn resolve(source: &RecipientSource) -> Result<RecipientSnapshot, Vec<MissingRecipientFact>> {
    let requirement = requirement(source);
    if !requirement.missing.is_empty() {
        return Err(requirement.missing);
    }
    if !requirement.required() {
        return Ok(RecipientSnapshot {
            particulars_requested: false,
            address: None,
            delivery_same_as_recipient: None,
            delivery_address: None,
        });
    }
    let registered = is_registered(source);
    let address = match &source.party {
        Some(party) => match &party.billing {
            PartyBilling::One(address) => (AddressSource::Party, address.clone()),
            // Unreachable: `requirement` already reported these as missing.
            PartyBilling::None | PartyBilling::Ambiguous => {
                return Err(vec![MissingRecipientFact::PartyAddress]);
            }
        },
        None => (AddressSource::Counter, source.counter_address.clone()),
    };
    let (delivery_same_as_recipient, delivery_address) = if registered {
        (None, None)
    } else {
        (
            Some(source.delivery_same_as_recipient),
            (!source.delivery_same_as_recipient).then(|| source.delivery_address.clone()),
        )
    };
    Ok(RecipientSnapshot {
        particulars_requested: source.particulars_requested,
        address: Some(address),
        delivery_same_as_recipient,
        delivery_address,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GSTIN: &str = "27AAPFU0939F1ZV";

    fn address(line1: Option<&str>, state: bool) -> AddressFacts {
        AddressFacts {
            line1: line1.map(str::to_owned),
            line2: None,
            city: Some("Pune".to_owned()),
            postal_code: Some("411001".to_owned()),
            state_id: state.then(|| "01997300-0000-7000-8000-000000000027".to_owned()),
            state_name: state.then(|| "Maharashtra".to_owned()),
            state_code: state.then(|| "27".to_owned()),
        }
    }

    fn walk_in(taxable: i64) -> RecipientSource {
        RecipientSource {
            party: None,
            counter_name: None,
            counter_address: AddressFacts::default(),
            particulars_requested: false,
            delivery_same_as_recipient: true,
            delivery_address: AddressFacts::default(),
            taxable_supply_value_paise: taxable,
        }
    }

    fn karnataka(line1: &str) -> AddressFacts {
        AddressFacts {
            line1: Some(line1.to_owned()),
            state_id: Some("01997300-0000-7000-8000-000000000029".to_owned()),
            state_name: Some("Karnataka".to_owned()),
            state_code: Some("29".to_owned()),
            ..AddressFacts::default()
        }
    }

    fn party(status: &str, gstin: Option<&str>, billing: PartyBilling) -> PartyRecipient {
        PartyRecipient {
            display_name: "Rahul Traders".to_owned(),
            gst_registration_status: status.to_owned(),
            normalized_gstin: gstin.map(str::to_owned),
            billing,
        }
    }

    #[test]
    fn an_ordinary_walk_in_below_the_threshold_needs_nothing() {
        let requirement = requirement(&walk_in(4_999_999));
        assert!(!requirement.required());
        assert!(requirement.missing.is_empty());
        let snapshot = resolve(&walk_in(100)).unwrap();
        assert_eq!(snapshot.address, None);
        assert_eq!(snapshot.delivery_same_as_recipient, None);
    }

    #[test]
    fn the_threshold_is_inclusive_at_exactly_fifty_thousand_rupees() {
        assert!(!requirement(&walk_in(4_999_999)).required());
        assert_eq!(
            reasons(&walk_in(5_000_000)),
            vec![RequirementReason::TaxableValueThreshold]
        );
        assert_eq!(
            reasons(&walk_in(5_000_001)),
            vec![RequirementReason::TaxableValueThreshold]
        );
    }

    #[test]
    fn at_the_threshold_a_walk_in_must_supply_name_address_and_state() {
        assert_eq!(
            requirement(&walk_in(5_000_000)).missing,
            vec![
                MissingRecipientFact::Name,
                MissingRecipientFact::CounterAddressLine1,
                MissingRecipientFact::CounterAddressState,
            ]
        );
    }

    #[test]
    fn a_complete_counter_recipient_is_frozen_as_typed() {
        let mut source = walk_in(5_000_000);
        source.counter_name = Some("Asha Patil".to_owned());
        source.counter_address = address(Some("4 Lake View"), true);
        let snapshot = resolve(&source).unwrap();
        assert_eq!(
            snapshot.address,
            Some((AddressSource::Counter, address(Some("4 Lake View"), true)))
        );
        assert_eq!(snapshot.delivery_same_as_recipient, Some(true));
        assert_eq!(snapshot.delivery_address, None);
    }

    #[test]
    fn a_request_below_the_threshold_requires_the_same_particulars() {
        let mut source = walk_in(100);
        source.particulars_requested = true;
        assert_eq!(
            reasons(&source),
            vec![RequirementReason::RecipientRequested]
        );
        assert_eq!(requirement(&source).missing.len(), 3);
    }

    #[test]
    fn a_registered_recipient_has_no_threshold() {
        let source = RecipientSource {
            party: Some(party("registered", Some(GSTIN), PartyBilling::None)),
            ..walk_in(100)
        };
        assert_eq!(
            reasons(&source),
            vec![RequirementReason::RegisteredRecipient]
        );
        assert_eq!(
            requirement(&source).missing,
            vec![MissingRecipientFact::PartyAddress]
        );
    }

    #[test]
    fn a_registered_recipient_with_an_address_and_valid_gstin_resolves_at_one_rupee() {
        let source = RecipientSource {
            party: Some(party(
                "registered",
                Some(GSTIN),
                PartyBilling::One(address(Some("7 Mill Road"), false)),
            )),
            ..walk_in(100)
        };
        let snapshot = resolve(&source).unwrap();
        // Rule 46(d) names no State and no address of delivery, so neither is demanded or recorded.
        assert_eq!(snapshot.address.unwrap().0, AddressSource::Party);
        assert_eq!(snapshot.delivery_same_as_recipient, None);
    }

    #[test]
    fn a_registered_recipient_in_another_state_resolves_with_its_own_address() {
        // A Karnataka GSTIN whose check character is genuinely right, found by the production
        // validator rather than hard-coded.
        let gstin = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"
            .chars()
            .map(|check| format!("29AAACM1234K1Z{check}"))
            .find(|candidate| normalize_gstin(candidate).is_ok())
            .expect("a valid check character exists");
        let source = RecipientSource {
            party: Some(party(
                "registered",
                Some(&gstin),
                PartyBilling::One(karnataka("Mysore Road")),
            )),
            ..walk_in(100)
        };
        let snapshot = resolve(&source).expect("an address in another State is not a refusal");
        assert_eq!(
            snapshot.address,
            Some((AddressSource::Party, karnataka("Mysore Road")))
        );
        assert_eq!(snapshot.delivery_same_as_recipient, None);
    }

    #[test]
    fn a_delivery_address_in_another_state_is_recorded_as_given() {
        let mut source = walk_in(5_000_000);
        source.counter_name = Some("Asha".to_owned());
        source.counter_address = address(Some("4 Lake View"), true);
        source.delivery_same_as_recipient = false;
        source.delivery_address = karnataka("Warehouse 2, Mysore Road");
        let snapshot =
            resolve(&source).expect("a different-State delivery address is not a refusal");
        assert_eq!(snapshot.delivery_same_as_recipient, Some(false));
        assert_eq!(
            snapshot.delivery_address,
            Some(karnataka("Warehouse 2, Mysore Road"))
        );
    }

    #[test]
    fn a_recipient_address_in_another_state_is_recorded_as_given() {
        let mut source = walk_in(5_000_000);
        source.counter_name = Some("Asha".to_owned());
        source.counter_address = karnataka("4 Brigade Road");
        let snapshot =
            resolve(&source).expect("a different-State recipient address is not a refusal");
        assert_eq!(
            snapshot.address,
            Some((AddressSource::Counter, karnataka("4 Brigade Road")))
        );
        assert_eq!(snapshot.delivery_same_as_recipient, Some(true));
    }

    #[test]
    fn a_different_state_delivery_address_still_needs_its_own_particulars() {
        let mut source = walk_in(5_000_000);
        source.counter_name = Some("Asha".to_owned());
        source.counter_address = karnataka("4 Brigade Road");
        source.delivery_same_as_recipient = false;
        source.delivery_address = AddressFacts {
            line1: Some("Warehouse 2".to_owned()),
            ..AddressFacts::default()
        };
        assert_eq!(
            requirement(&source).missing,
            vec![MissingRecipientFact::DeliveryAddressState]
        );
    }

    #[test]
    fn a_registered_gstin_failing_its_check_digit_is_missing() {
        let source = RecipientSource {
            party: Some(party(
                "registered",
                Some("27AAPFU0939F1ZX"),
                PartyBilling::One(address(Some("7 Mill Road"), true)),
            )),
            ..walk_in(100)
        };
        assert_eq!(
            requirement(&source).missing,
            vec![MissingRecipientFact::Gstin]
        );
    }

    #[test]
    fn an_unknown_registration_is_treated_as_unregistered() {
        let source = RecipientSource {
            party: Some(party("unknown", None, PartyBilling::None)),
            ..walk_in(4_999_999)
        };
        assert!(!requirement(&source).required());
    }

    #[test]
    fn several_billing_addresses_with_no_primary_are_not_guessed_between() {
        let source = RecipientSource {
            party: Some(party("registered", Some(GSTIN), PartyBilling::Ambiguous)),
            ..walk_in(100)
        };
        assert_eq!(
            requirement(&source).missing,
            vec![MissingRecipientFact::PartyAddressAmbiguous]
        );
    }

    #[test]
    fn an_unregistered_party_over_the_threshold_needs_a_state_on_its_address() {
        let source = RecipientSource {
            party: Some(party(
                "unregistered",
                None,
                PartyBilling::One(address(Some("7 Mill Road"), false)),
            )),
            ..walk_in(5_000_000)
        };
        assert_eq!(
            requirement(&source).missing,
            vec![MissingRecipientFact::PartyAddressState]
        );
    }

    #[test]
    fn delivery_elsewhere_needs_a_complete_delivery_address() {
        let mut source = walk_in(5_000_000);
        source.counter_name = Some("Asha".to_owned());
        source.counter_address = address(Some("4 Lake View"), true);
        source.delivery_same_as_recipient = false;
        assert_eq!(
            requirement(&source).missing,
            vec![
                MissingRecipientFact::DeliveryAddressLine1,
                MissingRecipientFact::DeliveryAddressState
            ]
        );
        source.delivery_address = address(Some("Site Office, Plot 9"), true);
        let snapshot = resolve(&source).unwrap();
        assert_eq!(snapshot.delivery_same_as_recipient, Some(false));
        assert_eq!(
            snapshot.delivery_address,
            Some(address(Some("Site Office, Plot 9"), true))
        );
    }

    #[test]
    fn nothing_typed_is_frozen_when_no_rule_applies() {
        let mut source = walk_in(100);
        source.counter_name = Some("Asha".to_owned());
        source.counter_address = address(Some("4 Lake View"), true);
        source.delivery_same_as_recipient = false;
        let snapshot = resolve(&source).unwrap();
        assert_eq!(snapshot.address, None);
        assert_eq!(snapshot.delivery_same_as_recipient, None);
        assert_eq!(snapshot.delivery_address, None);
    }

    #[test]
    fn a_blank_line_is_not_an_address() {
        let mut source = walk_in(5_000_000);
        source.counter_name = Some("Asha".to_owned());
        source.counter_address = address(Some("   "), true);
        assert_eq!(
            requirement(&source).missing,
            vec![MissingRecipientFact::CounterAddressLine1]
        );
    }
}
