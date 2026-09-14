//! Phase 1F tax rate resolution.
//!
//! The single place effective-date logic lives. A Product identifies its Tax Category; the rate
//! that applies on a given date is always resolved from `tax_rate_versions` and never stored on the
//! Product. Phase 1G asks this module rather than re-implementing interval arithmetic, and then
//! snapshots what it resolved onto the posted line.

use sqlx::{FromRow, SqliteExecutor};
use time::{Date, macros::format_description};

/// One resolved tax rate version.
///
/// Every component is an exact integer in basis points, where `100` is `1.00%`, matching the frozen
/// `tax_rate_versions` schema. No binary floating-point value appears anywhere in this path.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct ResolvedTaxRate {
    pub id: String,
    pub effective_from: String,
    pub effective_to: Option<String>,
    pub cgst_basis_points: i64,
    pub sgst_basis_points: i64,
    pub igst_basis_points: i64,
    pub cess_basis_points: i64,
}

#[derive(Debug)]
pub enum TaxResolutionError {
    InvalidDate,
    Database(sqlx::Error),
}

impl From<sqlx::Error> for TaxResolutionError {
    fn from(value: sqlx::Error) -> Self {
        Self::Database(value)
    }
}

/// Resolves the tax rate version in force for a Tax Category on a calendar date.
///
/// Periods are half-open `[effective_from, effective_to)` exactly as the frozen migration states and
/// as the frozen no-overlap triggers assume, so on the day equal to `effective_to` the *next*
/// version applies, or none. At most one active version can match, because those triggers forbid
/// overlapping active periods for one category.
///
/// Only `status = 'active'` versions resolve: an archived version is history, not a current rate.
/// The Tax Category's own status is deliberately **not** filtered — a Product that already
/// references a since-archived category must still resolve its rate for historical display.
/// Assignment is what archiving restricts, never reading.
///
/// The returned components are reported as stored. This function does **not** choose between
/// `CGST + SGST` and `IGST`: that depends on supplier or customer registration and place of supply,
/// which is transaction context Phase 1G supplies, not Product classification.
pub async fn resolve_tax_rate<'e, E>(
    executor: E,
    tax_category_id: &str,
    on_date: &str,
) -> Result<Option<ResolvedTaxRate>, TaxResolutionError>
where
    E: SqliteExecutor<'e>,
{
    let date = validate_calendar_date(on_date)?;
    sqlx::query_as::<_, ResolvedTaxRate>(
        "SELECT id,effective_from,effective_to,cgst_basis_points,sgst_basis_points,\
         igst_basis_points,cess_basis_points \
         FROM tax_rate_versions \
         WHERE tax_category_id = ?1 AND status = 'active' \
           AND effective_from <= ?2 AND (effective_to IS NULL OR effective_to > ?2) \
         LIMIT 1",
    )
    .bind(tax_category_id)
    .bind(&date)
    .fetch_optional(executor)
    .await
    .map_err(TaxResolutionError::Database)
}

