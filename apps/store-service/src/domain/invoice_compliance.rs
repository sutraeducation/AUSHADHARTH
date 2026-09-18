//! Phase 1L-A3 — the Store facts a posted Sale must settle before it can later be printed.
//!
//! Five questions, each answered from facts the pharmacy records and nothing the software infers:
//!
//! * **Does this seller charge GST at all?** Only a GST-registered seller may. CGST Act s.32(1): a
//!   person who is not registered "shall not collect … any amount by way of tax". An `unknown`
//!   registration decides nothing and posting is refused until the Store records it.
//! * **Does this invoice carry the Rule 46(s) declaration?** CGST Rule 46(s) requires it on every
//!   invoice issued "other than in the manner" of Rule 48(4) by a taxpayer whose aggregate turnover
//!   "in any preceding financial year from 2017-18 onwards" exceeded the turnover notified under
//!   Rule 48(4). AUSHADHARTH issues no e-invoice, so for a registered seller the answer is the
//!   Store's turnover history, which lives outside this database and is recorded by the owner.
//! * **Must this invoice be an e-invoice?** A different question. Notification No. 13/2020-CT (as
//!   amended) notifies a class of registered person under Rule 48(4) only "in respect of supply of
//!   goods or services or both to a registered person", excludes named classes, and may be relaxed
//!   by exemption. So it is asked ONLY for a Sale to a registered recipient, from a separate Store
//!   fact, and never inferred from the Rule 46(s) answer. Where it is required the Sale is refused:
//!   Rule 48(5) says such an invoice issued any other way "shall not be treated as an invoice", and
//!   AUSHADHARTH cannot obtain an IRN.
//! * **How many HSN digits must each line carry?** Notification No. 78/2020-Central Tax (issued
//!   under the first proviso to Rule 46, effective 01.04.2021): aggregate turnover in the preceding
//!   financial year up to Rs 5 crore — 4 digits, but a tax invoice to an UNREGISTERED person "may
//!   not mention" them; more than Rs 5 crore — 6 digits. Rule 49's first proviso applies Rule 46's
//!   provisos to a bill of supply "mutatis mutandis", so the same policy governs every line of every
//!   GST document this seller issues.
//! * **Which licence is the dealer's sale licence on the retail memo?** Drugs Rules
//!   r.65(4)(3)(i)(a). The operator designates it; `licence_type` is free text and is never read.
//!   Asked for a Sale containing a medicine — see `ComplianceSource::requires_retail_licence`.
//!
//! Everything here is pure. Posting evaluates it inside its `BEGIN IMMEDIATE` transaction, and the
//! quote evaluates the seller's GST status through the same function, so they cannot disagree.

/// Whether this seller charges GST on this Sale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SellerGst {
    Registered,
    Unregistered,
}

impl SellerGst {
    /// `None` for `unknown` — or anything else — because an ambiguous status must not decide
    /// whether tax is collected.
    pub fn from_status(status: &str) -> Option<Self> {
        match status {
            "registered" => Some(Self::Registered),
            "unregistered" => Some(Self::Unregistered),
            _ => None,
        }
    }
}

/// CGST Rule 46(s) applicability, as the owner recorded it for the business.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule46sDeclaration {
    Unknown,
    NotApplicable,
    Applicable,
}

impl Rule46sDeclaration {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "unknown" => Some(Self::Unknown),
            "not_applicable" => Some(Self::NotApplicable),
            "applicable" => Some(Self::Applicable),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::NotApplicable => "not_applicable",
            Self::Applicable => "applicable",
        }
    }
}

/// CGST Rule 48(4) applicability to the business's supplies to registered persons, as the owner
/// recorded it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EinvoiceApplicability {
    Unknown,
    NotRequired,
    Required,
}

impl EinvoiceApplicability {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "unknown" => Some(Self::Unknown),
            "not_required" => Some(Self::NotRequired),
            "required" => Some(Self::Required),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::NotRequired => "not_required",
            Self::Required => "required",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HsnBand {
    Unknown,
    UpToFiveCrore,
    AboveFiveCrore,
}

impl HsnBand {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "unknown" => Some(Self::Unknown),
            "up_to_5_crore" => Some(Self::UpToFiveCrore),
            "above_5_crore" => Some(Self::AboveFiveCrore),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::UpToFiveCrore => "up_to_5_crore",
            Self::AboveFiveCrore => "above_5_crore",
        }
    }
}

