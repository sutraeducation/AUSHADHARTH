use std::{collections::BTreeMap, str::FromStr};

use serde_json::{Map, Value};
use time::{Date, macros::format_description};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MasterKind {
    UnitOfMeasure,
    DosageForm,
    PharmaceuticalCompany,
    CompanyIdentifier,
    Brand,
    HsnCode,
    TaxCategory,
    TaxRateVersion,
    RegulatoryCategory,
    ControlledFormulation,
    PriceControlVersion,
    Ingredient,
    SaltForm,
    StrengthUnit,
    StateCode,
}

impl MasterKind {
    pub fn path(self) -> &'static str {
        match self {
            Self::UnitOfMeasure => "units",
            Self::DosageForm => "dosage-forms",
            Self::PharmaceuticalCompany => "companies",
            Self::CompanyIdentifier => "company-identifiers",
            Self::Brand => "brands",
            Self::HsnCode => "hsn-codes",
            Self::TaxCategory => "tax-categories",
            Self::TaxRateVersion => "tax-rate-versions",
            Self::RegulatoryCategory => "regulatory-categories",
            Self::ControlledFormulation => "controlled-formulations",
            Self::PriceControlVersion => "price-control-versions",
            Self::Ingredient => "ingredients",
            Self::SaltForm => "salt-forms",
            Self::StrengthUnit => "strength-units",
            Self::StateCode => "state-codes",
        }
    }

    pub fn table(self) -> &'static str {
        match self {
            Self::UnitOfMeasure => "units_of_measure",
            Self::DosageForm => "dosage_forms",
            Self::PharmaceuticalCompany => "pharmaceutical_companies",
            Self::CompanyIdentifier => "company_identifiers",
            Self::Brand => "brands",
            Self::HsnCode => "hsn_codes",
            Self::TaxCategory => "tax_categories",
            Self::TaxRateVersion => "tax_rate_versions",
            Self::RegulatoryCategory => "regulatory_categories",
            Self::ControlledFormulation => "controlled_formulations",
            Self::PriceControlVersion => "price_control_versions",
            Self::Ingredient => "ingredients",
            Self::SaltForm => "salt_forms",
            Self::StrengthUnit => "strength_units",
            Self::StateCode => "state_codes",
        }
    }

    pub fn entity_type(self) -> &'static str {
        match self {
            Self::UnitOfMeasure => "unit_of_measure",
            Self::DosageForm => "dosage_form",
            Self::PharmaceuticalCompany => "pharmaceutical_company",
            Self::CompanyIdentifier => "company_identifier",
            Self::Brand => "brand",
            Self::HsnCode => "hsn_code",
            Self::TaxCategory => "tax_category",
            Self::TaxRateVersion => "tax_rate_version",
            Self::RegulatoryCategory => "regulatory_category",
            Self::ControlledFormulation => "controlled_formulation",
            Self::PriceControlVersion => "price_control_version",
            Self::Ingredient => "ingredient",
            Self::SaltForm => "salt_form",
            Self::StrengthUnit => "strength_unit",
            Self::StateCode => "state_code",
        }
    }

    pub fn json_expression(self) -> &'static str {
        match self {
            Self::UnitOfMeasure => {
                "json_object('canonicalCode',canonical_code,'displayName',display_name,'dimension',dimension,'isDiscrete',json(CASE is_discrete WHEN 1 THEN 'true' ELSE 'false' END),'allowedScale',allowed_scale)"
            }
            Self::DosageForm => {
                "json_object('canonicalCode',canonical_code,'displayName',display_name,'description',description,'routeHint',route_hint,'releaseHint',release_hint)"
            }
            Self::PharmaceuticalCompany => {
                "json_object('displayName',display_name,'legalName',legal_name,'normalizedSearchName',normalized_search_name,'city',city,'state',state,'countryCode',country_code)"
            }
            Self::CompanyIdentifier => {
                "json_object('companyId',company_id,'namespace',namespace,'normalizedValue',normalized_value,'verificationState',verification_state)"
            }
            Self::Brand => {
                "json_object('displayName',display_name,'normalizedSearchName',normalized_search_name,'brandOwnerCompanyId',brand_owner_company_id)"
            }
            Self::HsnCode => {
                "json_object('jurisdiction',jurisdiction,'hsnCode',hsn_code,'description',description)"
            }
            Self::TaxCategory => {
                "json_object('jurisdiction',jurisdiction,'categoryCode',category_code,'displayName',display_name,'taxTreatment',tax_treatment)"
            }
            Self::TaxRateVersion => {
                "json_object('taxCategoryId',tax_category_id,'effectiveFrom',effective_from,'effectiveTo',effective_to,'cgstBasisPoints',cgst_basis_points,'sgstBasisPoints',sgst_basis_points,'igstBasisPoints',igst_basis_points,'cessBasisPoints',cess_basis_points)"
            }
            Self::RegulatoryCategory => {
                "json_object('jurisdiction',jurisdiction,'categorySystem',category_system,'categoryCode',category_code,'displayName',display_name,'effectiveFrom',effective_from,'effectiveTo',effective_to,'sourceReference',source_reference,'verificationState',verification_state)"
            }
            Self::ControlledFormulation => {
                "json_object('jurisdiction',jurisdiction,'formulationCode',formulation_code,'displayName',display_name,'dosageFormId',dosage_form_id,'strengthText',strength_text,'verificationState',verification_state,'sourceNote',source_note)"
            }
            Self::PriceControlVersion => {
                "json_object('controlledFormulationId',controlled_formulation_id,'effectiveFrom',effective_from,'effectiveTo',effective_to,'ceilingPricePaise',ceiling_price_paise,'ceilingBasis',ceiling_basis,'ceilingBasisUnitId',ceiling_basis_unit_id,'notificationReference',notification_reference,'sourceNote',source_note)"
            }
            Self::Ingredient => {
                "json_object('canonicalCode',canonical_code,'displayName',display_name,'normalizedSearchName',normalized_search_name,'description',description)"
            }
            Self::SaltForm => {
                "json_object('canonicalCode',canonical_code,'displayName',display_name,'normalizedSearchName',normalized_search_name)"
            }
            Self::StrengthUnit => {
                "json_object('canonicalCode',canonical_code,'displayName',display_name,'dimension',dimension,'allowedScale',allowed_scale)"
            }
            Self::StateCode => {
                "json_object('jurisdiction',jurisdiction,'stateCode',state_code,'displayName',display_name)"
            }
        }
    }

    pub fn search_expression(self) -> &'static str {
        match self {
            Self::UnitOfMeasure | Self::DosageForm => "canonical_code || ' ' || display_name",
            Self::PharmaceuticalCompany | Self::Brand => {
                "normalized_search_name || ' ' || display_name"
            }
            Self::CompanyIdentifier => "namespace || ' ' || normalized_value",
            Self::HsnCode => "jurisdiction || ' ' || hsn_code || ' ' || description",
            Self::TaxCategory => "jurisdiction || ' ' || category_code || ' ' || display_name",
            Self::TaxRateVersion => "effective_from || ' ' || COALESCE(effective_to, '')",
            Self::RegulatoryCategory => {
                "jurisdiction || ' ' || category_system || ' ' || category_code || ' ' || display_name"
            }
            Self::ControlledFormulation => {
                "jurisdiction || ' ' || formulation_code || ' ' || display_name || ' ' || COALESCE(strength_text, '')"
            }
            Self::PriceControlVersion => {
                "effective_from || ' ' || COALESCE(effective_to, '') || ' ' || COALESCE(notification_reference, '')"
            }
            Self::Ingredient | Self::SaltForm => {
                "canonical_code || ' ' || normalized_search_name || ' ' || display_name"
            }
            Self::StrengthUnit => "canonical_code || ' ' || display_name",
            Self::StateCode => "jurisdiction || ' ' || state_code || ' ' || display_name",
        }
    }
}

