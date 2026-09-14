//! Real Store Service integration gate (ADR-013).
//!
//! This target assembles the production service stack — real migrations, the real Axum router, real
//! cookie authentication — binds it to a real loopback TCP port, and drives it with raw HTTP/1.1.
//! It never opens the customer database: the path comes from a per-run temporary directory, so the
//! authoritative `%LOCALAPPDATA%` location is untouched and production startup is unmodified.
//!
//! Cargo builds targets under `tests/` only for `cargo test`, so none of this exists in a release
//! binary.

use std::net::SocketAddr;

use aushadharth_store_service::{api, infrastructure::database};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

struct Service {
    address: SocketAddr,
    database_path: std::path::PathBuf,
    _temp: tempfile::TempDir,
}

/// Starts the real service against a disposable database on an OS-assigned port.
async fn start() -> Service {
    let temp = tempfile::tempdir().expect("temporary directory");
    let database_path = temp.path().join("integration.sqlite3");
    // The real connect() applies the real migrations with the real pragmas.
    let pool = database::connect(&database_path)
        .await
        .expect("migrated database");
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let address = listener.local_addr().expect("bound address");
    // The same router main.rs serves, without the static web bundle.
    let router = api::router(pool, None);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Service {
        address,
        database_path,
        _temp: temp,
    }
}

struct Reply {
    status: u16,
    headers: String,
    body: Value,
}

/// A deliberately small HTTP/1.1 client. Writing one keeps the gate dependency-free, and the point
/// of the gate is that the bytes really cross a socket.
async fn call(
    service: &Service,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> Reply {
    let mut stream = TcpStream::connect(service.address)
        .await
        .expect("connect to service");
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\nAccept: application/json\r\n",
        service.address.port()
    );
    if !payload.is_empty() {
        request.push_str("Content-Type: application/json\r\n");
    }
    request.push_str(&format!("Content-Length: {}\r\n", payload.len()));
    if let Some(cookie) = cookie {
        request.push_str(&format!("Cookie: {cookie}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(&payload);
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("read full response");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, rest) = text.split_once("\r\n\r\n").expect("http response framing");
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .expect("status code");
    // Connection: close means the body is everything after the header block; chunked framing is
    // unwrapped when the service uses it.
    let body_text = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        unchunk(rest)
    } else {
        rest.to_owned()
    };
    let body = if body_text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(body_text.trim()).unwrap_or(Value::Null)
    };
    Reply {
        status,
        headers: head.to_owned(),
        body,
    }
}

fn unchunk(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some((size_line, remainder)) = rest.split_once("\r\n") {
        let Ok(size) = usize::from_str_radix(size_line.trim(), 16) else {
            break;
        };
        if size == 0 || remainder.len() < size {
            break;
        }
        out.push_str(&remainder[..size]);
        rest = remainder[size..].trim_start_matches("\r\n");
    }
    out
}

