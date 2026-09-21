-- Phase 1M-A — regulatory product classification and professional foundations.
--
-- The foundations a lawful pharmacy sale needs BEFORE any prescription workflow exists. Nothing
-- here dispenses anything: it records who says a product is a scheduled drug, on what authority,
-- from what date; who the store's registered pharmacist actually is; which licence forms the store
-- holds; which record method the licensee elected under rule 65; and it freezes those answers onto
-- a posted Sale so a historical document can never be re-derived from today's masters.
--
-- WHAT THE LAW SAYS, AND WHY THE SHAPE IS WHAT IT IS
--
--   * Rule 65(9)(a) of the Drugs Rules, 1945: substances specified in Schedule H and Schedule H1 or
--     Schedule X "shall not be sold by retail except on and in accordance with the prescription of a
--     Registered Medical Practitioner". That prescription workflow does not exist yet, so this
--     migration's job is to make such a sale IMPOSSIBLE rather than to wave it through.
--
--   * The schemes are INDEPENDENT, never one enum. Rule 97(1)(b)-(f) labels five separate cases:
--     Schedule H; Schedule H within the purview of the Narcotic Drugs and Psychotropic Substances
--     Act, 1985; Schedule X; Schedule H1; and Schedule H1 within that Act's purview. NDPS purview is
--     therefore an axis ACROSS H and H1, not a value beside them. Rule 65(3)(1)(f) names "Schedule C
--     or Schedule H and Schedule H1" together, so those coexist too. As the schedules stand today no
--     Schedule H1 substance appears in Schedule H, but that is a fact about two lists on one day,
--     not a rule, and nothing here encodes "H1 implies not H".
--
--   * Classification is EFFECTIVE-DATED because schedule membership moves, with commencement dates
--     in the future. Two final notifications prove it: G.S.R. 377(E) (Drugs (Second Amendment)
--     Rules, 2026) inserts "51. Pregabalin" into Schedule H1 and commences "after one eighty days
--     from the date of publication"; G.S.R. 607(E) (Drugs (Tenth Amendment) Rules, 2026) inserts
--     "52. All oral formulations containing more than 12% alcohol v/v (Ethyl Alcohol) packed and
--     sold in packings or bottles of more than 30 milliliters" and commences "after the 6 months of
--     publication". Neither is in force at the time of writing. A Sale is judged against the law in
--     force on ITS business date, so the same product resolves differently across dates without one
--     historical Sale changing.
--
--   * This migration SEEDS NOTHING. Both amendments above express commencement as a period after
--     publication rather than as a date, and the gazette carries more than one candidate publication
--     date, so the exact day cannot be proved from the notification text. An approximate date has no
--     place in a compliance gate. Classification enters this database only when a person with
--     authority records it, with a citation. AUSHADHARTH does not claim to be a national drug
--     schedule register.
--
--   * ABSENCE OF A ROW IS "UNKNOWN", NEVER "DOES NOT APPLY". Saying a product is outside a schedule
--     is a positive assertion and costs a row, a date and a source, exactly as saying it is inside
--     one does. This is structural: there is no boolean column whose false value could be mistaken
--     for a finding.
--
-- Additive throughout: no existing row is rewritten, and no classification is guessed from a name,
-- a brand, an HSN code, a dosage form, a tax category, a manufacturer or a barcode.