impl FromStr for MasterKind {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        [
            Self::UnitOfMeasure,
            Self::DosageForm,
            Self::PharmaceuticalCompany,
            Self::CompanyIdentifier,
            Self::Brand,
            Self::HsnCode,
            Self::TaxCategory,
            Self::TaxRateVersion,
            Self::RegulatoryCategory,
            Self::ControlledFormulation,
            Self::PriceControlVersion,
            Self::Ingredient,
            Self::SaltForm,
            Self::StrengthUnit,
            Self::StateCode,
        ]
        .into_iter()
        .find(|kind| kind.path() == value)
        .ok_or(())
    }
}

#[derive(Debug, Clone)]
pub enum DbValue {
    Text(Option<String>),
    Integer(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    pub field: String,
    pub message: String,
}

type ValidatedFields = BTreeMap<&'static str, DbValue>;

pub fn validate_attributes(
    kind: MasterKind,
    value: &Value,
) -> Result<ValidatedFields, Vec<ValidationIssue>> {
    let object = value
        .as_object()
        .ok_or_else(|| vec![issue("attributes", "must be an object")])?;
    let result = match kind {
        MasterKind::UnitOfMeasure => validate_unit(object),
        MasterKind::DosageForm => validate_dosage_form(object),
        MasterKind::PharmaceuticalCompany => validate_company(object),
        MasterKind::CompanyIdentifier => validate_company_identifier(object),
        MasterKind::Brand => validate_brand(object),
        MasterKind::HsnCode => validate_hsn(object),
        MasterKind::TaxCategory => validate_tax_category(object),
        MasterKind::TaxRateVersion => validate_tax_rate(object),
        MasterKind::RegulatoryCategory => validate_regulatory_category(object),
        MasterKind::ControlledFormulation => validate_controlled_formulation(object),
        MasterKind::PriceControlVersion => validate_price_control_version(object),
        MasterKind::Ingredient => validate_ingredient(object),
        MasterKind::SaltForm => validate_salt_form(object),
        MasterKind::StrengthUnit => validate_strength_unit(object),
        MasterKind::StateCode => validate_state_code(object),
    };
    result.map_err(|error| vec![error])
}

/// A jurisdiction's State, used as the place of supply and on postal addresses. The code is the one
/// a GSTIN carries in its first two positions, which is why it is compared rather than the name.
fn validate_state_code(object: &Map<String, Value>) -> Result<ValidatedFields, ValidationIssue> {
    let code = required_text(object, "stateCode", 2)?.to_ascii_uppercase();
    if code.len() != 2 || !code.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(issue(
            "stateCode",
            "must be exactly two letters or digits, as a GSTIN carries it",
        ));
    }
    fields([
        ("jurisdiction", DbValue::Text(Some(jurisdiction(object)?))),
        ("state_code", DbValue::Text(Some(code))),
        (
            "display_name",
            DbValue::Text(Some(required_text(object, "displayName", 100)?)),
        ),
    ])
}