/// One particular a Sale needs before it can post. Each is something the pharmacy can record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComplianceIssue {
    /// The Store has not recorded whether the Rule 46(s) declaration applies.
    Rule46sDeclarationUnknown,
    /// The Sale is to a registered recipient and the Store has not recorded whether Rule 48(4)
    /// requires it to e-invoice such supplies.
    EinvoiceApplicabilityUnknown,
    /// The HSN turnover band for this Sale's financial year is not recorded, and at least one line
    /// lacks a six-digit HSN that would satisfy any band.
    HsnPolicyUnresolved,
    HsnMissing {
        line_number: i64,
        required_digits: u8,
    },
    HsnTooShort {
        line_number: i64,
        required_digits: u8,
    },
    /// A medicine is on the Sale and no active licence is designated for the retail memo.
    RetailMemoLicenceNotDesignated,
}

impl ComplianceIssue {
    pub fn field(&self) -> String {
        match self {
            Self::Rule46sDeclarationUnknown => "store.rule46sDeclarationApplicability".to_owned(),
            Self::EinvoiceApplicabilityUnknown => "store.einvoiceApplicability".to_owned(),
            Self::HsnPolicyUnresolved => "store.hsnTurnoverBand".to_owned(),
            Self::HsnMissing { line_number, .. } | Self::HsnTooShort { line_number, .. } => {
                format!("lines[{line_number}].hsnCode")
            }
            Self::RetailMemoLicenceNotDesignated => "store.licences.includeOnRetailMemo".to_owned(),
        }
    }

    /// Said to the person at the counter, not to a lawyer.
    pub fn message(&self) -> String {
        match self {
            Self::Rule46sDeclarationUnknown => {
                "Record in Store Profile whether the Rule 46(s) e-invoicing declaration applies to \
                 this pharmacy."
                    .to_owned()
            }
            Self::EinvoiceApplicabilityUnknown => {
                "This customer is GST-registered. Record in Store Profile whether this pharmacy \
                 must issue e-invoices to registered customers."
                    .to_owned()
            }
            Self::HsnPolicyUnresolved => {
                "Record in Store Profile the turnover band that decides HSN digits for this \
                 financial year."
                    .to_owned()
            }
            Self::HsnMissing {
                line_number,
                required_digits,
            } => format!(
                "Line {line_number} needs an HSN code of at least {required_digits} digits. Add it to the product."
            ),
            Self::HsnTooShort {
                line_number,
                required_digits,
            } => format!(
                "Line {line_number}'s HSN code has fewer than {required_digits} digits. Correct it on the product."
            ),
            Self::RetailMemoLicenceNotDesignated => {
                "Choose in Store Profile which drug sale licence is printed on retail memos."
                    .to_owned()
            }
        }
    }
}

/// Why a Sale cannot post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComplianceRefusal {
    /// Facts the pharmacy can record are missing.
    Incomplete(Vec<ComplianceIssue>),
    /// The Store recorded that Rule 48(4) requires an e-invoice for this supply to a registered
    /// recipient. AUSHADHARTH cannot obtain an IRN, and Rule 48(5) says any other document "shall
    /// not be treated as an invoice" — so no fact the counter can record fixes this Sale.
    EinvoiceRequired,
}

/// The digits an HSN code actually carries. A code that is not purely numeric is not counted: it
/// is not what a digit requirement can be measured against, and guessing would mean inventing one.
pub fn hsn_digits(code: Option<&str>) -> u8 {
    match code.map(str::trim) {
        Some(code) if !code.is_empty() && code.bytes().all(|byte| byte.is_ascii_digit()) => {
            u8::try_from(code.len()).unwrap_or(u8::MAX)
        }
        _ => 0,
    }
}

/// The facts one posting is judged against, read in the posting transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComplianceSource {
    pub seller: SellerGst,
    pub rule46s: Rule46sDeclaration,
    pub einvoice: EinvoiceApplicability,
    pub hsn_band: HsnBand,
    /// The financial year of invoices the recorded band governs.
    pub hsn_band_financial_year: Option<String>,
    /// The financial year of this Sale's business date.
    pub sale_financial_year: String,
    pub recipient_registered: bool,
    /// Every line's frozen HSN, by line number.
    pub line_hsn: Vec<(i64, Option<String>)>,
    /// A `medicine` is on the Sale. Not a `device`: the catalogue's device kind carries no
    /// regulatory classification from which Drugs Rule 65 can be shown to govern every device.
    pub requires_retail_licence: bool,
    /// The designated active licences, already rendered; `None` when none is designated.
    pub designated_licence_text: Option<String>,
}

