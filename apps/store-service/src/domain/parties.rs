//! Phase 1E party identity validation.
//!
//! Tax registration identifiers are checked structurally *and* arithmetically where a published
//! algorithm exists, because a mistyped GSTIN that is merely well-shaped is worse than one that is
//! obviously wrong: it looks authoritative on a purchase document.

use crate::domain::catalog::CatalogValidationIssue;

/// Base-36 alphabet the GSTIN checksum is defined over.
const GSTIN_CHARSET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

pub const PARTY_ROLES: [&str; 2] = ["supplier", "customer"];
/// The roles this phase actually administers. Customer is schema-visible but not yet serviceable.
pub const SUPPORTED_PARTY_ROLES: [&str; 1] = ["supplier"];
pub const ADDRESS_ROLES: [&str; 2] = ["billing", "shipping"];
pub const GST_REGISTRATION_STATUSES: [&str; 3] = ["registered", "unregistered", "unknown"];

fn issue(field: &str, message: &str) -> CatalogValidationIssue {
    CatalogValidationIssue {
        field: field.to_owned(),
        message: message.to_owned(),
    }
}

/// Normalises a GSTIN and proves it is internally consistent.
///
/// Returns the value as entered alongside the normalised form. Whitespace is removed and letters
/// uppercased, so spacing and case can never create a second identity for one registration.
pub fn normalize_gstin(value: &str) -> Result<(String, String), CatalogValidationIssue> {
    let display = value.trim();
    if display.is_empty() {
        return Err(issue("gstin", "is required when the party is registered"));
    }
    let normalized = display
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    if normalized.len() != 15 || !normalized.is_ascii() {
        return Err(issue("gstin", "must be exactly 15 characters"));
    }
    let bytes = normalized.as_bytes();
    // 2 digits of state, a 10-character PAN, an entity number, the literal Z, and a check character.
    let shaped = bytes[0..2].iter().all(u8::is_ascii_digit)
        && bytes[2..7].iter().all(u8::is_ascii_uppercase)
        && bytes[7..11].iter().all(u8::is_ascii_digit)
        && bytes[11].is_ascii_uppercase()
        && (bytes[12].is_ascii_uppercase() || (bytes[12].is_ascii_digit() && bytes[12] != b'0'))
        && bytes[13] == b'Z'
        && bytes[14].is_ascii_alphanumeric()
        && !bytes[14].is_ascii_lowercase();
    if !shaped {
        return Err(issue("gstin", "does not have the shape of a GSTIN"));
    }
    let expected = gstin_check_character(&normalized[0..14])
        .ok_or_else(|| issue("gstin", "does not have the shape of a GSTIN"))?;
    if expected as u8 != bytes[14] {
        return Err(issue(
            "gstin",
            "fails its check digit; re-read the number from the certificate",
        ));
    }
    Ok((display.to_owned(), normalized))
}

/// The official mod-36 weighted checksum over the first fourteen characters.
fn gstin_check_character(first_fourteen: &str) -> Option<char> {
    let mut sum = 0_u32;
    for (index, byte) in first_fourteen.bytes().enumerate() {
        let value = GSTIN_CHARSET.iter().position(|entry| *entry == byte)? as u32;
        let product = value * if index % 2 == 0 { 1 } else { 2 };
        sum += product / 36 + product % 36;
    }
    GSTIN_CHARSET
        .get(((36 - (sum % 36)) % 36) as usize)
        .map(|byte| *byte as char)
}

/// The two-character State code a GSTIN carries in its first two positions.
pub fn gstin_state_code(normalized: &str) -> &str {
    &normalized[0..2]
}

/// Characters 3 to 12 of a GSTIN are the holder's PAN.
pub fn gstin_pan(normalized: &str) -> &str {
    &normalized[2..12]
}

/// Normalises a PAN. No checksum is applied: PAN's check character algorithm is not published, and
/// inventing one would reject valid numbers.
pub fn normalize_pan(value: &str) -> Result<(String, String), CatalogValidationIssue> {
    let display = value.trim();
    if display.is_empty() {
        return Err(issue("pan", "must not be blank"));
    }
    let normalized = display
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    let bytes = normalized.as_bytes();
    if normalized.len() != 10
        || !normalized.is_ascii()
        || !bytes[0..5].iter().all(u8::is_ascii_uppercase)
        || !bytes[5..9].iter().all(u8::is_ascii_digit)
        || !bytes[9].is_ascii_uppercase()
    {
        return Err(issue("pan", "must be five letters, four digits, then a letter"));
    }
    Ok((display.to_owned(), normalized))
}

/// Normalises a telephone number to an optional leading `+` followed by digits.
///
/// The normalised form is what is stored: unlike a batch number or a licence, a telephone number
/// has no meaningful "as printed" form worth preserving separately.
pub fn normalize_phone(value: &str) -> Result<String, CatalogValidationIssue> {
    let trimmed = value.trim();
    let international = trimmed.starts_with('+');
    let digits = trimmed
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>();
    // Anything that is not a digit, a separator, or the leading plus is a typo, not formatting.
    if trimmed
        .chars()
        .skip(usize::from(international))
        .any(|character| !character.is_ascii_digit() && !" -().".contains(character))
    {
        return Err(issue(
            "primaryPhone",
            "may contain only digits, spaces, hyphens, brackets, and a leading plus",
        ));
    }
    if !(6..=15).contains(&digits.len()) {
        return Err(issue("primaryPhone", "must have 6 to 15 digits"));
    }
    Ok(if international {
        format!("+{digits}")
    } else {
        digits
    })
}