/// The active moiety. Deliberately carries no salt, no strength, and no clinical attribute.
fn validate_ingredient(object: &Map<String, Value>) -> Result<ValidatedFields, ValidationIssue> {
    let display = required_text(object, "displayName", 160)?;
    fields([
        (
            "canonical_code",
            DbValue::Text(Some(canonical_code(required_text(
                object,
                "canonicalCode",
                64,
            )?)?)),
        ),
        ("display_name", DbValue::Text(Some(display.clone()))),
        (
            "normalized_search_name",
            DbValue::Text(Some(normalized_search_name(&display))),
        ),
        (
            "description",
            DbValue::Text(optional_text(object, "description", 500)?),
        ),
    ])
}

/// The chemical form modifier applied to an ingredient, never the ingredient itself.
fn validate_salt_form(object: &Map<String, Value>) -> Result<ValidatedFields, ValidationIssue> {
    let display = required_text(object, "displayName", 120)?;
    fields([
        (
            "canonical_code",
            DbValue::Text(Some(canonical_code(required_text(
                object,
                "canonicalCode",
                64,
            )?)?)),
        ),
        ("display_name", DbValue::Text(Some(display.clone()))),
        (
            "normalized_search_name",
            DbValue::Text(Some(normalized_search_name(&display))),
        ),
    ])
}