/// What a version-1 Sale freezes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComplianceSnapshot {
    pub retail_licence_text: Option<String>,
    /// `None` for an unregistered seller; never `Some(Unknown)`.
    pub rule46s: Option<Rule46sDeclaration>,
    /// `Some(NotRequired)` exactly for a registered seller's Sale to a registered recipient: the one
    /// transaction whose correctness depended on it. `None` everywhere else — "not needed", never
    /// "not required".
    pub einvoice: Option<EinvoiceApplicability>,
    pub hsn_band: Option<HsnBand>,
    pub hsn_band_financial_year: Option<String>,
    /// `Some(0)` not required, `Some(4)`/`Some(6)` required, `None` not determined (unknown band,
    /// every line already carrying six digits).
    pub hsn_required_digits: Option<u8>,
}

pub fn resolve(source: &ComplianceSource) -> Result<ComplianceSnapshot, ComplianceRefusal> {
    let mut issues = Vec::new();
    if source.requires_retail_licence && source.designated_licence_text.is_none() {
        issues.push(ComplianceIssue::RetailMemoLicenceNotDesignated);
    }

    if source.seller == SellerGst::Unregistered {
        // No GST document is issued, so no GST-document fact is recorded.
        return if issues.is_empty() {
            Ok(ComplianceSnapshot {
                retail_licence_text: source.designated_licence_text.clone(),
                rule46s: None,
                einvoice: None,
                hsn_band: None,
                hsn_band_financial_year: None,
                hsn_required_digits: None,
            })
        } else {
            Err(ComplianceRefusal::Incomplete(issues))
        };
    }

    // Rule 48(4) is asked only where it can apply: a supply to a registered person.
    let einvoice = if source.recipient_registered {
        match source.einvoice {
            EinvoiceApplicability::Required => return Err(ComplianceRefusal::EinvoiceRequired),
            EinvoiceApplicability::Unknown => {
                issues.push(ComplianceIssue::EinvoiceApplicabilityUnknown);
                None
            }
            EinvoiceApplicability::NotRequired => Some(EinvoiceApplicability::NotRequired),
        }
    } else {
        None
    };

    // Every registered seller's invoice depends on this answer — whether the declaration prints.
    if source.rule46s == Rule46sDeclaration::Unknown {
        issues.push(ComplianceIssue::Rule46sDeclarationUnknown);
    }

    // A band recorded for another financial year says nothing about this one: the notification
    // looks at the turnover of the year preceding the invoice.
    let band = match (source.hsn_band, source.hsn_band_financial_year.as_deref()) {
        (HsnBand::Unknown, _) => HsnBand::Unknown,
        (band, Some(year)) if year == source.sale_financial_year => band,
        _ => HsnBand::Unknown,
    };
    let required: Option<u8> = match band {
        HsnBand::AboveFiveCrore => Some(6),
        HsnBand::UpToFiveCrore if source.recipient_registered => Some(4),
        HsnBand::UpToFiveCrore => Some(0),
        HsnBand::Unknown => None,
    };
    match required {
        Some(digits) if digits > 0 => {
            for (line_number, hsn) in &source.line_hsn {
                let present = hsn
                    .as_deref()
                    .map(str::trim)
                    .is_some_and(|code| !code.is_empty());
                if !present {
                    issues.push(ComplianceIssue::HsnMissing {
                        line_number: *line_number,
                        required_digits: digits,
                    });
                } else if hsn_digits(hsn.as_deref()) < digits {
                    issues.push(ComplianceIssue::HsnTooShort {
                        line_number: *line_number,
                        required_digits: digits,
                    });
                }
            }
        }
        Some(_) => {}
        None => {
            if source
                .line_hsn
                .iter()
                .any(|(_, hsn)| hsn_digits(hsn.as_deref()) < 6)
            {
                issues.push(ComplianceIssue::HsnPolicyUnresolved);
            }
        }
    }

    if !issues.is_empty() {
        return Err(ComplianceRefusal::Incomplete(issues));
    }
    Ok(ComplianceSnapshot {
        retail_licence_text: source.designated_licence_text.clone(),
        rule46s: Some(source.rule46s),
        einvoice,
        hsn_band: Some(band),
        hsn_band_financial_year: (band != HsnBand::Unknown)
            .then(|| source.sale_financial_year.clone()),
        hsn_required_digits: required,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registered() -> ComplianceSource {
        ComplianceSource {
            seller: SellerGst::Registered,
            rule46s: Rule46sDeclaration::NotApplicable,
            einvoice: EinvoiceApplicability::NotRequired,
            hsn_band: HsnBand::UpToFiveCrore,
            hsn_band_financial_year: Some("2026-27".to_owned()),
            sale_financial_year: "2026-27".to_owned(),
            recipient_registered: false,
            line_hsn: vec![(1, None)],
            requires_retail_licence: true,
            designated_licence_text: Some("Form 20: MH-20-1234".to_owned()),
        }
    }

    fn incomplete(result: Result<ComplianceSnapshot, ComplianceRefusal>) -> Vec<ComplianceIssue> {
        match result {
            Err(ComplianceRefusal::Incomplete(issues)) => issues,
            other => panic!("expected missing facts, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_status_decides_nothing() {
        assert_eq!(SellerGst::from_status("unknown"), None);
        assert_eq!(SellerGst::from_status(""), None);
        assert_eq!(SellerGst::from_status("Registered"), None);
        assert_eq!(
            SellerGst::from_status("registered"),
            Some(SellerGst::Registered)
        );
        assert_eq!(
            SellerGst::from_status("unregistered"),
            Some(SellerGst::Unregistered)
        );
    }

    #[test]
    fn hsn_digits_are_counted_only_on_a_numeric_code() {
        assert_eq!(hsn_digits(Some("3004")), 4);
        assert_eq!(hsn_digits(Some("30049099")), 8);
        assert_eq!(hsn_digits(Some(" 300490 ")), 6);
        assert_eq!(hsn_digits(Some("30A4")), 0);
        assert_eq!(hsn_digits(Some("")), 0);
        assert_eq!(hsn_digits(None), 0);
    }

    #[test]
    fn up_to_five_crore_needs_no_hsn_on_a_b2c_invoice() {
        let snapshot = resolve(&registered()).unwrap();
        assert_eq!(snapshot.hsn_required_digits, Some(0));
        assert_eq!(snapshot.hsn_band, Some(HsnBand::UpToFiveCrore));
    }

    #[test]
    fn up_to_five_crore_needs_four_digits_on_a_b2b_invoice() {
        let mut source = registered();
        source.recipient_registered = true;
        source.line_hsn = vec![
            (1, None),
            (2, Some("30".to_owned())),
            (3, Some("3004".to_owned())),
        ];
        assert_eq!(
            incomplete(resolve(&source)),
            vec![
                ComplianceIssue::HsnMissing {
                    line_number: 1,
                    required_digits: 4
                },
                ComplianceIssue::HsnTooShort {
                    line_number: 2,
                    required_digits: 4
                },
            ]
        );
        source.line_hsn = vec![(1, Some("3004".to_owned()))];
        assert_eq!(resolve(&source).unwrap().hsn_required_digits, Some(4));
    }

    #[test]
    fn above_five_crore_needs_six_digits_on_every_invoice() {
        let mut source = registered();
        source.hsn_band = HsnBand::AboveFiveCrore;
        source.line_hsn = vec![(1, Some("3004".to_owned()))];
        assert_eq!(
            incomplete(resolve(&source)),
            vec![ComplianceIssue::HsnTooShort {
                line_number: 1,
                required_digits: 6
            }]
        );
        source.line_hsn = vec![(1, Some("300490".to_owned()))];
        assert_eq!(resolve(&source).unwrap().hsn_required_digits, Some(6));
    }

    #[test]
    fn a_band_recorded_for_another_year_is_treated_as_unknown() {
        let mut source = registered();
        source.hsn_band_financial_year = Some("2025-26".to_owned());
        assert_eq!(
            incomplete(resolve(&source)),
            vec![ComplianceIssue::HsnPolicyUnresolved]
        );
        // Six digits on every line satisfies any band, so the unknown band blocks nothing.
        source.line_hsn = vec![(1, Some("30049099".to_owned()))];
        let snapshot = resolve(&source).unwrap();
        assert_eq!(snapshot.hsn_band, Some(HsnBand::Unknown));
        assert_eq!(snapshot.hsn_band_financial_year, None);
        assert_eq!(snapshot.hsn_required_digits, None);
    }

    #[test]
    fn an_unknown_rule_46s_fact_blocks_every_registered_sale() {
        for recipient_registered in [false, true] {
            let mut source = registered();
            source.rule46s = Rule46sDeclaration::Unknown;
            source.recipient_registered = recipient_registered;
            source.line_hsn = vec![(1, Some("3004".to_owned()))];
            assert_eq!(
                incomplete(resolve(&source)),
                vec![ComplianceIssue::Rule46sDeclarationUnknown]
            );
        }
    }

    #[test]
    fn a_rule_46s_b2c_sale_posts_and_freezes_the_declaration() {
        // The high-turnover pharmacy's ordinary counter bill: the declaration will print, and
        // nothing about that is a reason to refuse the Sale — whatever Rule 48(4) says about B2B.
        for einvoice in [
            EinvoiceApplicability::Unknown,
            EinvoiceApplicability::NotRequired,
            EinvoiceApplicability::Required,
        ] {
            let mut source = registered();
            source.rule46s = Rule46sDeclaration::Applicable;
            source.einvoice = einvoice;
            let snapshot = resolve(&source).unwrap();
            assert_eq!(snapshot.rule46s, Some(Rule46sDeclaration::Applicable));
            assert_eq!(snapshot.einvoice, None);
        }
    }

    #[test]
    fn a_b2b_sale_is_decided_by_einvoice_applicability_alone() {
        for rule46s in [
            Rule46sDeclaration::NotApplicable,
            Rule46sDeclaration::Applicable,
        ] {
            let mut source = registered();
            source.recipient_registered = true;
            source.line_hsn = vec![(1, Some("3004".to_owned()))];
            source.rule46s = rule46s;

            source.einvoice = EinvoiceApplicability::NotRequired;
            let snapshot = resolve(&source).unwrap();
            assert_eq!(snapshot.einvoice, Some(EinvoiceApplicability::NotRequired));
            assert_eq!(snapshot.rule46s, Some(rule46s));

            source.einvoice = EinvoiceApplicability::Required;
            assert_eq!(resolve(&source), Err(ComplianceRefusal::EinvoiceRequired));

            source.einvoice = EinvoiceApplicability::Unknown;
            assert_eq!(
                incomplete(resolve(&source)),
                vec![ComplianceIssue::EinvoiceApplicabilityUnknown]
            );
        }
    }

    #[test]
    fn a_medicine_needs_a_designated_licence() {
        let mut source = registered();
        source.designated_licence_text = None;
        assert_eq!(
            incomplete(resolve(&source)),
            vec![ComplianceIssue::RetailMemoLicenceNotDesignated]
        );
        source.requires_retail_licence = false;
        assert_eq!(resolve(&source).unwrap().retail_licence_text, None);
    }

    #[test]
    fn an_unregistered_seller_records_no_gst_document_facts() {
        let mut source = registered();
        source.seller = SellerGst::Unregistered;
        source.rule46s = Rule46sDeclaration::Unknown;
        source.einvoice = EinvoiceApplicability::Required;
        source.recipient_registered = true;
        source.hsn_band = HsnBand::Unknown;
        source.hsn_band_financial_year = None;
        let snapshot = resolve(&source).unwrap();
        assert_eq!(snapshot.rule46s, None);
        assert_eq!(snapshot.einvoice, None);
        assert_eq!(snapshot.hsn_band, None);
        assert_eq!(snapshot.hsn_required_digits, None);
        assert_eq!(
            snapshot.retail_licence_text.as_deref(),
            Some("Form 20: MH-20-1234")
        );
    }

    #[test]
    fn an_unregistered_seller_still_needs_the_retail_licence_for_a_medicine() {
        let mut source = registered();
        source.seller = SellerGst::Unregistered;
        source.designated_licence_text = None;
        assert_eq!(
            incomplete(resolve(&source)),
            vec![ComplianceIssue::RetailMemoLicenceNotDesignated]
        );
    }
}