fn session_cookie(headers: &str) -> String {
    headers
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("set-cookie:"))
        .and_then(|line| line.split_once(':'))
        .map(|(_, value)| {
            value
                .trim()
                .split(';')
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .expect("session cookie")
}

/// The critical boundary: a real client, over a real socket, through real authentication and the
/// real inventory transaction, into real SQLite constraints, and back out as a derived balance.
#[tokio::test]
async fn real_service_posts_opening_stock_and_derives_the_balance_over_http() {
    let service = start().await;

    // First-run setup creates the owner and returns a real session cookie.
    let setup = call(
        &service,
        "POST",
        "/api/v1/auth/setup",
        Some(json!({
            "storeDisplayName": "Integration Pharmacy",
            "ownerDisplayName": "Integration Owner",
            "loginIdentifier": "integration.owner",
            "password": "Integration-Password-42"
        })),
        None,
    )
    .await;
    assert_eq!(setup.status, 201, "{:?}", setup.body);
    let cookie = session_cookie(&setup.headers);
    assert!(cookie.starts_with("aushadharth_session="));

    let status = call(&service, "GET", "/api/v1/auth/status", None, Some(&cookie)).await;
    assert_eq!(status.status, 200);
    assert_eq!(status.body["authenticated"], true);
    assert_eq!(status.body["user"]["role"], "owner_admin");

    // An unauthenticated caller is refused by the real auth layer, not by a double.
    let anonymous = call(&service, "GET", "/api/v1/inventory/stock", None, None).await;
    assert_eq!(anonymous.status, 401);
    assert_eq!(anonymous.body["code"], "authentication_required");

    // Seeded unit and a real Product plus Pack, created over HTTP.
    let tablet = "01997000-0000-7000-8000-000000000001";
    let strip = "01997000-0000-7000-8000-000000000004";
    let product = call(
        &service,
        "POST",
        "/api/v1/products",
        Some(json!({"product":{
            "productKind":"general_pharmacy_item","baseUnitId":tablet,
            "quantityScale":0,"displayName":"Integration item"
        }})),
        Some(&cookie),
    )
    .await;
    assert_eq!(product.status, 201, "{:?}", product.body);
    let product_id = product.body["id"].as_str().expect("product id").to_owned();

    let pack = call(
        &service,
        "POST",
        &format!("/api/v1/products/{product_id}/packs"),
        Some(json!({"containerUnitId":strip,"baseQuantityAtoms":10})),
        Some(&cookie),
    )
    .await;
    assert_eq!(pack.status, 201, "{:?}", pack.body);
    let pack_id = pack.body["id"].as_str().expect("pack id").to_owned();

    // Five strips of ten tablets is fifty base-unit atoms.
    let key = "01997000-0000-7000-8000-0000000000aa";
    let posting = json!({
        "idempotencyKey": key,
        "movementType": "opening_stock",
        "productPackId": pack_id,
        "quantityDeltaAtoms": 50,
        "occurredOn": "2026-04-01"
    });
    let posted = call(
        &service,
        "POST",
        "/api/v1/inventory/movements",
        Some(posting.clone()),
        Some(&cookie),
    )
    .await;
    assert_eq!(posted.status, 201, "{:?}", posted.body);
    assert_eq!(posted.body["quantityDeltaAtoms"], 50);
    assert_eq!(posted.body["movementType"], "opening_stock");

    // Idempotency survives the real transport: a replay returns the same movement.
    let replay = call(
        &service,
        "POST",
        "/api/v1/inventory/movements",
        Some(posting),
        Some(&cookie),
    )
    .await;
    assert_eq!(replay.status, 200, "{:?}", replay.body);
    assert_eq!(replay.body["id"], posted.body["id"]);

    // The balance is derived by summing the ledger in real SQLite.
    let stock = call(
        &service,
        "GET",
        "/api/v1/inventory/stock",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(stock.status, 200);
    assert_eq!(stock.body.as_array().expect("balances").len(), 1);
    assert_eq!(stock.body[0]["balanceAtoms"], 50);
    assert_eq!(stock.body[0]["productPackId"], pack_id.as_str());

    // Real SQLite constraints still bite across the wire.
    let negative = call(
        &service,
        "POST",
        "/api/v1/inventory/movements",
        Some(json!({
            "idempotencyKey": "01997000-0000-7000-8000-0000000000bb",
            "movementType": "adjustment",
            "productPackId": pack_id,
            "quantityDeltaAtoms": -60,
            "occurredOn": "2026-04-02"
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(negative.status, 409, "{:?}", negative.body);
    assert_eq!(negative.body["code"], "insufficient_stock");
    assert_eq!(negative.body["availableAtoms"], 50);

    // The disposable database is the one that was used, and it holds the ledger.
    assert!(service.database_path.exists());
    let pool = database::connect(&service.database_path)
        .await
        .expect("reopen disposable database");
    let movements: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inventory_movements")
        .fetch_one(&pool)
        .await
        .expect("count movements");
    assert_eq!(movements, 1, "the replay must not have posted twice");
    pool.close().await;
}

/// The gate must never be able to reach the customer's authoritative database.
#[tokio::test]
async fn the_gate_never_touches_the_real_customer_database() {
    let service = start().await;
    let used = service.database_path.to_string_lossy().to_ascii_lowercase();
    // A per-run temporary path, not the application data root, and not inside the repository.
    assert!(used.ends_with("integration.sqlite3"));
    assert!(!used.contains("bizarth technologies"));
    assert!(!used.contains("aushadharth\\database"));
    assert!(!used.contains("onedrive"));
    let repository_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("repository root")
        .to_string_lossy()
        .to_ascii_lowercase();
    assert!(!used.starts_with(&repository_root));
    // Production path policy still accepts it on its own terms rather than being bypassed.
    aushadharth_store_service::platform::runtime_paths::validate_database_path(
        &service.database_path,
        std::path::Path::new(&repository_root),
    )
    .expect("temporary database must satisfy the frozen path policy");
}

/// Party identity across the same real boundary: a real socket, a real session, the real GSTIN
/// checksum, and the real trigger that binds a registration to its State.
#[tokio::test]
async fn real_service_creates_a_supplier_and_enforces_its_tax_identity_over_http() {
    let service = start().await;
    let setup = call(
        &service,
        "POST",
        "/api/v1/auth/setup",
        Some(json!({
            "storeDisplayName": "Integration Pharmacy",
            "ownerDisplayName": "Integration Owner",
            "loginIdentifier": "integration.owner",
            "password": "Integration-Password-42"
        })),
        None,
    )
    .await;
    assert_eq!(setup.status, 201, "{:?}", setup.body);
    let cookie = session_cookie(&setup.headers);

    let maharashtra = "01997300-0000-7000-8000-000000000027";
    let karnataka = "01997300-0000-7000-8000-000000000029";
    // A genuine published GSTIN, so the check digit is really exercised.
    let gstin = "27AAPFU0939F1ZV";
    let supplier = |gstin: &str, state: &str| {
        json!({
            "party": {
                "displayName": "Sharma Medicals",
                "gstRegistrationStatus": "registered",
                "gstin": gstin,
                "placeOfSupplyStateId": state
            },
            "roles": [{ "role": "supplier" }],
            "addresses": [{
                "addressRole": "billing", "line1": "12 Market Road",
                "city": "Pune", "stateId": state, "postalCode": "411001", "isPrimary": true
            }]
        })
    };

    // An unauthenticated caller is refused by the real auth layer.
    let anonymous = call(&service, "GET", "/api/v1/parties", None, None).await;
    assert_eq!(anonymous.status, 401);
    assert_eq!(anonymous.body["code"], "authentication_required");

    // A mistyped GSTIN that is well-shaped is caught only by the checksum.
    let bad = call(
        &service,
        "POST",
        "/api/v1/parties",
        Some(supplier("27AAPFU0939F1ZX", maharashtra)),
        Some(&cookie),
    )
    .await;
    assert_eq!(bad.status, 422, "{:?}", bad.body);
    assert_eq!(bad.body["code"], "validation_failed");
    assert_eq!(bad.body["issues"][0]["field"], "gstin");

    // A Maharashtra registration cannot claim a Karnataka place of supply.
    let mismatched = call(
        &service,
        "POST",
        "/api/v1/parties",
        Some(supplier(gstin, karnataka)),
        Some(&cookie),
    )
    .await;
    assert_eq!(mismatched.status, 409, "{:?}", mismatched.body);
    assert_eq!(mismatched.body["code"], "party_conflict");

    let created = call(
        &service,
        "POST",
        "/api/v1/parties",
        Some(supplier(gstin, maharashtra)),
        Some(&cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    let party_id = created.body["id"].as_str().expect("party id").to_owned();
    assert_eq!(created.body["normalizedGstin"], gstin);
    assert_eq!(created.body["roles"][0]["role"], "supplier");
    assert_eq!(created.body["addresses"][0]["isPrimary"], true);

    // The same active GSTIN cannot be held twice.
    let duplicate = call(
        &service,
        "POST",
        "/api/v1/parties",
        Some(supplier(gstin, maharashtra)),
        Some(&cookie),
    )
    .await;
    assert_eq!(duplicate.status, 409, "{:?}", duplicate.body);
    assert_eq!(duplicate.body["code"], "duplicate_conflict");

    // Searching by the tax number finds it without knowing the name.
    let found = call(
        &service,
        "GET",
        "/api/v1/parties?search=27aapfu0939f1zv",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(found.status, 200);
    assert_eq!(found.body.as_array().expect("parties").len(), 1);
    assert_eq!(found.body[0]["id"], party_id.as_str());

    // The disposable database holds exactly one party, and it carries no accounting column.
    let pool = database::connect(&service.database_path)
        .await
        .expect("reopen disposable database");
    let parties: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM parties")
        .fetch_one(&pool)
        .await
        .expect("count parties");
    assert_eq!(parties, 1, "the refused attempts must not have written");
    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('parties')")
        .fetch_all(&pool)
        .await
        .expect("read columns");
    for column in &columns {
        for forbidden in ["balance", "outstanding", "credit", "amount", "paise"] {
            assert!(
                !column.contains(forbidden),
                "a Party is an identity, never an account; found {column}"
            );
        }
    }
    pool.close().await;
}

/// Product tax classification across the real boundary: a real socket, a real session, the real
/// reference masters, and the real effective-date resolver reading real migrated SQLite.
#[tokio::test]
async fn real_service_classifies_a_product_and_resolves_its_rate_by_date_over_http() {
    let service = start().await;
    let setup = call(
        &service,
        "POST",
        "/api/v1/auth/setup",
        Some(json!({
            "storeDisplayName": "Integration Pharmacy",
            "ownerDisplayName": "Integration Owner",
            "loginIdentifier": "integration.owner",
            "password": "Integration-Password-42"
        })),
        None,
    )
    .await;
    assert_eq!(setup.status, 201, "{:?}", setup.body);
    let cookie = session_cookie(&setup.headers);

    let reference = async |kind: &str, attributes: Value| {
        let reply = call(
            &service,
            "POST",
            &format!("/api/v1/reference/{kind}"),
            Some(json!({ "attributes": attributes })),
            Some(&cookie),
        )
        .await;
        assert_eq!(reply.status, 201, "{kind}: {:?}", reply.body);
        reply.body["id"].as_str().expect("reference id").to_owned()
    };

    let hsn = reference(
        "hsn-codes",
        json!({ "jurisdiction": "IN", "hsnCode": "30049099", "description": "Medicaments" }),
    )
    .await;
    let category = reference(
        "tax-categories",
        json!({
            "jurisdiction": "IN", "categoryCode": "gst-12",
            "displayName": "GST 12%", "taxTreatment": "taxable"
        }),
    )
    .await;
    // Two adjoining periods, so the half-open boundary is genuinely exercised end to end.
    reference(
        "tax-rate-versions",
        json!({
            "taxCategoryId": category, "effectiveFrom": "2025-01-01", "effectiveTo": "2026-01-01",
            "cgstBasisPoints": 250, "sgstBasisPoints": 250, "igstBasisPoints": 500
        }),
    )
    .await;
    reference(
        "tax-rate-versions",
        json!({
            "taxCategoryId": category, "effectiveFrom": "2026-01-01", "effectiveTo": null,
            "cgstBasisPoints": 600, "sgstBasisPoints": 600, "igstBasisPoints": 1200
        }),
    )
    .await;

    let tablet = "01997000-0000-7000-8000-000000000001";
    let product = call(
        &service,
        "POST",
        "/api/v1/products",
        Some(json!({"product":{
            "productKind":"general_pharmacy_item","baseUnitId":tablet,
            "quantityScale":0,"displayName":"Classified item"
        }})),
        Some(&cookie),
    )
    .await;
    assert_eq!(product.status, 201, "{:?}", product.body);
    let product_id = product.body["id"].as_str().expect("product id").to_owned();
    let classification_uri = format!("/api/v1/products/{product_id}/tax-classification");

    // An unclassified Product reads normally and says so honestly.
    let initial = call(&service, "GET", &classification_uri, None, Some(&cookie)).await;
    assert_eq!(initial.status, 200, "{:?}", initial.body);
    assert_eq!(initial.body["complete"], false);
    assert_eq!(initial.body["applicableRate"], Value::Null);

    let assigned = call(
        &service,
        "PUT",
        &classification_uri,
        Some(json!({
            "expectedRevision": 1, "hsnCodeId": hsn, "taxCategoryId": category,
            "reason": "Classified from the supplier invoice"
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(assigned.status, 200, "{:?}", assigned.body);
    assert_eq!(assigned.body["complete"], true);
    assert_eq!(assigned.body["revision"], 2);

    // The classification survives a round trip through the socket.
    let read_back = call(&service, "GET", &classification_uri, None, Some(&cookie)).await;
    assert_eq!(read_back.body["hsnCodeId"], hsn.as_str());
    assert_eq!(read_back.body["taxCategoryId"], category.as_str());

    // The resolver honours half-open periods across a real boundary.
    let rate_on = async |date: &str| {
        call(
            &service,
            "GET",
            &format!("{classification_uri}?asOf={date}"),
            None,
            Some(&cookie),
        )
        .await
        .body
    };
    assert_eq!(rate_on("2024-12-31").await["applicableRate"], Value::Null);
    assert_eq!(
        rate_on("2025-06-01").await["applicableRate"]["cgstBasisPoints"],
        250
    );
    assert_eq!(
        rate_on("2025-12-31").await["applicableRate"]["cgstBasisPoints"],
        250
    );
    // The end date belongs to the next version.
    assert_eq!(
        rate_on("2026-01-01").await["applicableRate"]["cgstBasisPoints"],
        600
    );
    assert_eq!(
        rate_on("2026-01-01").await["applicableRate"]["igstBasisPoints"],
        1200
    );
    assert_eq!(rate_on("2026-01-01").await["asOf"], "2026-01-01");

    // A read-only caller may look; an anonymous one may not.
    let anonymous = call(&service, "GET", &classification_uri, None, None).await;
    assert_eq!(anonymous.status, 401);
    assert_eq!(anonymous.body["code"], "authentication_required");

    // Archiving the HSN must not retroactively break the Product, but must block reassignment.
    let archive = call(
        &service,
        "POST",
        &format!("/api/v1/reference/hsn-codes/{hsn}/archive"),
        Some(json!({ "expectedRevision": 1, "reason": "Withdrawn heading" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(archive.status, 200, "{:?}", archive.body);
    let still_readable = call(&service, "GET", &classification_uri, None, Some(&cookie)).await;
    assert_eq!(still_readable.body["hsnCodeId"], hsn.as_str());

    let second_product = call(
        &service,
        "POST",
        "/api/v1/products",
        Some(json!({"product":{
            "productKind":"general_pharmacy_item","baseUnitId":tablet,
            "quantityScale":0,"displayName":"Second item"
        }})),
        Some(&cookie),
    )
    .await;
    let second_id = second_product.body["id"].as_str().expect("id").to_owned();
    let refused = call(
        &service,
        "PUT",
        &format!("/api/v1/products/{second_id}/tax-classification"),
        Some(json!({ "expectedRevision": 1, "hsnCodeId": hsn, "taxCategoryId": category })),
        Some(&cookie),
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(refused.body["code"], "archived_conflict");

    // Real SQLite enforces the model independently of the service, and stores no rate on a Product.
    let pool = database::connect(&service.database_path)
        .await
        .expect("reopen disposable database");
    let direct = sqlx::query("UPDATE products SET hsn_code_id=? WHERE id=?")
        .bind(&hsn)
        .bind(&second_id)
        .execute(&pool)
        .await;
    assert!(direct.is_err(), "the trigger must refuse an archived HSN");

    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('products')")
        .fetch_all(&pool)
        .await
        .expect("read columns");
    for column in &columns {
        for forbidden in [
            "rate",
            "basis_points",
            "percent",
            "cgst",
            "sgst",
            "igst",
            "cess",
        ] {
            assert!(
                !column.contains(forbidden),
                "a Product identifies its classification and never stores a rate; found {column}"
            );
        }
    }
    assert!(columns.iter().any(|column| column == "hsn_code_id"));
    assert!(columns.iter().any(|column| column == "tax_category_id"));
    pool.close().await;
}

/// The Store's own tax identity across the real boundary — the Purchase prerequisite.
///
/// Proves the half of the tax-treatment comparison that Phase 1E left missing now exists, is
/// validated by the same GSTIN rules a supplier gets, and is enforced by real SQLite.
#[tokio::test]
async fn real_service_records_the_store_place_of_supply_over_http() {
    let service = start().await;
    let setup = call(
        &service,
        "POST",
        "/api/v1/auth/setup",
        Some(json!({
            "storeDisplayName": "Integration Pharmacy",
            "ownerDisplayName": "Integration Owner",
            "loginIdentifier": "integration.owner",
            "password": "Integration-Password-42"
        })),
        None,
    )
    .await;
    assert_eq!(setup.status, 201, "{:?}", setup.body);
    let cookie = session_cookie(&setup.headers);

    let maharashtra = "01997300-0000-7000-8000-000000000027";
    let karnataka = "01997300-0000-7000-8000-000000000029";
    let gstin = "27AAPFU0939F1ZV";
    let uri = "/api/v1/store/tax-identity";

    // An unauthenticated caller is refused by the real auth layer.
    let anonymous = call(&service, "GET", uri, None, None).await;
    assert_eq!(anonymous.status, 401);
    assert_eq!(anonymous.body["code"], "authentication_required");

    // A fresh installation has no place of supply, which is exactly what blocks a GST document.
    let initial = call(&service, "GET", uri, None, Some(&cookie)).await;
    assert_eq!(initial.status, 200, "{:?}", initial.body);
    assert_eq!(initial.body["complete"], false);
    assert_eq!(initial.body["placeOfSupplyStateId"], Value::Null);
    assert_eq!(initial.body["revision"], 1);

    let update = |revision: i64, status: &str, gstin: Option<&str>, state: Option<&str>| {
        json!({
            "expectedRevision": revision,
            "gstRegistrationStatus": status,
            "gstin": gstin,
            "placeOfSupplyStateId": state
        })
    };

    // A mistyped GSTIN is caught by the shared party validator's check digit.
    let bad = call(
        &service,
        "PUT",
        uri,
        Some(update(
            1,
            "registered",
            Some("27AAPFU0939F1ZX"),
            Some(maharashtra),
        )),
        Some(&cookie),
    )
    .await;
    assert_eq!(bad.status, 422, "{:?}", bad.body);
    assert_eq!(bad.body["issues"][0]["field"], "gstin");

    // A Maharashtra GSTIN cannot claim a Karnataka place of supply.
    let mismatched = call(
        &service,
        "PUT",
        uri,
        Some(update(1, "registered", Some(gstin), Some(karnataka))),
        Some(&cookie),
    )
    .await;
    assert_eq!(mismatched.status, 409, "{:?}", mismatched.body);
    assert_eq!(mismatched.body["code"], "store_tax_conflict");

    let saved = call(
        &service,
        "PUT",
        uri,
        Some(update(1, "registered", Some(gstin), Some(maharashtra))),
        Some(&cookie),
    )
    .await;
    assert_eq!(saved.status, 200, "{:?}", saved.body);
    assert_eq!(saved.body["complete"], true);
    assert_eq!(saved.body["normalizedGstin"], gstin);
    assert_eq!(saved.body["revision"], 2);

    // It survives a round trip through the socket.
    let read_back = call(&service, "GET", uri, None, Some(&cookie)).await;
    assert_eq!(read_back.body["placeOfSupplyStateId"], maharashtra);
    assert_eq!(read_back.body["complete"], true);

    // A stale revision cannot overwrite it.
    let stale = call(
        &service,
        "PUT",
        uri,
        Some(update(1, "unknown", None, None)),
        Some(&cookie),
    )
    .await;
    assert_eq!(stale.status, 409, "{:?}", stale.body);
    assert_eq!(stale.body["code"], "revision_conflict");

    let pool = database::connect(&service.database_path)
        .await
        .expect("reopen disposable database");

    // Real SQLite enforces the GSTIN/State agreement independently of the service.
    let direct = sqlx::query(
        "UPDATE store_identity SET gst_registration_status='registered',gstin=?,normalized_gstin=?,\
         place_of_supply_state_id=?",
    )
    .bind(gstin)
    .bind(gstin)
    .bind(karnataka)
    .execute(&pool)
    .await;
    assert!(direct.is_err(), "the trigger must reject a state mismatch");

    // The audit recorded the change under the frozen append-only stream.
    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM master_change_events WHERE entity_type='store_tax_identity'",
    )
    .fetch_one(&pool)
    .await
    .expect("count audit rows");
    assert_eq!(audited, 1, "the refused attempts must not have audited");
    pool.close().await;
}

// ---------------------------------------------------------------------------------------------
// Phase 1G — GST-aware purchase inward
// ---------------------------------------------------------------------------------------------

/// Everything a purchase needs before it can be posted, created over real HTTP in the order a
/// pharmacy would: the store's own registration, a supplier, a product and its pack, the product's
/// tax classification, and a rate version in force on the invoice date.
struct PurchaseWorld {
    cookie: String,
    supplier: String,
    product: String,
    pack: String,
    category: String,
}

const MAHARASHTRA: &str = "01997300-0000-7000-8000-000000000027";
const KARNATAKA: &str = "01997300-0000-7000-8000-000000000029";
/// Genuine check digits, so the real GSTIN validator is exercised rather than bypassed.
const STORE_GSTIN: &str = "27AAPFU0939F1ZV";
const TABLET: &str = "01997000-0000-7000-8000-000000000001";
const STRIP: &str = "01997000-0000-7000-8000-000000000004";

async fn owner_cookie(service: &Service) -> String {
    let setup = call(
        service,
        "POST",
        "/api/v1/auth/setup",
        Some(json!({
            "storeDisplayName": "Integration Pharmacy",
            "ownerDisplayName": "Integration Owner",
            "loginIdentifier": "integration.owner",
            "password": "Integration-Password-42"
        })),
        None,
    )
    .await;
    assert_eq!(setup.status, 201, "{:?}", setup.body);
    session_cookie(&setup.headers)
}

/// Creates a supplier whose GSTIN really belongs to `state`, so the treatment comparison is made
/// from persisted facts rather than from a hand-written state code.
async fn create_supplier(service: &Service, cookie: &str, name: &str, state: &str) -> String {
    let gstin = gstin_for(state);
    let reply = call(
        service,
        "POST",
        "/api/v1/parties",
        Some(json!({
            "party": {
                "displayName": name, "gstRegistrationStatus": "registered",
                "gstin": gstin, "placeOfSupplyStateId": state
            },
            "roles": [{ "role": "supplier" }],
            "addresses": []
        })),
        Some(cookie),
    )
    .await;
    assert_eq!(reply.status, 201, "{:?}", reply.body);
    reply.body["id"].as_str().expect("supplier id").to_owned()
}

/// A valid GSTIN for one of the two States these tests use. The check digit is computed rather than
/// hardcoded so the real validator cannot be satisfied by a lucky literal.
fn gstin_for(state: &str) -> String {
    let code = if state == KARNATAKA { "29" } else { "27" };
    let body = format!("{code}AAACS1234A1Z");
    let alphabet: Vec<char> = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ".chars().collect();
    let mut sum = 0usize;
    for (index, character) in body.chars().enumerate() {
        let value = alphabet
            .iter()
            .position(|item| *item == character)
            .expect("gstin alphabet");
        let factor = if index % 2 == 0 { 1 } else { 2 };
        let product = value * factor;
        sum += product / 36 + product % 36;
    }
    let checksum = (36 - (sum % 36)) % 36;
    format!("{body}{}", alphabet[checksum])
}

async fn seed_purchase_world(
    service: &Service,
    supplier_state: &str,
    treatment: &str,
) -> PurchaseWorld {
    let cookie = owner_cookie(service).await;

    // The store's own place of supply — one half of the treatment decision.
    let store = call(
        service,
        "PUT",
        "/api/v1/store/tax-identity",
        Some(json!({
            "expectedRevision": 1, "gstRegistrationStatus": "registered",
            "gstin": STORE_GSTIN, "placeOfSupplyStateId": MAHARASHTRA
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(store.status, 200, "{:?}", store.body);

    let supplier_id = create_supplier(service, &cookie, "Sharma Medicals", supplier_state).await;

    let reference = async |kind: &str, attributes: Value| {
        let reply = call(
            service,
            "POST",
            &format!("/api/v1/reference/{kind}"),
            Some(json!({ "attributes": attributes })),
            Some(&cookie),
        )
        .await;
        assert_eq!(reply.status, 201, "{kind}: {:?}", reply.body);
        reply.body["id"].as_str().expect("reference id").to_owned()
    };

    let hsn = reference(
        "hsn-codes",
        json!({ "jurisdiction": "IN", "hsnCode": "30049099", "description": "Medicaments" }),
    )
    .await;
    let category = reference(
        "tax-categories",
        json!({
            "jurisdiction": "IN", "categoryCode": "gst-12",
            "displayName": "GST 12%", "taxTreatment": treatment
        }),
    )
    .await;
    // A rate in force on the invoice date these tests use. Exempt, nil-rated and non-GST
    // categories carry a zero-component version, which is a different thing from having no rate.
    let taxable = treatment == "taxable";
    reference(
        "tax-rate-versions",
        json!({
            "taxCategoryId": category, "effectiveFrom": "2026-01-01", "effectiveTo": null,
            "cgstBasisPoints": if taxable { 600 } else { 0 },
            "sgstBasisPoints": if taxable { 600 } else { 0 },
            "igstBasisPoints": if taxable { 1200 } else { 0 }
        }),
    )
    .await;

    let product = call(
        service,
        "POST",
        "/api/v1/products",
        Some(json!({"product":{
            "productKind":"general_pharmacy_item","baseUnitId":TABLET,
            "quantityScale":0,"displayName":"Crocin 500 mg Tablet"
        }})),
        Some(&cookie),
    )
    .await;
    assert_eq!(product.status, 201, "{:?}", product.body);
    let product_id = product.body["id"].as_str().expect("product id").to_owned();

    let pack = call(
        service,
        "POST",
        &format!("/api/v1/products/{product_id}/packs"),
        Some(json!({
            "containerUnitId": STRIP, "baseQuantityAtoms": 10, "displayLabel": "Strip of 10"
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(pack.status, 201, "{:?}", pack.body);
    let pack_id = pack.body["id"].as_str().expect("pack id").to_owned();

    let classified = call(
        service,
        "PUT",
        &format!("/api/v1/products/{product_id}/tax-classification"),
        Some(json!({ "expectedRevision": 1, "hsnCodeId": hsn, "taxCategoryId": category })),
        Some(&cookie),
    )
    .await;
    assert_eq!(classified.status, 200, "{:?}", classified.body);

    PurchaseWorld {
        cookie,
        supplier: supplier_id,
        product: product_id,
        pack: pack_id,
        category,
    }
}

async fn create_draft(service: &Service, world: &PurchaseWorld, invoice: &str) -> Value {
    let draft = call(
        service,
        "POST",
        "/api/v1/purchases",
        Some(json!({
            "supplierPartyId": world.supplier,
            "supplierInvoiceNumber": invoice,
            "invoiceDate": "2026-09-10"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(draft.status, 201, "{:?}", draft.body);
    draft.body
}

/// The whole Phase 1G workflow across a real socket: a draft becomes a posted GST document, a
/// proposed lot becomes a real batch, and the ledger gains exactly one inward movement.
#[tokio::test]
async fn real_service_posts_a_gst_purchase_and_moves_stock_over_http() {
    let service = start().await;
    let world = seed_purchase_world(&service, MAHARASHTRA, "taxable").await;

    let draft = create_draft(&service, &world, "INV-4471").await;
    let purchase_id = draft["id"].as_str().expect("purchase id").to_owned();
    assert_eq!(draft["status"], "draft");
    assert_eq!(draft["taxTreatment"], Value::Null);
    assert_eq!(draft["grandTotalPaise"], 0);

    // One line proposing a batch that does not exist yet.
    let with_line = call(
        &service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/lines"),
        Some(json!({
            "expectedRevision": 1,
            "productId": world.product,
            "productPackId": world.pack,
            "newBatchNumber": "B-2601",
            "newBatchExpiresOn": "2028-03-31",
            "newBatchMrpPaise": 4500,
            "quantityPacks": 10,
            "ratePerPackPaise": 3000
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);
    assert_eq!(with_line.body["lines"][0]["taxableValuePaise"], 30_000);
    assert_eq!(with_line.body["lines"][0]["quantityAtoms"], 100);
    // A draft line carries no tax at all; it is resolved only at posting.
    assert_eq!(with_line.body["lines"][0]["cgstPaise"], 0);
    assert_eq!(with_line.body["lines"][0]["taxRateVersionId"], Value::Null);
    let revision = with_line.body["revision"].as_i64().expect("revision");

    let key = "01997a00-0000-7000-8000-0000000000a1";
    let posted = call(
        &service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/post"),
        Some(json!({ "expectedRevision": revision, "idempotencyKey": key })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    assert_eq!(posted.body["status"], "posted");

    // The snapshots and the treatment, refetched rather than trusted from the write response.
    let detail = call(
        &service,
        "GET",
        &format!("/api/v1/purchases/{purchase_id}"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(detail.status, 200, "{:?}", detail.body);
    assert_eq!(detail.body["status"], "posted");
    assert_eq!(detail.body["supplierDisplayName"], "Sharma Medicals");
    assert_eq!(
        detail.body["supplierNormalizedGstin"],
        gstin_for(MAHARASHTRA)
    );
    assert_eq!(detail.body["supplierStateCode"], "27");
    assert_eq!(detail.body["storeNormalizedGstin"], STORE_GSTIN);
    assert_eq!(detail.body["storeStateCode"], "27");
    // Both places of supply are Maharashtra, so the tax splits.
    assert_eq!(detail.body["taxTreatment"], "intra_state");
    assert_eq!(detail.body["taxableValuePaise"], 30_000);
    assert_eq!(detail.body["cgstPaise"], 1_800);
    assert_eq!(detail.body["sgstPaise"], 1_800);
    assert_eq!(detail.body["igstPaise"], 0);
    assert_eq!(detail.body["grandTotalPaise"], 33_600);
    assert!(detail.body["postedAtUtc"].is_string());

    let line = &detail.body["lines"][0];
    assert_eq!(line["hsnCode"], "30049099");
    assert_eq!(line["taxTreatmentKind"], "taxable");
    assert_eq!(line["cgstBasisPoints"], 600);
    assert_eq!(line["igstBasisPoints"], 1_200);
    assert_eq!(line["lineTotalPaise"], 33_600);
    // The proposal became a real lot, and the scaffolding was cleared.
    let batch_id = line["batchId"]
        .as_str()
        .expect("materialised batch")
        .to_owned();
    assert_eq!(line["newBatchNumber"], Value::Null);
    assert_eq!(line["newBatchExpiresOn"], Value::Null);
    assert_eq!(line["newBatchMrpPaise"], Value::Null);

    let batches = call(
        &service,
        "GET",
        &format!("/api/v1/packs/{}/batches", world.pack),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(batches.status, 200, "{:?}", batches.body);
    assert_eq!(batches.body.as_array().expect("batches").len(), 1);
    assert_eq!(batches.body[0]["id"], batch_id.as_str());
    assert_eq!(batches.body[0]["batchNumber"], "B-2601");
    assert_eq!(batches.body[0]["expiresOn"], "2028-03-31");
    assert_eq!(batches.body[0]["mrpPaise"], 4_500);

    // The ledger, with provenance back to the line that caused it.
    let movements = call(
        &service,
        "GET",
        "/api/v1/inventory/movements",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(movements.status, 200, "{:?}", movements.body);
    let rows = movements.body.as_array().expect("movements");
    assert_eq!(rows.len(), 1, "one line must post exactly one movement");
    assert_eq!(rows[0]["movementType"], "purchase");
    assert_eq!(rows[0]["quantityDeltaAtoms"], 100);
    assert_eq!(rows[0]["batchId"], batch_id.as_str());
    assert_eq!(rows[0]["purchaseLineId"], line["id"]);

    // The balance is still derived by summing the ledger, never stored on the pack.
    let stock = call(
        &service,
        "GET",
        "/api/v1/inventory/stock",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(stock.status, 200, "{:?}", stock.body);
    assert_eq!(stock.body.as_array().expect("balances").len(), 1);
    assert_eq!(stock.body[0]["balanceAtoms"], 100);
    assert_eq!(stock.body[0]["productPackId"], world.pack.as_str());
}

/// A supplier in another State is charged IGST alone. The treatment is never taken from the
/// request; it comes from the two persisted State codes.
#[tokio::test]
async fn real_service_charges_igst_for_an_interstate_purchase_over_http() {
    let service = start().await;
    let world = seed_purchase_world(&service, KARNATAKA, "taxable").await;

    let draft = create_draft(&service, &world, "KL-77").await;
    let purchase_id = draft["id"].as_str().expect("purchase id").to_owned();
    let with_line = call(
        &service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/lines"),
        Some(json!({
            "expectedRevision": 1, "productId": world.product, "productPackId": world.pack,
            "newBatchNumber": "K-9001", "quantityPacks": 10, "ratePerPackPaise": 3000
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);

    let posted = call(
        &service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/post"),
        Some(json!({
            "expectedRevision": with_line.body["revision"],
            "idempotencyKey": "01997a00-0000-7000-8000-0000000000b1"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    assert_eq!(posted.body["taxTreatment"], "inter_state");
    assert_eq!(posted.body["storeStateCode"], "27");
    assert_eq!(posted.body["supplierStateCode"], "29");
    assert_eq!(posted.body["igstPaise"], 3_600);
    assert_eq!(posted.body["cgstPaise"], 0);
    assert_eq!(posted.body["sgstPaise"], 0);
    assert_eq!(posted.body["grandTotalPaise"], 33_600);
    // The line snapshots the rate version as published — the 12% slab is 6 + 6 or 12 — while the
    // amounts record what was actually charged. A CGST rate beside a zero CGST amount is the
    // honest reading of "this slab, charged as IGST because the States differ".
    assert_eq!(posted.body["lines"][0]["igstBasisPoints"], 1_200);
    assert_eq!(posted.body["lines"][0]["igstPaise"], 3_600);
    assert_eq!(posted.body["lines"][0]["cgstPaise"], 0);
    assert_eq!(posted.body["lines"][0]["sgstPaise"], 0);
}

/// Replaying one logical posting must not post the invoice twice, create a second lot, or move the
/// stock again — across the real transport, not a doubled service.
#[tokio::test]
async fn real_service_replays_a_posting_without_duplicating_its_effects_over_http() {
    let service = start().await;
    let world = seed_purchase_world(&service, MAHARASHTRA, "taxable").await;

    let draft = create_draft(&service, &world, "INV-9001").await;
    let purchase_id = draft["id"].as_str().expect("purchase id").to_owned();
    let with_line = call(
        &service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/lines"),
        Some(json!({
            "expectedRevision": 1, "productId": world.product, "productPackId": world.pack,
            "newBatchNumber": "B-7001", "quantityPacks": 4, "ratePerPackPaise": 2500
        })),
        Some(&world.cookie),
    )
    .await;
    let revision = with_line.body["revision"].as_i64().expect("revision");
    let key = "01997a00-0000-7000-8000-0000000000c1";
    let posting = json!({ "expectedRevision": revision, "idempotencyKey": key });

    let first = call(
        &service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/post"),
        Some(posting.clone()),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(first.status, 200, "{:?}", first.body);
    assert_eq!(first.body["status"], "posted");

    // The same key and the same facts: the original document comes back.
    let replay = call(
        &service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/post"),
        Some(posting),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(replay.status, 200, "{:?}", replay.body);
    assert_eq!(replay.body["id"], first.body["id"]);
    assert_eq!(replay.body["revision"], first.body["revision"]);
    assert_eq!(replay.body["postedAtUtc"], first.body["postedAtUtc"]);
    assert_eq!(
        replay.body["grandTotalPaise"],
        first.body["grandTotalPaise"]
    );

    // Nothing happened twice.
    let movements = call(
        &service,
        "GET",
        "/api/v1/inventory/movements",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(
        movements.body.as_array().expect("movements").len(),
        1,
        "the replay must not have moved stock again"
    );
    let batches = call(
        &service,
        "GET",
        &format!("/api/v1/packs/{}/batches", world.pack),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(
        batches.body.as_array().expect("batches").len(),
        1,
        "the replay must not have created a second lot"
    );
    let stock = call(
        &service,
        "GET",
        "/api/v1/inventory/stock",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(stock.body[0]["balanceAtoms"], 40);

    // A posted document refuses further change regardless of the key.
    let again = call(
        &service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/post"),
        Some(json!({
            "expectedRevision": revision,
            "idempotencyKey": "01997a00-0000-7000-8000-0000000000c2"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(again.status, 409, "{:?}", again.body);
    assert_eq!(again.body["code"], "purchase_not_draft");
}

/// The same posting key offered for a different invoice is a conflict about the key, and must say
/// so — telling the operator to check the invoice number would send them to the wrong problem.
#[tokio::test]
async fn real_service_refuses_a_reused_posting_key_on_another_invoice_over_http() {
    let service = start().await;
    let world = seed_purchase_world(&service, MAHARASHTRA, "taxable").await;
    let key = "01997a00-0000-7000-8000-0000000000d1";

    let post_one = async |invoice: &str, batch: &str| {
        let draft = create_draft(&service, &world, invoice).await;
        let purchase_id = draft["id"].as_str().expect("purchase id").to_owned();
        let with_line = call(
            &service,
            "POST",
            &format!("/api/v1/purchases/{purchase_id}/lines"),
            Some(json!({
                "expectedRevision": 1, "productId": world.product, "productPackId": world.pack,
                "newBatchNumber": batch, "quantityPacks": 2, "ratePerPackPaise": 1000
            })),
            Some(&world.cookie),
        )
        .await;
        assert_eq!(with_line.status, 201, "{:?}", with_line.body);
        call(
            &service,
            "POST",
            &format!("/api/v1/purchases/{purchase_id}/post"),
            Some(json!({
                "expectedRevision": with_line.body["revision"], "idempotencyKey": key
            })),
            Some(&world.cookie),
        )
        .await
    };

    let first = post_one("INV-A", "B-A1").await;
    assert_eq!(first.status, 200, "{:?}", first.body);

    let second = post_one("INV-B", "B-B1").await;
    assert_eq!(second.status, 409, "{:?}", second.body);
    assert_eq!(
        second.body["code"], "idempotency_conflict",
        "a reused posting key is a key conflict, not a duplicate invoice number"
    );

    // The refused posting left nothing behind.
    let movements = call(
        &service,
        "GET",
        "/api/v1/inventory/movements",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(movements.body.as_array().expect("movements").len(), 1);
}

/// A posting that fails partway must leave nothing: no lot from the line that succeeded, no
/// movement, no snapshot, and the document still a draft.
///
/// The failure is produced by a real constraint rather than a test-only hook: two lines on the same
/// pack propose the same batch number, so the second materialisation collides with the first inside
/// the same transaction.
#[tokio::test]
async fn real_service_leaves_no_residue_when_a_posting_fails_over_http() {
    let service = start().await;
    let world = seed_purchase_world(&service, MAHARASHTRA, "taxable").await;

    let draft = create_draft(&service, &world, "INV-ROLLBACK").await;
    let purchase_id = draft["id"].as_str().expect("purchase id").to_owned();

    let mut revision = 1;
    for _ in 0..2 {
        let reply = call(
            &service,
            "POST",
            &format!("/api/v1/purchases/{purchase_id}/lines"),
            Some(json!({
                "expectedRevision": revision, "productId": world.product,
                "productPackId": world.pack, "newBatchNumber": "B-COLLIDE",
                "quantityPacks": 3, "ratePerPackPaise": 2000
            })),
            Some(&world.cookie),
        )
        .await;
        assert_eq!(reply.status, 201, "{:?}", reply.body);
        revision = reply.body["revision"].as_i64().expect("revision");
    }

    let failed = call(
        &service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/post"),
        Some(json!({
            "expectedRevision": revision,
            "idempotencyKey": "01997a00-0000-7000-8000-0000000000e1"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(failed.status, 409, "{:?}", failed.body);
    assert_eq!(failed.body["code"], "batch_conflict");

    // The document is untouched.
    let detail = call(
        &service,
        "GET",
        &format!("/api/v1/purchases/{purchase_id}"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(detail.body["status"], "draft");
    assert_eq!(detail.body["revision"], revision);
    assert_eq!(detail.body["taxTreatment"], Value::Null);
    assert_eq!(detail.body["grandTotalPaise"], 0);
    assert_eq!(detail.body["postedAtUtc"], Value::Null);
    for line in detail.body["lines"].as_array().expect("lines") {
        assert_eq!(line["batchId"], Value::Null, "no line may hold a lot");
        assert_eq!(line["newBatchNumber"], "B-COLLIDE");
        assert_eq!(line["taxRateVersionId"], Value::Null);
        assert_eq!(line["lineTotalPaise"], 0);
    }

    // The first line's lot was rolled back with everything else.
    let batches = call(
        &service,
        "GET",
        &format!("/api/v1/packs/{}/batches", world.pack),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(
        batches.body.as_array().expect("batches").len(),
        0,
        "a failed posting must not leave an orphan lot"
    );

    let movements = call(
        &service,
        "GET",
        "/api/v1/inventory/movements",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(
        movements.body.as_array().expect("movements").len(),
        0,
        "a failed posting must not move stock"
    );
    let stock = call(
        &service,
        "GET",
        "/api/v1/inventory/stock",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(stock.body.as_array().expect("balances").len(), 0);
}

/// The typed failure matrix across the real transport. Every refusal must carry a safe code and a
/// safe message: no SQLite text, no SQL fragment, no Rust type name.
#[tokio::test]
async fn real_service_refuses_ineligible_purchases_with_safe_codes_over_http() {
    let service = start().await;
    let world = seed_purchase_world(&service, MAHARASHTRA, "taxable").await;

    let assert_safe = |reply: &Reply, code: &str| {
        assert_eq!(reply.body["code"], code, "{:?}", reply.body);
        let message = reply.body["message"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        for leak in [
            "sqlite",
            "constraint failed",
            "select ",
            "insert ",
            "update ",
            "rusqlite",
            "sqlx",
            "panicked",
            "purchaseerror",
            "unwrap",
            "no such column",
        ] {
            assert!(!message.contains(leak), "{code} leaked {leak:?}: {message}");
        }
    };

    // An anonymous caller never reaches the purchase API at all.
    let anonymous = call(&service, "GET", "/api/v1/purchases", None, None).await;
    assert_eq!(anonymous.status, 401);
    assert_safe(&anonymous, "authentication_required");

    let draft = create_draft(&service, &world, "INV-MATRIX").await;
    let purchase_id = draft["id"].as_str().expect("purchase id").to_owned();

    // The same supplier cannot be invoiced twice under the same number.
    let duplicate = call(
        &service,
        "POST",
        "/api/v1/purchases",
        Some(json!({
            "supplierPartyId": world.supplier,
            "supplierInvoiceNumber": "  inv-matrix  ",
            "invoiceDate": "2026-09-11"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(duplicate.status, 409, "{:?}", duplicate.body);
    assert_safe(&duplicate, "duplicate_supplier_invoice");

    // Punctuation is part of the number: these are two different invoices, not one.
    for invoice in ["INV/2026/001", "INV-2026-001"] {
        let distinct = call(
            &service,
            "POST",
            "/api/v1/purchases",
            Some(json!({
                "supplierPartyId": world.supplier,
                "supplierInvoiceNumber": invoice,
                "invoiceDate": "2026-09-11"
            })),
            Some(&world.cookie),
        )
        .await;
        assert_eq!(
            distinct.status, 201,
            "{invoice} must be its own document: {:?}",
            distinct.body
        );
    }

    // A stale revision is refused with the current one, never applied.
    let stale = call(
        &service,
        "PUT",
        &format!("/api/v1/purchases/{purchase_id}"),
        Some(json!({
            "expectedRevision": 99, "supplierPartyId": world.supplier,
            "supplierInvoiceNumber": "INV-MATRIX", "invoiceDate": "2026-09-12"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(stale.status, 409, "{:?}", stale.body);
    assert_safe(&stale, "revision_conflict");
    assert_eq!(stale.body["currentRevision"], 1);

    // A pack that belongs to another product cannot be put on a line.
    let other = call(
        &service,
        "POST",
        "/api/v1/products",
        Some(json!({"product":{
            "productKind":"general_pharmacy_item","baseUnitId":TABLET,
            "quantityScale":0,"displayName":"Unrelated item"
        }})),
        Some(&world.cookie),
    )
    .await;
    let other_id = other.body["id"].as_str().expect("product id").to_owned();
    let mismatch = call(
        &service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/lines"),
        Some(json!({
            "expectedRevision": 1, "productId": other_id, "productPackId": world.pack,
            "newBatchNumber": "B-X", "quantityPacks": 1, "ratePerPackPaise": 100
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(mismatch.status, 409, "{:?}", mismatch.body);
    assert_safe(&mismatch, "product_pack_mismatch");

    // A line for a product with no classification cannot be priced for tax.
    let unclassified_pack = call(
        &service,
        "POST",
        &format!("/api/v1/products/{other_id}/packs"),
        Some(json!({"containerUnitId": STRIP, "baseQuantityAtoms": 10})),
        Some(&world.cookie),
    )
    .await;
    let unclassified_pack_id = unclassified_pack.body["id"]
        .as_str()
        .expect("pack id")
        .to_owned();
    let added = call(
        &service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/lines"),
        Some(json!({
            "expectedRevision": 1, "productId": other_id, "productPackId": unclassified_pack_id,
            "newBatchNumber": "B-Y", "quantityPacks": 1, "ratePerPackPaise": 100
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(added.status, 201, "{:?}", added.body);
    let unclassified_posting = call(
        &service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/post"),
        Some(json!({
            "expectedRevision": added.body["revision"],
            "idempotencyKey": "01997a00-0000-7000-8000-0000000000f1"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(
        unclassified_posting.status, 409,
        "{:?}",
        unclassified_posting.body
    );
    assert_safe(
        &unclassified_posting,
        "product_tax_classification_incomplete",
    );

    // A classified product with no rate in force on the invoice date is a different refusal
    // entirely, and must never be treated as a zero rate.
    let dated = call(
        &service,
        "PUT",
        &format!("/api/v1/purchases/{purchase_id}"),
        Some(json!({
            "expectedRevision": added.body["revision"], "supplierPartyId": world.supplier,
            "supplierInvoiceNumber": "INV-MATRIX", "invoiceDate": "2025-06-01"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(dated.status, 200, "{:?}", dated.body);
    let classified = call(
        &service,
        "PUT",
        &format!("/api/v1/products/{other_id}/tax-classification"),
        Some(json!({ "expectedRevision": 1, "taxCategoryId": world.category })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(classified.status, 200, "{:?}", classified.body);
    let before_rate = call(
        &service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/post"),
        Some(json!({
            "expectedRevision": dated.body["revision"],
            "idempotencyKey": "01997a00-0000-7000-8000-0000000000f2"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(before_rate.status, 409, "{:?}", before_rate.body);
    assert_safe(&before_rate, "tax_rate_not_found");
}

/// A purchase cannot be posted while either place of supply is unknown. Guessing "same State"
/// would silently invent CGST + SGST on a document that might be interstate.
#[tokio::test]
async fn real_service_refuses_to_post_without_both_places_of_supply_over_http() {
    let service = start().await;
    let cookie = owner_cookie(&service).await;

    // A supplier with no place of supply at all.
    let supplier = call(
        &service,
        "POST",
        "/api/v1/parties",
        Some(json!({
            "party": { "displayName": "Unregistered Trader", "gstRegistrationStatus": "unregistered" },
            "roles": [{ "role": "supplier" }],
            "addresses": []
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(supplier.status, 201, "{:?}", supplier.body);
    let supplier_id = supplier.body["id"]
        .as_str()
        .expect("supplier id")
        .to_owned();

    let product = call(
        &service,
        "POST",
        "/api/v1/products",
        Some(json!({"product":{
            "productKind":"general_pharmacy_item","baseUnitId":TABLET,
            "quantityScale":0,"displayName":"Anything"
        }})),
        Some(&cookie),
    )
    .await;
    let product_id = product.body["id"].as_str().expect("product id").to_owned();
    let pack = call(
        &service,
        "POST",
        &format!("/api/v1/products/{product_id}/packs"),
        Some(json!({"containerUnitId": STRIP, "baseQuantityAtoms": 10})),
        Some(&cookie),
    )
    .await;
    let pack_id = pack.body["id"].as_str().expect("pack id").to_owned();

    let draft = call(
        &service,
        "POST",
        "/api/v1/purchases",
        Some(json!({
            "supplierPartyId": supplier_id, "supplierInvoiceNumber": "NO-STATE",
            "invoiceDate": "2026-09-10"
        })),
        Some(&cookie),
    )
    .await;
    let purchase_id = draft.body["id"].as_str().expect("purchase id").to_owned();
    let with_line = call(
        &service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/lines"),
        Some(json!({
            "expectedRevision": 1, "productId": product_id, "productPackId": pack_id,
            "newBatchNumber": "B-1", "quantityPacks": 1, "ratePerPackPaise": 100
        })),
        Some(&cookie),
    )
    .await;
    let revision = with_line.body["revision"].as_i64().expect("revision");

    // The store has no place of supply yet, so that is the first thing missing.
    let no_store = call(
        &service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/post"),
        Some(json!({
            "expectedRevision": revision,
            "idempotencyKey": "01997a00-0000-7000-8000-000000000101"
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(no_store.status, 409, "{:?}", no_store.body);
    assert_eq!(no_store.body["code"], "store_tax_profile_incomplete");

    // With the store recorded, the supplier's missing State is now what blocks it.
    let store = call(
        &service,
        "PUT",
        "/api/v1/store/tax-identity",
        Some(json!({
            "expectedRevision": 1, "gstRegistrationStatus": "registered",
            "gstin": STORE_GSTIN, "placeOfSupplyStateId": MAHARASHTRA
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(store.status, 200, "{:?}", store.body);
    let no_supplier = call(
        &service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/post"),
        Some(json!({
            "expectedRevision": revision,
            "idempotencyKey": "01997a00-0000-7000-8000-000000000102"
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(no_supplier.status, 409, "{:?}", no_supplier.body);
    assert_eq!(no_supplier.body["code"], "supplier_tax_profile_incomplete");

    // Nothing was posted by either refusal.
    let detail = call(
        &service,
        "GET",
        &format!("/api/v1/purchases/{purchase_id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(detail.body["status"], "draft");
    let movements = call(
        &service,
        "GET",
        "/api/v1/inventory/movements",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(movements.body.as_array().expect("movements").len(), 0);
}

/// Purchase entry is an Owner/Admin action. A reader may look; a reader may not write.
#[tokio::test]
async fn real_service_denies_purchase_mutation_to_a_non_admin_over_http() {
    let service = start().await;
    let world = seed_purchase_world(&service, MAHARASHTRA, "taxable").await;
    let draft = create_draft(&service, &world, "INV-ROLE").await;
    let purchase_id = draft["id"].as_str().expect("purchase id").to_owned();

    // There is no user-management API yet, so the reader is seeded directly into the disposable
    // database. Everything under test — the login, the session, the role check — still happens
    // over the real transport against the real service.
    let pool = database::connect(&service.database_path)
        .await
        .expect("reopen disposable database");
    let owner_hash: String = sqlx::query_scalar("SELECT password_hash FROM users LIMIT 1")
        .fetch_one(&pool)
        .await
        .expect("owner hash");
    sqlx::query(
        "INSERT INTO users (id,login_identifier,normalized_login_identifier,display_name,\
         password_hash,role,created_at_utc,updated_at_utc) \
         VALUES (?,?,?,?,?, 'pharmacist', strftime('%Y-%m-%dT%H:%M:%fZ','now'), \
         strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
    )
    .bind("01997a00-0000-7000-8000-000000000201")
    .bind("counter.pharmacist")
    .bind("counter.pharmacist")
    .bind("Counter Pharmacist")
    // The same verifier as the owner, so the real Argon2 path is exercised by the login below.
    .bind(&owner_hash)
    .execute(&pool)
    .await
    .expect("seed a reader");
    pool.close().await;

    let login = call(
        &service,
        "POST",
        "/api/v1/auth/login",
        Some(json!({
            "loginIdentifier": "counter.pharmacist", "password": "Integration-Password-42"
        })),
        None,
    )
    .await;
    assert_eq!(login.status, 200, "{:?}", login.body);
    let reader = session_cookie(&login.headers);

    // Reading is allowed.
    let list = call(&service, "GET", "/api/v1/purchases", None, Some(&reader)).await;
    assert_eq!(list.status, 200, "{:?}", list.body);

    // Writing is not, at every mutating entry point.
    let attempts = [
        (
            "POST",
            "/api/v1/purchases".to_owned(),
            json!({
                "supplierPartyId": world.supplier, "supplierInvoiceNumber": "SNEAK",
                "invoiceDate": "2026-09-10"
            }),
        ),
        (
            "POST",
            format!("/api/v1/purchases/{purchase_id}/lines"),
            json!({
                "expectedRevision": 1, "productId": world.product, "productPackId": world.pack,
                "newBatchNumber": "B-Z", "quantityPacks": 1, "ratePerPackPaise": 100
            }),
        ),
        (
            "POST",
            format!("/api/v1/purchases/{purchase_id}/post"),
            json!({
                "expectedRevision": 1, "idempotencyKey": "01997a00-0000-7000-8000-000000000111"
            }),
        ),
    ];
    for (method, path, body) in attempts {
        let denied = call(&service, method, &path, Some(body), Some(&reader)).await;
        assert_eq!(denied.status, 403, "{method} {path}: {:?}", denied.body);
        assert_eq!(denied.body["code"], "authorization_denied");
    }

    // And nothing leaked through.
    let detail = call(
        &service,
        "GET",
        &format!("/api/v1/purchases/{purchase_id}"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(detail.body["status"], "draft");
    assert_eq!(detail.body["lines"].as_array().expect("lines").len(), 0);
}