/// A unit a strength may be expressed in. Separate from inventory units of measure, which carry
/// discrete/subdivision semantics that have no meaning for a stated strength.
fn validate_strength_unit(object: &Map<String, Value>) -> Result<ValidatedFields, ValidationIssue> {
    fields([
        (
            "canonical_code",
            DbValue::Text(Some(canonical_code(required_text(
                object,
                "canonicalCode",
                32,
            )?)?)),
        ),
        (
            "display_name",
            DbValue::Text(Some(required_text(object, "displayName", 100)?)),
        ),
        (
            "dimension",
            DbValue::Text(Some(enum_text(
                object,
                "dimension",
                &[
                    "mass",
                    "volume",
                    "count",
                    "activity",
                    "substance_equivalent",
                ],
            )?)),
        ),
        (
            "allowed_scale",
            DbValue::Integer(required_integer(object, "allowedScale", 0, 6)?),
        ),
    ])
}

fn validate_unit(object: &Map<String, Value>) -> Result<ValidatedFields, ValidationIssue> {
    let code = canonical_code(required_text(object, "canonicalCode", 32)?)?;
    let display = required_text(object, "displayName", 100)?;
    let dimension = enum_text(
        object,
        "dimension",
        &["count", "container", "volume", "mass"],
    )?;
    let discrete = required_bool(object, "isDiscrete")?;
    let scale = required_integer(object, "allowedScale", 0, 6)?;
    if discrete && scale != 0 {
        return Err(issue("allowedScale", "must be zero for a discrete unit"));
    }
    fields([
        ("canonical_code", DbValue::Text(Some(code))),
        ("display_name", DbValue::Text(Some(display))),
        ("dimension", DbValue::Text(Some(dimension))),
        ("is_discrete", DbValue::Integer(i64::from(discrete))),
        ("allowed_scale", DbValue::Integer(scale)),
    ])
}

fn validate_dosage_form(object: &Map<String, Value>) -> Result<ValidatedFields, ValidationIssue> {
    fields([
        (
            "canonical_code",
            DbValue::Text(Some(canonical_code(required_text(
                object,
                "canonicalCode",
                32,
            )?)?)),
        ),
        (
            "display_name",
            DbValue::Text(Some(required_text(object, "displayName", 100)?)),
        ),
        (
            "description",
            DbValue::Text(optional_text(object, "description", 500)?),
        ),
        (
            "route_hint",
            DbValue::Text(optional_text(object, "routeHint", 100)?),
        ),
        (
            "release_hint",
            DbValue::Text(optional_text(object, "releaseHint", 100)?),
        ),
    ])
}

fn validate_company(object: &Map<String, Value>) -> Result<ValidatedFields, ValidationIssue> {
    let display = required_text(object, "displayName", 200)?;
    let country = optional_text(object, "countryCode", 2)?.map(|value| value.to_uppercase());
    if country.as_ref().is_some_and(|value| {
        value.len() != 2 || !value.bytes().all(|byte| byte.is_ascii_uppercase())
    }) {
        return Err(issue(
            "countryCode",
            "must be a two-letter ASCII country code",
        ));
    }
    fields([
        ("display_name", DbValue::Text(Some(display.clone()))),
        (
            "legal_name",
            DbValue::Text(optional_text(object, "legalName", 200)?),
        ),
        (
            "normalized_search_name",
            DbValue::Text(Some(normalized_search_name(&display))),
        ),
        ("city", DbValue::Text(optional_text(object, "city", 100)?)),
        ("state", DbValue::Text(optional_text(object, "state", 100)?)),
        ("country_code", DbValue::Text(country)),
    ])
}

fn validate_company_identifier(
    object: &Map<String, Value>,
) -> Result<ValidatedFields, ValidationIssue> {
    let company_id = uuid_v7_text(object, "companyId")?;
    let namespace = canonical_code(required_text(object, "namespace", 64)?)?;
    let normalized_value = required_text(object, "normalizedValue", 128)?.to_uppercase();
    let verification = enum_text(
        object,
        "verificationState",
        &["unverified", "verified", "rejected"],
    )?;
    fields([
        ("company_id", DbValue::Text(Some(company_id))),
        ("namespace", DbValue::Text(Some(namespace))),
        ("normalized_value", DbValue::Text(Some(normalized_value))),
        ("verification_state", DbValue::Text(Some(verification))),
    ])
}

