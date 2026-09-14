-- Phase 1F Product tax classification only.
-- Purchase, sales, tax invoices, GST returns, input credit, and accounting remain deferred.
--
-- A Product identifies its tax classification; it never stores a tax RATE. No rate, percentage, or
-- basis-point column exists on products and none may ever be added: rates live in
-- tax_rate_versions, are effective-dated, and are resolved by date. A posted document will snapshot
-- the figures it actually applied, which is what an audit needs — not what the Product happened to
-- be configured as later.
--
-- HSN and Tax Category are deliberately independent. The frozen masters encode no relationship
-- between them, and this migration invents none: an HSN code classifies goods, while a Tax Category
-- carries the effective-dated rates.

-- Additive and nullable, so every Product that already exists keeps reading and writing normally.
-- Tax classification becomes a requirement where it actually matters — at GST-aware transaction
-- posting — not retroactively across the catalog.
ALTER TABLE products ADD COLUMN hsn_code_id TEXT REFERENCES hsn_codes(id) ON DELETE RESTRICT;
ALTER TABLE products ADD COLUMN tax_category_id TEXT REFERENCES tax_categories(id) ON DELETE RESTRICT;

-- The grain Phase 1G will resolve rates at.
CREATE INDEX products_tax_category_idx ON products(tax_category_id);
CREATE INDEX products_hsn_code_idx ON products(hsn_code_id);

-- A newly assigned classification must reference an active master.
CREATE TRIGGER products_tax_classification_insert
BEFORE INSERT ON products
WHEN (NEW.hsn_code_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM hsn_codes WHERE id = NEW.hsn_code_id AND status = 'active'
    ))
  OR (NEW.tax_category_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM tax_categories WHERE id = NEW.tax_category_id AND status = 'active'
    ))
BEGIN
    SELECT RAISE(ABORT, 'product_tax_conflict');
END;

-- Only a reference that actually CHANGES is validated.
--
-- The rule is "a newly assigned reference must be active", not "every non-NULL reference must be
-- active". The classification endpoint writes both columns on every call, so re-validating unchanged
-- values would trap a Product holding a since-archived HSN: clearing only its Tax Category would
-- re-check the untouched archived HSN and be refused, leaving the Product permanently uneditable.
-- Comparing against OLD keeps an already-assigned archived reference readable and retained while
-- still refusing it as a new choice.
CREATE TRIGGER products_tax_classification_update
BEFORE UPDATE OF hsn_code_id, tax_category_id ON products
WHEN (NEW.hsn_code_id IS NOT NULL
      AND (OLD.hsn_code_id IS NULL OR NEW.hsn_code_id <> OLD.hsn_code_id)
      AND NOT EXISTS (
          SELECT 1 FROM hsn_codes WHERE id = NEW.hsn_code_id AND status = 'active'
      ))
  OR (NEW.tax_category_id IS NOT NULL
      AND (OLD.tax_category_id IS NULL OR NEW.tax_category_id <> OLD.tax_category_id)
      AND NOT EXISTS (
          SELECT 1 FROM tax_categories WHERE id = NEW.tax_category_id AND status = 'active'
      ))
BEGIN
    SELECT RAISE(ABORT, 'product_tax_conflict');
END;
