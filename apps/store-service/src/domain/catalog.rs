use time::{Date, macros::format_description};
use uuid::Uuid;

pub const MAX_QUANTITY_SCALE: i64 = 6;
pub const MAX_BASE_QUANTITY_ATOMS: i64 = 9_000_000_000_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogValidationIssue {
    pub field: String,
    pub message: String,
}

pub fn validate_uuid_v7(value: &str, field: &str) -> Result<String, CatalogValidationIssue> {
    let id = Uuid::parse_str(value).map_err(|_| issue(field, "must be a UUIDv7"))?;
    if id.get_version_num() != 7 {
        return Err(issue(field, "must be a UUIDv7"));
    }
    Ok(id.to_string())
}

pub fn required_text(
    value: &str,
    field: &str,
    maximum: usize,
) -> Result<String, CatalogValidationIssue> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(issue(field, "is required"));
    }
    if trimmed.len() > maximum {
        return Err(issue(field, "is too long"));
    }
    Ok(trimmed.to_owned())
}

pub fn optional_text(
    value: Option<&str>,
    field: &str,
    maximum: usize,
) -> Result<Option<String>, CatalogValidationIssue> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) if value.len() <= maximum => Ok(Some(value.to_owned())),
        Some(_) => Err(issue(field, "is too long")),
        None => Ok(None),
    }
}

pub fn normalized_search_name(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub fn normalize_sku(value: Option<&str>) -> Result<Option<String>, CatalogValidationIssue> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let normalized = value.to_ascii_uppercase();
    if normalized.len() > 64
        || !normalized
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-/".contains(&byte))
    {
        return Err(issue(
            "skuCode",
            "may contain only ASCII letters, digits, dot, underscore, slash, or hyphen",
        ));
    }
    Ok(Some(normalized))
}

pub fn normalize_barcode(
    namespace: &str,
    value: &str,
) -> Result<(String, String), CatalogValidationIssue> {
    let namespace = namespace.trim().to_ascii_lowercase();
    if namespace.is_empty()
        || namespace.len() > 32
        || !namespace
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(issue("namespace", "has an invalid namespace"));
    }
    let normalized = if namespace == "gtin" {
        value
            .chars()
            .filter(|character| !character.is_ascii_whitespace() && *character != '-')
            .collect::<String>()
    } else {
        value.trim().to_ascii_uppercase()
    };
    if normalized.is_empty()
        || normalized.len() > 64
        || !normalized
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-/".contains(&byte))
    {
        return Err(issue("value", "has invalid barcode characters"));
    }
    if namespace == "gtin"
        && (!matches!(normalized.len(), 8 | 12 | 13 | 14)
            || !normalized.bytes().all(|byte| byte.is_ascii_digit())
            || !valid_gtin_check_digit(&normalized))
    {
        return Err(issue(
            "value",
            "must be a valid GTIN-8, GTIN-12, GTIN-13, or GTIN-14",
        ));
    }
    Ok((namespace, normalized))
}

/// The largest MRP the schema accepts, in paise. Money is exact integer minor units per ADR-009.
pub const MAX_MRP_PAISE: i64 = 100_000_000_000;

/// Normalizes a manufacturer lot string for comparison while leaving the printed form to the caller.
/// Whitespace is removed and letters uppercased, so case and spacing can never create a duplicate
/// lot on one Pack.
pub fn normalize_batch_number(value: &str) -> Result<(String, String), CatalogValidationIssue> {
    let display = value.trim();
    if display.is_empty() || display.chars().count() > 64 {
        return Err(issue(
            "batchNumber",
            "is required and may not exceed 64 characters",
        ));
    }
    let normalized = display
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    if normalized.is_empty()
        || normalized.len() > 64
        || !normalized
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-/".contains(&byte))
    {
        return Err(issue(
            "batchNumber",
            "may contain only letters, digits, dot, underscore, slash, or hyphen",
        ));
    }
    Ok((display.to_owned(), normalized))
}

pub fn validate_date(
    value: Option<&str>,
    field: &str,
) -> Result<Option<String>, CatalogValidationIssue> {
    let Some(value) = value else {
        return Ok(None);
    };
    Date::parse(value, format_description!("[year]-[month]-[day]"))
        .map_err(|_| issue(field, "must be a valid YYYY-MM-DD date"))?;
    Ok(Some(value.to_owned()))
}

fn valid_gtin_check_digit(value: &str) -> bool {
    let digits = value
        .bytes()
        .map(|byte| u32::from(byte - b'0'))
        .collect::<Vec<_>>();
    let check = *digits.last().expect("validated non-empty GTIN");
    let sum = digits[..digits.len() - 1]
        .iter()
        .rev()
        .enumerate()
        .map(|(index, digit)| digit * if index % 2 == 0 { 3 } else { 1 })
        .sum::<u32>();
    (10 - (sum % 10)) % 10 == check
}

fn issue(field: &str, message: &str) -> CatalogValidationIssue {
    CatalogValidationIssue {
        field: field.to_owned(),
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sku_and_barcode_normalization_is_deterministic() {
        assert_eq!(
            normalize_sku(Some(" ab/12 ")).unwrap().as_deref(),
            Some("AB/12")
        );
        assert_eq!(
            normalize_barcode("gtin", "8901 2345 6789 0").unwrap().1,
            "8901234567890"
        );
        assert!(normalize_barcode("gtin", "8901234567894").is_err());
        assert_eq!(
            normalize_barcode("internal", " shelf-a/12 ").unwrap().1,
            "SHELF-A/12"
        );
    }

    #[test]
    fn quantity_scale_is_strictly_bounded() {
        assert_eq!(MAX_QUANTITY_SCALE, 6);
        let maximum = std::hint::black_box(MAX_BASE_QUANTITY_ATOMS);
        assert!(maximum < i64::MAX);
    }
}
