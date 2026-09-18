//! The seller a pharmacy is, as a posted Sale must remember it.
//!
//! Two decisions live here rather than in the API layer, because both have to produce the same
//! answer on every machine and after every restore:
//!
//!   * **what makes a seller profile complete enough to issue a memo** — the three particulars
//!     Rule 65(4)(3)(i) of the Drugs Rules requires of a retail drug sale: the dealer's name, the
//!     dealer's address, and the dealer's sale licence number;
//!   * **how several licences become one printable line** — deterministically, so two installations
//!     restoring the same backup print the same document.

use serde::Serialize;

/// One active licence, as the printable line needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SellerLicence {
    pub licence_type: String,
    pub licence_number: String,
    /// Alphanumerics only, upper-cased. Used for ordering and duplicate detection, never printed.
    pub normalized_licence_number: String,
    /// The operator designated this licence for printing on retail drug memos (Phase 1L-A3).
    /// Never inferred from `licence_type`.
    pub include_on_retail_memo: bool,
}

/// A seller fact a memo cannot lawfully be issued without.
///
/// Serialised into the refusal so the browser can point at the field instead of saying "something
/// is missing" to somebody standing at a counter with a customer waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MissingSellerFact {
    LegalName,
    AddressLine1,
    SaleLicence,
}

impl MissingSellerFact {
    pub fn field(self) -> &'static str {
        match self {
            Self::LegalName => "legalName",
            Self::AddressLine1 => "address.line1",
            Self::SaleLicence => "licences",
        }
    }

    /// Said to a pharmacy operator, not to a lawyer.
    pub fn message(self) -> &'static str {
        match self {
            Self::LegalName => "Record the pharmacy's registered name in Store Profile.",
            Self::AddressLine1 => "Record the pharmacy's address in Store Profile.",
            Self::SaleLicence => "Record at least one active drug sale licence in Store Profile.",
        }
    }
}

/// Everything a posted Sale freezes about the seller.
///
/// The GSTIN, the State code and the place of supply are deliberately absent: Phase 1H already
/// freezes those onto `sale_documents`, and a second copy would eventually disagree with the first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SellerSnapshot {
    pub legal_name: String,
    pub trade_name: String,
    pub address_line1: String,
    pub address_line2: Option<String>,
    pub city: Option<String>,
    pub postal_code: Option<String>,
    pub state_name: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub licence_text: String,
    /// The designated licences only, rendered the same way; `None` when none is designated.
    pub retail_memo_licence_text: Option<String>,
}

/// The seller as the Store currently stands, before it is known to be complete.
#[derive(Debug, Clone, Default)]
pub struct SellerProfileSource {
    pub legal_name: Option<String>,
    pub trade_name: String,
    pub address_line1: Option<String>,
    pub address_line2: Option<String>,
    pub city: Option<String>,
    pub postal_code: Option<String>,
    pub state_name: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub active_licences: Vec<SellerLicence>,
}