fn validate_brand(object: &Map<String, Value>) -> Result<ValidatedFields, ValidationIssue> {
    let display = required_text(object, "displayName", 200)?;
    let owner = optional_uuid_v7_text(object, "brandOwnerCompanyId")?;
    fields([
        ("display_name", DbValue::Text(Some(display.clone()))),
        (
            "normalized_search_name",
            DbValue::Text(Some(normalized_search_name(&display))),
        ),
        ("brand_owner_company_id", DbValue::Text(owner)),
    ])
}

fn validate_hsn(object: &Map<String, Value>) -> Result<ValidatedFields, ValidationIssue> {
    let hsn: String = required_text(object, "hsnCode", 32)?
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_uppercase();
    if !(2..=16).contains(&hsn.len()) || !hsn.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(issue(
            "hsnCode",
            "must normalize to 2-16 ASCII letters or digits",
        ));
    }
    fields([
        ("jurisdiction", DbValue::Text(Some(jurisdiction(object)?))),
        ("hsn_code", DbValue::Text(Some(hsn))),
        (
            "description",
            DbValue::Text(Some(required_text(object, "description", 500)?)),
        ),
    ])
}

fn validate_tax_category(object: &Map<String, Value>) -> Result<ValidatedFields, ValidationIssue> {
    fields([
        ("jurisdiction", DbValue::Text(Some(jurisdiction(object)?))),
        (
            "category_code",
            DbValue::Text(Some(canonical_code(required_text(
                object,
                "categoryCode",
                32,
            )?)?)),
        ),
        (
            "display_name",
            DbValue::Text(Some(required_text(object, "displayName", 100)?)),
        ),
        (
            "tax_treatment",
            DbValue::Text(Some(enum_text(
                object,
                "taxTreatment",
                &["taxable", "exempt", "nil_rated", "non_gst"],
            )?)),
        ),
    ])
}

fn validate_tax_rate(object: &Map<String, Value>) -> Result<ValidatedFields, ValidationIssue> {
    let from = date_text(object, "effectiveFrom", false)?.expect("required date");
    let to = date_text(object, "effectiveTo", true)?;
    if to.as_ref().is_some_and(|to| to <= &from) {
        return Err(issue(
            "effectiveTo",
            "must be later than effectiveFrom; periods are [from, to)",
        ));
    }
    fields([
        (
            "tax_category_id",
            DbValue::Text(Some(uuid_v7_text(object, "taxCategoryId")?)),
        ),
        ("effective_from", DbValue::Text(Some(from))),
        ("effective_to", DbValue::Text(to)),
        (
            "cgst_basis_points",
            DbValue::Integer(required_integer(object, "cgstBasisPoints", 0, 10_000)?),
        ),
        (
            "sgst_basis_points",
            DbValue::Integer(required_integer(object, "sgstBasisPoints", 0, 10_000)?),
        ),
        (
            "igst_basis_points",
            DbValue::Integer(required_integer(object, "igstBasisPoints", 0, 10_000)?),
        ),
        (
            "cess_basis_points",
            DbValue::Integer(optional_integer(object, "cessBasisPoints", 0, 10_000)?.unwrap_or(0)),
        ),
    ])
}

/// A notified formulation a ceiling price belongs to.
///
/// `strengthText` is recorded as written and deliberately never parsed: it exists so a human can
/// verify the mapping, and nothing computes from it. Matching a Product to a formulation is an
/// explicit operator assignment, never an inference from this text.
fn validate_controlled_formulation(
    object: &Map<String, Value>,
) -> Result<ValidatedFields, ValidationIssue> {
    fields([
        ("jurisdiction", DbValue::Text(Some(jurisdiction(object)?))),
        (
            "formulation_code",
            DbValue::Text(Some(
                required_text(object, "formulationCode", 64)?.to_uppercase(),
            )),
        ),
        (
            "display_name",
            DbValue::Text(Some(required_text(object, "displayName", 200)?)),
        ),
        (
            "dosage_form_id",
            DbValue::Text(optional_uuid_v7_text(object, "dosageFormId")?),
        ),
        (
            "strength_text",
            DbValue::Text(optional_text(object, "strengthText", 200)?),
        ),
        (
            "verification_state",
            DbValue::Text(Some(enum_text(
                object,
                "verificationState",
                &["unverified", "verified", "rejected"],
            )?)),
        ),
        (
            "source_note",
            DbValue::Text(optional_text(object, "sourceNote", 500)?),
        ),
    ])
}