-- ---------------------------------------------------------------------------------------------
-- 1. What the store asserts about a product's regulatory status.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE product_regulatory_classifications (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    product_id TEXT NOT NULL REFERENCES products(id) ON DELETE RESTRICT,

    -- The five schedules that bear on a retail sale, plus the NDPS axis that crosses two of them.
    -- Adding a scheme later is a migration, deliberately: a scheme is a legal category, not data.
    scheme TEXT NOT NULL CHECK (scheme IN (
        'schedule_h', 'schedule_h1', 'schedule_x', 'schedule_c', 'schedule_c1', 'ndps_purview'
    )),

    -- 1 = this product is within that scheme for the period; 0 = it is outside it. Both are
    -- findings, and both need the source below.
    applies INTEGER NOT NULL CHECK (applies IN (0, 1)),

    -- The period the finding governs, half-open: effective_from <= business_date < effective_to.
    -- A NULL effective_to means "still in force", not "forever": a later finding closes it.
    effective_from TEXT NOT NULL CHECK (effective_from GLOB '????-??-??'),
    effective_to TEXT CHECK (effective_to IS NULL OR effective_to GLOB '????-??-??'),

    -- Provenance is mandatory. A classification with no authority behind it is an opinion, and an
    -- opinion must not gate a sale. Typically a gazette citation; never left to be inferred.
    source_citation TEXT NOT NULL CHECK (length(trim(source_citation)) BETWEEN 3 AND 300),
    reason TEXT CHECK (reason IS NULL OR length(trim(reason)) BETWEEN 1 AND 500),

    determined_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,

    CHECK (effective_to IS NULL OR effective_to > effective_from),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE INDEX product_regulatory_classifications_lookup_idx
ON product_regulatory_classifications(product_id, scheme, effective_from)
WHERE status = 'active';

-- One authority per product, scheme and day. Two active findings whose periods overlap would make
-- the question "is this Schedule H on that date" unanswerable, so the database refuses the second
-- rather than letting a resolver pick a winner.
CREATE TRIGGER product_regulatory_classifications_no_overlap_insert
BEFORE INSERT ON product_regulatory_classifications
WHEN NEW.status = 'active' AND EXISTS (
    SELECT 1 FROM product_regulatory_classifications other
    WHERE other.status = 'active'
      AND other.product_id = NEW.product_id
      AND other.scheme = NEW.scheme
      AND (other.effective_to IS NULL OR other.effective_to > NEW.effective_from)
      AND (NEW.effective_to IS NULL OR NEW.effective_to > other.effective_from)
)
BEGIN
    SELECT RAISE(ABORT, 'regulatory_classification_period_overlaps');
END;

CREATE TRIGGER product_regulatory_classifications_no_overlap_update
BEFORE UPDATE ON product_regulatory_classifications
WHEN NEW.status = 'active' AND EXISTS (
    SELECT 1 FROM product_regulatory_classifications other
    WHERE other.status = 'active'
      AND other.id <> NEW.id
      AND other.product_id = NEW.product_id
      AND other.scheme = NEW.scheme
      AND (other.effective_to IS NULL OR other.effective_to > NEW.effective_from)
      AND (NEW.effective_to IS NULL OR NEW.effective_to > other.effective_from)
)
BEGIN
    SELECT RAISE(ABORT, 'regulatory_classification_period_overlaps');
END;

-- ---------------------------------------------------------------------------------------------
-- 2. Product facts a criterion-based schedule entry may test.
--
-- Schedule H1 entry 52, once commenced, is not a substance name but a test: an oral formulation of
-- more than 12% alcohol v/v, packed in more than 30 millilitres. Both halves are ordinary product
-- facts, so they are recorded as facts — transcribed from the label, never parsed out of a name.
-- Recording them classifies NOTHING by itself: only a classification row, with its own source and
-- effective date, puts a product inside a schedule.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE product_regulatory_attributes (
    product_id TEXT PRIMARY KEY NOT NULL REFERENCES products(id) ON DELETE RESTRICT,
    -- Hundredths of one per cent v/v, so 12.50% is 1250 and no float enters a compliance path.
    -- NULL is "not recorded", which is not the same as zero.
    alcohol_percent_vv_hundredths INTEGER CHECK (
        alcohol_percent_vv_hundredths IS NULL
        OR (alcohol_percent_vv_hundredths >= 0 AND alcohol_percent_vv_hundredths <= 10000)
    ),
    recorded_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z')
) STRICT;

-- Volume is a property of the pack that is sold, not of the formulation, so it lives beside the
-- pack. Hundredths of a millilitre for the same reason as above.
CREATE TABLE product_pack_regulatory_attributes (
    product_pack_id TEXT PRIMARY KEY NOT NULL REFERENCES product_packs(id) ON DELETE RESTRICT,
    net_volume_millilitres_hundredths INTEGER CHECK (
        net_volume_millilitres_hundredths IS NULL OR net_volume_millilitres_hundredths > 0
    ),
    recorded_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z')
) STRICT;