/// Dates are compared as `YYYY-MM-DD` strings, which orders correctly only for real calendar dates,
/// so a malformed value is refused rather than silently mis-comparing.
pub fn validate_calendar_date(value: &str) -> Result<String, TaxResolutionError> {
    Date::parse(value, format_description!("[year]-[month]-[day]"))
        .map(|_| value.to_owned())
        .map_err(|_| TaxResolutionError::InvalidDate)
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;
    use uuid::Uuid;

    use super::*;

    async fn pool() -> (tempfile::TempDir, SqlitePool) {
        let temp = tempfile::tempdir().unwrap();
        let pool = crate::infrastructure::database::connect(&temp.path().join("tax.sqlite3"))
            .await
            .unwrap();
        (temp, pool)
    }

    async fn category(pool: &SqlitePool, code: &str) -> String {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO tax_categories (id,jurisdiction,category_code,display_name,tax_treatment,\
             created_at_utc,updated_at_utc) VALUES (?,'IN',?,'Test category','taxable',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(code)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn rate(
        pool: &SqlitePool,
        category_id: &str,
        from: &str,
        to: Option<&str>,
        cgst: i64,
    ) -> String {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO tax_rate_versions (id,tax_category_id,effective_from,effective_to,\
             cgst_basis_points,sgst_basis_points,igst_basis_points,cess_basis_points,\
             created_at_utc,updated_at_utc) VALUES (?,?,?,?,?,?,?,0,\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(category_id)
        .bind(from)
        .bind(to)
        .bind(cgst)
        .bind(cgst)
        .bind(cgst * 2)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    #[tokio::test]
    async fn periods_are_half_open_so_the_end_date_belongs_to_the_next_version() {
        let (_temp, pool) = pool().await;
        let category_id = category(&pool, "gst-12").await;
        // 2.50% + 2.50% until 2026-01-01, then 6.00% + 6.00% open-ended.
        let first = rate(&pool, &category_id, "2025-01-01", Some("2026-01-01"), 250).await;
        let second = rate(&pool, &category_id, "2026-01-01", None, 600).await;

        let resolve = async |date: &str| {
            resolve_tax_rate(&pool, &category_id, date)
                .await
                .expect("resolution")
        };

        // Before anything is in force.
        assert!(resolve("2024-12-31").await.is_none());
        // Exactly effective_from is inside the period.
        assert_eq!(resolve("2025-01-01").await.unwrap().id, first);
        // Inside.
        assert_eq!(resolve("2025-06-15").await.unwrap().id, first);
        // The day before the boundary is still the first version.
        assert_eq!(resolve("2025-12-31").await.unwrap().id, first);
        // Exactly effective_to belongs to the NEXT version: [from, to).
        assert_eq!(resolve("2026-01-01").await.unwrap().id, second);
        // An open-ended final version keeps applying indefinitely.
        assert_eq!(resolve("2099-12-31").await.unwrap().id, second);
    }

    #[tokio::test]
    async fn a_gap_after_a_closed_final_version_resolves_to_nothing() {
        let (_temp, pool) = pool().await;
        let category_id = category(&pool, "gst-05").await;
        rate(&pool, &category_id, "2025-01-01", Some("2026-01-01"), 250).await;
        // Nothing follows, so after the close there is no applicable rate rather than a stale one.
        assert!(
            resolve_tax_rate(&pool, &category_id, "2026-01-02")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn components_resolve_as_exact_integers_without_choosing_a_treatment() {
        let (_temp, pool) = pool().await;
        let category_id = category(&pool, "gst-18").await;
        rate(&pool, &category_id, "2025-01-01", None, 900).await;
        let resolved = resolve_tax_rate(&pool, &category_id, "2025-07-01")
            .await
            .unwrap()
            .expect("a rate applies");
        assert_eq!(resolved.cgst_basis_points, 900);
        assert_eq!(resolved.sgst_basis_points, 900);
        assert_eq!(resolved.igst_basis_points, 1800);
        assert_eq!(resolved.cess_basis_points, 0);
        // Intra-state and inter-state figures are both present and neither is preferred here: the
        // choice belongs to the transaction, which knows the places of supply.
        assert_eq!(
            resolved.cgst_basis_points + resolved.sgst_basis_points,
            resolved.igst_basis_points
        );

        let stored: String = sqlx::query_scalar(
            "SELECT typeof(cgst_basis_points) FROM tax_rate_versions WHERE id=?",
        )
        .bind(&resolved.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored, "integer", "money-adjacent values are never REAL");
    }

    #[tokio::test]
    async fn an_archived_version_stops_resolving_but_an_archived_category_still_does() {
        let (_temp, pool) = pool().await;
        let category_id = category(&pool, "gst-28").await;
        let version = rate(&pool, &category_id, "2025-01-01", None, 1400).await;
        assert!(
            resolve_tax_rate(&pool, &category_id, "2025-07-01")
                .await
                .unwrap()
                .is_some()
        );

        // Archiving the CATEGORY must not hide history: a Product assigned before it was archived
        // still has to show what its rate was.
        sqlx::query(
            "UPDATE tax_categories SET status='archived',archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             archive_reason='no longer used' WHERE id=?",
        )
        .bind(&category_id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            resolve_tax_rate(&pool, &category_id, "2025-07-01")
                .await
                .unwrap()
                .is_some(),
            "an archived category must still resolve for historical display"
        );

        // Archiving the VERSION does remove it: it is no longer a rate in force.
        sqlx::query(
            "UPDATE tax_rate_versions SET status='archived',archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             archive_reason='superseded' WHERE id=?",
        )
        .bind(&version)
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            resolve_tax_rate(&pool, &category_id, "2025-07-01")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_malformed_date_is_refused_rather_than_compared_as_text() {
        let (_temp, pool) = pool().await;
        let category_id = category(&pool, "gst-00").await;
        rate(&pool, &category_id, "2025-01-01", None, 0).await;
        for bad in [
            "2025-13-01",
            "2025-02-30",
            "01-01-2025",
            "2025/01/01",
            "",
            "today",
        ] {
            assert!(
                matches!(
                    resolve_tax_rate(&pool, &category_id, bad).await,
                    Err(TaxResolutionError::InvalidDate)
                ),
                "accepted {bad}"
            );
        }
    }

    #[tokio::test]
    async fn one_category_never_resolves_another_categorys_rate() {
        let (_temp, pool) = pool().await;
        let first = category(&pool, "gst-a").await;
        let second = category(&pool, "gst-b").await;
        let first_version = rate(&pool, &first, "2025-01-01", None, 250).await;
        rate(&pool, &second, "2025-01-01", None, 900).await;
        let resolved = resolve_tax_rate(&pool, &first, "2025-07-01")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved.id, first_version);
        assert_eq!(resolved.cgst_basis_points, 250);
    }
}