/// One effective-dated ceiling.
///
/// A per-base-unit ceiling must name the unit it is quoted in, because that unit is what decides
/// whether the ceiling can be compared with a Product's rate at all. A per-pack ceiling is recorded
/// truthfully and reported as incomparable rather than divided by an assumed pack size.
fn validate_price_control_version(
    object: &Map<String, Value>,
) -> Result<ValidatedFields, ValidationIssue> {
    let from = date_text(object, "effectiveFrom", false)?.expect("required date");
    let to = date_text(object, "effectiveTo", true)?;
    if to.as_ref().is_some_and(|to| to <= &from) {
        return Err(issue(
            "effectiveTo",
            "must be later than effectiveFrom; periods are [from, to)",
        ));
    }
    let basis = enum_text(object, "ceilingBasis", &["per_base_unit", "per_pack"])?;
    let unit = optional_uuid_v7_text(object, "ceilingBasisUnitId")?;
    if basis == "per_base_unit" && unit.is_none() {
        return Err(issue(
            "ceilingBasisUnitId",
            "a per-base-unit ceiling must name the unit it is quoted in",
        ));
    }
    fields([
        (
            "controlled_formulation_id",
            DbValue::Text(Some(uuid_v7_text(object, "controlledFormulationId")?)),
        ),
        ("effective_from", DbValue::Text(Some(from))),
        ("effective_to", DbValue::Text(to)),
        (
            "ceiling_price_paise",
            DbValue::Integer(required_integer(
                object,
                "ceilingPricePaise",
                1,
                100_000_000_000,
            )?),
        ),
        ("ceiling_basis", DbValue::Text(Some(basis))),
        ("ceiling_basis_unit_id", DbValue::Text(unit)),
        (
            "notification_reference",
            DbValue::Text(optional_text(object, "notificationReference", 200)?),
        ),
        (
            "source_note",
            DbValue::Text(optional_text(object, "sourceNote", 500)?),
        ),
    ])
}

fn validate_regulatory_category(
    object: &Map<String, Value>,
) -> Result<ValidatedFields, ValidationIssue> {
    let from = date_text(object, "effectiveFrom", true)?;
    let to = date_text(object, "effectiveTo", true)?;
    if let (Some(from), Some(to)) = (&from, &to)
        && to <= from
    {
        return Err(issue("effectiveTo", "must be later than effectiveFrom"));
    }
    fields([
        ("jurisdiction", DbValue::Text(Some(jurisdiction(object)?))),
        (
            "category_system",
            DbValue::Text(Some(canonical_code(required_text(
                object,
                "categorySystem",
                64,
            )?)?)),
        ),
        (
            "category_code",
            DbValue::Text(Some(
                required_text(object, "categoryCode", 64)?.to_uppercase(),
            )),
        ),
        (
            "display_name",
            DbValue::Text(Some(required_text(object, "displayName", 200)?)),
        ),
        ("effective_from", DbValue::Text(from)),
        ("effective_to", DbValue::Text(to)),
        (
            "source_reference",
            DbValue::Text(optional_text(object, "sourceReference", 500)?),
        ),
        (
            "verification_state",
            DbValue::Text(Some(enum_text(
                object,
                "verificationState",
                &["unverified", "verified", "rejected"],
            )?)),
        ),
    ])
}

fn fields<const N: usize>(
    items: [(&'static str, DbValue); N],
) -> Result<ValidatedFields, ValidationIssue> {
    Ok(items.into_iter().collect())
}

fn issue(field: &str, message: &str) -> ValidationIssue {
    ValidationIssue {
        field: field.to_owned(),
        message: message.to_owned(),
    }
}

fn required_text(
    object: &Map<String, Value>,
    field: &str,
    maximum: usize,
) -> Result<String, ValidationIssue> {
    let value = object
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| issue(field, "is required"))?;
    if value.len() > maximum {
        return Err(issue(field, "is too long"));
    }
    Ok(value.to_owned())
}

fn optional_text(
    object: &Map<String, Value>,
    field: &str,
    maximum: usize,
) -> Result<Option<String>, ValidationIssue> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.trim().is_empty() => Ok(None),
        Some(Value::String(value)) if value.trim().len() <= maximum => {
            Ok(Some(value.trim().to_owned()))
        }
        Some(Value::String(_)) => Err(issue(field, "is too long")),
        _ => Err(issue(field, "must be text or null")),
    }
}