-- ---------------------------------------------------------------------------------------------
-- 3. The people the Drugs Rules name, as distinct from the people who use this software.
--
-- Rule 65(2): a supply on prescription is effected "only by or under the personal supervision of a
-- registered pharmacist". Rule 65(3)(1)(g) wants that pharmacist's signature in the register;
-- rules 65(4)(1)(f) and 65(21)(b)(x) want the signature of the person under whose supervision the
-- sale was effected. The Explanation to rule 65 defines "Registered Pharmacist" by reference to
-- section 2(i) of the Pharmacy Act, 1948 — a registration held by a person, not a role granted in
-- an application. `users.role = 'pharmacist'` is a permission this software grants; it is not
-- evidence of registration, and the two are kept in separate tables so they can never be confused.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE store_professionals (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    full_name TEXT NOT NULL CHECK (length(trim(full_name)) BETWEEN 1 AND 120),
    capacity TEXT NOT NULL CHECK (capacity IN ('registered_pharmacist', 'competent_person')),

    -- A registered pharmacist is registered; the number is what makes the claim checkable, so it is
    -- required for that capacity. The Rules prescribe no register for a competent person, so the
    -- number stays optional there rather than being invented.
    registration_number TEXT CHECK (
        registration_number IS NULL OR length(trim(registration_number)) BETWEEN 1 AND 60
    ),
    registering_authority TEXT CHECK (
        registering_authority IS NULL OR length(trim(registering_authority)) BETWEEN 1 AND 160
    ),
    valid_from TEXT CHECK (valid_from IS NULL OR valid_from GLOB '????-??-??'),
    valid_upto TEXT CHECK (valid_upto IS NULL OR valid_upto GLOB '????-??-??'),

    -- Optional, and only ever a convenience: this person may also hold a login. A professional with
    -- no login is a perfectly good record, and a login with no professional record is not a
    -- professional.
    linked_user_id TEXT REFERENCES users(id) ON DELETE RESTRICT,

    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,

    CHECK (capacity <> 'registered_pharmacist' OR length(trim(COALESCE(registration_number, ''))) > 0),
    CHECK (valid_upto IS NULL OR valid_from IS NULL OR valid_upto >= valid_from),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE INDEX store_professionals_store_idx ON store_professionals(store_id, status, capacity);

-- The same registration cannot be held twice at once under one capacity.
CREATE UNIQUE INDEX store_professionals_registration_uq
ON store_professionals(store_id, capacity, registration_number)
WHERE status = 'active' AND registration_number IS NOT NULL;

-- ---------------------------------------------------------------------------------------------
-- 4. The licence forms the store actually holds, as a typed assertion.
--
-- `store_licences.licence_type` is bounded free text on purpose — a State authority prints what it
-- prints, and Phase 1L-A3 refused to parse it. That text is fine for printing on a memo and useless
-- for gating a sale, so the typed assertion lives here instead, and NO existing free-text row is
-- reinterpreted into it. An operator may point a typed assertion at the display licence it
-- corresponds to; nothing derives one from the other.
--
-- Rule 61: forms 20, 20A and 20B for drugs other than those in Schedules C, C(1) and X; forms 21,
-- 21A and 21B for Schedules C and C(1) excluding Schedule X; forms 20F and 20G for Schedule X.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE store_compliance_licences (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    licence_form TEXT NOT NULL CHECK (licence_form IN (
        'form_20', 'form_20a', 'form_20b', 'form_20f', 'form_20g',
        'form_21', 'form_21a', 'form_21b'
    )),
    licence_number TEXT NOT NULL CHECK (length(trim(licence_number)) BETWEEN 1 AND 100),
    normalized_licence_number TEXT NOT NULL CHECK (
        normalized_licence_number = upper(normalized_licence_number)
        AND length(normalized_licence_number) BETWEEN 1 AND 100
    ),
    issuing_authority TEXT CHECK (
        issuing_authority IS NULL OR length(trim(issuing_authority)) BETWEEN 1 AND 160
    ),
    valid_from TEXT CHECK (valid_from IS NULL OR valid_from GLOB '????-??-??'),
    valid_upto TEXT CHECK (valid_upto IS NULL OR valid_upto GLOB '????-??-??'),

    -- The display licence this assertion is about, when the operator says so. Optional, and never
    -- inferred by matching text.
    display_licence_id TEXT REFERENCES store_licences(id) ON DELETE RESTRICT,

    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,

    CHECK (valid_upto IS NULL OR valid_from IS NULL OR valid_upto >= valid_from),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE UNIQUE INDEX store_compliance_licences_uq
ON store_compliance_licences(store_id, licence_form, normalized_licence_number)
WHERE status = 'active';

-- ---------------------------------------------------------------------------------------------
-- 5. The two rule 65 record elections.
--
-- Rule 65(3)(2): the option to maintain a prescription register or a cash or credit memo book, for
-- drugs supplied from or in the original container, "shall be made in writing to the Licensing
-- Authority at the time of application for the grant of the licence to sell by retail".
-- Rule 65(4)(2): the same option, separately, for the non-prescription supply of a Schedule C drug
-- recorded under rule 65(4)(1).
--
-- They are two elections, not one setting, and neither is a counter decision: the licensee elected
-- once, in writing, to the Licensing Authority. This table records WHAT WAS ELECTED, with the
-- period it governs and a reference to the writing — it does not make the election.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE store_record_elections (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    election TEXT NOT NULL CHECK (election IN (
        'rule_65_3_prescription_supply', 'rule_65_4_non_prescription_schedule_c'
    )),
    -- The rule's own two alternatives. Rule 65(3)(2) calls the register a "prescription register";
    -- rule 65(4)(1)(i) calls it "a register specially maintained for the purpose". The pairing below
    -- keeps each election to the words of its own sub-rule.
    method TEXT NOT NULL CHECK (method IN (
        'prescription_register', 'register', 'cash_or_credit_memo_book'
    )),
    effective_from TEXT NOT NULL CHECK (effective_from GLOB '????-??-??'),
    effective_to TEXT CHECK (effective_to IS NULL OR effective_to GLOB '????-??-??'),
    -- The writing to the Licensing Authority, as the store can identify it.
    evidence_reference TEXT CHECK (
        evidence_reference IS NULL OR length(trim(evidence_reference)) BETWEEN 1 AND 300
    ),
    reason TEXT CHECK (reason IS NULL OR length(trim(reason)) BETWEEN 1 AND 500),
    recorded_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,

    CHECK (effective_to IS NULL OR effective_to > effective_from),
    CHECK (
        (election = 'rule_65_3_prescription_supply'
            AND method IN ('prescription_register', 'cash_or_credit_memo_book'))
        OR (election = 'rule_65_4_non_prescription_schedule_c'
            AND method IN ('register', 'cash_or_credit_memo_book'))
    ),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE INDEX store_record_elections_lookup_idx
ON store_record_elections(store_id, election, effective_from)
WHERE status = 'active';

-- One elected method per election per day, for the same reason classifications may not overlap.
CREATE TRIGGER store_record_elections_no_overlap_insert
BEFORE INSERT ON store_record_elections
WHEN NEW.status = 'active' AND EXISTS (
    SELECT 1 FROM store_record_elections other
    WHERE other.status = 'active'
      AND other.store_id = NEW.store_id
      AND other.election = NEW.election
      AND (other.effective_to IS NULL OR other.effective_to > NEW.effective_from)
      AND (NEW.effective_to IS NULL OR NEW.effective_to > other.effective_from)
)
BEGIN
    SELECT RAISE(ABORT, 'record_election_period_overlaps');
END;

CREATE TRIGGER store_record_elections_no_overlap_update
BEFORE UPDATE ON store_record_elections
WHEN NEW.status = 'active' AND EXISTS (
    SELECT 1 FROM store_record_elections other
    WHERE other.status = 'active'
      AND other.id <> NEW.id
      AND other.store_id = NEW.store_id
      AND other.election = NEW.election
      AND (other.effective_to IS NULL OR other.effective_to > NEW.effective_from)
      AND (NEW.effective_to IS NULL OR NEW.effective_to > other.effective_from)
)
BEGIN
    SELECT RAISE(ABORT, 'record_election_period_overlaps');
END;

-- ---------------------------------------------------------------------------------------------
-- 6. What a posted Sale line freezes.
--
-- 0 = posted before this phase. Its regulatory position was never established, and this database
--     will not invent one for it. Every column below is then NULL, meaning UNRECORDED.
-- 1 = the classification in force on the Sale's business date was resolved line by line and frozen
--     here, together with the manufacturer in force on that date.
--
-- Rule 65(3)(1)(f), 65(4)(1)(e) and 65(21)(b)(v) all want the manufacturer's name in the statutory
-- record, so it is frozen now rather than read from today's `product_company_roles` when a
-- two-year-old record is reproduced. Only the 'manufacturer' role is ever used: a marketer, a brand
-- owner and an importer are different things, and no rule says one may stand in for the maker.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE sale_lines ADD COLUMN regulatory_snapshot_version INTEGER NOT NULL DEFAULT 0
    CHECK (regulatory_snapshot_version IN (0, 1));

ALTER TABLE sale_lines ADD COLUMN manufacturer_company_id TEXT;
ALTER TABLE sale_lines ADD COLUMN manufacturer_name TEXT
    CHECK (manufacturer_name IS NULL OR length(trim(manufacturer_name)) > 0);

-- One JSON object with one entry per scheme, each 'applies', 'does_not_apply' or 'unknown'. Stored
-- as the resolved ANSWER rather than as a pointer to the classification rows, because the rows can
-- later be archived or corrected and the document must still say what it said.
ALTER TABLE sale_lines ADD COLUMN regulatory_schemes_snapshot TEXT
    CHECK (regulatory_schemes_snapshot IS NULL OR json_valid(regulatory_schemes_snapshot));

-- A draft has nothing frozen about it.
CREATE TRIGGER sale_lines_regulatory_draft_only_insert
BEFORE INSERT ON sale_lines
WHEN (NEW.regulatory_snapshot_version <> 0
   OR NEW.manufacturer_company_id IS NOT NULL
   OR NEW.manufacturer_name IS NOT NULL
   OR NEW.regulatory_schemes_snapshot IS NOT NULL)
  AND NOT EXISTS (
    SELECT 1 FROM sale_documents WHERE id = NEW.sale_document_id AND status = 'posted'
)
BEGIN
    SELECT RAISE(ABORT, 'regulatory_snapshot_before_posting');
END;

-- A version-1 snapshot is coherent, whoever writes it: all six schemes answered with one of the
-- three permitted answers, and a manufacturer either wholly recorded or wholly absent. Posting
-- writes the snapshot by UPDATE while the document is still a draft; the INSERT form below closes
-- the same door against a line written straight into a document that is already posted.
CREATE TRIGGER sale_lines_regulatory_snapshot_coherent_insert
BEFORE INSERT ON sale_lines
WHEN NEW.regulatory_snapshot_version = 1 AND (
    NEW.regulatory_schemes_snapshot IS NULL
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_h')
        NOT IN ('applies', 'does_not_apply', 'unknown')
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_h1')
        NOT IN ('applies', 'does_not_apply', 'unknown')
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_x')
        NOT IN ('applies', 'does_not_apply', 'unknown')
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_c')
        NOT IN ('applies', 'does_not_apply', 'unknown')
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_c1')
        NOT IN ('applies', 'does_not_apply', 'unknown')
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.ndps_purview')
        NOT IN ('applies', 'does_not_apply', 'unknown')
 OR ((NEW.manufacturer_company_id IS NULL) <> (NEW.manufacturer_name IS NULL))
)
BEGIN
    SELECT RAISE(ABORT, 'regulatory_snapshot_incoherent');
END;

-- A line may not be added to a POSTED document carrying a position this phase cannot sell. The
-- document-level gate below tests the lines that exist when the document is posted; this tests a
-- line that arrives afterwards, which is a direct-SQL path only.
CREATE TRIGGER sale_lines_regulatory_gate_insert
BEFORE INSERT ON sale_lines
WHEN EXISTS (
    SELECT 1 FROM sale_documents WHERE id = NEW.sale_document_id AND status = 'posted'
) AND (
    NEW.regulatory_snapshot_version <> 1
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_h') = 'applies'
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_h1') = 'applies'
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_x') = 'applies'
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_c') = 'applies'
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_c1') = 'applies'
 OR EXISTS (
        SELECT 1 FROM products product
        WHERE product.id = NEW.product_id AND product.product_kind = 'medicine' AND (
            json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_h') = 'unknown'
         OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_h1') = 'unknown'
         OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_x') = 'unknown'
         OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_c') = 'unknown'
         OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_c1') = 'unknown'))
)
BEGIN
    SELECT RAISE(ABORT, 'regulatory_gate_refuses_posting');
END;

CREATE TRIGGER sale_lines_regulatory_snapshot_coherent_update
BEFORE UPDATE ON sale_lines
WHEN NEW.regulatory_snapshot_version = 1 AND (
    NEW.regulatory_schemes_snapshot IS NULL
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_h')
        NOT IN ('applies', 'does_not_apply', 'unknown')
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_h1')
        NOT IN ('applies', 'does_not_apply', 'unknown')
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_x')
        NOT IN ('applies', 'does_not_apply', 'unknown')
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_c')
        NOT IN ('applies', 'does_not_apply', 'unknown')
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_c1')
        NOT IN ('applies', 'does_not_apply', 'unknown')
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.ndps_purview')
        NOT IN ('applies', 'does_not_apply', 'unknown')
 OR ((NEW.manufacturer_company_id IS NULL) <> (NEW.manufacturer_name IS NULL))
)
BEGIN
    SELECT RAISE(ABORT, 'regulatory_snapshot_incoherent');
END;

-- ---------------------------------------------------------------------------------------------
-- 7. The gate itself.
--
-- A document may not become posted while any of its lines is one this software cannot lawfully
-- sell yet. Three separate refusals, in the database rather than only in the service, so a direct
-- SQL writer is refused exactly as an API caller is:
--
--   * Schedule H, H1 or X applies. Rule 65(9)(a) requires a Registered Medical Practitioner's
--     prescription; the prescription workflow arrives in a later phase, so the sale is refused
--     rather than posted without one.
--   * Schedule C or C(1) applies. Rule 65(4)(1) requires the purchaser's name and address, the
--     manufacturer, batch and expiry, and the signature of the supervising person in a register or
--     memo book; that record is not implemented yet, so the sale is refused.
--   * A medicine whose schedule position is unknown. Not knowing whether a drug is Schedule H is
--     not a reason to sell it. NDPS purview is deliberately NOT part of this test: it is a
--     cross-reference to another Act that attaches through Schedule H or H1 labelling under rule
--     97(1)(c) and (f), and no rule 65 retail obligation turns on it alone. A 'device' or a
--     'general_pharmacy_item' is not blocked for an unknown, so an unrelated counter sale is
--     unaffected — but a device that is positively classified into Schedule C is caught by the
--     second refusal above, because Schedule C includes "Sterile Disposable Devices for single use
--     only".
-- ---------------------------------------------------------------------------------------------
CREATE TRIGGER sale_documents_regulatory_gate_update
BEFORE UPDATE ON sale_documents
WHEN NEW.status = 'posted' AND OLD.status <> 'posted' AND EXISTS (
    SELECT 1 FROM sale_lines line
    JOIN products product ON product.id = line.product_id
    WHERE line.sale_document_id = NEW.id AND (
        line.regulatory_snapshot_version <> 1
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_h') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_h1') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_x') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c1') = 'applies'
     OR (product.product_kind = 'medicine' AND (
            json_extract(line.regulatory_schemes_snapshot, '$.schedule_h') = 'unknown'
         OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_h1') = 'unknown'
         OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_x') = 'unknown'
         OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c') = 'unknown'
         OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c1') = 'unknown'))
    )
)
BEGIN
    SELECT RAISE(ABORT, 'regulatory_gate_refuses_posting');
END;

-- The same test for a row forged straight into the posted state.
CREATE TRIGGER sale_documents_regulatory_gate_insert
BEFORE INSERT ON sale_documents
WHEN NEW.status = 'posted' AND EXISTS (
    SELECT 1 FROM sale_lines line
    JOIN products product ON product.id = line.product_id
    WHERE line.sale_document_id = NEW.id AND (
        line.regulatory_snapshot_version <> 1
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_h') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_h1') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_x') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c1') = 'applies'
     OR (product.product_kind = 'medicine' AND (
            json_extract(line.regulatory_schemes_snapshot, '$.schedule_h') = 'unknown'
         OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_h1') = 'unknown'
         OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_x') = 'unknown'
         OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c') = 'unknown'
         OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c1') = 'unknown'))
    )
)
BEGIN
    SELECT RAISE(ABORT, 'regulatory_gate_refuses_posting');
END;

-- ---------------------------------------------------------------------------------------------
-- 8. The audit log learns the new entity types.
--
-- Same recreate-and-copy shape Phase 1K and 1L-A used: the CHECK is the point of the column, so it
-- is widened by rebuilding the table rather than dropped.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE master_change_events RENAME TO master_change_events_phase1lb;

CREATE TABLE master_change_events (
    event_id TEXT PRIMARY KEY NOT NULL CHECK (
        length(event_id) = 36 AND substr(event_id, 15, 1) = '7'
        AND lower(substr(event_id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    entity_type TEXT NOT NULL CHECK (entity_type IN (
        'unit_of_measure', 'dosage_form', 'pharmaceutical_company', 'company_identifier',
        'brand', 'hsn_code', 'tax_category', 'tax_rate_version', 'regulatory_category',
        'product', 'product_company_role', 'product_pack', 'store_pack_policy', 'barcode',
        'ingredient', 'salt_form', 'strength_unit', 'product_composition_component',
        'product_batch',
        'state_code', 'party', 'party_role', 'party_address',
        'store_tax_identity',
        'purchase_document',
        'controlled_formulation', 'price_control_version', 'product_price_control',
        'sale_document',
        'return_document', 'stock_disposition', 'supplier_credit_note_evidence',
        'stock_operation',
        'backup', 'restore_operation',
        'store_profile', 'store_address', 'store_licence',
        'product_regulatory_classification', 'product_regulatory_attributes',
        'product_pack_regulatory_attributes',
        'store_professional', 'store_compliance_licence', 'store_record_election'
    )),
    entity_id TEXT NOT NULL CHECK (
        length(entity_id) = 36 AND substr(entity_id, 15, 1) = '7'
        AND lower(substr(entity_id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    entity_revision INTEGER NOT NULL CHECK (entity_revision >= 1),
    action TEXT NOT NULL CHECK (action IN ('created', 'updated', 'archived', 'restored', 'posted')),
    occurred_at_utc TEXT NOT NULL CHECK (occurred_at_utc GLOB '????-??-??T??:??:??*Z'),
    reason TEXT,
    payload_schema_version INTEGER NOT NULL CHECK (payload_schema_version >= 1),
    change_payload TEXT NOT NULL CHECK (json_valid(change_payload)),
    actor_id TEXT,
    terminal_id TEXT
) STRICT;

INSERT INTO master_change_events
SELECT * FROM master_change_events_phase1lb;

DROP TABLE master_change_events_phase1lb;

CREATE INDEX master_change_events_entity_idx
ON master_change_events(entity_type, entity_id, entity_revision);

CREATE TRIGGER master_change_events_no_update
BEFORE UPDATE ON master_change_events
BEGIN
    SELECT RAISE(ABORT, 'master_change_events_are_append_only');
END;

CREATE TRIGGER master_change_events_no_delete
BEFORE DELETE ON master_change_events
BEGIN
    SELECT RAISE(ABORT, 'master_change_events_are_append_only');
END;