/// Structural email validation only. Deliverability is not knowable offline, and a stricter pattern
/// would reject addresses that are valid in practice.
pub fn normalize_email(value: &str) -> Result<String, CatalogValidationIssue> {
    let normalized = value.trim().to_lowercase();
    if normalized.len() > 254 {
        return Err(issue("primaryEmail", "is too long"));
    }
    let mut parts = normalized.split('@');
    let local = parts.next().unwrap_or_default();
    let domain = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || local.is_empty()
        || domain.is_empty()
        || normalized.chars().any(char::is_whitespace)
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
        || domain.contains("..")
    {
        return Err(issue("primaryEmail", "must be a valid email address"));
    }
    Ok(normalized)
}

/// A drug licence is stored exactly as printed, so this only bounds and trims it. Comparison for
/// duplicate detection uppercases and removes whitespace at the point of comparison instead.
pub fn normalize_licence_comparison(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Genuine published GSTINs. Each carries a real check character, so a broken checksum
    /// implementation cannot pass this test.
    const VALID: [&str; 4] = [
        "27AAPFU0939F1ZV",
        "29AAGCB7383J1Z4",
        "09AAACH7409R1ZZ",
        "24AAACC1206D1ZM",
    ];

    #[test]
    fn real_gstins_pass_structure_and_checksum() {
        for value in VALID {
            let (display, normalized) = normalize_gstin(value).expect(value);
            assert_eq!(display, value);
            assert_eq!(normalized, value);
        }
        // Spacing and case are normalisation, not identity.
        assert_eq!(
            normalize_gstin(" 27aapfu0939f1zv ").unwrap().1,
            "27AAPFU0939F1ZV"
        );
        assert_eq!(
            normalize_gstin("27 AAPFU 0939 F1ZV").unwrap().1,
            "27AAPFU0939F1ZV"
        );
    }

    #[test]
    fn a_transposed_gstin_is_rejected_by_the_check_digit() {
        // Well-shaped but wrong: only the checksum can catch this, which is the point of having one.
        let broken = normalize_gstin("27AAPFU0939F1ZX");
        assert!(broken.is_err(), "a wrong check character must be rejected");
        assert!(
            broken.unwrap_err().message.contains("check digit"),
            "the message must say which test failed"
        );
        // A digit transposition inside the PAN block also breaks the checksum.
        assert!(normalize_gstin("27AAPFU9039F1ZV").is_err());
    }

    #[test]
    fn malformed_gstins_are_rejected_before_the_checksum() {
        for value in [
            "27AAPFU0939F1Z",          // too short
            "27AAPFU0939F1ZVV",        // too long
            "2AAAPFU0939F1ZV",         // state code is not two digits
            "27AAPFU0939F1YV",         // the fourteenth character must be Z
            "27AAPFU0939F0ZV",         // entity number may not be zero
            "27AAPF10939F1ZV",         // PAN block must be letters
            "27AAPFU09X9F1ZV",         // PAN digits block must be digits
            "",
        ] {
            assert!(normalize_gstin(value).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn the_pan_inside_a_gstin_is_extractable_and_agrees_with_a_typed_pan() {
        let (_, normalized) = normalize_gstin("27AAPFU0939F1ZV").unwrap();
        assert_eq!(gstin_state_code(&normalized), "27");
        assert_eq!(gstin_pan(&normalized), "AAPFU0939F");
        assert_eq!(normalize_pan(" aapfu0939f ").unwrap().1, "AAPFU0939F");
    }

    #[test]
    fn pan_shape_is_enforced() {
        for value in ["AAPFU0939", "AAPFU09391", "AAPF00939F", "AAPFUO939F", ""] {
            assert!(normalize_pan(value).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn phone_normalization_keeps_the_country_code_and_drops_formatting() {
        assert_eq!(normalize_phone(" 022-2345 6789 ").unwrap(), "02223456789");
        assert_eq!(normalize_phone("+91 98200 12345").unwrap(), "+919820012345");
        assert_eq!(normalize_phone("(022) 2345.6789").unwrap(), "02223456789");
        assert!(normalize_phone("12345").is_err(), "too few digits");
        assert!(normalize_phone("9820x12345").is_err(), "letters are typos");
        assert!(normalize_phone("+91+98200").is_err(), "one plus only");
    }

    #[test]
    fn email_normalization_is_structural_and_lowercases() {
        assert_eq!(
            normalize_email(" Sales@Sharma-Medicals.CO.IN ").unwrap(),
            "sales@sharma-medicals.co.in"
        );
        for value in ["", "sales", "sales@", "@example.com", "a@b", "a b@c.com", "a@@b.com", "a@b..c"] {
            assert!(normalize_email(value).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn licence_comparison_ignores_punctuation_and_case() {
        assert_eq!(
            normalize_licence_comparison(" 20b-1234 / 21B-5678 "),
            "20B123421B5678"
        );
    }

    #[test]
    fn only_the_supplier_role_is_serviceable_in_this_phase() {
        assert!(PARTY_ROLES.contains(&"customer"));
        assert!(!SUPPORTED_PARTY_ROLES.contains(&"customer"));
        assert!(SUPPORTED_PARTY_ROLES.contains(&"supplier"));
    }
}