fn required_bool(object: &Map<String, Value>, field: &str) -> Result<bool, ValidationIssue> {
    object
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| issue(field, "must be boolean"))
}

fn required_integer(
    object: &Map<String, Value>,
    field: &str,
    minimum: i64,
    maximum: i64,
) -> Result<i64, ValidationIssue> {
    object
        .get(field)
        .and_then(Value::as_i64)
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or_else(|| issue(field, "must be an integer in the allowed range"))
}

fn optional_integer(
    object: &Map<String, Value>,
    field: &str,
    minimum: i64,
    maximum: i64,
) -> Result<Option<i64>, ValidationIssue> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .filter(|number| (minimum..=maximum).contains(number))
            .map(Some)
            .ok_or_else(|| issue(field, "must be an integer in the allowed range")),
    }
}

fn enum_text(
    object: &Map<String, Value>,
    field: &str,
    allowed: &[&str],
) -> Result<String, ValidationIssue> {
    let value = required_text(object, field, 64)?.to_lowercase();
    allowed
        .contains(&value.as_str())
        .then_some(value)
        .ok_or_else(|| issue(field, "has an unsupported value"))
}

fn canonical_code(value: String) -> Result<String, ValidationIssue> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty()
        || !normalized
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(issue(
            "canonicalCode",
            "may contain only ASCII letters, digits, dot, underscore, or hyphen",
        ));
    }
    Ok(normalized)
}

fn normalized_search_name(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn jurisdiction(object: &Map<String, Value>) -> Result<String, ValidationIssue> {
    let value = required_text(object, "jurisdiction", 16)?.to_ascii_uppercase();
    if value.len() < 2
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(issue("jurisdiction", "must be a 2-16 character ASCII code"));
    }
    Ok(value)
}

fn uuid_v7_text(object: &Map<String, Value>, field: &str) -> Result<String, ValidationIssue> {
    let value = required_text(object, field, 36)?;
    let id = Uuid::parse_str(&value).map_err(|_| issue(field, "must be a UUID"))?;
    if id.get_version_num() != 7 {
        return Err(issue(field, "must be UUIDv7"));
    }
    Ok(id.to_string())
}

fn optional_uuid_v7_text(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<String>, ValidationIssue> {
    if object.get(field).is_none_or(Value::is_null) {
        return Ok(None);
    }
    uuid_v7_text(object, field).map(Some)
}

fn date_text(
    object: &Map<String, Value>,
    field: &str,
    optional: bool,
) -> Result<Option<String>, ValidationIssue> {
    if optional && object.get(field).is_none_or(Value::is_null) {
        return Ok(None);
    }
    let value = required_text(object, field, 10)?;
    Date::parse(&value, format_description!("[year]-[month]-[day]"))
        .map_err(|_| issue(field, "must be a valid YYYY-MM-DD date"))?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn normalization_and_validation_are_authoritative() {
        let unit = validate_attributes(
            MasterKind::UnitOfMeasure,
            &json!({
                "canonicalCode": " TABLET ", "displayName": "Tablet", "dimension": "count",
                "isDiscrete": true, "allowedScale": 0
            }),
        )
        .unwrap();
        assert!(
            matches!(unit.get("canonical_code"), Some(DbValue::Text(Some(value))) if value == "tablet")
        );

        let invalid = validate_attributes(
            MasterKind::TaxRateVersion,
            &json!({
                "taxCategoryId": "01997000-0000-7000-8000-000000000001",
                "effectiveFrom": "2027-02-30", "cgstBasisPoints": -1,
                "sgstBasisPoints": 0, "igstBasisPoints": 0
            }),
        );
        assert!(invalid.is_err());
    }
}