/// One printable licence line from every active licence.
///
/// Ordered by type and then by the normalised number, so the order does not depend on insertion
/// order, on row ids, or on whichever index SQLite happened to choose. Two installations restoring
/// the same backup must produce the same string, because that string is frozen onto the document.
///
/// Rendered as `"<type>: <number>"` joined by `", "`, and the number is printed exactly as the
/// operator entered it — a licence is transcribed from a certificate, not parsed.
pub fn licence_text(licences: &[SellerLicence]) -> Option<String> {
    let mut ordered: Vec<&SellerLicence> = licences.iter().collect();
    ordered.sort_by(|left, right| {
        left.licence_type
            .to_lowercase()
            .cmp(&right.licence_type.to_lowercase())
            .then_with(|| {
                left.normalized_licence_number
                    .cmp(&right.normalized_licence_number)
            })
    });
    let text = ordered
        .into_iter()
        .map(|licence| {
            format!(
                "{}: {}",
                licence.licence_type.trim(),
                licence.licence_number.trim()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    (!text.is_empty()).then_some(text)
}

/// Which required facts the Store is still missing, in the order an operator would fill them in.
pub fn missing_facts(source: &SellerProfileSource) -> Vec<MissingSellerFact> {
    let mut missing = Vec::new();
    if source
        .legal_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
    {
        missing.push(MissingSellerFact::LegalName);
    }
    if source
        .address_line1
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
    {
        missing.push(MissingSellerFact::AddressLine1);
    }
    if licence_text(&source.active_licences).is_none() {
        missing.push(MissingSellerFact::SaleLicence);
    }
    missing
}

/// The snapshot to freeze, or the exact reasons it cannot be taken.
///
/// This is the single place that decides a Sale may be posted. The service never assembles a
/// snapshot field by field, so it cannot accidentally produce one that is missing a particular.
pub fn resolve(source: &SellerProfileSource) -> Result<SellerSnapshot, Vec<MissingSellerFact>> {
    let missing = missing_facts(source);
    if !missing.is_empty() {
        return Err(missing);
    }
    Ok(SellerSnapshot {
        legal_name: source
            .legal_name
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_owned(),
        trade_name: source.trade_name.trim().to_owned(),
        address_line1: source
            .address_line1
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_owned(),
        address_line2: trimmed(source.address_line2.as_deref()),
        city: trimmed(source.city.as_deref()),
        postal_code: trimmed(source.postal_code.as_deref()),
        state_name: trimmed(source.state_name.as_deref()),
        phone: trimmed(source.phone.as_deref()),
        email: trimmed(source.email.as_deref()),
        licence_text: licence_text(&source.active_licences).unwrap_or_default(),
        retail_memo_licence_text: licence_text(
            &source
                .active_licences
                .iter()
                .filter(|licence| licence.include_on_retail_memo)
                .cloned()
                .collect::<Vec<_>>(),
        ),
    })
}

fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn licence(licence_type: &str, number: &str) -> SellerLicence {
        SellerLicence {
            include_on_retail_memo: false,
            licence_type: licence_type.to_owned(),
            licence_number: number.to_owned(),
            normalized_licence_number: number
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
                .to_ascii_uppercase(),
        }
    }

    fn complete() -> SellerProfileSource {
        SellerProfileSource {
            legal_name: Some("Sharma Medical Stores".to_owned()),
            trade_name: "Sharma Chemists".to_owned(),
            address_line1: Some("12 Market Road".to_owned()),
            address_line2: None,
            city: Some("Pune".to_owned()),
            postal_code: Some("411001".to_owned()),
            state_name: Some("Maharashtra".to_owned()),
            phone: Some("02012345678".to_owned()),
            email: None,
            active_licences: vec![licence("Form 20", "MH-20-1234")],
        }
    }

    #[test]
    fn a_complete_profile_resolves_to_a_snapshot() {
        let snapshot = resolve(&complete()).expect("complete profile");
        assert_eq!(snapshot.legal_name, "Sharma Medical Stores");
        assert_eq!(snapshot.trade_name, "Sharma Chemists");
        assert_eq!(snapshot.licence_text, "Form 20: MH-20-1234");
        assert_eq!(snapshot.email, None);
    }

    /// The three particulars Rule 65(4)(3)(i) names. Each is reported on its own so the operator is
    /// sent to one field rather than told the profile is "incomplete".
    #[test]
    fn each_required_particular_is_reported_separately() {
        let mut source = complete();
        source.legal_name = None;
        source.address_line1 = Some("   ".to_owned());
        source.active_licences.clear();
        assert_eq!(
            resolve(&source).unwrap_err(),
            vec![
                MissingSellerFact::LegalName,
                MissingSellerFact::AddressLine1,
                MissingSellerFact::SaleLicence
            ]
        );
    }

    /// Whitespace is not a name, an address, or a licence.
    #[test]
    fn blank_text_never_counts_as_a_recorded_fact() {
        for blank in ["", "   ", "\t", "\n "] {
            let mut source = complete();
            source.legal_name = Some(blank.to_owned());
            assert_eq!(
                resolve(&source).unwrap_err(),
                vec![MissingSellerFact::LegalName]
            );
        }
    }

    /// The printed order cannot depend on insertion order: the same set of licences must produce
    /// the same line however it reached the database.
    #[test]
    fn licence_order_is_deterministic_regardless_of_input_order() {
        let forward = vec![
            licence("Form 20", "MH-20-1234"),
            licence("Form 21", "MH-21-5678"),
            licence("Form 20B", "MH-20B-0001"),
        ];
        let mut reversed = forward.clone();
        reversed.reverse();
        let mut shuffled = vec![forward[1].clone(), forward[2].clone(), forward[0].clone()];
        shuffled.rotate_left(1);

        let expected = "Form 20: MH-20-1234, Form 20B: MH-20B-0001, Form 21: MH-21-5678";
        assert_eq!(licence_text(&forward).as_deref(), Some(expected));
        assert_eq!(licence_text(&reversed).as_deref(), Some(expected));
        assert_eq!(licence_text(&shuffled).as_deref(), Some(expected));
    }

    /// Two licences of the same type order by their number, not by chance.
    #[test]
    fn licences_of_one_type_order_by_number() {
        let licences = vec![
            licence("Retail", "ZZ-9999"),
            licence("Retail", "AA-0001"),
            licence("retail", "MM-5000"),
        ];
        assert_eq!(
            licence_text(&licences).as_deref(),
            Some("Retail: AA-0001, retail: MM-5000, Retail: ZZ-9999")
        );
    }

    /// The number is transcribed from a certificate, so it is printed exactly as entered — spacing,
    /// case and punctuation included.
    #[test]
    fn a_licence_number_is_printed_as_entered() {
        let licences = vec![licence("Form 20B/21B", "  20B-1234 / 21B-5678  ")];
        assert_eq!(
            licence_text(&licences).as_deref(),
            Some("Form 20B/21B: 20B-1234 / 21B-5678")
        );
    }

    #[test]
    fn no_licences_means_no_text_and_a_missing_particular() {
        assert_eq!(licence_text(&[]), None);
        let mut source = complete();
        source.active_licences.clear();
        assert_eq!(missing_facts(&source), vec![MissingSellerFact::SaleLicence]);
    }

    /// The optional facts are optional: a pharmacy with no email still sells medicines.
    #[test]
    fn optional_contact_details_do_not_block_a_sale() {
        let mut source = complete();
        source.phone = None;
        source.email = None;
        source.city = None;
        source.postal_code = None;
        source.state_name = None;
        source.address_line2 = None;
        assert!(missing_facts(&source).is_empty());
        let snapshot = resolve(&source).expect("still complete");
        assert_eq!(snapshot.city, None);
        assert_eq!(snapshot.address_line1, "12 Market Road");
    }

    /// A trade name and a legal name are different facts and neither substitutes for the other.
    #[test]
    fn the_trade_name_never_stands_in_for_a_missing_legal_name() {
        let mut source = complete();
        source.legal_name = None;
        assert_eq!(
            missing_facts(&source),
            vec![MissingSellerFact::LegalName],
            "the display name was silently accepted as the legal name"
        );
    }

    /// Text arriving from an operator is stored, not interpreted. Nothing here may strip or escape
    /// it: that is the renderer's problem, and a snapshot that quietly rewrote what was typed would
    /// no longer be what was issued.
    #[test]
    fn free_text_is_preserved_verbatim_apart_from_surrounding_whitespace() {
        let mut source = complete();
        source.legal_name = Some("  <script>alert(1)</script> & Söhne  ".to_owned());
        let snapshot = resolve(&source).expect("complete");
        assert_eq!(snapshot.legal_name, "<script>alert(1)</script> & Söhne");
    }
}
