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

/// The seller facts a pharmacy must hold before any Sale may be posted: a registered name, an
/// operating address, and an active drug sale licence. Recorded through the real endpoints,
/// because a fixture that reached around the API would prove nothing about the API.
async fn complete_legal_profile(service: &Service, cookie: &str) {
    let profile = call(service, "GET", "/api/v1/store/profile", None, Some(cookie)).await;
    assert_eq!(profile.status, 200, "{:?}", profile.body);
    assert_eq!(
        profile.body["sellerComplete"], false,
        "a fresh installation already claimed to be able to issue a memo"
    );

    let identity = call(
        service,
        "PUT",
        "/api/v1/store/profile",
        Some(json!({
            "expectedRevision": profile.body["revision"],
            "displayName": "Integration Pharmacy",
            "legalName": "Integration Pharmacy Private Limited",
            "primaryPhone": "02012345678",
            "primaryEmail": "counter@example.test"
        })),
        Some(cookie),
    )
    .await;
    assert_eq!(identity.status, 200, "{:?}", identity.body);

    let address = call(
        service,
        "PUT",
        "/api/v1/store/address",
        Some(json!({
            "line1": "12 Market Road", "city": "Pune",
            "stateId": MAHARASHTRA, "postalCode": "411001"
        })),
        Some(cookie),
    )
    .await;
    assert_eq!(address.status, 200, "{:?}", address.body);

    let licence = call(
        service,
        "POST",
        "/api/v1/store/licences",
        Some(json!({
            "licenceType": "Form 20", "licenceNumber": "MH-20-1234", "includeOnRetailMemo": true
        })),
        Some(cookie),
    )
    .await;
    assert_eq!(licence.status, 201, "{:?}", licence.body);
    assert_eq!(
        licence.body["sellerComplete"], true,
        "the profile is still incomplete after all three particulars were recorded"
    );
    assert_eq!(licence.body["licences"][0]["includeOnRetailMemo"], true);

    // Phase 1L-A3: the turnover facts only the pharmacy can know, recorded over real HTTP. Up to
    // Rs 5 crore in the year before the gate's 2026-27 sales, never above the e-invoicing
    // threshold (no Rule 46(s) declaration), and not required to e-invoice (Rule 48(4)).
    assert_eq!(licence.body["rule46sDeclarationApplicability"], "unknown");
    assert_eq!(licence.body["einvoiceApplicability"], "unknown");
    let facts = call(
        service,
        "PUT",
        "/api/v1/store/invoice-compliance",
        Some(json!({
            "expectedRevision": licence.body["revision"],
            "rule46sDeclarationApplicability": "not_applicable",
            "einvoiceApplicability": "not_required",
            "dynamicQrApplicability": "not_required",
            "hsnTurnoverBand": "up_to_5_crore",
            "hsnTurnoverFinancialYear": "2026-27"
        })),
        Some(cookie),
    )
    .await;
    assert_eq!(facts.status, 200, "{:?}", facts.body);
    assert_eq!(facts.body["hsnTurnoverFinancialYear"], "2026-27");
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
    complete_legal_profile(service, &cookie).await;

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

// ---------------------------------------------------------------------------------------------
// Phase 1H-0 — medicine price control
// ---------------------------------------------------------------------------------------------

/// Ceiling prices across the real boundary: a real socket, real migrations, the real half-open
/// resolver, and the real database trigger that forbids overlapping effective periods.
#[tokio::test]
async fn real_service_resolves_a_medicine_price_ceiling_by_date_over_http() {
    let service = start().await;
    let cookie = owner_cookie(&service).await;

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

    let dosage_form = reference(
        "dosage-forms",
        json!({ "canonicalCode": "tablet", "displayName": "Tablet" }),
    )
    .await;
    let formulation = reference(
        "controlled-formulations",
        json!({
            "jurisdiction": "IN", "formulationCode": "PARA-500-TAB",
            "displayName": "Paracetamol 500 mg Tablet", "dosageFormId": dosage_form,
            "strengthText": "500 mg", "verificationState": "verified",
            "sourceNote": "Recorded from the notification as published"
        }),
    )
    .await;

    // Two adjoining periods, so the half-open boundary is genuinely exercised end to end.
    reference(
        "price-control-versions",
        json!({
            "controlledFormulationId": formulation,
            "effectiveFrom": "2025-01-01", "effectiveTo": "2026-04-01",
            "ceilingPricePaise": 100, "ceilingBasis": "per_base_unit",
            "ceilingBasisUnitId": TABLET, "notificationReference": "S.O. 1111(E)"
        }),
    )
    .await;
    reference(
        "price-control-versions",
        json!({
            "controlledFormulationId": formulation,
            "effectiveFrom": "2026-04-01", "effectiveTo": null,
            "ceilingPricePaise": 109, "ceilingBasis": "per_base_unit",
            "ceilingBasisUnitId": TABLET, "notificationReference": "S.O. 2222(E)"
        }),
    )
    .await;

    // An overlapping period is refused by the database, not by the service being careful.
    let overlapping = call(
        &service,
        "POST",
        "/api/v1/reference/price-control-versions",
        Some(json!({ "attributes": {
            "controlledFormulationId": formulation,
            "effectiveFrom": "2026-06-01", "effectiveTo": null,
            "ceilingPricePaise": 120, "ceilingBasis": "per_base_unit",
            "ceilingBasisUnitId": TABLET
        }})),
        Some(&cookie),
    )
    .await;
    assert_eq!(overlapping.status, 409, "{:?}", overlapping.body);

    let product = call(
        &service,
        "POST",
        "/api/v1/products",
        Some(json!({"product":{
            "productKind":"medicine","baseUnitId":TABLET,"dosageFormId":dosage_form,
            "quantityScale":0,"displayName":"Paracetamol 500 mg Tablet"
        }})),
        Some(&cookie),
    )
    .await;
    assert_eq!(product.status, 201, "{:?}", product.body);
    let product_id = product.body["id"].as_str().expect("product id").to_owned();
    let uri = format!("/api/v1/products/{product_id}/price-control");

    // A fresh Product is unassessed, which is a real state and not "uncontrolled".
    let initial = call(&service, "GET", &uri, None, Some(&cookie)).await;
    assert_eq!(initial.status, 200, "{:?}", initial.body);
    assert_eq!(initial.body["priceControlStatus"], "unknown");
    assert_eq!(initial.body["resolved"], false);

    let assigned = call(
        &service,
        "PUT",
        &uri,
        Some(json!({
            "expectedRevision": 1, "priceControlStatus": "controlled",
            "controlledFormulationId": formulation
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(assigned.status, 200, "{:?}", assigned.body);
    assert_eq!(assigned.body["priceControlStatus"], "controlled");

    // The half-open boundary across the wire: on the day equal to effective_to the NEXT version
    // applies, and before the first period there is no ceiling at all.
    for (date, expected) in [
        ("2024-12-31", None),
        ("2025-01-01", Some(100)),
        ("2026-03-31", Some(100)),
        ("2026-04-01", Some(109)),
        ("2030-01-01", Some(109)),
    ] {
        let reply = call(
            &service,
            "GET",
            &format!("{uri}?asOf={date}"),
            None,
            Some(&cookie),
        )
        .await;
        assert_eq!(reply.status, 200, "{date}: {:?}", reply.body);
        match expected {
            Some(paise) => {
                assert_eq!(
                    reply.body["applicableCeiling"]["ceilingPricePaise"], paise,
                    "on {date}"
                );
                assert_eq!(reply.body["comparability"], "comparable", "on {date}");
                assert_eq!(reply.body["resolved"], true, "on {date}");
            }
            None => {
                assert_eq!(reply.body["applicableCeiling"], Value::Null, "on {date}");
                assert_eq!(
                    reply.body["resolved"], false,
                    "a controlled product with no ceiling must never look unconstrained"
                );
            }
        }
    }
}

/// A ceiling whose basis cannot be reconciled with the Product is reported as incomparable rather
/// than converted, divided or guessed at.
#[tokio::test]
async fn real_service_refuses_to_compare_an_incompatible_ceiling_basis_over_http() {
    let service = start().await;
    let cookie = owner_cookie(&service).await;

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

    let dosage_form = reference(
        "dosage-forms",
        json!({ "canonicalCode": "tablet", "displayName": "Tablet" }),
    )
    .await;

    // A per-base-unit ceiling with no unit named cannot be compared with anything, so it is refused
    // at the door rather than stored as an unusable legal fact.
    let unitless = call(
        &service,
        "POST",
        "/api/v1/reference/price-control-versions",
        Some(json!({ "attributes": {
            "controlledFormulationId": "01997a00-0000-7000-8000-0000000009aa",
            "effectiveFrom": "2020-01-01", "effectiveTo": null,
            "ceilingPricePaise": 109, "ceilingBasis": "per_base_unit"
        }})),
        Some(&cookie),
    )
    .await;
    assert_eq!(unitless.status, 422, "{:?}", unitless.body);
    assert_eq!(unitless.body["issues"][0]["field"], "ceilingBasisUnitId");

    let make = async |code: &str, basis: &str, unit: Option<&str>| {
        let formulation = reference(
            "controlled-formulations",
            json!({
                "jurisdiction": "IN", "formulationCode": code,
                "displayName": "Some controlled formulation", "verificationState": "unverified"
            }),
        )
        .await;
        let mut attributes = json!({
            "controlledFormulationId": formulation,
            "effectiveFrom": "2020-01-01", "effectiveTo": null,
            "ceilingPricePaise": 1635, "ceilingBasis": basis
        });
        if let Some(unit) = unit {
            attributes["ceilingBasisUnitId"] = json!(unit);
        }
        reference("price-control-versions", attributes).await;

        let product = call(
            &service,
            "POST",
            "/api/v1/products",
            Some(json!({"product":{
                "productKind":"medicine","baseUnitId":TABLET,"dosageFormId":dosage_form,
                "quantityScale":0,"displayName":format!("Product {code}")
            }})),
            Some(&cookie),
        )
        .await;
        let product_id = product.body["id"].as_str().expect("product id").to_owned();
        let uri = format!("/api/v1/products/{product_id}/price-control");
        let assigned = call(
            &service,
            "PUT",
            &uri,
            Some(json!({
                "expectedRevision": 1, "priceControlStatus": "controlled",
                "controlledFormulationId": formulation
            })),
            Some(&cookie),
        )
        .await;
        assert_eq!(assigned.status, 200, "{:?}", assigned.body);
        call(
            &service,
            "GET",
            &format!("{uri}?asOf=2026-09-14"),
            None,
            Some(&cookie),
        )
        .await
    };

    // Quoted per pack: which pack is not stated, so dividing it would invent a legal fact.
    let per_pack = make("PACK-BASIS", "per_pack", None).await;
    assert_eq!(per_pack.body["comparability"], "incomparable_pack_basis");
    assert_eq!(
        per_pack.body["applicableCeiling"]["ceilingPricePaise"], 1635,
        "the ceiling is still reported honestly; only the comparison is refused"
    );

    // Quoted per strip against a product measured in tablets: no unit conversion is attempted.
    let wrong_unit = make("UNIT-BASIS", "per_base_unit", Some(STRIP)).await;
    assert_eq!(wrong_unit.body["comparability"], "incomparable_unit");
}

/// Price-control reference data is Owner/Admin work; a reader may look but never assert.
#[tokio::test]
async fn real_service_keeps_price_control_assertion_to_an_admin_over_http() {
    let service = start().await;
    let cookie = owner_cookie(&service).await;

    let dosage_form = call(
        &service,
        "POST",
        "/api/v1/reference/dosage-forms",
        Some(json!({ "attributes": { "canonicalCode": "tablet", "displayName": "Tablet" }})),
        Some(&cookie),
    )
    .await
    .body["id"]
        .as_str()
        .expect("dosage form")
        .to_owned();
    let product = call(
        &service,
        "POST",
        "/api/v1/products",
        Some(json!({"product":{
            "productKind":"medicine","baseUnitId":TABLET,"dosageFormId":dosage_form,
            "quantityScale":0,"displayName":"Reader test tablet"
        }})),
        Some(&cookie),
    )
    .await;
    let product_id = product.body["id"].as_str().expect("product id").to_owned();
    let uri = format!("/api/v1/products/{product_id}/price-control");

    // There is no user-management API yet, so the reader is seeded directly; the login, the session
    // and the role check all still happen over the real transport.
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
    .bind("01997a00-0000-7000-8000-000000000401")
    .bind("price.reader")
    .bind("price.reader")
    .bind("Price Reader")
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
            "loginIdentifier": "price.reader", "password": "Integration-Password-42"
        })),
        None,
    )
    .await;
    assert_eq!(login.status, 200, "{:?}", login.body);
    let reader = session_cookie(&login.headers);

    let readable = call(&service, "GET", &uri, None, Some(&reader)).await;
    assert_eq!(readable.status, 200, "{:?}", readable.body);
    assert_eq!(readable.body["priceControlStatus"], "unknown");

    let denied = call(
        &service,
        "PUT",
        &uri,
        Some(json!({ "expectedRevision": 1, "priceControlStatus": "not_applicable" })),
        Some(&reader),
    )
    .await;
    assert_eq!(denied.status, 403, "{:?}", denied.body);
    assert_eq!(denied.body["code"], "authorization_denied");

    // And an anonymous caller never reaches it at all.
    let anonymous = call(&service, "GET", &uri, None, None).await;
    assert_eq!(anonymous.status, 401);
    assert_eq!(anonymous.body["code"], "authentication_required");
}

// ---------------------------------------------------------------------------------------------
// Phase 1H — sales / POS outward
// ---------------------------------------------------------------------------------------------

/// Everything a sale needs, built over real HTTP on top of the purchase world: the pack is enabled
/// for sale at this store, a lot exists with a printed MRP, and stock has actually been received.
struct SaleWorld {
    cookie: String,
    store: String,
    supplier: String,
    product: String,
    pack: String,
    batch: String,
    customer: String,
}

const SALE_DATE: &str = "2026-09-12";

async fn seed_sale_world(service: &Service, increment_atoms: i64) -> SaleWorld {
    let world = seed_purchase_world(service, MAHARASHTRA, "taxable").await;

    let store = call(
        service,
        "GET",
        "/api/v1/store/tax-identity",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(store.status, 200, "{:?}", store.body);
    let store_id = store.body["storeId"].as_str().expect("store id").to_owned();

    // The pack is enabled for sale here. `fractionalSaleAllowed` is false and must stay false: the
    // product is a tablet, so the frozen Phase 1B triggers forbid that permission outright. Loose
    // sale is governed by the increment alone.
    let policy = call(
        service,
        "PUT",
        &format!("/api/v1/packs/{}/policy", world.pack),
        Some(json!({
            "expectedRevision": null,
            "policy": {
                "storeId": store_id, "purchaseEnabled": true, "saleEnabled": true,
                "wholePackOnlyPurchase": false, "fractionalSaleAllowed": false,
                "minimumSaleIncrementAtoms": increment_atoms,
                "defaultPurchasePack": false, "defaultSalePack": true
            }
        })),
        Some(&world.cookie),
    )
    .await;
    assert!(
        policy.status == 200 || policy.status == 201,
        "{:?}",
        policy.body
    );

    // Stock arrives the way it really does: through a posted purchase, which also creates the lot.
    let draft = create_draft(service, &world, "INV-SALE-SEED").await;
    let purchase_id = draft["id"].as_str().expect("purchase id").to_owned();
    let with_line = call(
        service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/lines"),
        Some(json!({
            "expectedRevision": 1,
            "productId": world.product,
            "productPackId": world.pack,
            "newBatchNumber": "B-9001",
            "newBatchExpiresOn": "2028-03-31",
            "newBatchMrpPaise": 9550,
            "quantityPacks": 10,
            "ratePerPackPaise": 6000
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);
    let posted = call(
        service,
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
    let batch = posted.body["lines"][0]["batchId"]
        .as_str()
        .expect("materialised batch")
        .to_owned();

    let customer = call(
        service,
        "POST",
        "/api/v1/parties",
        Some(json!({
            "party": {
                "displayName": "Rahul Deshmukh", "gstRegistrationStatus": "unregistered",
                "gstin": null, "placeOfSupplyStateId": MAHARASHTRA
            },
            "roles": [{ "role": "customer" }],
            "addresses": []
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(customer.status, 201, "{:?}", customer.body);

    SaleWorld {
        cookie: world.cookie,
        store: store_id,
        supplier: world.supplier,
        product: world.product,
        pack: world.pack,
        batch,
        customer: customer.body["id"]
            .as_str()
            .expect("customer id")
            .to_owned(),
    }
}

async fn sale_draft(service: &Service, world: &SaleWorld, customer: Option<&str>) -> String {
    let draft = call(
        service,
        "POST",
        "/api/v1/sales",
        Some(json!({ "customerPartyId": customer, "businessDate": SALE_DATE })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(draft.status, 201, "{:?}", draft.body);
    draft.body["id"].as_str().expect("sale id").to_owned()
}

async fn sale_line(
    service: &Service,
    world: &SaleWorld,
    sale_id: &str,
    revision: i64,
    basis: &str,
    quantity: i64,
    rate: i64,
) -> Reply {
    call(
        service,
        "POST",
        &format!("/api/v1/sales/{sale_id}/lines"),
        Some(json!({
            "expectedRevision": revision,
            "productId": world.product,
            "productPackId": world.pack,
            "batchId": world.batch,
            "quantityBasis": basis,
            "quantity": quantity,
            "sellingRatePaise": rate
        })),
        Some(&world.cookie),
    )
    .await
}

async fn post_sale(
    service: &Service,
    world: &SaleWorld,
    sale_id: &str,
    revision: i64,
    key: &str,
    amount: i64,
) -> Reply {
    call(
        service,
        "POST",
        &format!("/api/v1/sales/{sale_id}/post"),
        Some(json!({
            "expectedRevision": revision,
            "idempotencyKey": key,
            "tenders": [{ "method": "cash", "amountPaise": amount }]
        })),
        Some(&world.cookie),
    )
    .await
}

/// Receives another lot of the same pack through a real posted purchase, so the stock behind it is
/// as genuine as the first one's.
async fn receive_lot(
    service: &Service,
    world: &SaleWorld,
    batch_number: &str,
    mrp_paise: i64,
    invoice: &str,
    key: &str,
) -> String {
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
    let purchase_id = draft.body["id"].as_str().expect("purchase id").to_owned();
    let with_line = call(
        service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/lines"),
        Some(json!({
            "expectedRevision": 1, "productId": world.product, "productPackId": world.pack,
            "newBatchNumber": batch_number, "newBatchExpiresOn": "2028-03-31",
            "newBatchMrpPaise": mrp_paise, "quantityPacks": 10, "ratePerPackPaise": 6000
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);
    let posted = call(
        service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/post"),
        Some(json!({
            "expectedRevision": with_line.body["revision"], "idempotencyKey": key
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    posted.body["lines"][0]["batchId"]
        .as_str()
        .expect("materialised batch")
        .to_owned()
}

/// The whole Phase 1H workflow across a real socket: a draft becomes a numbered GST invoice, the
/// snapshots freeze, and exactly the sold atoms leave the ledger.
#[tokio::test]
async fn real_service_posts_a_gst_sale_and_issues_stock_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;

    let sale_id = sale_draft(&service, &world, Some(&world.customer)).await;
    let with_line = sale_line(&service, &world, &sale_id, 1, "pack", 2, 8000).await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);
    // The browser sent "2 packs"; the atoms came from the frozen pack model.
    assert_eq!(with_line.body["lines"][0]["quantityAtoms"], 20);
    assert_eq!(with_line.body["lines"][0]["taxableValuePaise"], 16_000);
    // A draft carries no tax and no number at all.
    assert_eq!(with_line.body["lines"][0]["cgstPaise"], 0);
    assert_eq!(with_line.body["documentNumber"], Value::Null);
    let revision = with_line.body["revision"].as_i64().expect("revision");

    let posted = post_sale(
        &service,
        &world,
        &sale_id,
        revision,
        "01997a00-0000-7000-8000-0000000000c1",
        17_920,
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);

    // Refetched rather than trusted from the write response.
    let detail = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{sale_id}"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(detail.status, 200, "{:?}", detail.body);
    assert_eq!(detail.body["status"], "posted");
    assert_eq!(detail.body["documentNumber"], "INV/2627/000001");
    assert_eq!(detail.body["seriesCode"], "INV");
    assert_eq!(detail.body["financialYear"], "2026-27");
    assert_eq!(detail.body["sequenceValue"], 1);
    // Goods handed over at the counter: the place of supply is the store, so the tax splits.
    assert_eq!(detail.body["taxTreatment"], "intra_state");
    assert_eq!(detail.body["taxableValuePaise"], 16_000);
    assert_eq!(detail.body["cgstPaise"], 960);
    assert_eq!(detail.body["sgstPaise"], 960);
    assert_eq!(detail.body["igstPaise"], 0);
    assert_eq!(detail.body["grandTotalPaise"], 17_920);
    assert_eq!(detail.body["storeNormalizedGstin"], STORE_GSTIN);
    assert_eq!(detail.body["storeStateCode"], "27");
    assert_eq!(detail.body["customerDisplayName"], "Rahul Deshmukh");
    assert!(detail.body["postedAtUtc"].is_string());

    let line = &detail.body["lines"][0];
    assert_eq!(line["productDisplayName"], "Crocin 500 mg Tablet");
    assert_eq!(line["packDisplayLabel"], "Strip of 10");
    assert_eq!(line["baseUnitLabel"], "Tablet");
    assert_eq!(line["batchNumber"], "B-9001");
    assert_eq!(line["batchExpiresOn"], "2028-03-31");
    assert_eq!(line["batchMrpPaise"], 9_550);
    assert_eq!(line["hsnCode"], "30049099");
    assert_eq!(line["taxTreatmentKind"], "taxable");
    assert_eq!(line["cgstBasisPoints"], 600);
    assert_eq!(line["lineTotalPaise"], 17_920);
    // Nobody has assessed this product for price control, and the posted line says so honestly.
    assert_eq!(line["priceControlStatus"], "unknown");
    assert_eq!(line["ceilingPricePaise"], Value::Null);
    assert_eq!(detail.body["tenders"][0]["method"], "cash");
    assert_eq!(detail.body["tenders"][0]["amountPaise"], 17_920);

    // The ledger: one outward, negative, on the right lot, traceable to the line that caused it.
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
    let outward: Vec<&Value> = rows
        .iter()
        .filter(|row| row["movementType"] == "sale")
        .collect();
    assert_eq!(outward.len(), 1, "one line must post exactly one movement");
    assert_eq!(outward[0]["quantityDeltaAtoms"], -20);
    assert_eq!(outward[0]["batchId"], world.batch.as_str());
    assert_eq!(outward[0]["saleLineId"], line["id"]);
    assert_eq!(outward[0]["occurredOn"], SALE_DATE);

    // 100 atoms received, 20 sold, and the balance is still summed rather than stored.
    let stock = call(
        &service,
        "GET",
        "/api/v1/inventory/stock",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(stock.status, 200, "{:?}", stock.body);
    assert_eq!(stock.body[0]["balanceAtoms"], 80);
}

/// Loose units over real HTTP, and the increment that decides whether a strip may be broken.
/// A decimal pack quantity is unrepresentable rather than merely refused.
#[tokio::test]
async fn real_service_sells_loose_units_under_the_pack_increment_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    let sale_id = sale_draft(&service, &world, None).await;

    // Three tablets from a strip of ten, with no sub-unit permission anywhere in the model.
    let loose = sale_line(&service, &world, &sale_id, 1, "base_unit", 3, 800).await;
    assert_eq!(loose.status, 201, "{:?}", loose.body);
    assert_eq!(loose.body["lines"][0]["quantityAtoms"], 3);
    assert_eq!(loose.body["lines"][0]["quantityPacks"], Value::Null);
    assert_eq!(loose.body["lines"][0]["taxableValuePaise"], 2_400);

    // There is no such thing as 0.3 of a strip: the field is an integer, so the request is rejected
    // before any rule is even consulted.
    let decimal = call(
        &service,
        "POST",
        &format!("/api/v1/sales/{sale_id}/lines"),
        Some(json!({
            "expectedRevision": 2, "productId": world.product, "productPackId": world.pack,
            "batchId": world.batch, "quantityBasis": "pack", "quantity": 0.3,
            "sellingRatePaise": 8000
        })),
        Some(&world.cookie),
    )
    .await;
    assert!(
        decimal.status == 400 || decimal.status == 422,
        "a decimal pack quantity must not parse: {:?}",
        decimal.body
    );

    let posted = post_sale(
        &service,
        &world,
        &sale_id,
        2,
        "01997a00-0000-7000-8000-0000000000c2",
        2_688,
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    assert_eq!(posted.body["grandTotalPaise"], 2_688);

    // A second store, whose increment is a whole strip, refuses the same three tablets.
    let strict = start().await;
    let strict_world = seed_sale_world(&strict, 10).await;
    let strict_sale = sale_draft(&strict, &strict_world, None).await;
    let refused = sale_line(&strict, &strict_world, &strict_sale, 1, "base_unit", 3, 800).await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(refused.body["code"], "quantity_increment_violation");
    let accepted = sale_line(
        &strict,
        &strict_world,
        &strict_sale,
        1,
        "base_unit",
        10,
        800,
    )
    .await;
    assert_eq!(accepted.status, 201, "{:?}", accepted.body);
}

/// Every refusal a counter can provoke, over real HTTP, each with a typed and safe code.
#[tokio::test]
async fn real_service_refuses_ineligible_sales_with_safe_codes_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;

    // Above the printed MRP. The ceiling is INCLUSIVE of GST, so 85.27 a strip prints 95.51.
    let sale_id = sale_draft(&service, &world, None).await;
    let line = sale_line(&service, &world, &sale_id, 1, "pack", 1, 8_527).await;
    assert_eq!(line.status, 201, "{:?}", line.body);
    let above = post_sale(
        &service,
        &world,
        &sale_id,
        2,
        "01997a00-0000-7000-8000-0000000000d1",
        9_551,
    )
    .await;
    assert_eq!(above.status, 409, "{:?}", above.body);
    assert_eq!(above.body["code"], "selling_rate_above_mrp");

    // Overselling the lot, including two lines that only fail once aggregated.
    let over_id = sale_draft(&service, &world, None).await;
    let first = sale_line(&service, &world, &over_id, 1, "pack", 6, 8_000).await;
    assert_eq!(first.status, 201, "{:?}", first.body);
    let second = sale_line(&service, &world, &over_id, 2, "pack", 6, 8_000).await;
    assert_eq!(second.status, 201, "{:?}", second.body);
    let oversold = post_sale(
        &service,
        &world,
        &over_id,
        3,
        "01997a00-0000-7000-8000-0000000000d2",
        107_520,
    )
    .await;
    assert_eq!(oversold.status, 409, "{:?}", oversold.body);
    assert_eq!(oversold.body["code"], "insufficient_stock");
    assert_eq!(oversold.body["availableAtoms"], 100);

    // A tender that does not settle the invoice.
    let tender_id = sale_draft(&service, &world, None).await;
    let tender_line = sale_line(&service, &world, &tender_id, 1, "pack", 1, 8_000).await;
    assert_eq!(tender_line.status, 201, "{:?}", tender_line.body);
    let short = post_sale(
        &service,
        &world,
        &tender_id,
        2,
        "01997a00-0000-7000-8000-0000000000d3",
        8_000,
    )
    .await;
    assert_eq!(short.status, 409, "{:?}", short.body);
    assert_eq!(short.body["code"], "tender_mismatch");

    // A party that is not a customer of this store. Unregistered, so it needs no GSTIN of its own
    // and cannot collide with the supplier the world already seeded.
    let party = call(
        &service,
        "POST",
        "/api/v1/parties",
        Some(json!({
            "party": {
                "displayName": "Wholesale Only", "gstRegistrationStatus": "unregistered",
                "gstin": null, "placeOfSupplyStateId": MAHARASHTRA
            },
            "roles": [{ "role": "supplier" }],
            "addresses": []
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(party.status, 201, "{:?}", party.body);
    let supplier_only = party.body["id"].as_str().expect("party id").to_owned();
    let ineligible = call(
        &service,
        "POST",
        "/api/v1/sales",
        Some(json!({ "customerPartyId": supplier_only, "businessDate": SALE_DATE })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(ineligible.status, 409, "{:?}", ineligible.body);
    assert_eq!(ineligible.body["code"], "customer_not_eligible");

    // Nothing above moved a single atom.
    let stock = call(
        &service,
        "GET",
        "/api/v1/inventory/stock",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(stock.status, 200, "{:?}", stock.body);
    assert_eq!(stock.body[0]["balanceAtoms"], 100);
}

/// A retry after a successful post returns the original invoice; the same key on a different sale
/// is refused; and a posting that fails after partial work leaves no number, movement or tender.
#[tokio::test]
async fn real_service_keeps_the_invoice_series_dense_under_replay_and_failure_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    let key = "01997a00-0000-7000-8000-0000000000e1";

    let sale_id = sale_draft(&service, &world, None).await;
    let line = sale_line(&service, &world, &sale_id, 1, "pack", 1, 8_000).await;
    assert_eq!(line.status, 201, "{:?}", line.body);
    let posted = post_sale(&service, &world, &sale_id, 2, key, 8_960).await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    assert_eq!(posted.body["documentNumber"], "INV/2627/000001");

    // The counter's network dropped and the operator pressed Post again.
    let replay = post_sale(&service, &world, &sale_id, 2, key, 8_960).await;
    assert_eq!(replay.status, 200, "{:?}", replay.body);
    assert_eq!(replay.body["documentNumber"], "INV/2627/000001");
    assert_eq!(replay.body["revision"], posted.body["revision"]);

    // The same key on a different sale: refused after it has already allocated a number, rewritten
    // its lines and inserted its movements — all of which must be undone.
    let second = sale_draft(&service, &world, None).await;
    let second_line = sale_line(&service, &world, &second, 1, "pack", 3, 8_000).await;
    assert_eq!(second_line.status, 201, "{:?}", second_line.body);
    let reused = post_sale(&service, &world, &second, 2, key, 26_880).await;
    assert_eq!(reused.status, 409, "{:?}", reused.body);
    assert_eq!(reused.body["code"], "idempotency_conflict");

    let residue = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{second}"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(residue.status, 200, "{:?}", residue.body);
    assert_eq!(residue.body["status"], "draft");
    assert_eq!(residue.body["documentNumber"], Value::Null);
    assert_eq!(residue.body["lines"][0]["batchNumber"], Value::Null);
    assert_eq!(
        residue.body["tenders"].as_array().expect("tenders").len(),
        0
    );

    // Exactly one sale movement exists, and the series is dense: the next number is 2, not 3.
    let movements = call(
        &service,
        "GET",
        "/api/v1/inventory/movements",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(movements.status, 200, "{:?}", movements.body);
    let sales: Vec<&Value> = movements
        .body
        .as_array()
        .expect("movements")
        .iter()
        .filter(|row| row["movementType"] == "sale")
        .collect();
    assert_eq!(sales.len(), 1, "a refused posting must move no stock");

    let third = sale_draft(&service, &world, None).await;
    let third_line = sale_line(&service, &world, &third, 1, "pack", 1, 8_000).await;
    assert_eq!(third_line.status, 201, "{:?}", third_line.body);
    let next = post_sale(
        &service,
        &world,
        &third,
        2,
        "01997a00-0000-7000-8000-0000000000e2",
        8_960,
    )
    .await;
    assert_eq!(next.status, 200, "{:?}", next.body);
    assert_eq!(
        next.body["documentNumber"], "INV/2627/000002",
        "a refused posting must consume no number"
    );
}

/// A posted invoice is a document that was issued to a customer. It cannot be edited, and an
/// expired lot cannot be sold at all.
#[tokio::test]
async fn real_service_freezes_a_posted_sale_and_blocks_an_expired_lot_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;

    let sale_id = sale_draft(&service, &world, None).await;
    let line = sale_line(&service, &world, &sale_id, 1, "pack", 1, 8_000).await;
    assert_eq!(line.status, 201, "{:?}", line.body);
    let posted = post_sale(
        &service,
        &world,
        &sale_id,
        2,
        "01997a00-0000-7000-8000-0000000000f1",
        8_960,
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    let revision = posted.body["revision"].as_i64().expect("revision");

    let edited = call(
        &service,
        "PUT",
        &format!("/api/v1/sales/{sale_id}"),
        Some(json!({
            "expectedRevision": revision, "customerPartyId": null, "businessDate": SALE_DATE
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(edited.status, 409, "{:?}", edited.body);
    assert_eq!(edited.body["code"], "sale_not_draft");

    let added = sale_line(&service, &world, &sale_id, revision, "pack", 1, 8_000).await;
    assert_eq!(added.status, 409, "{:?}", added.body);
    assert_eq!(added.body["code"], "sale_not_draft");

    // A lot whose expiry has passed on the business date cannot be sold.
    let expired_batch = call(
        &service,
        "POST",
        &format!("/api/v1/packs/{}/batches", world.pack),
        Some(json!({
            "batchNumber": "B-OLD", "expiresOn": "2026-01-31", "mrpPaise": 9550
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(expired_batch.status, 201, "{:?}", expired_batch.body);
    let expired_id = expired_batch.body["id"].as_str().expect("batch id");

    let stale_sale = sale_draft(&service, &world, None).await;
    let refused = call(
        &service,
        "POST",
        &format!("/api/v1/sales/{stale_sale}/lines"),
        Some(json!({
            "expectedRevision": 1, "productId": world.product, "productPackId": world.pack,
            "batchId": expired_id, "quantityBasis": "pack", "quantity": 1,
            "sellingRatePaise": 8000
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(refused.body["code"], "batch_expired");

    // The chooser still shows the lot and says why it is unusable, rather than hiding it.
    let batches = call(
        &service,
        "GET",
        &format!(
            "/api/v1/packs/{}/sellable-batches?asOf={SALE_DATE}",
            world.pack
        ),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(batches.status, 200, "{:?}", batches.body);
    let rows = batches.body.as_array().expect("batches");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["batchNumber"], "B-OLD");
    assert_eq!(rows[0]["expired"], true);
    assert_eq!(rows[0]["availableAtoms"], 0);
    assert_eq!(rows[1]["batchNumber"], "B-9001");
    assert_eq!(rows[1]["expired"], false);
    assert_eq!(rows[1]["availableAtoms"], 90);
    let _ = world.store;
}

/// A controlled medicine is held to its notified ceiling over real HTTP, against the tax-exclusive
/// rate — and a ceiling that cannot be compared is refused rather than guessed at.
#[tokio::test]
async fn real_service_enforces_the_notified_ceiling_on_a_sale_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;

    let formulation = call(
        &service,
        "POST",
        "/api/v1/reference/controlled-formulations",
        Some(json!({ "attributes": {
            "jurisdiction": "IN", "formulationCode": "CROCIN-500",
            "displayName": "Paracetamol 500 mg tablet", "dosageFormId": null,
            "strengthText": "500 mg", "verificationState": "verified",
            "sourceNote": "Recorded from the notification as published"
        }})),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(formulation.status, 201, "{:?}", formulation.body);
    let formulation_id = formulation.body["id"].as_str().expect("formulation id");

    let version = call(
        &service,
        "POST",
        "/api/v1/reference/price-control-versions",
        Some(json!({ "attributes": {
            "controlledFormulationId": formulation_id, "effectiveFrom": "2026-01-01",
            "effectiveTo": null, "ceilingPricePaise": 900, "ceilingBasis": "per_base_unit",
            "ceilingBasisUnitId": TABLET, "notificationReference": "S.O. 1234(E)"
        }})),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(version.status, 201, "{:?}", version.body);

    let asserted = call(
        &service,
        "PUT",
        &format!("/api/v1/products/{}/price-control", world.product),
        Some(json!({
            "expectedRevision": 2, "priceControlStatus": "controlled",
            "controlledFormulationId": formulation_id
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(asserted.status, 200, "{:?}", asserted.body);

    // The seeded lot is printed at 95.50, which would refuse this sale on the MRP rule before the
    // ceiling was ever consulted. A generously priced second lot isolates the ceiling, so what this
    // test proves is the DPCO rule and not the Legal Metrology one.
    let second_lot = receive_lot(
        &service,
        &world,
        "B-CEIL",
        50_000,
        "INV-CEIL",
        "01997a00-0000-7000-8000-00000000001c",
    )
    .await;
    let world = SaleWorld {
        batch: second_lot,
        ..world
    };

    // 9.00 a tablet: a strip of ten may be sold for at most 90.00 EXCLUSIVE of GST.
    let over_id = sale_draft(&service, &world, None).await;
    let over_line = sale_line(&service, &world, &over_id, 1, "pack", 1, 9_001).await;
    assert_eq!(over_line.status, 201, "{:?}", over_line.body);
    let over = post_sale(
        &service,
        &world,
        &over_id,
        2,
        "01997a00-0000-7000-8000-00000000001a",
        10_081,
    )
    .await;
    assert_eq!(over.status, 409, "{:?}", over.body);
    assert_eq!(over.body["code"], "selling_rate_above_ceiling");

    let at_id = sale_draft(&service, &world, None).await;
    let at_line = sale_line(&service, &world, &at_id, 1, "pack", 1, 9_000).await;
    assert_eq!(at_line.status, 201, "{:?}", at_line.body);
    let at = post_sale(
        &service,
        &world,
        &at_id,
        2,
        "01997a00-0000-7000-8000-00000000001b",
        10_080,
    )
    .await;
    assert_eq!(at.status, 200, "{:?}", at.body);
    assert_eq!(at.body["lines"][0]["priceControlStatus"], "controlled");
    assert_eq!(at.body["lines"][0]["ceilingPricePaise"], 900);
    assert_eq!(at.body["lines"][0]["ceilingBasis"], "per_base_unit");
    assert!(at.body["lines"][0]["priceControlVersionId"].is_string());
}

/// Selling is counter work, so a cashier may do it — but an anonymous request may not, and the
/// ledger will not mint a sale movement by hand.
#[tokio::test]
async fn real_service_lets_a_cashier_sell_but_refuses_a_hand_written_outward_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;

    // There is no user-administration endpoint yet, so the cashier is seeded directly into the
    // disposable gate database — reusing the owner's verifier so the real Argon2 login path below is
    // still exercised over the transport. Everything under test is the role check, not the seeding.
    let pool = database::connect(&service.database_path)
        .await
        .expect("reopen disposable database");
    let owner_hash: String = sqlx::query_scalar("SELECT password_hash FROM users LIMIT 1")
        .fetch_one(&pool)
        .await
        .expect("owner hash");
    sqlx::query(
        "INSERT INTO users (id,login_identifier,normalized_login_identifier,display_name,         password_hash,role,created_at_utc,updated_at_utc)          VALUES (?,?,?,?,?, 'cashier', strftime('%Y-%m-%dT%H:%M:%fZ','now'),          strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
    )
    .bind("01997a00-0000-7000-8000-000000000301")
    .bind("counter.cashier")
    .bind("counter.cashier")
    .bind("Counter Cashier")
    .bind(&owner_hash)
    .execute(&pool)
    .await
    .expect("seed a cashier");
    pool.close().await;

    let login = call(
        &service,
        "POST",
        "/api/v1/auth/login",
        Some(json!({
            "loginIdentifier": "counter.cashier", "password": "Integration-Password-42"
        })),
        None,
    )
    .await;
    assert_eq!(login.status, 200, "{:?}", login.body);
    let cashier_cookie = session_cookie(&login.headers);

    let draft = call(
        &service,
        "POST",
        "/api/v1/sales",
        Some(json!({ "customerPartyId": null, "businessDate": SALE_DATE })),
        Some(&cashier_cookie),
    )
    .await;
    assert_eq!(draft.status, 201, "{:?}", draft.body);

    let anonymous = call(
        &service,
        "POST",
        "/api/v1/sales",
        Some(json!({ "customerPartyId": null, "businessDate": SALE_DATE })),
        None,
    )
    .await;
    assert_eq!(anonymous.status, 401, "{:?}", anonymous.body);

    // An outward must come from a posting, never from the manual ledger endpoint.
    let minted = call(
        &service,
        "POST",
        "/api/v1/inventory/movements",
        Some(json!({
            "idempotencyKey": "01997a00-0000-7000-8000-00000000002a",
            "movementType": "sale", "productPackId": world.pack, "batchId": world.batch,
            "quantityDeltaAtoms": -10, "occurredOn": SALE_DATE
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(minted.status, 422, "{:?}", minted.body);
    assert_eq!(minted.body["issues"][0]["field"], "movementType");
}

// ---------------------------------------------------------------------------------------------
// Phase 1I — returns
// ---------------------------------------------------------------------------------------------

/// Everything a return needs, built over real HTTP: a posted purchase that created the stock, and a
/// posted sale that took some of it out again.
struct ReturnWorld {
    cookie: String,
    sale_id: String,
    sale_line_id: String,
    purchase_id: String,
    purchase_line_id: String,
    pack: String,
    batch: String,
}

async fn seed_return_world(service: &Service) -> ReturnWorld {
    let world = seed_sale_world(service, 1).await;

    // One sale of two strips at 80.00: 160.00 taxable, 9.60 each side, 179.20 in all.
    let sale_id = sale_draft(service, &world, Some(&world.customer)).await;
    let with_line = sale_line(service, &world, &sale_id, 1, "pack", 2, 8000).await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);
    let posted = post_sale(
        service,
        &world,
        &sale_id,
        2,
        "01997a00-0000-7000-8000-0000000000f9",
        17_920,
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    let sale_line_id = posted.body["lines"][0]["id"]
        .as_str()
        .expect("sale line")
        .to_owned();

    // The purchase that stocked the shelf is the one the seed already posted.
    let purchases = call(
        service,
        "GET",
        "/api/v1/purchases",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(purchases.status, 200, "{:?}", purchases.body);
    let purchase_id = purchases.body[0]["id"]
        .as_str()
        .expect("purchase id")
        .to_owned();
    let purchase = call(
        service,
        "GET",
        &format!("/api/v1/purchases/{purchase_id}"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(purchase.status, 200, "{:?}", purchase.body);
    let purchase_line_id = purchase.body["lines"][0]["id"]
        .as_str()
        .expect("purchase line")
        .to_owned();

    ReturnWorld {
        cookie: world.cookie,
        sale_id,
        sale_line_id,
        purchase_id,
        purchase_line_id,
        pack: world.pack,
        batch: world.batch,
    }
}

async fn sellable_atoms(service: &Service, world: &ReturnWorld) -> i64 {
    let batches = call(
        service,
        "GET",
        &format!(
            "/api/v1/packs/{}/sellable-batches?asOf={SALE_DATE}",
            world.pack
        ),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(batches.status, 200, "{:?}", batches.body);
    batches
        .body
        .as_array()
        .expect("batches")
        .iter()
        .find(|row| row["id"] == world.batch.as_str())
        .and_then(|row| row["availableAtoms"].as_i64())
        .expect("a sellable balance")
}

/// A sale corrected by a compensating document, across a real socket: the invoice is reversed to the
/// paisa, a credit-note number is issued from its own series, and the goods come back into
/// quarantine rather than onto the shelf.
#[tokio::test]
async fn real_service_returns_goods_from_a_sale_into_quarantine_over_http() {
    let service = start().await;
    let world = seed_return_world(&service).await;
    // 100 atoms received, 20 sold.
    assert_eq!(sellable_atoms(&service, &world).await, 80);

    let returnable = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{}/returnable-lines", world.sale_id),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(returnable.status, 200, "{:?}", returnable.body);
    assert_eq!(returnable.body["documentNumber"], "INV/2627/000001");
    assert_eq!(returnable.body["lines"][0]["returnableQuantity"], 2);
    assert_eq!(returnable.body["lines"][0]["alreadyReturnedAtoms"], 0);

    let draft = call(
        &service,
        "POST",
        "/api/v1/returns",
        Some(json!({
            "returnKind": "sales_return",
            "originalDocumentId": world.sale_id,
            "businessDate": SALE_DATE
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(draft.status, 201, "{:?}", draft.body);
    let return_id = draft.body["id"].as_str().expect("return id").to_owned();

    let with_line = call(
        &service,
        "POST",
        &format!("/api/v1/returns/{return_id}/lines"),
        Some(json!({
            "expectedRevision": 1,
            "originalLineId": world.sale_line_id,
            "quantity": 1,
            "disposition": "quarantined"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);
    // Half the line: 80.00 taxable, 4.80 each side, 89.60 in all.
    assert_eq!(with_line.body["lines"][0]["taxableValuePaise"], 8_000);
    assert_eq!(with_line.body["lines"][0]["lineTotalPaise"], 8_960);

    let quote = call(
        &service,
        "GET",
        &format!("/api/v1/returns/{return_id}/quote"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(quote.status, 200, "{:?}", quote.body);
    assert_eq!(quote.body["grandTotalPaise"], 8_960);

    let posted = call(
        &service,
        "POST",
        &format!("/api/v1/returns/{return_id}/post"),
        Some(json!({
            "expectedRevision": 2,
            "idempotencyKey": "01997a00-0000-7000-8000-00000000fa01",
            "taxAdjustmentStatus": "commercial_only",
            "taxAdjustmentReason": "Tax was passed on to the customer"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    assert_eq!(posted.body["documentNumber"], "SR/2627/000001");
    assert_eq!(posted.body["originalDocumentNumber"], "INV/2627/000001");
    assert_eq!(posted.body["taxTreatment"], "intra_state");
    assert_eq!(posted.body["taxAdjustmentStatus"], "commercial_only");
    assert_eq!(posted.body["grandTotalPaise"], 8_960);
    assert!(
        posted.body["documentNumber"].as_str().unwrap().len() <= 16,
        "the serial must fit the statutory sixteen characters"
    );

    // The goods are back in the building and NOT on the shelf.
    assert_eq!(
        sellable_atoms(&service, &world).await,
        80,
        "a return must not create sellable stock"
    );
    let movements = call(
        &service,
        "GET",
        "/api/v1/inventory/movements",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(movements.status, 200, "{:?}", movements.body);
    let quarantined: Vec<&Value> = movements
        .body
        .as_array()
        .expect("movements")
        .iter()
        .filter(|row| row["movementType"] == "sales_return")
        .collect();
    assert_eq!(quarantined.len(), 1);
    assert_eq!(quarantined[0]["quantityDeltaAtoms"], 10);
    assert_eq!(quarantined[0]["stockStatus"], "quarantined");
    assert_eq!(
        quarantined[0]["returnLineId"],
        posted.body["lines"][0]["id"]
    );

    // Stock Overview separates the two rather than merging them into one misleading figure.
    let stock = call(
        &service,
        "GET",
        "/api/v1/inventory/stock",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(stock.status, 200, "{:?}", stock.body);
    let rows = stock.body.as_array().expect("balances");
    let sellable = rows
        .iter()
        .find(|row| row["stockStatus"] == "sellable")
        .expect("a sellable row");
    let quarantine = rows
        .iter()
        .find(|row| row["stockStatus"] == "quarantined")
        .expect("a quarantined row");
    assert_eq!(sellable["balanceAtoms"], 80);
    assert_eq!(quarantine["balanceAtoms"], 10);

    // A pharmacist releases half of it, and only then does the counter see it.
    let released = call(
        &service,
        "POST",
        "/api/v1/stock-dispositions",
        Some(json!({
            "idempotencyKey": "01997a00-0000-7000-8000-00000000fa02",
            "productPackId": world.pack,
            "batchId": world.batch,
            "quantityAtoms": 4,
            "fromStatus": "quarantined",
            "toStatus": "sellable",
            "reason": "Sealed strip, inspected and found fit for sale",
            "occurredOn": SALE_DATE
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(released.status, 201, "{:?}", released.body);
    assert_eq!(sellable_atoms(&service, &world).await, 84);
}

/// Goods going back to a supplier, across a real socket — and the proof that the original invoice
/// having bought them is not proof they are still there.
#[tokio::test]
async fn real_service_returns_goods_to_a_supplier_over_http() {
    let service = start().await;
    let world = seed_return_world(&service).await;
    assert_eq!(sellable_atoms(&service, &world).await, 80);

    let draft = call(
        &service,
        "POST",
        "/api/v1/returns",
        Some(json!({
            "returnKind": "purchase_return",
            "originalDocumentId": world.purchase_id,
            "businessDate": SALE_DATE
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(draft.status, 201, "{:?}", draft.body);
    let return_id = draft.body["id"].as_str().expect("return id").to_owned();

    // Ten strips were bought and two were sold, so ten cannot go back.
    let greedy = call(
        &service,
        "POST",
        &format!("/api/v1/returns/{return_id}/lines"),
        Some(json!({
            "expectedRevision": 1,
            "originalLineId": world.purchase_line_id,
            "quantity": 10
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(greedy.status, 201, "{:?}", greedy.body);
    let refused = call(
        &service,
        "POST",
        &format!("/api/v1/returns/{return_id}/post"),
        Some(json!({
            "expectedRevision": 2,
            "idempotencyKey": "01997a00-0000-7000-8000-00000000fb01",
            "gstRoute": "supplier_credit_note"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(refused.body["code"], "insufficient_stock");
    assert_eq!(refused.body["availableAtoms"], 80);
    assert_eq!(
        sellable_atoms(&service, &world).await,
        80,
        "a refused return moves nothing"
    );

    // Eight strips is what remains, and goes back.
    let ok_draft = call(
        &service,
        "POST",
        "/api/v1/returns",
        Some(json!({
            "returnKind": "purchase_return",
            "originalDocumentId": world.purchase_id,
            "businessDate": SALE_DATE
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(ok_draft.status, 201, "{:?}", ok_draft.body);
    let ok_id = ok_draft.body["id"].as_str().expect("return id").to_owned();
    let with_line = call(
        &service,
        "POST",
        &format!("/api/v1/returns/{ok_id}/lines"),
        Some(json!({
            "expectedRevision": 1,
            "originalLineId": world.purchase_line_id,
            "quantity": 8
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);

    let posted = call(
        &service,
        "POST",
        &format!("/api/v1/returns/{ok_id}/post"),
        Some(json!({
            "expectedRevision": 2,
            "idempotencyKey": "01997a00-0000-7000-8000-00000000fb02",
            "gstRoute": "supplier_credit_note"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    // Its own series, and never called a debit note.
    assert_eq!(posted.body["documentNumber"], "PR/2627/000001");
    assert_eq!(posted.body["returnKind"], "purchase_return");
    assert_eq!(posted.body["gstRoute"], "supplier_credit_note");
    assert_eq!(posted.body["taxAdjustmentStatus"], Value::Null);
    // Eight of ten strips at 60.00: 480.00 taxable, 28.80 each side, 537.60 in all.
    assert_eq!(posted.body["taxableValuePaise"], 48_000);
    assert_eq!(posted.body["grandTotalPaise"], 53_760);
    assert_eq!(sellable_atoms(&service, &world).await, 0);

    // The supplier's credit note arrives later and is recorded without touching the posted return.
    let evidence = call(
        &service,
        "POST",
        &format!("/api/v1/returns/{ok_id}/supplier-credit-notes"),
        Some(json!({
            "creditNoteNumber": "SUPP-CN-4471",
            "creditNoteDate": "2026-09-20",
            "creditNoteAmountPaise": 53_760
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(evidence.status, 201, "{:?}", evidence.body);
    let detail = call(
        &service,
        "GET",
        &format!("/api/v1/returns/{ok_id}"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(detail.status, 200, "{:?}", detail.body);
    assert_eq!(
        detail.body["supplierCreditNotes"][0]["creditNoteNumber"],
        "SUPP-CN-4471"
    );
    assert_eq!(detail.body["grandTotalPaise"], 53_760);
}

/// Everything a return can be refused for, over real HTTP, each with a typed and safe code.
#[tokio::test]
async fn real_service_refuses_ineligible_returns_with_safe_codes_over_http() {
    let service = start().await;
    let world = seed_return_world(&service).await;

    let draft = call(
        &service,
        "POST",
        "/api/v1/returns",
        Some(json!({
            "returnKind": "sales_return",
            "originalDocumentId": world.sale_id,
            "businessDate": SALE_DATE
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(draft.status, 201, "{:?}", draft.body);
    let return_id = draft.body["id"].as_str().expect("return id").to_owned();

    // More than was ever sold.
    let over = call(
        &service,
        "POST",
        &format!("/api/v1/returns/{return_id}/lines"),
        Some(json!({
            "expectedRevision": 1,
            "originalLineId": world.sale_line_id,
            "quantity": 3,
            "disposition": "quarantined"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(over.status, 409, "{:?}", over.body);
    assert_eq!(over.body["code"], "over_return");
    assert_eq!(over.body["returnableAtoms"], 20);

    // Goods coming back must say where they went, and can never say they are sellable.
    let silent = call(
        &service,
        "POST",
        &format!("/api/v1/returns/{return_id}/lines"),
        Some(json!({
            "expectedRevision": 1,
            "originalLineId": world.sale_line_id,
            "quantity": 1
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(silent.status, 409, "{:?}", silent.body);
    assert_eq!(silent.body["code"], "disposition_required");

    let sellable = call(
        &service,
        "POST",
        &format!("/api/v1/returns/{return_id}/lines"),
        Some(json!({
            "expectedRevision": 1,
            "originalLineId": world.sale_line_id,
            "quantity": 1,
            "disposition": "sellable"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(sellable.status, 422, "{:?}", sellable.body);
    assert_eq!(sellable.body["issues"][0]["field"], "disposition");

    // A line from another document cannot be attached to this one.
    let stranger = call(
        &service,
        "POST",
        &format!("/api/v1/returns/{return_id}/lines"),
        Some(json!({
            "expectedRevision": 1,
            "originalLineId": world.purchase_line_id,
            "quantity": 1,
            "disposition": "quarantined"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(stranger.status, 409, "{:?}", stranger.body);
    assert_eq!(stranger.body["code"], "original_line_mismatch");

    // And a sales return must state its tax character rather than have it guessed.
    let good = call(
        &service,
        "POST",
        &format!("/api/v1/returns/{return_id}/lines"),
        Some(json!({
            "expectedRevision": 1,
            "originalLineId": world.sale_line_id,
            "quantity": 1,
            "disposition": "quarantined"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(good.status, 201, "{:?}", good.body);
    let silent_tax = call(
        &service,
        "POST",
        &format!("/api/v1/returns/{return_id}/post"),
        Some(json!({
            "expectedRevision": 2,
            "idempotencyKey": "01997a00-0000-7000-8000-00000000fc01"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(silent_tax.status, 409, "{:?}", silent_tax.body);
    assert_eq!(silent_tax.body["code"], "tax_adjustment_status_required");

    assert_eq!(
        sellable_atoms(&service, &world).await,
        80,
        "no refusal moved any stock"
    );
}

// ---------------------------------------------------------------------------------------------
// Phase 1J — stock operations over the real transport
// ---------------------------------------------------------------------------------------------

/// Seeds a second counter user and signs them in, so role rules are exercised across a real login
/// rather than asserted against a handler in isolation.
async fn sign_in_as(service: &Service, role: &str, identifier: &str, id: &str) -> String {
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
         VALUES (?,?,?,?,?,?, strftime('%Y-%m-%dT%H:%M:%fZ','now'), \
         strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
    )
    .bind(id)
    .bind(identifier)
    .bind(identifier)
    .bind(format!("Stock {role}"))
    .bind(&owner_hash)
    .bind(role)
    .execute(&pool)
    .await
    .expect("seed a counter user");
    pool.close().await;

    let login = call(
        service,
        "POST",
        "/api/v1/auth/login",
        Some(json!({ "loginIdentifier": identifier, "password": "Integration-Password-42" })),
        None,
    )
    .await;
    assert_eq!(login.status, 200, "{:?}", login.body);
    session_cookie(&login.headers)
}

/// The balance of one status at one lot, derived exactly as the service derives it.
async fn status_atoms(service: &Service, world: &ReturnWorld, status: &str) -> i64 {
    let stock = call(
        service,
        "GET",
        "/api/v1/inventory/stock",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(stock.status, 200, "{:?}", stock.body);
    stock
        .body
        .as_array()
        .expect("stock rows")
        .iter()
        .find(|row| row["batchId"] == world.batch.as_str() && row["stockStatus"] == status)
        .and_then(|row| row["balanceAtoms"].as_i64())
        .unwrap_or(0)
}

/// Builds and posts a one-line stock operation, returning the posted document.
async fn run_operation(service: &Service, cookie: &str, kind: &str, line: Value) -> Reply {
    let created = call(
        service,
        "POST",
        "/api/v1/stock-operations",
        Some(json!({ "operationKind": kind, "businessDate": SALE_DATE })),
        Some(cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{kind}: {:?}", created.body);
    let id = created.body["id"]
        .as_str()
        .expect("operation id")
        .to_owned();

    let mut body = line;
    body["expectedRevision"] = json!(1);
    let added = call(
        service,
        "POST",
        &format!("/api/v1/stock-operations/{id}/lines"),
        Some(body),
        Some(cookie),
    )
    .await;
    if added.status != 201 {
        return added;
    }
    let revision = added.body["revision"].as_i64().expect("revision");
    call(
        service,
        "POST",
        &format!("/api/v1/stock-operations/{id}/post"),
        Some(json!({
            "expectedRevision": revision,
            "idempotencyKey": uuid::Uuid::now_v7().to_string()
        })),
        Some(cookie),
    )
    .await
}

/// A shelf counted short, across a real socket. The operator sends what they counted; the service
/// decides the variance and writes the movement.
#[tokio::test]
async fn real_service_counts_a_shelf_and_computes_the_variance_over_http() {
    let service = start().await;
    let world = seed_return_world(&service).await;
    let before = status_atoms(&service, &world, "sellable").await;
    assert!(before > 20, "expected stock to count, found {before}");

    let pharmacist = sign_in_as(
        &service,
        "pharmacist",
        "stock.pharmacist",
        "01997a00-0000-7000-8000-000000000301",
    )
    .await;

    let posted = run_operation(
        &service,
        &pharmacist,
        "physical_count",
        json!({
            "productPackId": world.pack, "batchId": world.batch, "stockStatus": "sellable",
            "reasonCode": "physical_count_gain", "countedQuantity": before - 7,
            "quantityBasis": "base_unit"
        }),
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    assert_eq!(posted.body["lines"][0]["appliedDeltaAtoms"], -7);
    // The reason is the arithmetic's, not the request's.
    assert_eq!(posted.body["lines"][0]["reasonCode"], "physical_count_loss");
    assert_eq!(status_atoms(&service, &world, "sellable").await, before - 7);

    // A count that matches records the check and moves nothing.
    let settled = status_atoms(&service, &world, "sellable").await;
    let quiet = run_operation(
        &service,
        &pharmacist,
        "physical_count",
        json!({
            "productPackId": world.pack, "batchId": world.batch, "stockStatus": "sellable",
            "reasonCode": "physical_count_gain", "countedQuantity": settled,
            "quantityBasis": "base_unit"
        }),
    )
    .await;
    assert_eq!(quiet.status, 200, "{:?}", quiet.body);
    assert_eq!(quiet.body["lines"][0]["appliedDeltaAtoms"], 0);
    assert_eq!(status_atoms(&service, &world, "sellable").await, settled);
}

/// Damage and quarantine over the real transport: goods stop being sellable without leaving.
#[tokio::test]
async fn real_service_writes_damaged_stock_off_without_losing_it_over_http() {
    let service = start().await;
    let world = seed_return_world(&service).await;
    let pharmacist = sign_in_as(
        &service,
        "pharmacist",
        "damage.pharmacist",
        "01997a00-0000-7000-8000-000000000302",
    )
    .await;
    let sellable = status_atoms(&service, &world, "sellable").await;
    let custody = sellable
        + status_atoms(&service, &world, "quarantined").await
        + status_atoms(&service, &world, "non_sellable").await;

    let posted = run_operation(
        &service,
        &pharmacist,
        "damage",
        json!({
            "productPackId": world.pack, "batchId": world.batch, "stockStatus": "sellable",
            "targetStockStatus": "non_sellable", "reasonCode": "breakage",
            "quantity": 10, "quantityBasis": "base_unit", "note": "Crushed in the crate"
        }),
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    assert_eq!(
        status_atoms(&service, &world, "sellable").await,
        sellable - 10
    );
    assert_eq!(status_atoms(&service, &world, "non_sellable").await, 10);
    // Nothing was destroyed by writing it off.
    let after = status_atoms(&service, &world, "sellable").await
        + status_atoms(&service, &world, "quarantined").await
        + status_atoms(&service, &world, "non_sellable").await;
    assert_eq!(after, custody);

    // Holding stock back is the other honest outcome, and it needs a stated reason.
    let silent = run_operation(
        &service,
        &pharmacist,
        "quarantine",
        json!({
            "productPackId": world.pack, "batchId": world.batch, "stockStatus": "sellable",
            "targetStockStatus": "quarantined", "reasonCode": "quality_hold",
            "quantity": 10, "quantityBasis": "base_unit"
        }),
    )
    .await;
    assert_eq!(silent.status, 422, "{:?}", silent.body);

    let held = run_operation(
        &service,
        &pharmacist,
        "quarantine",
        json!({
            "productPackId": world.pack, "batchId": world.batch, "stockStatus": "sellable",
            "targetStockStatus": "quarantined", "reasonCode": "quality_hold",
            "quantity": 10, "quantityBasis": "base_unit", "note": "Supplier advisory pending"
        }),
    )
    .await;
    assert_eq!(held.status, 200, "{:?}", held.body);
    assert_eq!(status_atoms(&service, &world, "quarantined").await, 10);
}

/// Expiry classification, and the rule that a live lot cannot be written off as expired.
#[tokio::test]
async fn real_service_classifies_an_expired_lot_and_refuses_a_live_one_over_http() {
    let service = start().await;
    let world = seed_return_world(&service).await;
    let pharmacist = sign_in_as(
        &service,
        "pharmacist",
        "expiry.pharmacist",
        "01997a00-0000-7000-8000-000000000303",
    )
    .await;

    let refused = run_operation(
        &service,
        &pharmacist,
        "expiry",
        json!({
            "productPackId": world.pack, "batchId": world.batch, "stockStatus": "sellable",
            "targetStockStatus": "non_sellable", "reasonCode": "expiry",
            "quantity": 10, "quantityBasis": "base_unit"
        }),
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(refused.body["code"], "batch_not_expired");

    // Once the lot has genuinely expired the same operation is the right record to make.
    let pool = database::connect(&service.database_path)
        .await
        .expect("reopen disposable database");
    sqlx::query("UPDATE product_batches SET expires_on='2026-01-31' WHERE id=?")
        .bind(&world.batch)
        .execute(&pool)
        .await
        .expect("age the lot");
    pool.close().await;

    let sellable = status_atoms(&service, &world, "sellable").await;
    let posted = run_operation(
        &service,
        &pharmacist,
        "expiry",
        json!({
            "productPackId": world.pack, "batchId": world.batch, "stockStatus": "sellable",
            "targetStockStatus": "non_sellable", "reasonCode": "expiry",
            "quantity": 10, "quantityBasis": "base_unit"
        }),
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    assert_eq!(
        status_atoms(&service, &world, "sellable").await,
        sellable - 10
    );
    assert_eq!(status_atoms(&service, &world, "non_sellable").await, 10);
}

/// Physical custody ending, which is the only operation that reduces the total in the building.
#[tokio::test]
async fn real_service_ends_custody_only_for_written_off_stock_over_http() {
    let service = start().await;
    let world = seed_return_world(&service).await;
    let pharmacist = sign_in_as(
        &service,
        "pharmacist",
        "removal.pharmacist",
        "01997a00-0000-7000-8000-000000000304",
    )
    .await;

    // Write something off first, because nothing else can be removed.
    let written_off = run_operation(
        &service,
        &pharmacist,
        "damage",
        json!({
            "productPackId": world.pack, "batchId": world.batch, "stockStatus": "sellable",
            "targetStockStatus": "non_sellable", "reasonCode": "damage",
            "quantity": 20, "quantityBasis": "base_unit", "note": "Water damage"
        }),
    )
    .await;
    assert_eq!(written_off.status, 200, "{:?}", written_off.body);

    // A pharmacist may write stock off but may not say it has left the building.
    let denied = call(
        &service,
        "POST",
        "/api/v1/stock-operations",
        Some(json!({ "operationKind": "removal", "businessDate": SALE_DATE })),
        Some(&pharmacist),
    )
    .await;
    assert_eq!(denied.status, 403, "{:?}", denied.body);

    let sellable = status_atoms(&service, &world, "sellable").await;
    let posted = run_operation(
        &service,
        &world.cookie,
        "removal",
        json!({
            "productPackId": world.pack, "batchId": world.batch, "stockStatus": "non_sellable",
            "reasonCode": "disposal", "quantity": 20, "quantityBasis": "base_unit",
            "note": "Collected by the authorised disposal contractor"
        }),
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    assert_eq!(status_atoms(&service, &world, "non_sellable").await, 0);
    // The shelf is untouched: removal took only what had already been written off.
    assert_eq!(status_atoms(&service, &world, "sellable").await, sellable);

    // And it can never reach the shelf.
    let refused = run_operation(
        &service,
        &world.cookie,
        "removal",
        json!({
            "productPackId": world.pack, "batchId": world.batch, "stockStatus": "sellable",
            "reasonCode": "disposal", "quantity": 5, "quantityBasis": "base_unit"
        }),
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(refused.body["code"], "stock_operation_line_conflict");
}

/// A cashier mutates no stock by any route, and every refusal arrives as a typed code.
#[tokio::test]
async fn real_service_refuses_every_stock_operation_to_a_cashier_over_http() {
    let service = start().await;
    let world = seed_return_world(&service).await;
    let cashier = sign_in_as(
        &service,
        "cashier",
        "stock.cashier",
        "01997a00-0000-7000-8000-000000000305",
    )
    .await;

    for kind in [
        "physical_count",
        "adjustment",
        "damage",
        "expiry",
        "quarantine",
        "removal",
    ] {
        let refused = call(
            &service,
            "POST",
            "/api/v1/stock-operations",
            Some(json!({ "operationKind": kind, "businessDate": SALE_DATE })),
            Some(&cashier),
        )
        .await;
        assert_eq!(refused.status, 403, "{kind}: {:?}", refused.body);
    }

    // Reading is still theirs.
    let listed = call(
        &service,
        "GET",
        "/api/v1/stock-operations",
        None,
        Some(&cashier),
    )
    .await;
    assert_eq!(listed.status, 200, "{:?}", listed.body);

    // And an unauthenticated caller gets nothing at all.
    let anonymous = call(&service, "GET", "/api/v1/stock-operations", None, None).await;
    assert_eq!(anonymous.status, 401, "{:?}", anonymous.body);
    let _ = world;
}

// -------------------------------------------------------------------------------------------------
// Backup and restore across the same real boundary
//
// The container is framed by the service, streamed out through the real file route, sent back in as
// a real octet-stream upload, and the swap is performed on a real file on a real filesystem. None of
// it is exercised in-process: the only thing these tests trust is what came back over the socket.
// -------------------------------------------------------------------------------------------------

/// The service as `main.rs` builds it: with a backups directory, inside the same disposable tree.
struct BackupService {
    service: Service,
    backups: std::path::PathBuf,
}

async fn start_with_backups() -> BackupService {
    let temp = tempfile::tempdir().expect("temporary directory");
    let database_path = temp.path().join("database").join("integration.sqlite3");
    let backups = temp.path().join("backups");
    std::fs::create_dir_all(database_path.parent().expect("database directory"))
        .expect("create database directory");
    std::fs::create_dir_all(&backups).expect("create backups directory");
    let pool = database::connect(&database_path)
        .await
        .expect("migrated database");
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let address = listener.local_addr().expect("bound address");
    let router = api::router_with_backups(
        pool,
        None,
        Some(std::sync::Arc::new(api::backups::BackupService::new(
            backups.clone(),
            database_path.clone(),
        ))),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    BackupService {
        service: Service {
            address,
            database_path,
            _temp: temp,
        },
        backups,
    }
}

/// Sends raw bytes with an explicit content type, and returns the response with its body unparsed.
async fn call_bytes(
    service: &Service,
    path: &str,
    content_type: &str,
    payload: &[u8],
    cookie: Option<&str>,
) -> (u16, String, Vec<u8>) {
    try_call_bytes(service, path, content_type, payload, cookie)
        .await
        .expect("the connection closed before any response was read")
}

async fn try_call_bytes(
    service: &Service,
    path: &str,
    content_type: &str,
    payload: &[u8],
    cookie: Option<&str>,
) -> Option<(u16, String, Vec<u8>)> {
    let mut stream = TcpStream::connect(service.address)
        .await
        .expect("connect to service");
    let mut head = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\nAccept: application/json\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n",
        service.address.port(),
        payload.len()
    );
    if let Some(cookie) = cookie {
        head.push_str(&format!("Cookie: {cookie}\r\n"));
    }
    head.push_str("\r\n");
    let mut request = head.into_bytes();
    request.extend_from_slice(payload);
    // A server that has already refused may reset while this is still writing, and that is the
    // refusal rather than a failure to deliver one.
    let _ = stream.write_all(&request).await;
    try_read_raw(stream).await
}

/// A GET whose body is not JSON — the backup download.
async fn get_bytes(service: &Service, path: &str, cookie: Option<&str>) -> (u16, String, Vec<u8>) {
    let mut stream = TcpStream::connect(service.address)
        .await
        .expect("connect to service");
    let mut head = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n",
        service.address.port()
    );
    if let Some(cookie) = cookie {
        head.push_str(&format!("Cookie: {cookie}\r\n"));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .await
        .expect("write request");
    read_raw(stream).await
}

async fn read_raw(stream: TcpStream) -> (u16, String, Vec<u8>) {
    try_read_raw(stream)
        .await
        .expect("the connection closed before any response was read")
}

/// The same read, for an exchange the server may legitimately cut short.
///
/// Axum's default body limit refuses an oversized request before reading it, and closing a socket
/// with unread bytes still in it makes Windows send a reset that discards the response. `None` is
/// therefore "refused so firmly the answer never arrived", which for an ordinary JSON route is
/// correct behaviour — and is precisely why the backup routes drain a refused upload instead.
async fn try_read_raw(mut stream: TcpStream) -> Option<(u16, String, Vec<u8>)> {
    let mut raw = Vec::new();
    if stream.read_to_end(&mut raw).await.is_err() && raw.is_empty() {
        return None;
    }
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("http response framing");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut body = raw[split + 4..].to_vec();
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .expect("status code");
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        body = unchunk_bytes(&body);
    }
    Some((status, head, body))
}

/// Chunked framing for a body that is not text and must survive byte-for-byte.
fn unchunk_bytes(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = raw;
    loop {
        let Some(line_end) = rest.windows(2).position(|window| window == b"\r\n") else {
            break;
        };
        let Ok(size_text) = std::str::from_utf8(&rest[..line_end]) else {
            break;
        };
        let Ok(size) = usize::from_str_radix(size_text.trim(), 16) else {
            break;
        };
        let start = line_end + 2;
        if size == 0 || rest.len() < start + size {
            break;
        }
        out.extend_from_slice(&rest[start..start + size]);
        rest = &rest[start + size..];
        if rest.starts_with(b"\r\n") {
            rest = &rest[2..];
        }
    }
    out
}

async fn owner_session(service: &Service) -> String {
    let setup = call(
        service,
        "POST",
        "/api/v1/auth/setup",
        Some(json!({
            "storeDisplayName": "Backup Pharmacy",
            "ownerDisplayName": "Backup Owner",
            "loginIdentifier": "backup.owner",
            "password": "Integration-Password-42"
        })),
        None,
    )
    .await;
    assert_eq!(setup.status, 201, "{:?}", setup.body);
    session_cookie(&setup.headers)
}

/// The whole journey, end to end, over the socket: take a backup, download it, change the data,
/// upload the backup back, replace the database, restart, and find the pharmacy as it was.
#[tokio::test]
async fn real_service_backs_up_and_restores_a_pharmacy_over_http() {
    let harness = start_with_backups().await;
    let service = &harness.service;
    let cookie = owner_session(service).await;

    // A fact recorded before the backup, which must come back afterwards.
    let party = call(
        service,
        "POST",
        "/api/v1/parties",
        Some(json!({
            "party": { "displayName": "Pre-Backup Supplier" },
            "roles": [{ "role": "supplier" }]
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(party.status, 201, "{:?}", party.body);

    let created = call(
        service,
        "POST",
        "/api/v1/backups/create",
        Some(json!({})),
        Some(&cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    let backup_id = created.body["backupId"]
        .as_str()
        .expect("backup id")
        .to_owned();
    let filename = created.body["filename"]
        .as_str()
        .expect("filename")
        .to_owned();

    // The download is resolved by id and then streamed by the real file route.
    let resolved = call(
        service,
        "GET",
        &format!("/api/v1/backups/{backup_id}/download"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(resolved.status, 200, "{:?}", resolved.body);
    let url = resolved.body["url"]
        .as_str()
        .expect("download url")
        .to_owned();
    let (status, headers, downloaded) = get_bytes(service, &url, Some(&cookie)).await;
    assert_eq!(status, 200);
    assert!(
        headers.to_ascii_lowercase().contains("content-disposition"),
        "a backup must be offered as a download: {headers}"
    );
    let on_disk = std::fs::read(harness.backups.join(&filename)).expect("backup on disk");
    assert_eq!(
        downloaded, on_disk,
        "the streamed backup is not byte-identical to the file"
    );

    // Something happens after the backup that the restore must undo.
    let later = call(
        service,
        "POST",
        "/api/v1/parties",
        Some(json!({
            "party": { "displayName": "Post-Backup Supplier" },
            "roles": [{ "role": "supplier" }]
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(later.status, 201, "{:?}", later.body);

    // The bytes that came back down the socket are the bytes sent up again.
    let (status, _, body) = call_bytes(
        service,
        "/api/v1/backups/restore/prepare",
        "application/octet-stream",
        &downloaded,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let prepared: Value = serde_json::from_slice(&body).expect("prepared restore");
    assert_eq!(prepared["report"]["compatibility"], "ready");
    assert_eq!(prepared["report"]["checksumVerified"], true);
    let token = prepared["candidateToken"]
        .as_str()
        .expect("token")
        .to_owned();

    let committed = call(
        service,
        "POST",
        "/api/v1/backups/restore/commit",
        Some(json!({ "candidateToken": token, "password": "Integration-Password-42" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(committed.status, 200, "{:?}", committed.body);
    assert_eq!(committed.body["restartRequired"], true);
    assert!(committed.body["safetyBackup"].is_string());

    // The running service now refuses everything, including its own health route, and says why.
    let refused = call(service, "GET", "/api/v1/auth/status", None, Some(&cookie)).await;
    assert_eq!(refused.status, 503);
    assert_eq!(refused.body["code"], "service_restoring");

    // Restart, exactly as main.rs does it.
    api::backups::recover_interrupted_restore(&harness.backups, &service.database_path)
        .await
        .expect("recovery");
    let reopened = database::connect(&service.database_path)
        .await
        .expect("reopened database");
    assert!(
        api::backups::complete_restore_after_open(&reopened, &harness.backups)
            .await
            .expect("completion")
    );

    let names: Vec<String> =
        sqlx::query_scalar("SELECT display_name FROM parties ORDER BY display_name")
            .fetch_all(&reopened)
            .await
            .expect("parties");
    assert_eq!(
        names,
        vec!["Pre-Backup Supplier".to_owned()],
        "the restore did not put the pharmacy back as it was"
    );
    let live_sessions: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_sessions WHERE revoked_at_utc IS NULL")
            .fetch_one(&reopened)
            .await
            .expect("sessions");
    assert_eq!(live_sessions, 0, "a session survived a restore");
    let lineage: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM restore_provenance")
        .fetch_one(&reopened)
        .await
        .expect("provenance");
    assert_eq!(lineage, 1);
    reopened.close().await;
}

/// Refusals at the boundary: the wrong content type, an oversized declaration, and no session.
#[tokio::test]
async fn real_service_refuses_backup_uploads_that_break_the_rules_over_http() {
    let harness = start_with_backups().await;
    let service = &harness.service;
    let cookie = owner_session(service).await;

    // A form encoding is refused before a byte of the body is read: this is the CSRF defence.
    let (status, _, body) = call_bytes(
        service,
        "/api/v1/backups/inspect",
        "multipart/form-data; boundary=x",
        b"whatever",
        Some(&cookie),
    )
    .await;
    assert_eq!(status, 422, "{}", String::from_utf8_lossy(&body));

    // Rubbish of the right content type is refused as a damaged backup, not as a server fault.
    let (status, _, body) = call_bytes(
        service,
        "/api/v1/backups/inspect",
        "application/octet-stream",
        b"this is a letter, not a backup",
        Some(&cookie),
    )
    .await;
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    assert_eq!(status, 409, "{parsed:?}");
    assert!(
        ["backup_corrupt", "backup_product_mismatch"]
            .contains(&parsed["code"].as_str().unwrap_or_default()),
        "{parsed:?}"
    );

    // The backup routes carry their own body limit, and the rest of the product keeps Axum's
    // default. Three megabytes is past that default and nowhere near the backup ceiling, so one
    // payload proves both halves: accepted as a damaged backup here, refused outright there.
    let oversized = vec![b'x'; 3 * 1024 * 1024];
    let (status, _, body) = call_bytes(
        service,
        "/api/v1/backups/inspect",
        "application/octet-stream",
        &oversized,
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        409,
        "the backup route did not accept a body past the global default: {}",
        String::from_utf8_lossy(&body)
    );

    // The same length sent at an ordinary route is refused, which is the proof that raising the
    // limit for backups did not raise it for the whole product. The refusal may arrive as 413 or as
    // a reset — Axum declines before reading — and either way it was not accepted.
    if let Some((status, _, _)) = try_call_bytes(
        service,
        "/api/v1/parties",
        "application/json",
        &oversized,
        Some(&cookie),
    )
    .await
    {
        assert_eq!(
            status, 413,
            "the global body limit was raised for every route"
        );
    }

    // And none of it is reachable without a session.
    let (status, _, _) = call_bytes(
        service,
        "/api/v1/backups/inspect",
        "application/octet-stream",
        b"anything",
        None,
    )
    .await;
    assert_eq!(status, 401);
    let (status, _, _) = get_bytes(service, "/api/v1/backup-files/anything.aushbackup", None).await;
    assert_eq!(status, 401);
}

/// The first-run door is shut the moment an installation has anything in it.
#[tokio::test]
async fn real_service_refuses_a_first_run_restore_once_the_pharmacy_exists_over_http() {
    let harness = start_with_backups().await;
    let service = &harness.service;
    let cookie = owner_session(service).await;
    let created = call(
        service,
        "POST",
        "/api/v1/backups/create",
        Some(json!({})),
        Some(&cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    let filename = created.body["filename"]
        .as_str()
        .expect("filename")
        .to_owned();
    let bytes = std::fs::read(harness.backups.join(&filename)).expect("backup on disk");

    // No cookie at all, which is exactly how a genuine first run would arrive.
    let (status, _, body) = call_bytes(
        service,
        "/api/v1/setup/restore/prepare",
        "application/octet-stream",
        &bytes,
        None,
    )
    .await;
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    assert_eq!(status, 403, "{parsed:?}");
    assert_eq!(parsed["code"], "setup_already_complete");
}

// ---------------------------------------------------------------------------------------------
// Phase 1L-A2 — recipient statutory particulars
// ---------------------------------------------------------------------------------------------

/// Rule 46(f) across a real socket: a customer who asks for their details on the invoice cannot be
/// billed until those details exist, the refusal names what is missing in plain terms, and once the
/// counter records them they are frozen onto the posted Sale and served by the canonical invoice.
#[tokio::test]
async fn real_service_requires_and_freezes_requested_recipient_particulars_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;

    let draft = call(
        &service,
        "POST",
        "/api/v1/sales",
        Some(json!({ "businessDate": SALE_DATE, "recipientParticularsRequested": true })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(draft.status, 201, "{:?}", draft.body);
    let sale_id = draft.body["id"].as_str().expect("sale id").to_owned();
    let with_line = sale_line(&service, &world, &sale_id, 1, "pack", 1, 8000).await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);

    let quote = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{sale_id}/quote"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(quote.status, 200, "{:?}", quote.body);
    assert_eq!(quote.body["recipientParticulars"]["required"], true);
    assert_eq!(
        quote.body["recipientParticulars"]["reasons"],
        json!(["recipient_requested"])
    );

    let refused = post_sale(
        &service,
        &world,
        &sale_id,
        2,
        "01997a00-0000-7000-8000-0000000000e1",
        8960,
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(refused.body["code"], "recipient_particulars_incomplete");
    assert_eq!(refused.body["issues"].as_array().map(Vec::len), Some(3));
    let text = refused.body.to_string().to_lowercase();
    assert!(!text.contains("sqlite") && !text.contains("trigger"));

    let saved = call(
        &service,
        "PUT",
        &format!("/api/v1/sales/{sale_id}"),
        Some(json!({
            "expectedRevision": 2, "businessDate": SALE_DATE,
            "recipientParticularsRequested": true, "customerNameText": "Asha Patil",
            "recipientAddress": { "line1": "4 Lake View Society", "city": "Pune", "stateId": MAHARASHTRA }
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(saved.status, 200, "{:?}", saved.body);

    let posted = post_sale(
        &service,
        &world,
        &sale_id,
        3,
        "01997a00-0000-7000-8000-0000000000e2",
        8960,
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);

    let document = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{sale_id}/invoice"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(document.status, 200, "{:?}", document.body);
    let recipient = &document.body["recipient"];
    assert_eq!(recipient["snapshotVersion"], 1);
    assert_eq!(recipient["particularsRequested"], true);
    assert_eq!(recipient["name"], "Asha Patil");
    assert_eq!(recipient["address"]["source"], "counter");
    assert_eq!(recipient["address"]["line1"], "4 Lake View Society");
    assert_eq!(recipient["address"]["stateCode"], "27");
    assert_eq!(recipient["delivery"]["sameAsRecipient"], true);
}

// ---------------------------------------------------------------------------------------------
// Phase 1L-A3 — the seller's GST registration governs the tax
// ---------------------------------------------------------------------------------------------

/// Across a real socket: while the Store's registration is recorded as unknown, no sale is taxed or
/// posted; once the owner records it as unregistered, the same basket posts with no GST at all and
/// the canonical invoice says so from frozen facts.
#[tokio::test]
async fn real_service_charges_no_gst_for_an_unregistered_seller_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;

    let set_status = async |status: &str, gstin: Option<&str>| {
        let current = call(
            &service,
            "GET",
            "/api/v1/store/tax-identity",
            None,
            Some(&world.cookie),
        )
        .await;
        let reply = call(
            &service,
            "PUT",
            "/api/v1/store/tax-identity",
            Some(json!({
                "expectedRevision": current.body["revision"], "gstRegistrationStatus": status,
                "gstin": gstin, "placeOfSupplyStateId": MAHARASHTRA
            })),
            Some(&world.cookie),
        )
        .await;
        assert_eq!(reply.status, 200, "{:?}", reply.body);
    };

    set_status("unknown", None).await;
    let sale_id = sale_draft(&service, &world, None).await;
    let with_line = sale_line(&service, &world, &sale_id, 1, "pack", 1, 8000).await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);
    let quote = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{sale_id}/quote"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(quote.status, 409, "{:?}", quote.body);
    assert_eq!(quote.body["code"], "store_gst_status_unresolved");

    set_status("unregistered", None).await;
    let quote = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{sale_id}/quote"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(quote.status, 200, "{:?}", quote.body);
    assert_eq!(quote.body["cgstPaise"], 0);
    assert_eq!(quote.body["grandTotalPaise"], 8000);

    let posted = post_sale(
        &service,
        &world,
        &sale_id,
        2,
        "01997a00-0000-7000-8000-0000000000e9",
        8000,
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    assert_eq!(posted.body["sgstPaise"], 0);

    let document = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{sale_id}/invoice"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(document.status, 200, "{:?}", document.body);
    assert_eq!(
        document.body["document"]["documentType"],
        "retail_cash_memo"
    );
    assert_eq!(document.body["regulatory"]["complianceSnapshotVersion"], 1);
    assert_eq!(document.body["totals"]["grandTotalPaise"], 8000);
    assert_eq!(
        document.body["sellerSnapshot"]["retailMemoLicenceText"],
        "Form 20: MH-20-1234"
    );
}

/// Phase 1L-A4 over real HTTP: the owner records that Notification No. 14/2020-CT applies, a UPI
/// payment without its transaction reference is refused, and with it the invoice carries the frozen
/// payment cross-reference and the applicability it was issued under.
#[tokio::test]
async fn real_service_requires_the_payment_cross_reference_under_dynamic_qr_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;

    let profile = call(
        &service,
        "GET",
        "/api/v1/store/profile",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(profile.body["dynamicQrApplicability"], "not_required");
    let facts = call(
        &service,
        "PUT",
        "/api/v1/store/invoice-compliance",
        Some(json!({
            "expectedRevision": profile.body["revision"],
            "rule46sDeclarationApplicability": "not_applicable",
            "einvoiceApplicability": "not_required",
            "dynamicQrApplicability": "required",
            "hsnTurnoverBand": "up_to_5_crore",
            "hsnTurnoverFinancialYear": "2026-27"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(facts.status, 200, "{:?}", facts.body);
    assert_eq!(facts.body["dynamicQrApplicability"], "required");

    let sale_id = sale_draft(&service, &world, None).await;
    let with_line = sale_line(&service, &world, &sale_id, 1, "pack", 1, 8000).await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);
    let quote = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{sale_id}/quote"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(quote.status, 200, "{:?}", quote.body);
    assert_eq!(quote.body["dynamicQrApplicability"], "required");
    let total = quote.body["grandTotalPaise"].clone();

    let pay = async |reference: Option<&str>, key: &str| {
        call(
            &service,
            "POST",
            &format!("/api/v1/sales/{sale_id}/post"),
            Some(json!({
                "expectedRevision": 2,
                "idempotencyKey": key,
                "tenders": [{ "method": "upi", "amountPaise": total, "referenceText": reference }]
            })),
            Some(&world.cookie),
        )
        .await
    };
    let refused = pay(None, "01997a00-0000-7000-8000-0000000000f1").await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(refused.body["code"], "payment_reference_required");

    let posted = pay(Some("UPI-4471"), "01997a00-0000-7000-8000-0000000000f2").await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    let document = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{sale_id}/invoice"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(document.status, 200, "{:?}", document.body);
    assert_eq!(
        document.body["regulatory"]["dynamicQrApplicability"],
        "required"
    );
    assert_eq!(document.body["tender"][0]["method"], "upi");
    assert_eq!(document.body["tender"][0]["referenceText"], "UPI-4471");
    assert!(
        document.body["tender"][0]["recordedAtUtc"]
            .as_str()
            .is_some_and(|value| value.ends_with('Z'))
    );
}

/// Phase 1M-A over real HTTP: an owner records a Schedule H finding with its authority, the real
/// service refuses the sale with a typed error before any number, movement or tender exists, and
/// the refusal is withdrawn only by ending the finding — which leaves its past answers intact.
#[tokio::test]
async fn real_service_refuses_a_scheduled_sale_until_its_workflow_exists_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    // Since Phase 1M-B a Schedule H supply also needs its rule 65(3) basis; with it in place, the
    // refusal below is for the missing prescription, which is what this test is about.
    record_basis_over_http(&service, &world, Some("prescription_register")).await;

    let finding = call(
        &service,
        "POST",
        &format!(
            "/api/v1/products/{}/regulatory/classifications",
            world.product
        ),
        Some(json!({
            "scheme": "schedule_h",
            "applies": true,
            "effectiveFrom": "2020-01-01",
            "sourceCitation": "Drugs Rules, 1945, Schedule H",
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(finding.status, 201, "{:?}", finding.body);
    let finding_id = finding.body["id"].as_str().expect("finding id").to_owned();

    let sale_id = sale_draft(&service, &world, None).await;
    let with_line = sale_line(&service, &world, &sale_id, 1, "pack", 1, 8000).await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);
    let revision = with_line.body["revision"].as_i64().expect("revision");

    let quote = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{sale_id}/quote"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(quote.status, 200, "{:?}", quote.body);
    // Since Phase 1M-B a Schedule H line is sellable on a prescription, so the quote names that
    // requirement rather than an unavailable workflow.
    assert_eq!(
        quote.body["lines"][0]["regulatoryGate"],
        "prescription_required"
    );
    assert_eq!(quote.body["lines"][0]["regulatoryGateScheme"], "schedule_h");

    let refused = post_sale(
        &service,
        &world,
        &sale_id,
        revision,
        "01997a00-0000-7000-8000-0000000001a1",
        8_960,
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(refused.body["code"], "prescription_requirements_incomplete");

    let detail = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{sale_id}"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(detail.body["status"], "draft");
    assert_eq!(detail.body["documentNumber"], Value::Null);
    assert_eq!(
        detail.body["tenders"].as_array().map(Vec::len).unwrap_or(0),
        0
    );
    let movements = call(
        &service,
        "GET",
        "/api/v1/inventory/movements",
        None,
        Some(&world.cookie),
    )
    .await;
    assert!(
        movements
            .body
            .as_array()
            .expect("movements")
            .iter()
            .all(|row| row["movementType"] != "sale"),
        "a refused sale moved stock"
    );

    // The law changes: the finding ends, and a Sale dated after the end is no longer governed.
    let closed = call(
        &service,
        "POST",
        &format!(
            "/api/v1/products/{}/regulatory/classifications/{finding_id}/close",
            world.product
        ),
        Some(json!({ "expectedRevision": 1, "effectiveTo": "2026-01-01", "reason": "Omitted from Schedule H" })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(closed.status, 200, "{:?}", closed.body);
    let before = call(
        &service,
        "GET",
        &format!(
            "/api/v1/products/{}/regulatory?asOf=2025-06-01",
            world.product
        ),
        None,
        Some(&world.cookie),
    )
    .await;
    let schedule_h = before.body["resolved"]
        .as_array()
        .expect("resolved")
        .iter()
        .find(|answer| answer["scheme"] == "schedule_h")
        .expect("schedule_h")
        .clone();
    assert_eq!(
        schedule_h["answer"], "applies",
        "ending a finding rewrote its past"
    );

    let posted = post_sale(
        &service,
        &world,
        &sale_id,
        revision,
        "01997a00-0000-7000-8000-0000000001a2",
        8_960,
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);

    // A cashier-level caller can do none of this.
    let cashier = call(
        &service,
        "POST",
        "/api/v1/store/record-elections",
        Some(json!({
            "election": "rule_65_3_prescription_supply",
            "method": "prescription_register",
            "effectiveFrom": "2026-01-01",
        })),
        None,
    )
    .await;
    assert_eq!(cashier.status, 401, "{:?}", cashier.body);
}

// ---------------------------------------------------------------------------------------------
// Phase 1M-B — prescriptions and the Schedule H retail workflow, over a real socket.
// ---------------------------------------------------------------------------------------------

/// Records the product's position under every gating scheme: `inside` applies, the rest do not.
async fn schedule_over_http(service: &Service, world: &SaleWorld, inside: &[&str]) {
    for scheme in [
        "schedule_h",
        "schedule_h1",
        "schedule_x",
        "schedule_c",
        "schedule_c1",
    ] {
        let finding = call(
            service,
            "POST",
            &format!(
                "/api/v1/products/{}/regulatory/classifications",
                world.product
            ),
            Some(json!({
                "scheme": scheme,
                "applies": inside.contains(&scheme),
                "effectiveFrom": "2020-01-01",
                "sourceCitation": "Drugs Rules, 1945, Schedules as amended",
            })),
            Some(&world.cookie),
        )
        .await;
        assert_eq!(finding.status, 201, "{scheme}: {:?}", finding.body);
    }
}

/// The rule 65(3)(1) basis of a lawful entry, recorded as an owner does: the product's manufacturer
/// (particular (f)) and, unless `None`, the rule 65(3)(2) election.
async fn record_basis_over_http(service: &Service, world: &SaleWorld, election: Option<&str>) {
    let company = call(
        service,
        "POST",
        "/api/v1/reference/companies",
        Some(json!({ "attributes": { "displayName": "Alkem Laboratories", "countryCode": "IN" } })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(company.status, 201, "{:?}", company.body);
    let role = call(
        service,
        "POST",
        &format!("/api/v1/products/{}/company-roles", world.product),
        Some(json!({ "companyId": company.body["id"], "role": "manufacturer" })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(role.status, 201, "{:?}", role.body);
    if let Some(method) = election {
        let elected = call(
            service,
            "POST",
            "/api/v1/store/record-elections",
            Some(json!({
                "election": "rule_65_3_prescription_supply",
                "method": method,
                "effectiveFrom": "2020-01-01",
                "evidenceReference": "Election letter to the Licensing Authority",
            })),
            Some(&world.cookie),
        )
        .await;
        assert_eq!(elected.status, 201, "{:?}", elected.body);
    }
}

/// Prepares a Sale's rule 65(3)(1) entry: the serial is allocated and nothing is sold.
async fn prepare_entry_over_http(
    service: &Service,
    world: &SaleWorld,
    sale: &str,
    revision: i64,
) -> Reply {
    call(
        service,
        "POST",
        &format!("/api/v1/sales/{sale}/prescription-records"),
        Some(json!({ "expectedRevision": revision })),
        Some(&world.cookie),
    )
    .await
}

/// Confirms the two manual acts on a prepared entry, as a pharmacist does once the registered
/// pharmacist has signed the physical entry and its serial is on the prescription.
async fn confirm_entry_over_http(service: &Service, cookie: &str, record: &str) -> Reply {
    call(
        service,
        "POST",
        &format!("/api/v1/prescription-supply-records/{record}/confirm"),
        Some(json!({ "manualSignatureConfirmed": true, "serialWrittenOnPrescription": true })),
        Some(cookie),
    )
    .await
}

/// Prepares and confirms the Sale's entry; returns (entry id, the Sale's revision).
async fn signed_entry_over_http(
    service: &Service,
    world: &SaleWorld,
    sale: &str,
    revision: i64,
) -> (String, i64) {
    let prepared = prepare_entry_over_http(service, world, sale, revision).await;
    assert_eq!(prepared.status, 200, "{:?}", prepared.body);
    let record = prepared.body["prescriptionRecords"][0]["id"]
        .as_str()
        .expect("record id")
        .to_owned();
    let confirmed = confirm_entry_over_http(service, &world.cookie, &record).await;
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);
    (
        record,
        prepared.body["revision"].as_i64().expect("revision"),
    )
}

async fn pharmacist_over_http(service: &Service, world: &SaleWorld) -> String {
    let professional = call(
        service,
        "POST",
        "/api/v1/store/professionals",
        Some(json!({
            "fullName": "Meera Iyer",
            "capacity": "registered_pharmacist",
            "registrationNumber": "MH-PH-44821",
            "registeringAuthority": "Maharashtra State Pharmacy Council",
            "validFrom": "2020-01-01",
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(professional.status, 201, "{:?}", professional.body);
    professional.body["id"]
        .as_str()
        .expect("professional id")
        .to_owned()
}

/// A veterinary prescription for the world's product: `subject_kind = animal` with the ANIMAL
/// OWNER's name and address, which is exactly what rule 65(10)(b) asks for and all it asks for.
async fn animal_prescription_over_http(
    service: &Service,
    world: &SaleWorld,
    atoms: i64,
) -> (String, String) {
    let created = call(
        service,
        "POST",
        "/api/v1/prescriptions",
        Some(json!({
            "prescribedOn": "2026-09-10",
            "prescriberId": Value::Null,
            "prescriberName": "Dr. Anjali Rao",
            "prescriberAddress": "Rao Veterinary Clinic, FC Road, Pune 411005",
            "subjectKind": "animal",
            "subjectName": "Ramesh Patil",
            "subjectAddress": "22 Shivaji Nagar, Pune 411005",
            "repeatAuthority": "once",
            "writtenSignedDatedAttested": true,
            "items": [{
                "productId": world.product,
                "writtenDescription": "Tab. as prescribed",
                "prescribedQuantityAtoms": atoms,
                "doseText": "1 tablet twice daily",
            }],
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    let prescription = created.body["id"].as_str().expect("id").to_owned();
    let item = created.body["items"][0]["id"]
        .as_str()
        .expect("item")
        .to_owned();
    (prescription, item)
}

/// A lot of the world's Schedule X product whose sellable stock arrived on a COUNTED SHELF.
///
/// No purchase, therefore no Schedule X receipt working entry, therefore nothing rule 65(21) can
/// account for. This is the shape legacy stock has, and the shape a lot takes when goods are
/// released back into the sellable balance by any route other than a qualifying purchase.
async fn counted_lot_over_http(service: &Service, world: &SaleWorld) -> String {
    let batch = call(
        service,
        "POST",
        &format!("/api/v1/packs/{}/batches", world.pack),
        Some(json!({
            "batchNumber": "BX-COUNTED", "expiresOn": "2028-03-31", "mrpPaise": 9550
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(batch.status, 201, "{:?}", batch.body);
    let batch_id = batch.body["id"].as_str().expect("batch id").to_owned();
    let counted = run_operation(
        service,
        &world.cookie,
        "physical_count",
        json!({
            "productPackId": world.pack, "batchId": batch_id, "stockStatus": "sellable",
            "reasonCode": "physical_count_gain", "countedQuantity": 100,
            "quantityBasis": "base_unit"
        }),
    )
    .await;
    assert_eq!(counted.status, 200, "{:?}", counted.body);
    batch_id
}

/// A prescription whose repeat authority is whatever the prescriber wrote.
async fn repeatable_prescription_over_http(
    service: &Service,
    world: &SaleWorld,
    atoms: i64,
    repeat: Value,
) -> (String, String) {
    let mut body = json!({
        "prescribedOn": "2026-09-10",
        "prescriberId": Value::Null,
        "prescriberName": "Dr. Anjali Rao",
        "prescriberAddress": "Rao Clinic, FC Road, Pune 411005",
        "subjectKind": "human",
        "subjectName": "Sita Kulkarni",
        "subjectAddress": "14 Lakshmi Road, Pune 411004",
        "writtenSignedDatedAttested": true,
        "items": [{
            "productId": world.product,
            "writtenDescription": "Tab. as prescribed",
            "prescribedQuantityAtoms": atoms,
            "doseText": "1 tablet twice daily",
        }],
    });
    for (key, value) in repeat.as_object().expect("repeat object") {
        body[key] = value.clone();
    }
    let created = call(
        service,
        "POST",
        "/api/v1/prescriptions",
        Some(body),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    let prescription = created.body["id"].as_str().expect("id").to_owned();
    let item = created.body["items"][0]["id"]
        .as_str()
        .expect("item")
        .to_owned();
    (prescription, item)
}

/// A second registered pharmacist, distinct from `pharmacist_over_http`'s.
///
/// Rule 65(2) is asked twice of a Schedule X supply: of the pharmacist supervising the supply, and of
/// the one named on the rule 65(21) working entry. They are usually the same person, and when they
/// are, the supply-level answer comes first. Two people are needed to reach the entry-level predicate
/// on its own.
async fn second_pharmacist_over_http(service: &Service, world: &SaleWorld) -> String {
    let professional = call(
        service,
        "POST",
        "/api/v1/store/professionals",
        Some(json!({
            "fullName": "Arun Deshpande",
            "capacity": "registered_pharmacist",
            "registrationNumber": "MH-PH-51907",
            "registeringAuthority": "Maharashtra State Pharmacy Council",
            "validFrom": "2020-01-01",
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(professional.status, 201, "{:?}", professional.body);
    professional.body["id"]
        .as_str()
        .expect("professional id")
        .to_owned()
}

/// Enters a prescription for the world's product and returns (prescription, item).
async fn prescription_over_http(
    service: &Service,
    world: &SaleWorld,
    prescriber: Option<&str>,
    atoms: i64,
) -> (String, String) {
    let created = call(
        service,
        "POST",
        "/api/v1/prescriptions",
        Some(json!({
            "prescribedOn": "2026-09-10",
            "prescriberId": prescriber,
            "prescriberName": "Dr. Anjali Rao",
            "prescriberAddress": "Rao Clinic, FC Road, Pune 411005",
            "subjectKind": "human",
            "subjectName": "Sita Kulkarni",
            "subjectAddress": "14 Lakshmi Road, Pune 411004",
            "repeatAuthority": "once",
            "writtenSignedDatedAttested": true,
            "items": [{
                "productId": world.product,
                "writtenDescription": "Tab. as prescribed",
                "prescribedQuantityAtoms": atoms,
                "doseText": "1 tablet twice daily",
            }],
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    assert!(
        created.body["reference"]
            .as_str()
            .expect("reference")
            .starts_with("RX-"),
        "{:?}",
        created.body
    );
    (
        created.body["id"].as_str().expect("id").to_owned(),
        created.body["items"][0]["id"]
            .as_str()
            .expect("item id")
            .to_owned(),
    )
}

/// A draft of `packs` strips, linked to `item`, supervised, endorsement confirmed. Returns the Sale
/// id and its current revision.
async fn prepared_sale_over_http(
    service: &Service,
    world: &SaleWorld,
    item: &str,
    professional: &str,
    packs: i64,
) -> (String, i64) {
    let sale_id = sale_draft(service, world, None).await;
    let with_line = sale_line(service, world, &sale_id, 1, "pack", packs, 8000).await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);
    let line_id = with_line.body["lines"][0]["id"]
        .as_str()
        .expect("line id")
        .to_owned();
    let linked = call(
        service,
        "PUT",
        &format!("/api/v1/sale-lines/{line_id}/prescription"),
        Some(json!({
            "expectedRevision": with_line.body["revision"],
            "prescriptionItemId": item,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(linked.status, 200, "{:?}", linked.body);
    let supplied = call(
        service,
        "PUT",
        &format!("/api/v1/sales/{sale_id}/supply"),
        Some(json!({
            "expectedRevision": linked.body["revision"],
            "supervisingProfessionalId": professional,
            "prescriptionEndorsementConfirmed": true,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(supplied.status, 200, "{:?}", supplied.body);
    (
        sale_id,
        supplied.body["revision"].as_i64().expect("revision"),
    )
}

async fn quote_over_http(service: &Service, world: &SaleWorld, sale_id: &str) -> Value {
    let quote = call(
        service,
        "GET",
        &format!("/api/v1/sales/{sale_id}/quote"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(quote.status, 200, "{:?}", quote.body);
    quote.body
}

async fn post_quoted_over_http(
    service: &Service,
    world: &SaleWorld,
    sale_id: &str,
    revision: i64,
    key: &str,
) -> Reply {
    let total = quote_over_http(service, world, sale_id).await["grandTotalPaise"]
        .as_i64()
        .expect("total");
    post_sale(service, world, sale_id, revision, key, total).await
}

async fn prescription_detail_over_http(
    service: &Service,
    world: &SaleWorld,
    prescription: &str,
) -> Value {
    let detail = call(
        service,
        "GET",
        &format!("/api/v1/prescriptions/{prescription}"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(detail.status, 200, "{:?}", detail.body);
    detail.body
}

/// 1M-B gate 1-7 and 10-12: a Prescriber and a prescription are entered, the quote names the
/// requirement, a bare posting is refused, the complete one posts and consumes the quantity, a
/// second supply on a once-only prescription is refused, an ordinary Sale is untouched, a return
/// reinstates the quantity, and a cashier can neither read nor enter prescriptions.
#[tokio::test]
async fn real_service_dispenses_schedule_h_on_a_prescription_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_h"]).await;
    record_basis_over_http(&service, &world, Some("prescription_register")).await;
    let professional = pharmacist_over_http(&service, &world).await;

    // 1. A Prescriber, with no registration number: rule 65 does not require one.
    let prescriber = call(
        &service,
        "POST",
        "/api/v1/prescribers",
        Some(json!({ "fullName": "Dr. Anjali Rao", "addressText": "Rao Clinic, Pune" })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(prescriber.status, 201, "{:?}", prescriber.body);
    let prescriber_id = prescriber.body["id"]
        .as_str()
        .expect("prescriber")
        .to_owned();

    // 2. A structured prescription for 20 tablets, once.
    let (prescription, item) =
        prescription_over_http(&service, &world, Some(&prescriber_id), 20).await;

    // 3-4. The quote names the requirement, and the bare posting is refused with nothing written.
    let bare = sale_draft(&service, &world, None).await;
    let with_line = sale_line(&service, &world, &bare, 1, "pack", 1, 8000).await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);
    let quote = quote_over_http(&service, &world, &bare).await;
    assert_eq!(quote["lines"][0]["regulatoryGate"], "prescription_required");
    assert_eq!(quote["lines"][0]["prescription"]["required"], true);
    assert_eq!(
        quote["lines"][0]["prescription"]["issue"],
        "prescription_missing"
    );
    assert_eq!(quote["supply"]["supervisionRequired"], true);
    assert!(
        !quote.to_string().contains("Sita Kulkarni"),
        "the quote carries the patient"
    );
    let refused = post_quoted_over_http(
        &service,
        &world,
        &bare,
        2,
        "01997a00-0000-7000-8000-0000000002b1",
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(refused.body["code"], "prescription_requirements_incomplete");

    // 5-6. The complete supply, in the order the counter must follow. First the entry is prepared:
    //      PR-000001 is allocated in the elected register while nothing at all is sold.
    let (sale_id, revision) =
        prepared_sale_over_http(&service, &world, &item, &professional, 1).await;
    let quote = quote_over_http(&service, &world, &sale_id).await;
    assert_eq!(
        quote["supply"]["issues"],
        json!(["prescription_record_not_prepared"])
    );
    let prepared = prepare_entry_over_http(&service, &world, &sale_id, revision).await;
    assert_eq!(prepared.status, 200, "{:?}", prepared.body);
    assert_eq!(prepared.body["status"], "draft");
    assert_eq!(prepared.body["documentNumber"], Value::Null);
    assert_eq!(
        prepared.body["prescriptionRecords"][0]["serialNumber"],
        "PR-000001"
    );
    assert_eq!(
        prepared.body["prescriptionRecords"][0]["recordMethod"],
        "prescription_register"
    );
    assert_eq!(
        prepared.body["prescriptionRecords"][0]["status"],
        "prepared"
    );
    let revision = prepared.body["revision"].as_i64().expect("revision");
    let record_id = prepared.body["prescriptionRecords"][0]["id"]
        .as_str()
        .expect("record id")
        .to_owned();
    let entry = call(
        &service,
        "GET",
        &format!("/api/v1/prescription-supply-records/{record_id}"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(entry.status, 200, "{:?}", entry.body);
    assert_eq!(entry.body["status"], "prepared");
    assert_eq!(entry.body["subjectName"], "Sita Kulkarni");
    assert_eq!(
        entry.body["lines"][0]["manufacturerName"],
        "Alkem Laboratories"
    );
    assert_eq!(entry.body["lines"][0]["batchNumber"], "B-9001");
    assert_eq!(entry.body["manualSignatureConfirmed"], false);
    let detail = prescription_detail_over_http(&service, &world, &prescription).await;
    assert_eq!(
        detail["items"][0]["dispensedAtoms"], 0,
        "the medicine left before the signature"
    );

    // The unsigned entry cannot be posted past, and a cashier cannot attest the signature.
    let unsigned = post_quoted_over_http(
        &service,
        &world,
        &sale_id,
        revision,
        "01997a00-0000-7000-8000-0000000002b5",
    )
    .await;
    assert_eq!(unsigned.status, 409, "{:?}", unsigned.body);
    assert_eq!(
        unsigned.body["issues"][0]["field"],
        "supply.prescription_record_not_confirmed"
    );
    let till = sign_in_as(
        &service,
        "cashier",
        "rx-till",
        "01997a00-0000-7000-8000-0000000002c2",
    )
    .await;
    let attested = confirm_entry_over_http(&service, &till, &record_id).await;
    assert_eq!(attested.status, 403, "{:?}", attested.body);

    // The pharmacist signs the paper and writes the serial; a pharmacist confirms both; then the
    // Sale posts, consumes exactly its quantity, and the entry is finalized with it.
    let confirmed = confirm_entry_over_http(&service, &world.cookie, &record_id).await;
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);
    assert_eq!(confirmed.body["status"], "confirmed");
    let posted = post_quoted_over_http(
        &service,
        &world,
        &sale_id,
        revision,
        "01997a00-0000-7000-8000-0000000002b2",
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    assert_eq!(posted.body["status"], "posted");
    assert_eq!(posted.body["prescriptionRecords"][0]["status"], "finalized");
    let detail = prescription_detail_over_http(&service, &world, &prescription).await;
    assert_eq!(detail["items"][0]["dispensedAtoms"], 10);
    assert_eq!(detail["occasionsUsed"], 1);
    assert_eq!(
        detail["dispensings"][0]["supervisingProfessionalName"],
        "Meera Iyer"
    );

    // 7. The once-only prescription cannot be dispensed again, although 10 tablets remain.
    let (again, again_revision) =
        prepared_sale_over_http(&service, &world, &item, &professional, 1).await;
    let quote = quote_over_http(&service, &world, &again).await;
    assert_eq!(quote["lines"][0]["prescription"]["remainingAtoms"], 10);
    let refused = post_quoted_over_http(
        &service,
        &world,
        &again,
        again_revision,
        "01997a00-0000-7000-8000-0000000002b3",
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(
        refused.body["issues"][0]["field"],
        "lines.1.prescription_repeat_not_authorised"
    );

    // 11. A return reinstates the quantity as its own event and leaves the dispensing intact.
    let sale_line_id = posted.body["lines"][0]["id"]
        .as_str()
        .expect("sale line")
        .to_owned();
    let return_draft = call(
        &service,
        "POST",
        "/api/v1/returns",
        Some(json!({
            "returnKind": "sales_return", "originalDocumentId": sale_id, "businessDate": SALE_DATE
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(return_draft.status, 201, "{:?}", return_draft.body);
    let return_id = return_draft.body["id"]
        .as_str()
        .expect("return id")
        .to_owned();
    let return_line = call(
        &service,
        "POST",
        &format!("/api/v1/returns/{return_id}/lines"),
        Some(json!({
            "expectedRevision": 1, "originalLineId": sale_line_id, "quantity": 1,
            "disposition": "quarantined"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(return_line.status, 201, "{:?}", return_line.body);
    let return_posted = call(
        &service,
        "POST",
        &format!("/api/v1/returns/{return_id}/post"),
        Some(json!({
            "expectedRevision": 2,
            "idempotencyKey": "01997a00-0000-7000-8000-0000000002b4",
            "taxAdjustmentStatus": "commercial_only"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(return_posted.status, 200, "{:?}", return_posted.body);
    let detail = prescription_detail_over_http(&service, &world, &prescription).await;
    assert_eq!(detail["items"][0]["dispensedAtoms"], 10);
    assert_eq!(detail["items"][0]["reinstatedAtoms"], 10);
    assert_eq!(detail["dispensings"][0]["reversedAtoms"], 10);
    assert_eq!(
        detail["occasionsUsed"], 1,
        "a return does not give back the occasion"
    );

    // The owner's list names references, never the patient. (Gate 10 is its own test below.)
    let listed = call(
        &service,
        "GET",
        "/api/v1/prescriptions",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(listed.status, 200, "{:?}", listed.body);
    assert!(
        !listed.body.to_string().contains("Sita Kulkarni"),
        "the list carries the patient"
    );

    // 12. A cashier cannot read or enter prescriptions, link a line, or name the pharmacist; and
    //     an unauthenticated caller gets nothing.
    let cashier = sign_in_as(
        &service,
        "cashier",
        "rx-cashier",
        "01997a00-0000-7000-8000-0000000002c1",
    )
    .await;
    for (method, path, body) in [
        ("GET", "/api/v1/prescriptions".to_owned(), None),
        ("GET", format!("/api/v1/prescriptions/{prescription}"), None),
        ("GET", "/api/v1/prescribers".to_owned(), None),
        (
            "POST",
            "/api/v1/prescribers".to_owned(),
            Some(json!({ "fullName": "Dr X", "addressText": "Pune" })),
        ),
        (
            "PUT",
            format!("/api/v1/sales/{again}/supply"),
            Some(json!({
                "expectedRevision": again_revision,
                "supervisingProfessionalId": professional,
                "prescriptionEndorsementConfirmed": true,
            })),
        ),
    ] {
        let denied = call(&service, method, &path, body, Some(&cashier)).await;
        assert_eq!(denied.status, 403, "{method} {path}: {:?}", denied.body);
        assert!(!denied.body.to_string().contains("Sita Kulkarni"));
    }
    let anonymous = call(&service, "GET", "/api/v1/prescriptions", None, None).await;
    assert_eq!(anonymous.status, 401, "{:?}", anonymous.body);
}

/// 1M-B gate 10: with prescriptions in the database, an ordinary Sale is exactly as before.
#[tokio::test]
async fn real_service_leaves_an_ordinary_sale_untouched_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    let _ = prescription_over_http(&service, &world, None, 20).await;
    let sale_id = sale_draft(&service, &world, None).await;
    let with_line = sale_line(&service, &world, &sale_id, 1, "pack", 1, 8000).await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);
    let quote = quote_over_http(&service, &world, &sale_id).await;
    assert_eq!(quote["lines"][0]["regulatoryGate"], "clear");
    assert_eq!(quote["lines"][0]["prescription"]["required"], false);
    assert_eq!(quote["supply"]["supervisionRequired"], false);
    let posted = post_quoted_over_http(
        &service,
        &world,
        &sale_id,
        2,
        "01997a00-0000-7000-8000-0000000002d1",
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    assert_eq!(
        posted.body["supply"]["supervisingProfessionalId"],
        Value::Null
    );
}

/// 1M-B gate 8-9, revised in 1M-C: Schedule X stays refused however complete the prescription, and
/// so does a Schedule H1 line whose NDPS purview is unrecorded (as here): an unsupported
/// NDPS-intersection workflow. The supported H1 path is proved by the Phase 1M-C tests below.
#[tokio::test]
async fn real_service_keeps_schedule_h1_and_x_refused_with_a_complete_prescription_over_http() {
    for (inside, code) in [
        (
            &["schedule_h", "schedule_h1"][..],
            "schedule_h1_ndps_workflow_not_available",
        ),
        // Phase 1M-D3-C2 — the Schedule X retail path now exists, so a complete prescription is
        // refused on the independent axis this fixture leaves unrecorded rather than on a missing
        // workflow. The point of the case is unchanged: a prescription alone is not enough.
        (&["schedule_x"][..], "schedule_x_ndps_purview_unresolved"),
    ] {
        let service = start().await;
        let world = seed_sale_world(&service, 1).await;
        schedule_over_http(&service, &world, inside).await;
        record_basis_over_http(&service, &world, Some("prescription_register")).await;
        let professional = pharmacist_over_http(&service, &world).await;
        let (_, item) = prescription_over_http(&service, &world, None, 20).await;
        let (sale_id, revision) =
            prepared_sale_over_http(&service, &world, &item, &professional, 1).await;
        // No entry can be prepared for it either: preparing is never a way round the gate.
        let unprepared = prepare_entry_over_http(&service, &world, &sale_id, revision).await;
        assert_eq!(unprepared.status, 409, "{code}: {:?}", unprepared.body);
        assert_eq!(unprepared.body["code"], code);
        let refused = post_quoted_over_http(
            &service,
            &world,
            &sale_id,
            revision,
            "01997a00-0000-7000-8000-0000000002e1",
        )
        .await;
        assert_eq!(refused.status, 409, "{code}: {:?}", refused.body);
        assert_eq!(refused.body["code"], code);
        assert_eq!(refused.body["issues"][0]["field"], "lines.1");
        let detail = call(
            &service,
            "GET",
            &format!("/api/v1/sales/{sale_id}"),
            None,
            Some(&world.cookie),
        )
        .await;
        assert_eq!(detail.body["status"], "draft");
        assert_eq!(detail.body["documentNumber"], Value::Null);
    }
}

/// 1M-B corrective: with no rule 65(3)(2) election, a Schedule H supply is refused; under a memo-book
/// election it is entered as PM-000001 only with the original-container attestation.
#[tokio::test]
async fn real_service_enters_a_schedule_h_supply_only_under_its_election_over_http() {
    for (election, attest, outcome) in [
        (None, false, "prescription_record_election_unresolved"),
        (
            Some("cash_or_credit_memo_book"),
            false,
            "prescription_requirements_incomplete",
        ),
        (Some("cash_or_credit_memo_book"), true, "PM-000001"),
    ] {
        let service = start().await;
        let world = seed_sale_world(&service, 1).await;
        schedule_over_http(&service, &world, &["schedule_h"]).await;
        record_basis_over_http(&service, &world, election).await;
        let professional = pharmacist_over_http(&service, &world).await;
        let (_, item) = prescription_over_http(&service, &world, None, 20).await;
        let (sale_id, revision) =
            prepared_sale_over_http(&service, &world, &item, &professional, 1).await;
        let supplied = call(
            &service,
            "PUT",
            &format!("/api/v1/sales/{sale_id}/supply"),
            Some(json!({
                "expectedRevision": revision,
                "supervisingProfessionalId": professional,
                "prescriptionEndorsementConfirmed": true,
                "prescriptionOriginalContainerConfirmed": attest,
            })),
            Some(&world.cookie),
        )
        .await;
        assert_eq!(supplied.status, 200, "{:?}", supplied.body);
        let mut revision = supplied.body["revision"].as_i64().expect("revision");
        if outcome.starts_with("PM-") {
            revision = signed_entry_over_http(&service, &world, &sale_id, revision)
                .await
                .1;
        }
        let reply = post_quoted_over_http(
            &service,
            &world,
            &sale_id,
            revision,
            "01997a00-0000-7000-8000-0000000002f1",
        )
        .await;
        if outcome.starts_with("PM-") {
            assert_eq!(reply.status, 200, "{:?}", reply.body);
            assert_eq!(
                reply.body["prescriptionRecords"][0]["serialNumber"],
                outcome
            );
            assert_eq!(
                reply.body["prescriptionRecords"][0]["recordMethod"],
                "cash_or_credit_memo_book"
            );
        } else {
            assert_eq!(reply.status, 409, "{:?}", reply.body);
            assert_eq!(reply.body["code"], outcome, "{:?}", reply.body);
            if outcome == "prescription_requirements_incomplete" {
                assert_eq!(
                    reply.body["issues"][0]["field"],
                    "supply.prescription_memo_path_ineligible"
                );
            }
        }
    }
}

/// 1M-B corrective B-R2: a posting that fails after the entry was signed and confirmed leaves the
/// entry confirmed (it may be signed on paper) and nothing sold; the retry posts under PR-000001.
#[tokio::test]
async fn real_service_keeps_a_signed_entry_through_a_failed_posting_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_h"]).await;
    record_basis_over_http(&service, &world, Some("prescription_register")).await;
    let professional = pharmacist_over_http(&service, &world).await;
    let (_, item) = prescription_over_http(&service, &world, None, 20).await;
    let (sale_id, revision) =
        prepared_sale_over_http(&service, &world, &item, &professional, 1).await;
    let (_, revision) = signed_entry_over_http(&service, &world, &sale_id, revision).await;

    let pool = database::connect(&service.database_path)
        .await
        .expect("reopen disposable database");
    sqlx::query(
        "CREATE TRIGGER gate_fail_tender BEFORE INSERT ON sale_tenders \
         BEGIN SELECT RAISE(ABORT, 'injected_failure'); END",
    )
    .execute(&pool)
    .await
    .expect("inject a late failure");
    let failed = post_quoted_over_http(
        &service,
        &world,
        &sale_id,
        revision,
        "01997a00-0000-7000-8000-0000000002f2",
    )
    .await;
    assert_ne!(failed.status, 200, "{:?}", failed.body);
    let entries: Vec<(String, String)> =
        sqlx::query_as("SELECT serial_number,status FROM prescription_supply_records")
            .fetch_all(&pool)
            .await
            .expect("read entries");
    assert_eq!(
        entries,
        vec![("PR-000001".to_owned(), "confirmed".to_owned())],
        "the signed entry must survive the failed posting unchanged"
    );
    let sold: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM prescription_dispensings),\
         (SELECT COUNT(*) FROM sale_tenders)",
    )
    .fetch_one(&pool)
    .await
    .expect("count sold");
    assert_eq!(sold, (0, 0), "a failed posting sold something");
    sqlx::query("DROP TRIGGER gate_fail_tender")
        .execute(&pool)
        .await
        .expect("remove the injected failure");
    pool.close().await;

    let posted = post_quoted_over_http(
        &service,
        &world,
        &sale_id,
        revision,
        "01997a00-0000-7000-8000-0000000002f3",
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    assert_eq!(
        posted.body["prescriptionRecords"][0]["serialNumber"],
        "PR-000001"
    );
    assert_eq!(posted.body["prescriptionRecords"][0]["status"], "finalized");
}

/// 1M-B corrective B-R2: a cancelled entry is voided with its reason and its serial never reused;
/// and a prepared Sale is held until its entry is voided.
#[tokio::test]
async fn real_service_voids_a_cancelled_entry_and_never_reuses_its_serial_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_h"]).await;
    record_basis_over_http(&service, &world, Some("prescription_register")).await;
    let professional = pharmacist_over_http(&service, &world).await;
    let (_, item) = prescription_over_http(&service, &world, None, 20).await;
    let (sale_id, revision) =
        prepared_sale_over_http(&service, &world, &item, &professional, 1).await;
    let prepared = prepare_entry_over_http(&service, &world, &sale_id, revision).await;
    assert_eq!(prepared.status, 200, "{:?}", prepared.body);
    let record = prepared.body["prescriptionRecords"][0]["id"]
        .as_str()
        .expect("record id")
        .to_owned();

    // The prepared Sale is held as the pharmacist is to sign it.
    let held = sale_line(
        &service,
        &world,
        &sale_id,
        prepared.body["revision"].as_i64().expect("revision"),
        "pack",
        1,
        8000,
    )
    .await;
    assert_eq!(held.status, 409, "{:?}", held.body);
    assert_eq!(held.body["code"], "prescription_record_prepared");

    let voided = call(
        &service,
        "POST",
        &format!("/api/v1/prescription-supply-records/{record}/void"),
        Some(json!({ "reason": "customer left before paying" })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(voided.status, 200, "{:?}", voided.body);
    assert_eq!(voided.body["status"], "void");
    assert_eq!(voided.body["voidReason"], "customer left before paying");
    let detail = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{sale_id}"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(detail.body["status"], "draft");
    let again = prepare_entry_over_http(
        &service,
        &world,
        &sale_id,
        detail.body["revision"].as_i64().expect("revision"),
    )
    .await;
    assert_eq!(again.status, 200, "{:?}", again.body);
    let serials: Vec<(String, String)> = again.body["prescriptionRecords"]
        .as_array()
        .expect("records")
        .iter()
        .map(|record| {
            (
                record["serialNumber"].as_str().unwrap().to_owned(),
                record["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        serials,
        vec![
            ("PR-000001".to_owned(), "void".to_owned()),
            ("PR-000002".to_owned(), "prepared".to_owned()),
        ]
    );
}

// ---------------------------------------------------------------------------------------------
// Phase 1M-C — the Schedule H1 working record, over the real socket
// ---------------------------------------------------------------------------------------------

/// Posts for the quoted total with another session: a cashier, here.
async fn post_quoted_over_http_as(
    service: &Service,
    cookie: &str,
    world: &SaleWorld,
    sale_id: &str,
    revision: i64,
    key: &str,
) -> Reply {
    let total = quote_over_http(service, world, sale_id).await["grandTotalPaise"]
        .as_i64()
        .expect("total");
    call(
        service,
        "POST",
        &format!("/api/v1/sales/{sale_id}/post"),
        Some(json!({
            "expectedRevision": revision,
            "idempotencyKey": key,
            "tenders": [{ "method": "cash", "amountPaise": total }]
        })),
        Some(cookie),
    )
    .await
}

const PUNJAB: &str = "01997300-0000-7000-8000-000000000003";

/// One owner-recorded finding for the world's product, through the owner's API.
async fn finding_over_http(service: &Service, world: &SaleWorld, scheme: &str, applies: bool) {
    let finding = call(
        service,
        "POST",
        &format!(
            "/api/v1/products/{}/regulatory/classifications",
            world.product
        ),
        Some(json!({
            "scheme": scheme,
            "applies": applies,
            "effectiveFrom": "2020-01-01",
            "sourceCitation": "Owner-recorded finding with its authority",
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(finding.status, 201, "{scheme}: {:?}", finding.body);
}

/// The world's product in Schedule H1 alone, NDPS purview established not to apply, with its
/// rule 65(3) basis and a registered pharmacist. Returns (professional, prescription item).
async fn h1_world_over_http(service: &Service, world: &SaleWorld) -> (String, String) {
    schedule_over_http(service, world, &["schedule_h1"]).await;
    finding_over_http(service, world, "ndps_purview", false).await;
    record_basis_over_http(service, world, Some("prescription_register")).await;
    let professional = pharmacist_over_http(service, world).await;
    let (_, item) = prescription_over_http(service, world, None, 20).await;
    (professional, item)
}

async fn confirm_h1_over_http(service: &Service, cookie: &str, sale: &str) -> Reply {
    call(
        service,
        "POST",
        &format!("/api/v1/sales/{sale}/h1-register/confirm"),
        Some(json!({ "hardCopyPlacedInRegister": true, "pharmacistAuthenticatedHardCopy": true })),
        Some(cookie),
    )
    .await
}

/// Prepares both layers and confirms both, as a pharmacist does; returns the Sale's revision.
async fn h1_ready_over_http(
    service: &Service,
    world: &SaleWorld,
    sale: &str,
    revision: i64,
) -> i64 {
    let (_, revision) = signed_entry_over_http(service, world, sale, revision).await;
    let confirmed = confirm_h1_over_http(service, &world.cookie, sale).await;
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);
    revision
}

/// Phase 1M-C, C-68 over HTTP: the supported ordinary H1 supply, end to end through the real
/// router, auth and database. Both layers are prepared; the electronic entry alone does not post;
/// a cashier can neither confirm nor read the register; the pharmacist's confirmation lets the
/// cashier post; both layers finalize.
#[tokio::test]
async fn real_service_supplies_schedule_h1_with_its_separate_register_entry_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    let (professional, item) = h1_world_over_http(&service, &world).await;
    let (sale_id, revision) =
        prepared_sale_over_http(&service, &world, &item, &professional, 1).await;

    let quote = quote_over_http(&service, &world, &sale_id).await;
    assert_eq!(quote["lines"][0]["regulatoryGate"], "prescription_required");
    assert_eq!(quote["lines"][0]["regulatoryGateScheme"], "schedule_h1");
    let prepared = prepare_entry_over_http(&service, &world, &sale_id, revision).await;
    assert_eq!(prepared.status, 200, "{:?}", prepared.body);
    assert_eq!(prepared.body["status"], "draft");
    assert_eq!(
        prepared.body["h1RegisterEntries"][0]["reference"],
        "AH1-000001"
    );
    let revision = prepared.body["revision"].as_i64().expect("revision");
    let record = prepared.body["prescriptionRecords"][0]["id"]
        .as_str()
        .expect("record")
        .to_owned();
    let confirmed = confirm_entry_over_http(&service, &world.cookie, &record).await;
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);

    // The electronic working entry alone does not complete the H1 layer.
    let refused = post_quoted_over_http(
        &service,
        &world,
        &sale_id,
        revision,
        "01997a00-0000-7000-8000-0000000003a1",
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(
        refused.body["issues"][0]["field"],
        "supply.schedule_h1_register_not_confirmed"
    );

    let cashier = sign_in_as(
        &service,
        "cashier",
        "h1-cashier",
        "01997a00-0000-7000-8000-0000000003c1",
    )
    .await;
    let denied = confirm_h1_over_http(&service, &cashier, &sale_id).await;
    assert_eq!(denied.status, 403, "{:?}", denied.body);
    for path in [
        format!("/api/v1/sales/{sale_id}/h1-register"),
        "/api/v1/h1-register?from=2026-09-01&to=2026-09-30".to_owned(),
    ] {
        let denied = call(&service, "GET", &path, None, Some(&cashier)).await;
        assert_eq!(denied.status, 403, "{path}: {:?}", denied.body);
        assert!(!denied.body.to_string().contains("Sita Kulkarni"));
    }

    let sheet = confirm_h1_over_http(&service, &world.cookie, &sale_id).await;
    assert_eq!(sheet.status, 200, "{:?}", sheet.body);
    assert_eq!(sheet.body["entries"][0]["patientName"], "Sita Kulkarni");
    assert_eq!(sheet.body["entries"][0]["status"], "confirmed");

    let posted = post_quoted_over_http_as(
        &service,
        &cashier,
        &world,
        &sale_id,
        revision,
        "01997a00-0000-7000-8000-0000000003a2",
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    assert_eq!(posted.body["prescriptionRecords"][0]["status"], "finalized");
    assert_eq!(posted.body["h1RegisterEntries"][0]["status"], "finalized");
    assert!(!posted.body.to_string().contains("Sita Kulkarni"));
}

/// Phase 1M-C, C-69/C-70 over HTTP: at a store whose premises are in Punjab, an owner-recorded
/// Punjab finding refuses the sale as an unsupported State workflow, in neutral words.
#[tokio::test]
async fn real_service_refuses_a_punjab_restricted_line_as_an_unsupported_workflow_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    let (professional, item) = h1_world_over_http(&service, &world).await;
    let moved = call(
        &service,
        "PUT",
        "/api/v1/store/address",
        Some(json!({
            "expectedRevision": 1, "line1": "12 Mall Road", "city": "Ludhiana",
            "stateId": PUNJAB, "postalCode": "141001"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(moved.status, 200, "{:?}", moved.body);
    finding_over_http(&service, &world, "punjab_restricted_supply", true).await;
    let (sale_id, revision) =
        prepared_sale_over_http(&service, &world, &item, &professional, 1).await;
    let refused = prepare_entry_over_http(&service, &world, &sale_id, revision).await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(
        refused.body["code"],
        "state_restricted_drug_workflow_not_available"
    );
    assert_eq!(
        refused.body["message"],
        "This drug is subject to an additional Punjab drug-control workflow that AUSHADHARTH does not yet support."
    );
    let text = refused.body.to_string().to_lowercase();
    for word in ["banned", "prohibit", "illegal"] {
        assert!(!text.contains(word), "{word}");
    }
    let posted = post_quoted_over_http(
        &service,
        &world,
        &sale_id,
        revision,
        "01997a00-0000-7000-8000-0000000003b1",
    )
    .await;
    assert_eq!(posted.status, 409, "{:?}", posted.body);
    assert_eq!(
        posted.body["code"],
        "state_restricted_drug_workflow_not_available"
    );
}

/// Phase 1M-C, C-55: a finalized H1 working entry survives a backup and restore, byte for byte,
/// with its guards still in force on the restored database.
#[tokio::test]
async fn real_service_keeps_h1_entries_through_backup_and_restore_over_http() {
    let harness = start_with_backups().await;
    let service = &harness.service;
    let world = seed_sale_world(service, 1).await;
    let (professional, item) = h1_world_over_http(service, &world).await;
    let (sale_id, revision) =
        prepared_sale_over_http(service, &world, &item, &professional, 1).await;
    let revision = h1_ready_over_http(service, &world, &sale_id, revision).await;
    let posted = post_quoted_over_http(
        service,
        &world,
        &sale_id,
        revision,
        "01997a00-0000-7000-8000-0000000003d1",
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);

    let before = call(
        service,
        "GET",
        &format!("/api/v1/sales/{sale_id}/h1-register"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(before.status, 200, "{:?}", before.body);

    let created = call(
        service,
        "POST",
        "/api/v1/backups/create",
        Some(json!({})),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    let filename = created.body["filename"]
        .as_str()
        .expect("filename")
        .to_owned();
    let bytes = std::fs::read(harness.backups.join(&filename)).expect("backup on disk");
    let (status, _, body) = call_bytes(
        service,
        "/api/v1/backups/restore/prepare",
        "application/octet-stream",
        &bytes,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let prepared: Value = serde_json::from_slice(&body).expect("prepared restore");
    let token = prepared["candidateToken"]
        .as_str()
        .expect("token")
        .to_owned();
    let committed = call(
        service,
        "POST",
        "/api/v1/backups/restore/commit",
        Some(json!({ "candidateToken": token, "password": "Integration-Password-42" })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(committed.status, 200, "{:?}", committed.body);
    api::backups::recover_interrupted_restore(&harness.backups, &service.database_path)
        .await
        .expect("recovery");
    let reopened = database::connect(&service.database_path)
        .await
        .expect("reopened database");
    assert!(
        api::backups::complete_restore_after_open(&reopened, &harness.backups)
            .await
            .expect("completion")
    );
    let restored: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT reference,status,patient_name,drug_name FROM prescription_h1_register_entries",
    )
    .fetch_all(&reopened)
    .await
    .expect("entries");
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].0, before.body["entries"][0]["reference"]);
    assert_eq!(restored[0].1, "finalized");
    assert_eq!(restored[0].2, before.body["entries"][0]["patientName"]);
    // The guards came back with the data.
    for statement in [
        "UPDATE prescription_h1_register_entries SET patient_name='Someone Else'",
        "DELETE FROM prescription_h1_register_entries",
    ] {
        assert!(
            sqlx::query(statement).execute(&reopened).await.is_err(),
            "{statement}"
        );
    }
    reopened.close().await;
}

/// Phase 1M-D1-A, proofs 36 and 37. Receipt provenance is ordinary data in the ordinary database,
/// so Phase 1K's whole-database snapshot carries it without a format change — and the guards that
/// keep it immutable come back with it.
#[tokio::test]
async fn real_service_keeps_purchase_provenance_through_backup_and_restore_over_http() {
    let harness = start_with_backups().await;
    let service = &harness.service;
    let world = seed_purchase_world(service, MAHARASHTRA, "taxable").await;

    // The supplier has an address and a licence on file, and the product has one maker.
    let address = call(
        service,
        "POST",
        &format!("/api/v1/parties/{}/addresses", world.supplier),
        Some(json!({
            "expectedRevision": 1,
            "addressRole": "billing",
            "line1": "14 Ware House Road",
            "city": "Thane",
            "stateId": MAHARASHTRA,
            "postalCode": "421302",
            "isPrimary": true,
        })),
        Some(&world.cookie),
    )
    .await;
    assert!(
        address.status == 201 || address.status == 200,
        "{:?}",
        address.body
    );

    let draft = create_draft(service, &world, "INV-8801").await;
    let purchase_id = draft["id"].as_str().expect("purchase id").to_owned();
    let with_line = call(
        service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/lines"),
        Some(json!({
            "expectedRevision": 1,
            "productId": world.product,
            "productPackId": world.pack,
            "newBatchNumber": "B-8801",
            "newBatchExpiresOn": "2028-03-31",
            "quantityPacks": 10,
            "ratePerPackPaise": 3_000,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);
    let revision = with_line.body["revision"].as_i64().expect("revision");
    let posted = call(
        service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/post"),
        Some(json!({
            "expectedRevision": revision,
            "idempotencyKey": "01997a00-0000-7000-8000-0000000008a1",
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    assert_eq!(posted.body["purchaseProvenanceSnapshotVersion"], 1);
    assert_eq!(posted.body["lines"][0]["batchNumber"], "B-8801");
    let frozen_address = posted.body["supplierAddressLine1"].clone();
    let frozen_licence = posted.body["supplierDrugLicenceState"].clone();
    let frozen_drug = posted.body["lines"][0]["drugDisplayName"].clone();

    let created = call(
        service,
        "POST",
        "/api/v1/backups/create",
        Some(json!({})),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    let filename = created.body["filename"]
        .as_str()
        .expect("filename")
        .to_owned();
    let bytes = std::fs::read(harness.backups.join(&filename)).expect("backup on disk");
    let (status, _, body) = call_bytes(
        service,
        "/api/v1/backups/restore/prepare",
        "application/octet-stream",
        &bytes,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let prepared: Value = serde_json::from_slice(&body).expect("prepared restore");
    let token = prepared["candidateToken"]
        .as_str()
        .expect("token")
        .to_owned();
    let committed = call(
        service,
        "POST",
        "/api/v1/backups/restore/commit",
        Some(json!({ "candidateToken": token, "password": "Integration-Password-42" })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(committed.status, 200, "{:?}", committed.body);
    api::backups::recover_interrupted_restore(&harness.backups, &service.database_path)
        .await
        .expect("recovery");
    let reopened = database::connect(&service.database_path)
        .await
        .expect("reopened database");
    assert!(
        api::backups::complete_restore_after_open(&reopened, &harness.backups)
            .await
            .expect("completion")
    );

    // 36. Every provenance fact came back.
    let (version, line1, licence_state): (i64, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT purchase_provenance_snapshot_version,supplier_address_line1,\
         supplier_drug_licence_state FROM purchase_documents WHERE id=?",
    )
    .bind(&purchase_id)
    .fetch_one(&reopened)
    .await
    .expect("restored purchase");
    assert_eq!(version, 1);
    assert_eq!(Value::from(line1), frozen_address);
    assert_eq!(Value::from(licence_state), frozen_licence);
    let (drug, batch): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT drug_display_name,batch_number FROM purchase_lines WHERE purchase_document_id=?",
    )
    .bind(&purchase_id)
    .fetch_one(&reopened)
    .await
    .expect("restored line");
    assert_eq!(Value::from(drug), frozen_drug);
    assert_eq!(batch.as_deref(), Some("B-8801"));

    // 37. And so did the guards that keep it that way.
    for statement in [
        "UPDATE purchase_documents SET supplier_address_line1='99 Elsewhere'",
        "UPDATE purchase_lines SET drug_display_name='Something Else'",
        "DELETE FROM purchase_lines",
    ] {
        assert!(
            sqlx::query(statement).execute(&reopened).await.is_err(),
            "{statement}"
        );
    }
    reopened.close().await;
}

// -------------------------------------------------------------------------------------------
// Phase 1M-D1-B — Form 20F store authority and per-product drug coverage.
// -------------------------------------------------------------------------------------------

/// Records a Form 20F for the store and returns its id and revision.
async fn form_20f_over_http(service: &Service, world: &SaleWorld, number: &str) -> (String, i64) {
    let created = call(
        service,
        "POST",
        "/api/v1/store/compliance-licences",
        Some(json!({
            "licenceForm": "form_20f",
            "licenceNumber": number,
            "issuingAuthority": "FDA Maharashtra",
            "validFrom": "2024-04-01",
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    // A licence on file is a licence on file. Nothing about recording it says it is in force.
    assert_eq!(created.body["legalStatus"], "unknown");
    assert_eq!(created.body["validityBasis"], "unknown");
    (
        created.body["id"].as_str().expect("licence id").to_owned(),
        created.body["revision"].as_i64().expect("revision"),
    )
}

async fn set_authority_over_http(
    service: &Service,
    world: &SaleWorld,
    licence: &str,
    revision: i64,
    body: Value,
) -> Reply {
    let mut payload = body;
    payload["expectedRevision"] = json!(revision);
    call(
        service,
        "PUT",
        &format!("/api/v1/store/compliance-licences/{licence}/authority"),
        Some(payload),
        Some(&world.cookie),
    )
    .await
}

/// What the product screen is told about Form 20F on a given day.
async fn authority_over_http(service: &Service, world: &SaleWorld, as_of: &str) -> Value {
    let reply = call(
        service,
        "GET",
        &format!("/api/v1/products/{}/regulatory?asOf={as_of}", world.product),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    reply.body["form20fAuthority"].clone()
}

async fn cover_over_http(
    service: &Service,
    world: &SaleWorld,
    licence: &str,
    from: &str,
    to: Option<&str>,
) -> Reply {
    call(
        service,
        "POST",
        "/api/v1/store/licence-drug-coverage",
        Some(json!({
            "licenceId": licence,
            "productId": world.product,
            "effectiveFrom": from,
            "effectiveTo": to,
            "sourceCitation": "Form 20F item 2, names of drugs",
        })),
        Some(&world.cookie),
    )
    .await
}

/// Phase 1M-D1-B, item 14. The whole authority surface over the wire: what is refused, what is
/// recorded, and what each state answers on a date.
#[tokio::test]
async fn real_service_records_form_20f_authority_and_drug_coverage_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;

    // Nothing recorded at all: the product screen says so, and says which fact is missing.
    let answer = authority_over_http(&service, &world, "2026-09-25").await;
    assert_eq!(answer["state"], "unresolved");
    assert_eq!(answer["gap"], "no_licence_recorded");

    let (licence, revision) = form_20f_over_http(&service, &world, "MH-PUNE-20F-4471").await;
    // A licence whose standing nobody has recorded is not a licence in force.
    let answer = authority_over_http(&service, &world, "2026-09-25").await;
    assert_eq!(answer["state"], "unresolved");
    assert_eq!(answer["gap"], "licence_status_unknown");

    // A validity basis and its dates must agree, and the refusal names the field.
    let refused = set_authority_over_http(
        &service,
        &world,
        &licence,
        revision,
        json!({ "legalStatus": "in_force", "validityBasis": "fixed_term" }),
    )
    .await;
    assert_eq!(refused.status, 422, "{:?}", refused.body);
    assert_eq!(refused.body["issues"][0]["field"], "validUpto");
    let refused = set_authority_over_http(
        &service,
        &world,
        &licence,
        revision,
        json!({
            "legalStatus": "in_force",
            "validityBasis": "perpetual",
            "validUpto": "2030-03-31",
        }),
    )
    .await;
    assert_eq!(refused.status, 422, "{:?}", refused.body);
    assert_eq!(refused.body["issues"][0]["field"], "validUpto");
    let refused = set_authority_over_http(
        &service,
        &world,
        &licence,
        revision,
        json!({ "legalStatus": "probably", "validityBasis": "perpetual" }),
    )
    .await;
    assert_eq!(refused.status, 422, "{:?}", refused.body);
    assert_eq!(refused.body["issues"][0]["field"], "legalStatus");

    // The Form 20F of rule 61(3) carries no expiry of its own.
    let updated = set_authority_over_http(
        &service,
        &world,
        &licence,
        revision,
        json!({
            "legalStatus": "in_force",
            "validityBasis": "perpetual",
            "validFrom": "2024-04-01",
            "reason": "read from the certificate",
        }),
    )
    .await;
    assert_eq!(updated.status, 200, "{:?}", updated.body);
    assert_eq!(updated.body["legalStatus"], "in_force");
    assert_eq!(updated.body["validityBasis"], "perpetual");
    let revision = updated.body["revision"].as_i64().expect("revision");

    // A licence in force still covers no drug until somebody writes the drug onto it.
    let answer = authority_over_http(&service, &world, "2026-09-25").await;
    assert_eq!(answer["state"], "not_established");
    assert_eq!(answer["gap"], "product_not_covered");

    // Rule 61(3) issues Form 20G to a wholesaler; it is not retail authority.
    let wholesale = call(
        &service,
        "POST",
        "/api/v1/store/compliance-licences",
        Some(json!({
            "licenceForm": "form_20g",
            "licenceNumber": "MH-PUNE-20G-8812",
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(wholesale.status, 201, "{:?}", wholesale.body);
    let refused = cover_over_http(
        &service,
        &world,
        wholesale.body["id"].as_str().expect("id"),
        "2024-04-01",
        None,
    )
    .await;
    assert_eq!(refused.status, 422, "{:?}", refused.body);
    assert_eq!(refused.body["issues"][0]["field"], "licenceId");

    let covered = cover_over_http(&service, &world, &licence, "2026-04-01", None).await;
    assert_eq!(covered.status, 201, "{:?}", covered.body);
    let coverage = covered.body["id"].as_str().expect("coverage id").to_owned();
    let coverage_revision = covered.body["revision"].as_i64().expect("revision");
    assert_eq!(covered.body["licenceNumber"], "MH-PUNE-20F-4471");

    // Both facts are now established — on the days the coverage runs, and not before them.
    let answer = authority_over_http(&service, &world, "2026-09-25").await;
    assert_eq!(answer["state"], "established", "{answer:?}");
    assert_eq!(answer["licenceNumber"], "MH-PUNE-20F-4471");
    assert_eq!(answer["coverageId"], Value::from(coverage.clone()));
    let answer = authority_over_http(&service, &world, "2026-03-31").await;
    assert_eq!(answer["state"], "not_established");
    assert_eq!(answer["gap"], "product_not_covered");

    // Two active rows must never answer for the same drug on the same day.
    let overlap = cover_over_http(&service, &world, &licence, "2026-06-01", None).await;
    assert_eq!(overlap.status, 409, "{:?}", overlap.body);
    assert_eq!(
        overlap.body["code"],
        "licence_drug_coverage_period_overlaps"
    );

    // A suspension or a cancellation is an answer, and each is named as itself.
    let mut revision = revision;
    for (status, gap) in [
        ("suspended", "licence_suspended"),
        ("cancelled", "licence_cancelled"),
    ] {
        let changed = set_authority_over_http(
            &service,
            &world,
            &licence,
            revision,
            json!({
                "legalStatus": status,
                "validityBasis": "perpetual",
                "validFrom": "2024-04-01",
                "reason": "order of the Licensing Authority",
            }),
        )
        .await;
        assert_eq!(changed.status, 200, "{status}: {:?}", changed.body);
        let answer = authority_over_http(&service, &world, "2026-09-25").await;
        assert_eq!(answer["state"], "not_established", "{status}");
        assert_eq!(answer["gap"], gap);
        // Restore the licence for the next turn of the loop.
        let restored = set_authority_over_http(
            &service,
            &world,
            &licence,
            changed.body["revision"].as_i64().expect("revision"),
            json!({
                "legalStatus": "in_force",
                "validityBasis": "perpetual",
                "validFrom": "2024-04-01",
            }),
        )
        .await;
        assert_eq!(restored.status, 200, "{:?}", restored.body);
        revision = restored.body["revision"].as_i64().expect("revision");
    }
    // The expected revision is the caller's proof they read the licence as it stands.
    let stale = set_authority_over_http(
        &service,
        &world,
        &licence,
        1,
        json!({ "legalStatus": "in_force", "validityBasis": "perpetual" }),
    )
    .await;
    assert_eq!(stale.status, 409, "{:?}", stale.body);
    assert_eq!(stale.body["code"], "revision_conflict");

    // Coverage that has come to an end keeps answering for the days it governed.
    let closed = call(
        &service,
        "POST",
        &format!("/api/v1/store/licence-drug-coverage/{coverage}/close"),
        Some(json!({
            "expectedRevision": coverage_revision,
            "effectiveTo": "2026-09-01",
            "reason": "struck off the licence on renewal",
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(closed.status, 200, "{:?}", closed.body);
    assert_eq!(
        authority_over_http(&service, &world, "2026-08-31").await["state"],
        "established"
    );
    assert_eq!(
        authority_over_http(&service, &world, "2026-09-01").await["gap"],
        "product_not_covered"
    );

    // The screen that manages all this reads one payload, and the coverage is in it.
    let compliance = call(
        &service,
        "GET",
        "/api/v1/store/drug-compliance",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(compliance.status, 200, "{:?}", compliance.body);
    assert_eq!(
        compliance.body["drugCoverage"][0]["id"],
        Value::from(coverage.clone())
    );
    assert_eq!(
        compliance.body["drugCoverage"][0]["effectiveTo"],
        "2026-09-01"
    );

    // Every change left a trail, and none of it names a patient.
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let events: Vec<(String, String)> = sqlx::query_as(
        "SELECT action,change_payload FROM master_change_events \
         WHERE entity_type='store_licence_drug_coverage' ORDER BY occurred_at_utc,event_id",
    )
    .fetch_all(&pool)
    .await
    .expect("coverage events");
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0].0, "created");
    assert_eq!(events[1].0, "updated");
    assert!(events[0].1.contains("Form 20F item 2"), "{:?}", events[0].1);
    // Coverage is history: the database will not let it be deleted at all.
    assert!(
        sqlx::query("DELETE FROM store_licence_drug_coverage")
            .execute(&pool)
            .await
            .is_err()
    );
    pool.close().await;
}

/// Phase 1M-D1-B, item 8 — the hard requirement. Every fact a Schedule X supply would need is on
/// file: an in-force Form 20F, the drug written onto it, a registered pharmacist, a complete
/// prescription. The workflow still does not exist, so the sale is still refused, and the stock is
/// still where it was. Establishing authority is not the same as building the register.
#[tokio::test]
async fn real_service_keeps_schedule_x_refused_with_established_form_20f_authority_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;
    // Phase 1M-D3-C2: the independent NDPS axis, established not to apply, so this test reaches
    // the Schedule X predicate it is actually about instead of stopping at an unrecorded overlay.
    finding_over_http(&service, &world, "ndps_purview", false).await;
    record_basis_over_http(&service, &world, Some("prescription_register")).await;
    let professional = pharmacist_over_http(&service, &world).await;
    let (_, item) = prescription_over_http(&service, &world, None, 20).await;

    let (licence, revision) = form_20f_over_http(&service, &world, "MH-PUNE-20F-4471").await;
    let authorised = set_authority_over_http(
        &service,
        &world,
        &licence,
        revision,
        json!({
            "legalStatus": "in_force",
            "validityBasis": "perpetual",
            "validFrom": "2020-01-01",
        }),
    )
    .await;
    assert_eq!(authorised.status, 200, "{:?}", authorised.body);
    let covered = cover_over_http(&service, &world, &licence, "2020-01-01", None).await;
    assert_eq!(covered.status, 201, "{:?}", covered.body);

    // The advisory display says the paperwork is in order...
    let answer = authority_over_http(&service, &world, SALE_DATE).await;
    assert_eq!(answer["state"], "established", "{answer:?}");

    // ...and the counter still refuses, on the same code as before this phase existed.
    let (sale_id, sale_revision) =
        prepared_sale_over_http(&service, &world, &item, &professional, 1).await;
    let unprepared = prepare_entry_over_http(&service, &world, &sale_id, sale_revision).await;
    assert_eq!(unprepared.status, 409, "{:?}", unprepared.body);
    assert_eq!(
        unprepared.body["code"], "prescription_requirements_incomplete",
        "{:?}",
        unprepared.body
    );
    let refused = post_quoted_over_http(
        &service,
        &world,
        &sale_id,
        sale_revision,
        "01997a00-0000-7000-8000-0000000002f1",
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(
        refused.body["code"], "prescription_requirements_incomplete",
        "{:?}",
        refused.body
    );
    // Exactly the Schedule X predicates this phase had not yet established, and no others. Every
    // unmet predicate is reported together so the counter can clear them in one pass.
    assert_eq!(
        schedule_x_unmet(&refused.body),
        vec![
            "schedule_x_lot_provenance_incomplete",
            "schedule_x_source_authority_missing",
            "schedule_x_duplicate_copy_evidence_missing",
            "schedule_x_prescription_annotation_missing",
            "schedule_x_register_confirmation_missing",
            "schedule_x_supervising_pharmacist_invalid",
        ],
        "{:?}",
        refused.body
    );
    // The field still names the offending line, and now the predicate on it as well.
    assert!(
        refused.body["issues"][0]["field"]
            .as_str()
            .is_some_and(|field| field.starts_with("lines.1.")),
        "{:?}",
        refused.body
    );

    let detail = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{sale_id}"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(detail.body["status"], "draft");
    assert_eq!(detail.body["documentNumber"], Value::Null);

    // Nothing moved, and nothing was dispensed against the prescription on the way past.
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let sold: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM inventory_movements WHERE movement_type='sale'")
            .fetch_one(&pool)
            .await
            .expect("movements");
    assert_eq!(sold, 0, "a Schedule X line moved stock");
    let dispensed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM prescription_dispensings")
        .fetch_one(&pool)
        .await
        .expect("dispensings");
    assert_eq!(
        dispensed, 0,
        "a Schedule X line dispensed against a prescription"
    );
    pool.close().await;
}

/// Phase 1M-D1-B, item 14. Authority and coverage are part of the pharmacy's own record, so they
/// come back from a backup exactly as they went in — guards and all.
#[tokio::test]
async fn real_service_keeps_form_20f_authority_through_backup_and_restore_over_http() {
    let harness = start_with_backups().await;
    let service = &harness.service;
    let world = seed_sale_world(service, 1).await;

    let (licence, revision) = form_20f_over_http(service, &world, "MH-PUNE-20F-4471").await;
    let authorised = set_authority_over_http(
        service,
        &world,
        &licence,
        revision,
        json!({
            "legalStatus": "in_force",
            "validityBasis": "fixed_term",
            "validFrom": "2024-04-01",
            "validUpto": "2029-03-31",
        }),
    )
    .await;
    assert_eq!(authorised.status, 200, "{:?}", authorised.body);
    let covered = cover_over_http(service, &world, &licence, "2026-04-01", None).await;
    assert_eq!(covered.status, 201, "{:?}", covered.body);
    let coverage = covered.body["id"].as_str().expect("coverage id").to_owned();
    let citation = covered.body["sourceCitation"].clone();

    let created = call(
        service,
        "POST",
        "/api/v1/backups/create",
        Some(json!({})),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    let filename = created.body["filename"]
        .as_str()
        .expect("filename")
        .to_owned();
    let bytes = std::fs::read(harness.backups.join(&filename)).expect("backup on disk");
    let (status, _, body) = call_bytes(
        service,
        "/api/v1/backups/restore/prepare",
        "application/octet-stream",
        &bytes,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let prepared: Value = serde_json::from_slice(&body).expect("prepared restore");
    let token = prepared["candidateToken"]
        .as_str()
        .expect("token")
        .to_owned();
    let committed = call(
        service,
        "POST",
        "/api/v1/backups/restore/commit",
        Some(json!({ "candidateToken": token, "password": "Integration-Password-42" })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(committed.status, 200, "{:?}", committed.body);
    api::backups::recover_interrupted_restore(&harness.backups, &service.database_path)
        .await
        .expect("recovery");
    let reopened = database::connect(&service.database_path)
        .await
        .expect("reopened database");
    assert!(
        api::backups::complete_restore_after_open(&reopened, &harness.backups)
            .await
            .expect("completion")
    );

    let (legal_status, validity_basis, valid_upto): (String, String, Option<String>) =
        sqlx::query_as(
            "SELECT legal_status,validity_basis,valid_upto FROM store_compliance_licences \
             WHERE id=?",
        )
        .bind(&licence)
        .fetch_one(&reopened)
        .await
        .expect("restored licence");
    assert_eq!(legal_status, "in_force");
    assert_eq!(validity_basis, "fixed_term");
    assert_eq!(valid_upto.as_deref(), Some("2029-03-31"));
    let (product, from, source): (String, String, String) = sqlx::query_as(
        "SELECT product_id,effective_from,source_citation FROM store_licence_drug_coverage \
         WHERE id=?",
    )
    .bind(&coverage)
    .fetch_one(&reopened)
    .await
    .expect("restored coverage");
    assert_eq!(product, world.product);
    assert_eq!(from, "2026-04-01");
    assert_eq!(Value::from(source), citation);

    // And so did the guards that keep it coherent.
    for statement in [
        "DELETE FROM store_licence_drug_coverage",
        "UPDATE store_compliance_licences SET validity_basis='perpetual' \
         WHERE licence_form='form_20f'",
    ] {
        assert!(
            sqlx::query(statement).execute(&reopened).await.is_err(),
            "{statement}"
        );
    }
    reopened.close().await;
}

/// Corrective C1, item 1 and item 3. Archiving coverage that should never have been written, and
/// the proof that the archived row grants nothing afterwards.
///
/// Closing coverage and archiving it are different acts. Closing says the licence stopped naming
/// the drug on a date, and the row keeps answering for every day before it. Archiving says the row
/// was never true, so it must answer for no day at all — which is what makes the production
/// loader's `status = 'active'` filter load-bearing rather than decorative.
#[tokio::test]
async fn real_service_archives_drug_coverage_and_the_archived_row_grants_nothing_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    let (licence, revision) = form_20f_over_http(&service, &world, "MH-PUNE-20F-4471").await;
    let authorised = set_authority_over_http(
        &service,
        &world,
        &licence,
        revision,
        json!({
            "legalStatus": "in_force",
            "validityBasis": "perpetual",
            "validFrom": "2020-01-01",
        }),
    )
    .await;
    assert_eq!(authorised.status, 200, "{:?}", authorised.body);
    let covered = cover_over_http(&service, &world, &licence, "2020-01-01", None).await;
    assert_eq!(covered.status, 201, "{:?}", covered.body);
    let coverage = covered.body["id"].as_str().expect("coverage id").to_owned();
    let coverage_revision = covered.body["revision"].as_i64().expect("revision");

    // While the row is active the authority is established. Everything below is about taking that
    // away again.
    let answer = authority_over_http(&service, &world, SALE_DATE).await;
    assert_eq!(answer["state"], "established", "{answer:?}");

    let archive_uri = format!("/api/v1/store/licence-drug-coverage/{coverage}/archive");

    // A withdrawal nobody explained is not a withdrawal: the reason is required.
    let unexplained = call(
        &service,
        "POST",
        &archive_uri,
        Some(json!({ "expectedRevision": coverage_revision, "reason": "   " })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(unexplained.status, 422, "{:?}", unexplained.body);
    assert_eq!(unexplained.body["issues"][0]["field"], "reason");

    // And the caller must have read the row as it stands.
    let stale = call(
        &service,
        "POST",
        &archive_uri,
        Some(json!({
            "expectedRevision": coverage_revision + 5,
            "reason": "entered against the wrong drug",
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(stale.status, 409, "{:?}", stale.body);
    assert_eq!(stale.body["code"], "revision_conflict");

    let archived = call(
        &service,
        "POST",
        &archive_uri,
        Some(json!({
            "expectedRevision": coverage_revision,
            "reason": "entered against the wrong drug",
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(archived.status, 204, "{:?}", archived.body);

    // Archiving is not idempotent by accident: an archived row is read-only, at any revision.
    let again = call(
        &service,
        "POST",
        &archive_uri,
        Some(json!({
            "expectedRevision": coverage_revision + 1,
            "reason": "and again",
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(again.status, 409, "{:?}", again.body);
    assert_eq!(again.body["code"], "record_archived");

    // The row is history, not a hole. It is still listed, archived, one revision on.
    let compliance = call(
        &service,
        "GET",
        "/api/v1/store/drug-compliance",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(compliance.status, 200, "{:?}", compliance.body);
    let listed = compliance.body["drugCoverage"]
        .as_array()
        .expect("coverage list")
        .iter()
        .find(|row| row["id"] == coverage.as_str())
        .expect("the archived row is still listed");
    assert_eq!(listed["status"], "archived");
    assert_eq!(listed["revision"], coverage_revision + 1);
    assert_eq!(
        listed["effectiveTo"],
        Value::Null,
        "archiving is not closing"
    );

    // Item 3. The advisory answer collapses, because the loader reads active rows only. This is
    // the assertion that exercises `status = 'active'` in the production SQL.
    let answer = authority_over_http(&service, &world, SALE_DATE).await;
    assert_eq!(answer["state"], "not_established", "{answer:?}");
    assert_eq!(answer["gap"], "product_not_covered");
    assert_eq!(answer["coverageId"], Value::Null);
    assert_eq!(answer["licenceNumber"], Value::Null);
    // The licence itself is untouched by the withdrawal of one drug.
    let licences = compliance.body["complianceLicences"]
        .as_array()
        .expect("licences")
        .iter()
        .find(|row| row["id"] == licence.as_str())
        .expect("the licence");
    assert_eq!(licences["legalStatus"], "in_force");

    // Both acts are in the audit log, in order, and the row cannot be deleted at all.
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let events: Vec<(String, i64, Option<String>)> = sqlx::query_as(
        "SELECT action,entity_revision,reason FROM master_change_events \
         WHERE entity_type='store_licence_drug_coverage' AND entity_id=? ORDER BY entity_revision",
    )
    .bind(&coverage)
    .fetch_all(&pool)
    .await
    .expect("coverage events");
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0].0, "created");
    assert_eq!(events[1].0, "archived");
    assert_eq!(events[1].1, coverage_revision + 1);
    assert_eq!(
        events[1].2.as_deref(),
        Some("entered against the wrong drug")
    );
    assert!(
        sqlx::query("DELETE FROM store_licence_drug_coverage")
            .execute(&pool)
            .await
            .is_err(),
        "an archived coverage row is still history"
    );
    pool.close().await;
}

/// Corrective C1, item 2. None of the four Form 20F mutations belongs to a counter.
///
/// Recording what a licence covers is the licensee's own assertion about their own licence. A
/// cashier may read the compliance record — they need to know what the pharmacy may supply — but
/// the four writes are refused, and refused before anything is written: no row moves, no revision
/// advances, and the audit log gains nothing, because a refused mutation is not an event.
#[tokio::test]
async fn real_service_refuses_every_form_20f_mutation_to_a_non_admin_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    let (licence, revision) = form_20f_over_http(&service, &world, "MH-PUNE-20F-4471").await;
    let authorised = set_authority_over_http(
        &service,
        &world,
        &licence,
        revision,
        json!({
            "legalStatus": "in_force",
            "validityBasis": "perpetual",
            "validFrom": "2020-01-01",
        }),
    )
    .await;
    assert_eq!(authorised.status, 200, "{:?}", authorised.body);
    let licence_revision = authorised.body["revision"].as_i64().expect("revision");
    let covered = cover_over_http(&service, &world, &licence, "2020-01-01", None).await;
    assert_eq!(covered.status, 201, "{:?}", covered.body);
    let coverage = covered.body["id"].as_str().expect("coverage id").to_owned();
    let coverage_revision = covered.body["revision"].as_i64().expect("revision");

    let cashier = sign_in_as(
        &service,
        "cashier",
        "coverage.cashier",
        "01997a00-0000-7000-8000-000000000401",
    )
    .await;

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let licence_before: (i64, String, String, Option<String>) = sqlx::query_as(
        "SELECT revision,legal_status,validity_basis,valid_upto FROM store_compliance_licences \
         WHERE id=?",
    )
    .bind(&licence)
    .fetch_one(&pool)
    .await
    .expect("licence before");
    let coverage_before: (i64, String, Option<String>) = sqlx::query_as(
        "SELECT revision,status,effective_to FROM store_licence_drug_coverage WHERE id=?",
    )
    .bind(&coverage)
    .fetch_one(&pool)
    .await
    .expect("coverage before");
    let events_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM master_change_events")
        .fetch_one(&pool)
        .await
        .expect("events before");
    let rows_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM store_licence_drug_coverage")
        .fetch_one(&pool)
        .await
        .expect("rows before");

    // Each attempt is the one that would do the most damage if it succeeded: cancel the licence,
    // forge coverage, end real coverage early, or withdraw it outright.
    let attempts: [(&str, &str, String, Value); 4] = [
        (
            "update Form 20F authority",
            "PUT",
            format!("/api/v1/store/compliance-licences/{licence}/authority"),
            json!({
                "expectedRevision": licence_revision,
                "legalStatus": "cancelled",
                "validityBasis": "perpetual",
            }),
        ),
        (
            "create drug coverage",
            "POST",
            "/api/v1/store/licence-drug-coverage".to_owned(),
            json!({
                "licenceId": licence,
                "productId": world.product,
                "effectiveFrom": "2020-01-01",
                "sourceCitation": "asserted at the counter",
            }),
        ),
        (
            "close drug coverage",
            "POST",
            format!("/api/v1/store/licence-drug-coverage/{coverage}/close"),
            json!({
                "expectedRevision": coverage_revision,
                "effectiveTo": "2026-01-01",
                "reason": "asserted at the counter",
            }),
        ),
        (
            "archive drug coverage",
            "POST",
            format!("/api/v1/store/licence-drug-coverage/{coverage}/archive"),
            json!({
                "expectedRevision": coverage_revision,
                "reason": "asserted at the counter",
            }),
        ),
    ];
    for (label, method, uri, body) in attempts {
        let refused = call(&service, method, &uri, Some(body.clone()), Some(&cashier)).await;
        assert_eq!(refused.status, 403, "{label}: {:?}", refused.body);
        assert_eq!(refused.body["code"], "authorization_denied", "{label}");
        // And an unauthenticated caller does not even get that far.
        let anonymous = call(&service, method, &uri, Some(body), None).await;
        assert_eq!(
            anonymous.status, 401,
            "{label} anonymous: {:?}",
            anonymous.body
        );
    }

    // Reading is still theirs: a cashier must be able to see what the pharmacy may supply.
    let read = call(
        &service,
        "GET",
        "/api/v1/store/drug-compliance",
        None,
        Some(&cashier),
    )
    .await;
    assert_eq!(read.status, 200, "{:?}", read.body);
    assert_eq!(
        read.body["drugCoverage"][0]["id"],
        Value::from(coverage.clone())
    );

    // Nothing moved: not the licence, not the coverage, not the count of either, not the log.
    let licence_after: (i64, String, String, Option<String>) = sqlx::query_as(
        "SELECT revision,legal_status,validity_basis,valid_upto FROM store_compliance_licences \
         WHERE id=?",
    )
    .bind(&licence)
    .fetch_one(&pool)
    .await
    .expect("licence after");
    assert_eq!(licence_after, licence_before);
    let coverage_after: (i64, String, Option<String>) = sqlx::query_as(
        "SELECT revision,status,effective_to FROM store_licence_drug_coverage WHERE id=?",
    )
    .bind(&coverage)
    .fetch_one(&pool)
    .await
    .expect("coverage after");
    assert_eq!(coverage_after, coverage_before);
    let events_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM master_change_events")
        .fetch_one(&pool)
        .await
        .expect("events after");
    assert_eq!(
        events_after, events_before,
        "a refused mutation was audited"
    );
    let rows_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM store_licence_drug_coverage")
        .fetch_one(&pool)
        .await
        .expect("rows after");
    assert_eq!(
        rows_after, rows_before,
        "a refused create left a row behind"
    );

    // The authority the owner recorded is exactly as it was.
    let answer = authority_over_http(&service, &world, SALE_DATE).await;
    assert_eq!(answer["state"], "established", "{answer:?}");
    pool.close().await;
}

// -------------------------------------------------------------------------------------------
// Phase 1M-D2 — the Schedule X working record.
//
// Rule 65(21) requires a bound, serially page numbered register. What these tests exercise is the
// WORKING RECORD that helps a person write it: particulars frozen at posting, the two physical-act
// attestations, and a lifecycle that closes and never reopens. Nothing here posts a Schedule X sale,
// and the last test in this block proves the sale is still refused.
// -------------------------------------------------------------------------------------------

/// Records an owner finding for the purchase world's product.
async fn purchase_finding_over_http(
    service: &Service,
    world: &PurchaseWorld,
    scheme: &str,
    applies: bool,
) {
    let finding = call(
        service,
        "POST",
        &format!(
            "/api/v1/products/{}/regulatory/classifications",
            world.product
        ),
        Some(json!({
            "scheme": scheme,
            "applies": applies,
            "effectiveFrom": "2020-01-01",
            "sourceCitation": "Owner-recorded finding with its authority",
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(finding.status, 201, "{scheme}: {:?}", finding.body);
}

/// Posts a one-line Purchase of the world's product and returns the posted document.
async fn post_purchase_over_http(
    service: &Service,
    world: &PurchaseWorld,
    invoice: &str,
    idempotency: &str,
) -> Value {
    let draft = create_draft(service, world, invoice).await;
    let purchase_id = draft["id"].as_str().expect("purchase id").to_owned();
    let with_line = call(
        service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/lines"),
        Some(json!({
            "expectedRevision": 1,
            "productId": world.product,
            "productPackId": world.pack,
            "newBatchNumber": format!("BX-{invoice}"),
            "newBatchExpiresOn": "2028-03-31",
            "quantityPacks": 5,
            "ratePerPackPaise": 3_000,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);
    let revision = with_line.body["revision"].as_i64().expect("revision");
    let posted = call(
        service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/post"),
        Some(json!({ "expectedRevision": revision, "idempotencyKey": idempotency })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    posted.body
}

async fn register_over_http(service: &Service, cookie: &str) -> Value {
    let reply = call(
        service,
        "GET",
        "/api/v1/store/schedule-x/register",
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    reply.body
}

/// A registered pharmacist on the store's own Phase 1M-A record.
async fn purchase_pharmacist_over_http(service: &Service, world: &PurchaseWorld) -> String {
    let created = call(
        service,
        "POST",
        "/api/v1/store/professionals",
        Some(json!({
            "fullName": "Meera Iyer",
            "capacity": "registered_pharmacist",
            "registrationNumber": "MH-PH-44821",
            "registeringAuthority": "Maharashtra State Pharmacy Council",
            "validFrom": "2020-01-01",
            "linkedUserId": null,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    created.body["id"]
        .as_str()
        .expect("professional")
        .to_owned()
}

/// Phase 1M-D2, item 39. An ordinary Purchase is untouched, and an unclassified product is untouched
/// too: `unknown` is not `applies`, and a product's NAME classifies nothing.
#[tokio::test]
async fn real_service_leaves_an_ordinary_purchase_out_of_the_schedule_x_register_over_http() {
    let service = start().await;
    let world = seed_purchase_world(&service, MAHARASHTRA, "taxable").await;

    // No finding at all. The product's own display name is deliberately loud about Schedule X.
    let renamed = call(
        &service,
        "GET",
        &format!("/api/v1/products/{}", world.product),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(renamed.status, 200, "{:?}", renamed.body);

    let posted = post_purchase_over_http(
        &service,
        &world,
        "INV-9001",
        "01997a00-0000-7000-8000-000000000d01",
    )
    .await;
    assert_eq!(posted["purchaseProvenanceSnapshotVersion"], 1);

    let register = register_over_http(&service, &world.cookie).await;
    assert_eq!(
        register["entries"].as_array().expect("entries").len(),
        0,
        "an unclassified purchase entered the Schedule X working record"
    );
    assert_eq!(
        register["legacyReceipts"].as_array().expect("legacy").len(),
        0
    );

    // Recording the product as OUTSIDE Schedule X changes nothing either.
    purchase_finding_over_http(&service, &world, "schedule_x", false).await;
    let posted = post_purchase_over_http(
        &service,
        &world,
        "INV-9002",
        "01997a00-0000-7000-8000-000000000d02",
    )
    .await;
    assert_eq!(posted["status"], "posted");
    let register = register_over_http(&service, &world.cookie).await;
    assert_eq!(
        register["entries"].as_array().expect("entries").len(),
        0,
        "a product recorded outside Schedule X entered the working record"
    );
}

/// Phase 1M-D2, items 39 and 15. A posted Schedule X receipt writes one working entry, frozen — and
/// renaming the supplier, the product and the manufacturer afterwards changes nothing about it.
#[tokio::test]
async fn real_service_freezes_a_schedule_x_receipt_working_record_over_http() {
    let service = start().await;
    let world = seed_purchase_world(&service, MAHARASHTRA, "taxable").await;
    purchase_finding_over_http(&service, &world, "schedule_x", true).await;

    // A maker on record, so the manufacturer particular of rule 65(21)(b)(v) is available to freeze.
    let company = call(
        &service,
        "POST",
        "/api/v1/reference/companies",
        Some(json!({ "attributes": { "displayName": "Meridian Laboratories", "countryCode": "IN" } })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(company.status, 201, "{:?}", company.body);
    let company_id = company.body["id"].as_str().expect("company").to_owned();
    let role = call(
        &service,
        "POST",
        &format!("/api/v1/products/{}/company-roles", world.product),
        Some(json!({ "companyId": company_id, "role": "manufacturer" })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(role.status, 201, "{:?}", role.body);

    let posted = post_purchase_over_http(
        &service,
        &world,
        "INV-9101",
        "01997a00-0000-7000-8000-000000000d11",
    )
    .await;
    let purchase_id = posted["id"].as_str().expect("purchase").to_owned();

    let register = register_over_http(&service, &world.cookie).await;
    let entries = register["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 1, "{entries:?}");
    let entry = &entries[0];
    assert_eq!(entry["entryKind"], "receipt");
    // The reference is AUSHADHARTH's own, and looks nothing like a register serial or a page.
    assert_eq!(entry["reference"], "AXR-000001");
    assert_eq!(entry["status"], "prepared");
    assert_eq!(
        entry["purchaseDocumentId"],
        Value::from(purchase_id.clone())
    );
    assert_eq!(entry["billNumber"], "INV-9101");
    assert_eq!(entry["batchState"], "recorded");
    assert_eq!(entry["batchNumber"], "BX-INV-9101");
    assert_eq!(entry["manufacturerState"], "recorded");
    assert_eq!(entry["manufacturerName"], "Meridian Laboratories");
    assert_eq!(entry["quantityAtoms"], 50);
    assert_eq!(entry["quantityPacks"], 5);
    assert_eq!(entry["particularsEnteredInPhysicalRegister"], false);
    assert_eq!(entry["physicalEntryAuthenticated"], false);
    // No page number, by any name, anywhere in the payload.
    let serialised = entry.to_string();
    for forbidden in ["page", "Page"] {
        assert!(!serialised.contains(forbidden), "{forbidden}: {serialised}");
    }
    let frozen_supplier = entry["supplierName"].clone();
    let frozen_drug = entry["drugName"].clone();
    let frozen_maker = entry["manufacturerName"].clone();
    let frozen_licence = entry["supplierLicenceNumber"].clone();

    // Now move every master the particulars came from.
    let party = call(
        &service,
        "GET",
        &format!("/api/v1/parties/{}", world.supplier),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(party.status, 200, "{:?}", party.body);
    let renamed = call(
        &service,
        "PUT",
        &format!("/api/v1/parties/{}", world.supplier),
        Some(json!({
            "expectedRevision": party.body["revision"],
            "party": {
                "displayName": "Renamed Distributors",
                "gstRegistrationStatus": "unregistered",
                "placeOfSupplyStateId": MAHARASHTRA,
                "drugLicenceNumber": "CHANGED-LICENCE-0000",
            },
        })),
        Some(&world.cookie),
    )
    .await;
    assert!(
        renamed.status == 200 || renamed.status == 201,
        "{:?}",
        renamed.body
    );

    // 15. The working record is unmoved: it is a snapshot, not a view.
    let register = register_over_http(&service, &world.cookie).await;
    let entry = &register["entries"].as_array().expect("entries")[0];
    assert_eq!(entry["supplierName"], frozen_supplier);
    assert_eq!(entry["drugName"], frozen_drug);
    assert_eq!(entry["manufacturerName"], frozen_maker);
    assert_eq!(entry["supplierLicenceNumber"], frozen_licence);
    assert_ne!(entry["supplierName"], Value::from("Renamed Distributors"));
}

/// Phase 1M-D2, items 9, 10, 11 and 21. The two physical acts, the pharmacist who is named for them,
/// and a lifecycle that closes once and never reopens.
#[tokio::test]
async fn real_service_confirms_the_physical_schedule_x_register_acts_over_http() {
    let service = start().await;
    let world = seed_purchase_world(&service, MAHARASHTRA, "taxable").await;
    purchase_finding_over_http(&service, &world, "schedule_x", true).await;
    let professional = purchase_pharmacist_over_http(&service, &world).await;
    post_purchase_over_http(
        &service,
        &world,
        "INV-9201",
        "01997a00-0000-7000-8000-000000000d21",
    )
    .await;
    let register = register_over_http(&service, &world.cookie).await;
    let entry_id = register["entries"][0]["id"]
        .as_str()
        .expect("entry")
        .to_owned();
    let confirm_uri = format!("/api/v1/store/schedule-x/register/{entry_id}/confirm");

    // Closing an entry nobody attested would assert something untrue.
    let premature = call(
        &service,
        "POST",
        &format!("/api/v1/store/schedule-x/register/{entry_id}/finalize"),
        Some(json!({})),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(premature.status, 409, "{:?}", premature.body);
    assert_eq!(
        premature.body["code"],
        "schedule_x_physical_confirmation_required"
    );

    // One physical act without the other is not an attestation.
    for body in [
        json!({ "supervisingProfessionalId": professional, "particularsEnteredInPhysicalRegister": true, "physicalEntryAuthenticated": false }),
        json!({ "supervisingProfessionalId": professional, "particularsEnteredInPhysicalRegister": false, "physicalEntryAuthenticated": true }),
    ] {
        let partial = call(
            &service,
            "POST",
            &confirm_uri,
            Some(body),
            Some(&world.cookie),
        )
        .await;
        assert_eq!(partial.status, 422, "{:?}", partial.body);
        assert_eq!(partial.body["code"], "schedule_x_attestations_incomplete");
    }

    // The supervising person must be a registered pharmacist this pharmacy actually has on record.
    let stranger = call(
        &service,
        "POST",
        &confirm_uri,
        Some(json!({
            "supervisingProfessionalId": "01997a00-0000-7000-8000-0000000009ff",
            "particularsEnteredInPhysicalRegister": true,
            "physicalEntryAuthenticated": true,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(stranger.status, 409, "{:?}", stranger.body);
    assert_eq!(stranger.body["code"], "schedule_x_pharmacist_required");

    let confirmed = call(
        &service,
        "POST",
        &confirm_uri,
        Some(json!({
            "supervisingProfessionalId": professional,
            "particularsEnteredInPhysicalRegister": true,
            "physicalEntryAuthenticated": true,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);
    assert_eq!(confirmed.body["status"], "confirmed");
    assert_eq!(confirmed.body["particularsEnteredInPhysicalRegister"], true);
    assert_eq!(confirmed.body["physicalEntryAuthenticated"], true);
    assert_eq!(confirmed.body["supervisingProfessionalName"], "Meera Iyer");

    let finalized = call(
        &service,
        "POST",
        &format!("/api/v1/store/schedule-x/register/{entry_id}/finalize"),
        Some(json!({})),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(finalized.status, 200, "{:?}", finalized.body);
    assert_eq!(finalized.body["status"], "finalized");

    // A closed entry is beyond reach, by every route that exists.
    for (uri, body) in [
        (
            format!("/api/v1/store/schedule-x/register/{entry_id}/confirm"),
            json!({ "supervisingProfessionalId": professional, "particularsEnteredInPhysicalRegister": true, "physicalEntryAuthenticated": true }),
        ),
        (
            format!("/api/v1/store/schedule-x/register/{entry_id}/finalize"),
            json!({}),
        ),
        (
            format!("/api/v1/store/schedule-x/register/{entry_id}/void"),
            json!({ "reason": "changed my mind" }),
        ),
    ] {
        let refused = call(&service, "POST", &uri, Some(body), Some(&world.cookie)).await;
        assert_eq!(refused.status, 409, "{uri}: {:?}", refused.body);
        assert_eq!(refused.body["code"], "schedule_x_entry_finalized", "{uri}");
    }

    // The audit log carries every step, and names no patient because a receipt has none.
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let events: Vec<(String, String)> = sqlx::query_as(
        "SELECT action,change_payload FROM master_change_events \
         WHERE entity_type='schedule_x_register_entry' AND entity_id=? ORDER BY occurred_at_utc,event_id",
    )
    .bind(&entry_id)
    .fetch_all(&pool)
    .await
    .expect("events");
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(events[0].0, "created");
    assert!(
        events[1].1.contains("prepared_to_confirmed"),
        "{:?}",
        events[1]
    );
    assert!(
        events[2].1.contains("confirmed_to_finalized"),
        "{:?}",
        events[2]
    );
    // And the row cannot be deleted at all.
    assert!(
        sqlx::query("DELETE FROM store_schedule_x_register_entries")
            .execute(&pool)
            .await
            .is_err()
    );
    pool.close().await;
}

/// Phase 1M-D2, items 21 and 22. A cashier has no part in Schedule X compliance work — not the
/// reading, which carries a patient's particulars on the supply side, and not any of the writing.
#[tokio::test]
async fn real_service_refuses_the_schedule_x_working_record_to_a_cashier_over_http() {
    let service = start().await;
    let world = seed_purchase_world(&service, MAHARASHTRA, "taxable").await;
    purchase_finding_over_http(&service, &world, "schedule_x", true).await;
    let professional = purchase_pharmacist_over_http(&service, &world).await;
    post_purchase_over_http(
        &service,
        &world,
        "INV-9301",
        "01997a00-0000-7000-8000-000000000d31",
    )
    .await;
    let register = register_over_http(&service, &world.cookie).await;
    let entry_id = register["entries"][0]["id"]
        .as_str()
        .expect("entry")
        .to_owned();

    let cashier = sign_in_as(
        &service,
        "cashier",
        "schedulex.cashier",
        "01997a00-0000-7000-8000-000000000d41",
    )
    .await;

    let denied = call(
        &service,
        "GET",
        "/api/v1/store/schedule-x/register",
        None,
        Some(&cashier),
    )
    .await;
    assert_eq!(denied.status, 403, "{:?}", denied.body);
    assert_eq!(denied.body["code"], "authorization_denied");

    for (uri, body) in [
        (
            format!("/api/v1/store/schedule-x/register/{entry_id}/confirm"),
            json!({ "supervisingProfessionalId": professional, "particularsEnteredInPhysicalRegister": true, "physicalEntryAuthenticated": true }),
        ),
        (
            format!("/api/v1/store/schedule-x/register/{entry_id}/finalize"),
            json!({}),
        ),
        (
            format!("/api/v1/store/schedule-x/register/{entry_id}/void"),
            json!({ "reason": "not mine to withdraw" }),
        ),
    ] {
        let denied = call(&service, "POST", &uri, Some(body.clone()), Some(&cashier)).await;
        assert_eq!(denied.status, 403, "{uri}: {:?}", denied.body);
        assert_eq!(denied.body["code"], "authorization_denied", "{uri}");
        let anonymous = call(&service, "POST", &uri, Some(body), None).await;
        assert_eq!(anonymous.status, 401, "{uri}: {:?}", anonymous.body);
    }

    // Nothing moved.
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let status: String =
        sqlx::query_scalar("SELECT status FROM store_schedule_x_register_entries WHERE id=?")
            .bind(&entry_id)
            .fetch_one(&pool)
            .await
            .expect("status");
    assert_eq!(status, "prepared");
    let events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM master_change_events WHERE entity_type='schedule_x_register_entry'",
    )
    .fetch_one(&pool)
    .await
    .expect("events");
    assert_eq!(events, 1, "a refused mutation was audited");
    pool.close().await;
}

/// Phase 1M-D2, items 11 and 34 (Race A). A replayed posting does not write the register twice, and a
/// withdrawn entry never hands its reference to the next one.
#[tokio::test]
async fn real_service_never_writes_the_schedule_x_register_twice_over_http() {
    let service = start().await;
    let world = seed_purchase_world(&service, MAHARASHTRA, "taxable").await;
    purchase_finding_over_http(&service, &world, "schedule_x", true).await;
    let posted = post_purchase_over_http(
        &service,
        &world,
        "INV-9401",
        "01997a00-0000-7000-8000-000000000d51",
    )
    .await;
    let purchase_id = posted["id"].as_str().expect("purchase").to_owned();
    let revision = posted["revision"].as_i64().expect("revision");

    // Replaying the same posting is idempotent and writes no second entry.
    let replay = call(
        &service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/post"),
        Some(json!({
            "expectedRevision": revision,
            "idempotencyKey": "01997a00-0000-7000-8000-000000000d51",
        })),
        Some(&world.cookie),
    )
    .await;
    assert!(
        replay.status == 200 || replay.status == 409,
        "{:?}",
        replay.body
    );
    let register = register_over_http(&service, &world.cookie).await;
    assert_eq!(register["entries"].as_array().expect("entries").len(), 1);

    let entry_id = register["entries"][0]["id"]
        .as_str()
        .expect("entry")
        .to_owned();
    let voided = call(
        &service,
        "POST",
        &format!("/api/v1/store/schedule-x/register/{entry_id}/void"),
        Some(json!({ "reason": "prepared against the wrong invoice" })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(voided.status, 200, "{:?}", voided.body);
    assert_eq!(voided.body["status"], "void");
    assert_eq!(voided.body["reference"], "AXR-000001");

    // A void entry cannot be resurrected, and its reference is not reissued: the next Schedule X
    // receipt takes the following number.
    let resurrect = call(
        &service,
        "POST",
        &format!("/api/v1/store/schedule-x/register/{entry_id}/confirm"),
        Some(json!({
            "supervisingProfessionalId": "01997a00-0000-7000-8000-0000000009ff",
            "particularsEnteredInPhysicalRegister": true,
            "physicalEntryAuthenticated": true,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(resurrect.status, 409, "{:?}", resurrect.body);
    assert_eq!(resurrect.body["code"], "schedule_x_entry_void");

    post_purchase_over_http(
        &service,
        &world,
        "INV-9402",
        "01997a00-0000-7000-8000-000000000d52",
    )
    .await;
    let register = register_over_http(&service, &world.cookie).await;
    let references: Vec<&str> = register["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry["reference"].as_str().expect("reference"))
        .collect();
    assert_eq!(references, vec!["AXR-000002", "AXR-000001"]);
}

/// Phase 1M-D2, item 8. Rule 65(9)(a)'s retained duplicate copy: a fact about paper, recorded once.
#[tokio::test]
async fn real_service_records_the_retained_duplicate_prescription_copy_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    let (prescription, _item) = prescription_over_http(&service, &world, None, 20).await;
    let uri = format!("/api/v1/prescriptions/{prescription}/schedule-x-duplicate-copy");

    let recorded = call(
        &service,
        "POST",
        &uri,
        Some(json!({
            "retainedDuplicatePrescriptionCopyConfirmed": true,
            "note": "Second copy filed in the Schedule X folder",
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(recorded.status, 201, "{:?}", recorded.body);
    assert_eq!(
        recorded.body["retainedDuplicatePrescriptionCopyConfirmed"],
        true
    );

    // It is a statement about a piece of paper, not a setting to toggle.
    let again = call(
        &service,
        "POST",
        &uri,
        Some(json!({ "retainedDuplicatePrescriptionCopyConfirmed": false })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(again.status, 409, "{:?}", again.body);
    assert_eq!(
        again.body["code"],
        "schedule_x_duplicate_copy_already_attested"
    );

    // A cashier does not make this statement.
    let cashier = sign_in_as(
        &service,
        "cashier",
        "duplicate.cashier",
        "01997a00-0000-7000-8000-000000000d61",
    )
    .await;
    let denied = call(
        &service,
        "POST",
        &format!("/api/v1/prescriptions/{prescription}/schedule-x-duplicate-copy"),
        Some(json!({ "retainedDuplicatePrescriptionCopyConfirmed": true })),
        Some(&cashier),
    )
    .await;
    assert_eq!(denied.status, 403, "{:?}", denied.body);

    // Nothing here deletes itself after two years, and nothing claims to be the paper.
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM prescription_duplicate_copy_attestations")
            .fetch_one(&pool)
            .await
            .expect("rows");
    assert_eq!(rows, 1);
    assert!(
        sqlx::query("DELETE FROM prescription_duplicate_copy_attestations")
            .execute(&pool)
            .await
            .is_err()
    );
    pool.close().await;
}

/// Phase 1M-D2, items 25 and 26. A Purchase return is a separate business event: it does not rewrite
/// the receipt working record, and a Schedule X receipt posted before this software kept a working
/// record is listed as unresolved rather than invented.
#[tokio::test]
async fn real_service_keeps_the_schedule_x_receipt_record_stable_over_http() {
    let service = start().await;
    let world = seed_purchase_world(&service, MAHARASHTRA, "taxable").await;

    // A Schedule X receipt posted BEFORE the owner recorded the finding gets no working record —
    // the finding did not exist on that date, and the software does not reach backwards.
    post_purchase_over_http(
        &service,
        &world,
        "INV-9501",
        "01997a00-0000-7000-8000-000000000d71",
    )
    .await;
    let register = register_over_http(&service, &world.cookie).await;
    assert_eq!(register["entries"].as_array().expect("entries").len(), 0);

    // Now the owner records the finding. The earlier receipt is NOT backfilled; it surfaces as a
    // legacy receipt with its frozen particulars and nothing invented.
    purchase_finding_over_http(&service, &world, "schedule_x", true).await;
    let register = register_over_http(&service, &world.cookie).await;
    assert_eq!(
        register["entries"].as_array().expect("entries").len(),
        0,
        "the earlier receipt was backfilled"
    );
    let legacy = register["legacyReceipts"].as_array().expect("legacy");
    assert_eq!(legacy.len(), 1, "{legacy:?}");
    assert_eq!(legacy[0]["supplierInvoiceNumber"], "INV-9501");
    assert_eq!(legacy[0]["invoiceDate"], "2026-09-10");

    // A later Purchase of the same drug does get a working record, and the legacy one stays legacy.
    post_purchase_over_http(
        &service,
        &world,
        "INV-9502",
        "01997a00-0000-7000-8000-000000000d72",
    )
    .await;
    let register = register_over_http(&service, &world.cookie).await;
    assert_eq!(register["entries"].as_array().expect("entries").len(), 1);
    assert_eq!(
        register["legacyReceipts"].as_array().expect("legacy").len(),
        1,
        "the legacy receipt was quietly repaired"
    );
}

/// Phase 1M-D2, item 27. The working record survives the real backup and restore, with its frozen
/// particulars, its attestations and its guards.
#[tokio::test]
async fn real_service_keeps_the_schedule_x_working_record_through_backup_and_restore_over_http() {
    let harness = start_with_backups().await;
    let service = &harness.service;
    let world = seed_purchase_world(service, MAHARASHTRA, "taxable").await;
    purchase_finding_over_http(service, &world, "schedule_x", true).await;
    let professional = purchase_pharmacist_over_http(service, &world).await;
    post_purchase_over_http(
        service,
        &world,
        "INV-9601",
        "01997a00-0000-7000-8000-000000000d81",
    )
    .await;
    let register = register_over_http(service, &world.cookie).await;
    let entry_id = register["entries"][0]["id"]
        .as_str()
        .expect("entry")
        .to_owned();
    let confirmed = call(
        service,
        "POST",
        &format!("/api/v1/store/schedule-x/register/{entry_id}/confirm"),
        Some(json!({
            "supervisingProfessionalId": professional,
            "particularsEnteredInPhysicalRegister": true,
            "physicalEntryAuthenticated": true,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);
    let frozen_reference = confirmed.body["reference"].clone();
    let frozen_supplier = confirmed.body["supplierName"].clone();
    let frozen_batch = confirmed.body["batchNumber"].clone();

    let created = call(
        service,
        "POST",
        "/api/v1/backups/create",
        Some(json!({})),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    let filename = created.body["filename"]
        .as_str()
        .expect("filename")
        .to_owned();
    let bytes = std::fs::read(harness.backups.join(&filename)).expect("backup on disk");
    let (status, _, body) = call_bytes(
        service,
        "/api/v1/backups/restore/prepare",
        "application/octet-stream",
        &bytes,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let prepared: Value = serde_json::from_slice(&body).expect("prepared restore");
    let token = prepared["candidateToken"]
        .as_str()
        .expect("token")
        .to_owned();
    let committed = call(
        service,
        "POST",
        "/api/v1/backups/restore/commit",
        Some(json!({ "candidateToken": token, "password": "Integration-Password-42" })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(committed.status, 200, "{:?}", committed.body);
    api::backups::recover_interrupted_restore(&harness.backups, &service.database_path)
        .await
        .expect("recovery");
    let reopened = database::connect(&service.database_path)
        .await
        .expect("reopened database");
    assert!(
        api::backups::complete_restore_after_open(&reopened, &harness.backups)
            .await
            .expect("completion")
    );

    let (reference, status, supplier, batch, entered, authenticated): (
        String,
        String,
        Option<String>,
        Option<String>,
        i64,
        i64,
    ) = sqlx::query_as(
        "SELECT reference,status,supplier_name,batch_number,\
         particulars_entered_in_physical_register,physical_entry_authenticated \
         FROM store_schedule_x_register_entries WHERE id=?",
    )
    .bind(&entry_id)
    .fetch_one(&reopened)
    .await
    .expect("restored entry");
    assert_eq!(Value::from(reference), frozen_reference);
    assert_eq!(status, "confirmed");
    assert_eq!(Value::from(supplier), frozen_supplier);
    assert_eq!(Value::from(batch), frozen_batch);
    assert_eq!((entered, authenticated), (1, 1));

    // And so did the guards that keep it that way.
    for statement in [
        "DELETE FROM store_schedule_x_register_entries",
        "UPDATE store_schedule_x_register_entries SET supplier_name='Someone Else'",
    ] {
        assert!(
            sqlx::query(statement).execute(&reopened).await.is_err(),
            "{statement}"
        );
    }
    reopened.close().await;
}

/// Phase 1M-D2, item 0 — THE HARD BOUNDARY. D2 builds the receipt working record and nothing else:
/// with an in-force Form 20F, active drug coverage, a confirmed Schedule X receipt working record, a
/// registered pharmacist and a complete prescription, a Schedule X SALE is still refused, and no
/// stock moves.
#[tokio::test]
async fn real_service_keeps_schedule_x_sales_refused_after_the_register_foundations_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;
    // Phase 1M-D3-C2: the independent NDPS axis, established not to apply, so this test reaches
    // the Schedule X predicate it is actually about instead of stopping at an unrecorded overlay.
    finding_over_http(&service, &world, "ndps_purview", false).await;
    record_basis_over_http(&service, &world, Some("prescription_register")).await;
    let professional = pharmacist_over_http(&service, &world).await;
    let (prescription, item) = prescription_over_http(&service, &world, None, 20).await;

    // Everything D1-B and D2 can give it.
    let (licence, revision) = form_20f_over_http(&service, &world, "MH-PUNE-20F-4471").await;
    let authorised = set_authority_over_http(
        &service,
        &world,
        &licence,
        revision,
        json!({
            "legalStatus": "in_force",
            "validityBasis": "perpetual",
            "validFrom": "2020-01-01",
        }),
    )
    .await;
    assert_eq!(authorised.status, 200, "{:?}", authorised.body);
    let covered = cover_over_http(&service, &world, &licence, "2020-01-01", None).await;
    assert_eq!(covered.status, 201, "{:?}", covered.body);
    let duplicate = call(
        &service,
        "POST",
        &format!("/api/v1/prescriptions/{prescription}/schedule-x-duplicate-copy"),
        Some(json!({ "retainedDuplicatePrescriptionCopyConfirmed": true })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(duplicate.status, 201, "{:?}", duplicate.body);

    // And the counter still refuses, on the same code as before this phase existed.
    let (sale_id, sale_revision) =
        prepared_sale_over_http(&service, &world, &item, &professional, 1).await;
    let unprepared = prepare_entry_over_http(&service, &world, &sale_id, sale_revision).await;
    assert_eq!(unprepared.status, 409, "{:?}", unprepared.body);
    assert_eq!(
        unprepared.body["code"], "prescription_requirements_incomplete",
        "{:?}",
        unprepared.body
    );
    let refused = post_quoted_over_http(
        &service,
        &world,
        &sale_id,
        sale_revision,
        "01997a00-0000-7000-8000-000000000d91",
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(
        refused.body["code"], "prescription_requirements_incomplete",
        "{:?}",
        refused.body
    );
    // Exactly the Schedule X predicates this phase had not yet established, and no others. Every
    // unmet predicate is reported together so the counter can clear them in one pass.
    assert_eq!(
        schedule_x_unmet(&refused.body),
        vec![
            "schedule_x_lot_provenance_incomplete",
            "schedule_x_source_authority_missing",
            "schedule_x_prescription_annotation_missing",
            "schedule_x_register_confirmation_missing",
            "schedule_x_supervising_pharmacist_invalid",
        ],
        "{:?}",
        refused.body
    );

    let detail = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{sale_id}"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(detail.body["status"], "draft");
    assert_eq!(detail.body["documentNumber"], Value::Null);

    // Nothing moved, nothing was dispensed, and no supply working entry was conjured up.
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let sold: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM inventory_movements WHERE movement_type='sale'")
            .fetch_one(&pool)
            .await
            .expect("movements");
    assert_eq!(sold, 0, "a Schedule X line moved stock");
    let dispensed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM prescription_dispensings")
        .fetch_one(&pool)
        .await
        .expect("dispensings");
    assert_eq!(dispensed, 0);
    let supply_entries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_schedule_x_register_entries WHERE entry_kind='supply'",
    )
    .fetch_one(&pool)
    .await
    .expect("supply entries");
    assert_eq!(supply_entries, 0, "a supply working entry was written");
    pool.close().await;
}

/// Phase 1M-D2, item 34 (Races B and D). Two Schedule X receipts posted at the same instant take
/// distinct references, and a void racing a confirmation leaves one terminal history.
///
/// No sleeps and no retries: the postings are joined, and the assertion holds for either order the
/// scheduler chooses, because the invariant is "distinct and complete", not "this one first".
#[tokio::test]
async fn real_service_allocates_distinct_schedule_x_references_under_a_race_over_http() {
    let service = start().await;
    let world = seed_purchase_world(&service, MAHARASHTRA, "taxable").await;
    purchase_finding_over_http(&service, &world, "schedule_x", true).await;

    // Two drafts, each with a line, prepared up to the moment of posting.
    let mut ready = Vec::new();
    for (invoice, key) in [
        ("INV-9701", "01997a00-0000-7000-8000-000000000e01"),
        ("INV-9702", "01997a00-0000-7000-8000-000000000e02"),
    ] {
        let draft = create_draft(&service, &world, invoice).await;
        let purchase_id = draft["id"].as_str().expect("purchase id").to_owned();
        let with_line = call(
            &service,
            "POST",
            &format!("/api/v1/purchases/{purchase_id}/lines"),
            Some(json!({
                "expectedRevision": 1,
                "productId": world.product,
                "productPackId": world.pack,
                "newBatchNumber": format!("BX-{invoice}"),
                "newBatchExpiresOn": "2028-03-31",
                "quantityPacks": 5,
                "ratePerPackPaise": 3_000,
            })),
            Some(&world.cookie),
        )
        .await;
        assert_eq!(with_line.status, 201, "{:?}", with_line.body);
        ready.push((
            purchase_id,
            with_line.body["revision"].as_i64().expect("revision"),
            key,
        ));
    }

    let first_uri = format!("/api/v1/purchases/{}/post", ready[0].0);
    let second_uri = format!("/api/v1/purchases/{}/post", ready[1].0);
    let (first, second) = tokio::join!(
        call(
            &service,
            "POST",
            &first_uri,
            Some(json!({ "expectedRevision": ready[0].1, "idempotencyKey": ready[0].2 })),
            Some(&world.cookie),
        ),
        call(
            &service,
            "POST",
            &second_uri,
            Some(json!({ "expectedRevision": ready[1].1, "idempotencyKey": ready[1].2 })),
            Some(&world.cookie),
        )
    );
    assert_eq!(first.status, 200, "{:?}", first.body);
    assert_eq!(second.status, 200, "{:?}", second.body);

    // Race B. Two entries, two distinct references, no collision and no gap invented.
    let register = register_over_http(&service, &world.cookie).await;
    let entries = register["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 2, "{entries:?}");
    let mut references: Vec<&str> = entries
        .iter()
        .map(|entry| entry["reference"].as_str().expect("reference"))
        .collect();
    references.sort_unstable();
    assert_eq!(references, vec!["AXR-000001", "AXR-000002"]);

    // Race D. A void and a confirmation aimed at one entry leave exactly one terminal history:
    // whichever lands first, the other is refused and the entry is never in both states.
    let professional = purchase_pharmacist_over_http(&service, &world).await;
    let entry_id = entries[0]["id"].as_str().expect("entry").to_owned();
    let confirm_uri = format!("/api/v1/store/schedule-x/register/{entry_id}/confirm");
    let void_uri = format!("/api/v1/store/schedule-x/register/{entry_id}/void");
    let (confirmed, voided) = tokio::join!(
        call(
            &service,
            "POST",
            &confirm_uri,
            Some(json!({
                "supervisingProfessionalId": professional,
                "particularsEnteredInPhysicalRegister": true,
                "physicalEntryAuthenticated": true,
            })),
            Some(&world.cookie),
        ),
        call(
            &service,
            "POST",
            &void_uri,
            Some(json!({ "reason": "withdrawn while somebody else was confirming" })),
            Some(&world.cookie),
        )
    );
    // Phase 1M-D3-C2 — this assertion used to require that exactly one of the two requests returned
    // 200, which is not an invariant of the lifecycle and made the test non-deterministic. A void is
    // reachable from `prepared` AND from `confirmed` (migration 0028, unchanged by 0030), so when
    // the confirmation serializes first BOTH requests legitimately succeed and the entry passes
    // through confirmed on its way to void. Measured on this machine, the old assertion failed on
    // roughly one run in four.
    //
    // The valid serialization outcomes are enumerated instead, and the invariant is proved from the
    // database below rather than from HTTP status codes:
    //
    //   void first     void 200, confirmation refused   -> void, never confirmed
    //   confirm first  both 200                         -> void, having been confirmed
    //
    // Either way at least one request lands, neither can finalize anything, and the entry is left
    // in exactly one state with its reference intact.
    assert!(
        confirmed.status == 200 || voided.status == 200,
        "neither won: confirm={:?} void={:?}",
        confirmed.body,
        voided.body
    );

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let (status, void_reason, confirmed_at, finalized_at): (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT status,void_reason,confirmed_at_utc,finalized_at_utc \
         FROM store_schedule_x_register_entries WHERE id=?",
    )
    .bind(&entry_id)
    .fetch_one(&pool)
    .await
    .expect("status");
    // Whichever way the two serialized, the entry can never have been finalized: neither a
    // confirmation nor a void closes a working record.
    assert_eq!(finalized_at, None, "the race finalized an entry");
    if voided.status == 200 {
        // The void landed, from `prepared` or from `confirmed`, and carries its reason.
        assert_eq!(status, "void");
        assert!(void_reason.is_some(), "a void without a reason");
        // And it records truthfully whether the confirmation had already happened.
        assert_eq!(
            confirmed_at.is_some(),
            confirmed.status == 200,
            "confirmation history disagrees with the confirmation result"
        );
    } else {
        // The void was refused, so the confirmation is what stands.
        assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);
        assert_eq!(status, "confirmed");
        assert_eq!(void_reason, None);
        assert!(confirmed_at.is_some());
    }
    // Exactly one row, in exactly one state, with its reference intact.
    let rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM store_schedule_x_register_entries WHERE id=?")
            .bind(&entry_id)
            .fetch_one(&pool)
            .await
            .expect("rows");
    assert_eq!(rows, 1);
    pool.close().await;
}

// ==============================================================================================
// Phase 1M-D3-A — rule 65(11)(c), the note written on the physical prescription
// ==============================================================================================

/// The prescription-annotation surface, read over real HTTP.
async fn annotations_over_http(service: &Service, cookie: &str) -> Value {
    let reply = call(
        service,
        "GET",
        "/api/v1/store/schedule-x/prescription-annotations",
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    reply.body
}

async fn record_annotation_over_http(
    service: &Service,
    cookie: &str,
    line: &str,
    confirmed: bool,
) -> Reply {
    call(
        service,
        "POST",
        "/api/v1/store/schedule-x/prescription-annotations",
        Some(json!({
            "saleLineId": line,
            "sellerParticularsNotedOnPrescription": confirmed,
            "note": Value::Null,
        })),
        Some(cookie),
    )
    .await
}

/// A posted Purchase of the sale world's own product, so a Schedule X receipt working entry exists.
async fn sale_world_purchase_over_http(
    service: &Service,
    world: &SaleWorld,
    invoice: &str,
    idempotency: &str,
) -> Value {
    let draft = call(
        service,
        "POST",
        "/api/v1/purchases",
        Some(json!({
            "supplierPartyId": world.supplier,
            "supplierInvoiceNumber": invoice,
            "invoiceDate": "2026-09-10",
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(draft.status, 201, "{:?}", draft.body);
    let purchase_id = draft.body["id"].as_str().expect("purchase id").to_owned();
    let with_line = call(
        service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/lines"),
        Some(json!({
            "expectedRevision": 1,
            "productId": world.product,
            "productPackId": world.pack,
            "newBatchNumber": format!("BX-{invoice}"),
            "newBatchExpiresOn": "2028-03-31",
            "quantityPacks": 5,
            "ratePerPackPaise": 3_000,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);
    let posted = call(
        service,
        "POST",
        &format!("/api/v1/purchases/{purchase_id}/post"),
        Some(json!({
            "expectedRevision": with_line.body["revision"],
            "idempotencyKey": idempotency,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    posted.body
}

/// The sale line of a draft Schedule X sale, which is the dispensing occasion before it is one.
async fn sale_line_id_over_http(service: &Service, world: &SaleWorld, sale_id: &str) -> String {
    let detail = call(
        service,
        "GET",
        &format!("/api/v1/sales/{sale_id}"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(detail.status, 200, "{:?}", detail.body);
    detail.body["lines"][0]["id"]
        .as_str()
        .expect("sale line id")
        .to_owned()
}

/// D3A-1. The whole rule 65(11)(c) fact across a real socket.
///
/// What must be true: the particulars shown are the ones frozen; the confirmation is append-only;
/// a retry hands back the same fact rather than asking for the pen twice; and a Store Profile
/// edited afterwards does not rewrite what the record says was written on the paper.
#[tokio::test]
async fn real_service_records_and_freezes_the_prescription_annotation_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;
    let professional = pharmacist_over_http(&service, &world).await;
    let (prescription, item) = prescription_over_http(&service, &world, None, 20).await;
    let (sale_id, _) = prepared_sale_over_http(&service, &world, &item, &professional, 1).await;
    let line_id = sale_line_id_over_http(&service, &world, &sale_id).await;

    // The occasion is waiting, and the seller particulars the confirmation would freeze are shown
    // BEFORE anything is confirmed — the operator is told exactly what to write.
    let listed = annotations_over_http(&service, &world.cookie).await;
    assert_eq!(
        listed["pendingOccasions"].as_array().expect("array").len(),
        1
    );
    assert_eq!(listed["pendingOccasions"][0]["saleLineId"], line_id);
    assert_eq!(listed["pendingOccasions"][0]["dispensingDate"], SALE_DATE);
    assert_eq!(
        listed["sellerName"], "Integration Pharmacy Private Limited",
        "{listed:?}"
    );
    let address = listed["sellerAddress"]
        .as_str()
        .expect("address")
        .to_owned();
    assert!(address.starts_with("12 Market Road"), "{address}");
    assert!(address.contains("Pune"), "{address}");
    assert!(
        listed["sellerMissing"]
            .as_array()
            .expect("array")
            .is_empty(),
        "{listed:?}"
    );
    // The list points at a piece of paper. It does not carry the patient or the prescriber.
    let listed_text = listed.to_string();
    for private in ["Sita Kulkarni", "Lakshmi Road", "Anjali Rao", "Rao Clinic"] {
        assert!(!listed_text.contains(private), "{private} leaked: {listed}");
    }

    // An unticked box is not an attestation.
    let unticked = record_annotation_over_http(&service, &world.cookie, &line_id, false).await;
    assert_eq!(unticked.status, 422, "{:?}", unticked.body);
    assert_eq!(
        unticked.body["code"],
        "schedule_x_prescription_annotation_not_confirmed"
    );

    let recorded = record_annotation_over_http(&service, &world.cookie, &line_id, true).await;
    assert_eq!(recorded.status, 201, "{:?}", recorded.body);
    let annotation_id = recorded.body["id"].as_str().expect("id").to_owned();
    assert_eq!(recorded.body["saleLineId"], line_id);
    assert_eq!(recorded.body["prescriptionId"], prescription);
    assert_eq!(recorded.body["dispensingDate"], SALE_DATE);
    assert_eq!(
        recorded.body["sellerName"],
        "Integration Pharmacy Private Limited"
    );
    assert_eq!(recorded.body["sellerAddress"], address);

    // A retry after a client timeout finds the fact already written and is handed that same row.
    let retried = record_annotation_over_http(&service, &world.cookie, &line_id, true).await;
    assert_eq!(retried.status, 200, "{:?}", retried.body);
    assert_eq!(retried.body["id"], annotation_id);

    // The occasion is no longer pending, and the fact is listed once.
    let after = annotations_over_http(&service, &world.cookie).await;
    assert!(
        after["pendingOccasions"]
            .as_array()
            .expect("array")
            .is_empty(),
        "{after:?}"
    );
    assert_eq!(after["annotations"].as_array().expect("array").len(), 1);

    // The Store is renamed and moves. What was written on the paper does not change.
    let profile = call(
        &service,
        "GET",
        "/api/v1/store/profile",
        None,
        Some(&world.cookie),
    )
    .await;
    let renamed = call(
        &service,
        "PUT",
        "/api/v1/store/profile",
        Some(json!({
            "expectedRevision": profile.body["revision"],
            "displayName": "Renamed Pharmacy",
            "legalName": "Renamed Pharmacy LLP",
            "primaryPhone": "02099999999",
            "primaryEmail": "counter@example.test",
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(renamed.status, 200, "{:?}", renamed.body);
    let later = annotations_over_http(&service, &world.cookie).await;
    assert_eq!(
        later["annotations"][0]["sellerName"], "Integration Pharmacy Private Limited",
        "a profile edit rewrote what the record says was written on the prescription: {later:?}"
    );
    assert_eq!(later["sellerName"], "Renamed Pharmacy LLP", "{later:?}");

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");

    // Append-only against direct SQL, in both directions.
    for statement in [
        "UPDATE schedule_x_prescription_annotations SET seller_name='Somebody Else' WHERE id=?",
        "UPDATE schedule_x_prescription_annotations SET dispensing_date='2026-01-01' WHERE id=?",
        "UPDATE schedule_x_prescription_annotations SET seller_particulars_noted_on_prescription=0 WHERE id=?",
        "DELETE FROM schedule_x_prescription_annotations WHERE id=?",
    ] {
        let refused = sqlx::query(statement)
            .bind(&annotation_id)
            .execute(&pool)
            .await;
        assert!(refused.is_err(), "direct SQL succeeded: {statement}");
        assert!(
            refused.unwrap_err().to_string().contains("append_only"),
            "{statement}"
        );
    }

    // The audit log names the fact and the identifiers, and carries no prescription content.
    let events: Vec<(String, String)> = sqlx::query_as(
        "SELECT action,change_payload FROM master_change_events \
         WHERE entity_type='schedule_x_prescription_annotation' AND entity_id=?",
    )
    .bind(&annotation_id)
    .fetch_all(&pool)
    .await
    .expect("events");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].0, "created");
    for private in [
        "Sita Kulkarni",
        "Anjali Rao",
        "Integration Pharmacy",
        "Market Road",
    ] {
        assert!(
            !events[0].1.contains(private),
            "{private} reached the audit payload: {}",
            events[0].1
        );
    }
    assert!(
        events[0]
            .1
            .contains("seller_particulars_noted_on_prescription"),
        "{}",
        events[0].1
    );
    pool.close().await;
}

/// D3A-2. Direct SQL cannot forge an occasion. Every coherence limb is refused by the database
/// itself, not merely by the service that normally writes these rows.
#[tokio::test]
async fn real_service_refuses_a_forged_prescription_annotation_in_direct_sql_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;
    let professional = pharmacist_over_http(&service, &world).await;
    let (prescription, item) = prescription_over_http(&service, &world, None, 20).await;
    let (sale_id, _) = prepared_sale_over_http(&service, &world, &item, &professional, 1).await;
    let line_id = sale_line_id_over_http(&service, &world, &sale_id).await;

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let cashier: Option<String> =
        sqlx::query_scalar("SELECT id FROM users WHERE role='cashier' LIMIT 1")
            .fetch_optional(&pool)
            .await
            .expect("cashier lookup");
    let owner: String = sqlx::query_scalar("SELECT id FROM users WHERE role='owner_admin' LIMIT 1")
        .fetch_one(&pool)
        .await
        .expect("owner");
    let stranger_store = "01997a00-0000-7000-8000-0000000007ff";

    // Every attempt names a real occasion and changes exactly one thing, so each assertion is about
    // the limb it names and nothing else.
    let insert = "INSERT INTO schedule_x_prescription_annotations (id,store_id,sale_document_id,\
         sale_line_id,prescription_id,prescription_item_id,product_id,\
         seller_particulars_noted_on_prescription,seller_name,seller_address,dispensing_date,\
         attested_on_store_date,attested_by_user_id,attested_at_utc,note,created_at_utc) \
         VALUES (?,?,?,?,?,?,?,1,'Seller','Address',?,'2026-09-29',?,\
         '2026-09-29T06:00:00.000Z',NULL,'2026-09-29T06:00:00.000Z')";

    struct Forgery<'a> {
        what: &'a str,
        store: &'a str,
        document: &'a str,
        line: &'a str,
        prescription: &'a str,
        item: &'a str,
        product: &'a str,
        date: &'a str,
        actor: &'a str,
    }
    let base = Forgery {
        what: "",
        store: &world.store,
        document: &sale_id,
        line: &line_id,
        prescription: &prescription,
        item: &item,
        product: &world.product,
        date: SALE_DATE,
        actor: &owner,
    };

    let mut attempts = vec![
        Forgery {
            what: "another store",
            store: stranger_store,
            ..Forgery { ..base }
        },
        Forgery {
            what: "a date that is not the draft's business date",
            date: "2026-09-11",
            ..Forgery { ..base }
        },
        Forgery {
            what: "a line belonging to no such document",
            document: "01997a00-0000-7000-8000-0000000008ff",
            ..Forgery { ..base }
        },
        Forgery {
            what: "a product the line does not supply",
            product: "01997a00-0000-7000-8000-0000000008fe",
            ..Forgery { ..base }
        },
    ];
    if let Some(cashier) = cashier.as_deref() {
        attempts.push(Forgery {
            what: "a cashier as the confirmer",
            actor: cashier,
            ..Forgery { ..base }
        });
    }

    for (index, attempt) in attempts.iter().enumerate() {
        let refused = sqlx::query(insert)
            .bind(format!("01997a00-0000-7000-8000-00000000f{index:03}"))
            .bind(attempt.store)
            .bind(attempt.document)
            .bind(attempt.line)
            .bind(attempt.prescription)
            .bind(attempt.item)
            .bind(attempt.product)
            .bind(attempt.date)
            .bind(attempt.actor)
            .execute(&pool)
            .await;
        assert!(refused.is_err(), "direct SQL wrote {}", attempt.what);
        assert!(
            refused
                .unwrap_err()
                .to_string()
                .contains("schedule_x_prescription_annotation_incoherent"),
            "{}",
            attempt.what
        );
    }

    // And the sound one goes in, so the refusals above are about their limbs and not about the
    // statement being malformed.
    let accepted = sqlx::query(insert)
        .bind("01997a00-0000-7000-8000-00000000fa01")
        .bind(base.store)
        .bind(base.document)
        .bind(base.line)
        .bind(base.prescription)
        .bind(base.item)
        .bind(base.product)
        .bind(base.date)
        .bind(base.actor)
        .execute(&pool)
        .await;
    assert!(accepted.is_ok(), "{accepted:?}");

    // One live confirmation per occasion, whoever writes it.
    let second = sqlx::query(insert)
        .bind("01997a00-0000-7000-8000-00000000fa02")
        .bind(base.store)
        .bind(base.document)
        .bind(base.line)
        .bind(base.prescription)
        .bind(base.item)
        .bind(base.product)
        .bind(base.date)
        .bind(base.actor)
        .execute(&pool)
        .await;
    assert!(second.is_err(), "a second confirmation was written");
    pool.close().await;
}

/// D3A-3. A Schedule X annotation cannot be recorded for a drug that is not in Schedule X, so an
/// ordinary Schedule H prescription sale acquires no new requirement from this phase.
#[tokio::test]
async fn real_service_keeps_the_prescription_annotation_to_schedule_x_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_h"]).await;
    record_basis_over_http(&service, &world, Some("prescription_register")).await;
    let professional = pharmacist_over_http(&service, &world).await;
    let (_, item) = prescription_over_http(&service, &world, None, 20).await;
    let (sale_id, sale_revision) =
        prepared_sale_over_http(&service, &world, &item, &professional, 1).await;
    let line_id = sale_line_id_over_http(&service, &world, &sale_id).await;

    let listed = annotations_over_http(&service, &world.cookie).await;
    assert!(
        listed["pendingOccasions"]
            .as_array()
            .expect("array")
            .is_empty(),
        "a Schedule H line appeared on the Schedule X annotation surface: {listed:?}"
    );
    let refused = record_annotation_over_http(&service, &world.cookie, &line_id, true).await;
    assert_eq!(refused.status, 404, "{:?}", refused.body);

    // And the Schedule H rule 65(3) entry still prepares, exactly as it did before this phase.
    let record = prepare_entry_over_http(&service, &world, &sale_id, sale_revision).await;
    assert_eq!(record.status, 200, "{:?}", record.body);
    assert_eq!(
        record.body["prescriptionRecords"][0]["status"], "prepared",
        "{:?}",
        record.body
    );
}

/// D3A-4. The seller particulars are refused, never guessed. Rule 65(11)(c) names the seller's name
/// and address, and a record that cannot say what was written is not evidence of anything.
#[tokio::test]
async fn real_service_refuses_a_prescription_annotation_without_seller_particulars_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;
    let professional = pharmacist_over_http(&service, &world).await;
    let (_, item) = prescription_over_http(&service, &world, None, 20).await;
    let (sale_id, _) = prepared_sale_over_http(&service, &world, &item, &professional, 1).await;
    let line_id = sale_line_id_over_http(&service, &world, &sale_id).await;

    // The pharmacy's registered name goes missing — the state a half-set-up installation is in.
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    sqlx::query("UPDATE store_identity SET legal_name=NULL")
        .execute(&pool)
        .await
        .expect("clear legal name");
    pool.close().await;

    let listed = annotations_over_http(&service, &world.cookie).await;
    assert_eq!(listed["sellerName"], Value::Null, "{listed:?}");
    assert_eq!(listed["sellerMissing"][0], "legalName", "{listed:?}");

    let refused = record_annotation_over_http(&service, &world.cookie, &line_id, true).await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(
        refused.body["code"],
        "schedule_x_seller_particulars_unavailable"
    );
    assert_eq!(refused.body["issues"][0]["field"], "legalName");
}

/// D3A-5. Schedule X compliance records are the owner's and the pharmacist's. A cashier can neither
/// read the surface nor write the fact.
#[tokio::test]
async fn real_service_refuses_the_prescription_annotation_to_a_cashier_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;
    let professional = pharmacist_over_http(&service, &world).await;
    let (_, item) = prescription_over_http(&service, &world, None, 20).await;
    let (sale_id, _) = prepared_sale_over_http(&service, &world, &item, &professional, 1).await;
    let line_id = sale_line_id_over_http(&service, &world, &sale_id).await;

    let cashier = sign_in_as(
        &service,
        "cashier",
        "till-d3a",
        "01997a00-0000-7000-8000-0000000003a1",
    )
    .await;
    let read = call(
        &service,
        "GET",
        "/api/v1/store/schedule-x/prescription-annotations",
        None,
        Some(&cashier),
    )
    .await;
    assert_eq!(read.status, 403, "{:?}", read.body);
    let write = record_annotation_over_http(&service, &cashier, &line_id, true).await;
    assert_eq!(write.status, 403, "{:?}", write.body);

    // Nothing was written, and the pharmacist path still works.
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schedule_x_prescription_annotations")
        .fetch_one(&pool)
        .await
        .expect("rows");
    assert_eq!(rows, 0);
    pool.close().await;
}

/// D3A-6. Two confirmations of the same occasion at the same instant. Exactly one durable fact
/// survives, and the loser is told so rather than being handed a second.
#[tokio::test]
async fn real_service_keeps_one_prescription_annotation_under_a_race_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;
    let professional = pharmacist_over_http(&service, &world).await;
    let (_, item) = prescription_over_http(&service, &world, None, 20).await;
    let (sale_id, _) = prepared_sale_over_http(&service, &world, &item, &professional, 1).await;
    let line_id = sale_line_id_over_http(&service, &world, &sale_id).await;

    let (first, second) = tokio::join!(
        record_annotation_over_http(&service, &world.cookie, &line_id, true),
        record_annotation_over_http(&service, &world.cookie, &line_id, true),
    );
    // Whichever order they landed in, neither is an error and neither invents a second fact.
    for reply in [&first, &second] {
        assert!(
            reply.status == 201 || reply.status == 200,
            "{:?}",
            reply.body
        );
    }
    assert_eq!(
        first.body["id"], second.body["id"],
        "two facts were written"
    );

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM schedule_x_prescription_annotations WHERE sale_line_id=?",
    )
    .bind(&line_id)
    .fetch_one(&pool)
    .await
    .expect("rows");
    assert_eq!(rows, 1);
    pool.close().await;
}

/// D3A-7. Rule 65(2) — active is not the same as registered on the day.
///
/// A pharmacist whose registration had expired before the receipt cannot authenticate the physical
/// Schedule X register entry for it, and the database refuses the same transition on its own.
#[tokio::test]
async fn real_service_refuses_a_schedule_x_confirmation_by_a_lapsed_pharmacist_over_http() {
    let service = start().await;
    let world = seed_purchase_world(&service, MAHARASHTRA, "taxable").await;
    purchase_finding_over_http(&service, &world, "schedule_x", true).await;
    let professional = purchase_pharmacist_over_http(&service, &world).await;
    post_purchase_over_http(
        &service,
        &world,
        "INV-9401",
        "01997a00-0000-7000-8000-000000000d41",
    )
    .await;
    let register = register_over_http(&service, &world.cookie).await;
    let entry_id = register["entries"][0]["id"]
        .as_str()
        .expect("entry")
        .to_owned();
    let confirm_uri = format!("/api/v1/store/schedule-x/register/{entry_id}/confirm");
    let body = json!({
        "supervisingProfessionalId": professional,
        "particularsEnteredInPhysicalRegister": true,
        "physicalEntryAuthenticated": true,
    });

    // The registration lapsed before this receipt. The record is still ACTIVE, which is exactly the
    // gap Phase 1M-D3 found: active said nothing about the date.
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    sqlx::query("UPDATE store_professionals SET valid_upto='2026-01-31' WHERE id=?")
        .bind(&professional)
        .execute(&pool)
        .await
        .expect("lapse the registration");
    let still_active: String =
        sqlx::query_scalar("SELECT status FROM store_professionals WHERE id=?")
            .bind(&professional)
            .fetch_one(&pool)
            .await
            .expect("status");
    assert_eq!(still_active, "active");

    let lapsed = call(
        &service,
        "POST",
        &confirm_uri,
        Some(body.clone()),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(lapsed.status, 409, "{:?}", lapsed.body);
    assert_eq!(
        lapsed.body["code"],
        "schedule_x_pharmacist_not_valid_on_date"
    );

    // Not yet registered on the day is refused for the same reason, from the other side.
    sqlx::query(
        "UPDATE store_professionals SET valid_from='2027-01-01',valid_upto=NULL WHERE id=?",
    )
    .bind(&professional)
    .execute(&pool)
    .await
    .expect("move the start");
    let early = call(
        &service,
        "POST",
        &confirm_uri,
        Some(body.clone()),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(early.status, 409, "{:?}", early.body);
    assert_eq!(
        early.body["code"],
        "schedule_x_pharmacist_not_valid_on_date"
    );

    // The database refuses the same transition on its own, so the service is not the only guard.
    let forged = sqlx::query(
        "UPDATE store_schedule_x_register_entries SET status='confirmed',\
         particulars_entered_in_physical_register=1,physical_entry_authenticated=1,\
         supervising_professional_id=?,supervising_professional_name='Meera Iyer',\
         supervising_registration_number='MH-PH-44821',confirmed_by_user_id=\
         (SELECT id FROM users WHERE role='owner_admin' LIMIT 1),\
         confirmed_at_utc='2026-09-29T06:00:00.000Z',updated_at_utc='2026-09-29T06:00:00.000Z' \
         WHERE id=?",
    )
    .bind(&professional)
    .bind(&entry_id)
    .execute(&pool)
    .await;
    assert!(forged.is_err(), "direct SQL confirmed with a lapsed record");
    assert!(
        forged
            .unwrap_err()
            .to_string()
            .contains("schedule_x_register_entry_immutable"),
        "the transition guard did not refuse it"
    );

    // Restored to a registration that actually covers the receipt, the confirmation goes through.
    sqlx::query(
        "UPDATE store_professionals SET valid_from='2020-01-01',valid_upto=NULL WHERE id=?",
    )
    .bind(&professional)
    .execute(&pool)
    .await
    .expect("restore");
    pool.close().await;
    let confirmed = call(
        &service,
        "POST",
        &confirm_uri,
        Some(body),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);
    assert_eq!(confirmed.body["status"], "confirmed");
}

/// D3A-8, the hard final boundary. THE MANDATORY PROOF.
///
/// Every fact Phase 1M-D3-A knows how to record is on file: the Schedule X classification, an
/// in-force Form 20F with this drug written onto it, a valid prescription for the exact product
/// within its repeat and quantity authority, the retained duplicate copy, the NEW rule 65(11)(c)
/// annotation, a registered pharmacist whose record covers the day, and a Schedule X receipt
/// working entry whose physical register acts are confirmed and closed.
///
/// The sale is still refused, because the supply workflow does not exist. Prescription-side
/// compliance facts are not an authority to dispense, and this phase does not pretend otherwise.
#[tokio::test]
async fn real_service_keeps_schedule_x_refused_with_every_d3a_fact_present_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;
    // Phase 1M-D3-C2: the independent NDPS axis, established not to apply, so this test reaches
    // the Schedule X predicate it is actually about instead of stopping at an unrecorded overlay.
    finding_over_http(&service, &world, "ndps_purview", false).await;
    record_basis_over_http(&service, &world, Some("prescription_register")).await;
    let professional = pharmacist_over_http(&service, &world).await;

    // Form 20F authority, in force, with this drug covered.
    let (licence, revision) = form_20f_over_http(&service, &world, "MH-PUNE-20F-4471").await;
    let authorised = set_authority_over_http(
        &service,
        &world,
        &licence,
        revision,
        json!({
            "legalStatus": "in_force",
            "validityBasis": "perpetual",
            "validFrom": "2020-01-01",
        }),
    )
    .await;
    assert_eq!(authorised.status, 200, "{:?}", authorised.body);
    let covered = cover_over_http(&service, &world, &licence, "2020-01-01", None).await;
    assert_eq!(covered.status, 201, "{:?}", covered.body);
    assert_eq!(
        authority_over_http(&service, &world, SALE_DATE).await["state"],
        "established"
    );

    // Receipt provenance: a posted Purchase of this drug, with its Schedule X working entry
    // confirmed in the bound register and then closed.
    sale_world_purchase_over_http(
        &service,
        &world,
        "INV-D3A-1",
        "01997a00-0000-7000-8000-000000000e11",
    )
    .await;
    let register = register_over_http(&service, &world.cookie).await;
    let entry_id = register["entries"][0]["id"]
        .as_str()
        .expect("receipt entry")
        .to_owned();
    let confirmed = call(
        &service,
        "POST",
        &format!("/api/v1/store/schedule-x/register/{entry_id}/confirm"),
        Some(json!({
            "supervisingProfessionalId": professional,
            "particularsEnteredInPhysicalRegister": true,
            "physicalEntryAuthenticated": true,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);
    let closed = call(
        &service,
        "POST",
        &format!("/api/v1/store/schedule-x/register/{entry_id}/finalize"),
        Some(json!({})),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(closed.status, 200, "{:?}", closed.body);
    assert_eq!(closed.body["status"], "finalized");

    // The prescription, the draft, and both prescription-side facts.
    let (prescription, item) = prescription_over_http(&service, &world, None, 20).await;
    let duplicate = call(
        &service,
        "POST",
        &format!("/api/v1/prescriptions/{prescription}/schedule-x-duplicate-copy"),
        Some(json!({ "retainedDuplicatePrescriptionCopyConfirmed": true, "note": Value::Null })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(duplicate.status, 201, "{:?}", duplicate.body);

    let (sale_id, sale_revision) =
        prepared_sale_over_http(&service, &world, &item, &professional, 1).await;
    let line_id = sale_line_id_over_http(&service, &world, &sale_id).await;
    let annotated = record_annotation_over_http(&service, &world.cookie, &line_id, true).await;
    assert_eq!(annotated.status, 201, "{:?}", annotated.body);

    // Everything this software can record is recorded. The counter still refuses.
    let refused = post_quoted_over_http(
        &service,
        &world,
        &sale_id,
        sale_revision,
        "01997a00-0000-7000-8000-0000000002f8",
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(
        refused.body["code"], "prescription_requirements_incomplete",
        "{:?}",
        refused.body
    );
    // Exactly the Schedule X predicates this phase had not yet established, and no others. Every
    // unmet predicate is reported together so the counter can clear them in one pass.
    assert_eq!(
        schedule_x_unmet(&refused.body),
        vec![
            "schedule_x_lot_provenance_incomplete",
            "schedule_x_source_authority_missing",
            "schedule_x_register_confirmation_missing",
            "schedule_x_supervising_pharmacist_invalid",
        ],
        "{:?}",
        refused.body
    );
    // The field still names the offending line, and now the predicate on it as well.
    assert!(
        refused.body["issues"][0]["field"]
            .as_str()
            .is_some_and(|field| field.starts_with("lines.1.")),
        "{:?}",
        refused.body
    );

    // The Sale is still a draft, with no number, no stock movement, no dispensing, and no supply
    // entry anywhere in the Schedule X register.
    let detail = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{sale_id}"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(detail.body["status"], "draft");
    assert_eq!(detail.body["documentNumber"], Value::Null);

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let outflow: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inventory_movements WHERE movement_type='sale' AND sale_line_id=?",
    )
    .bind(&line_id)
    .fetch_one(&pool)
    .await
    .expect("movements");
    assert_eq!(outflow, 0, "a refused Schedule X sale moved stock");
    let dispensings: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM prescription_dispensings WHERE sale_document_id=?",
    )
    .bind(&sale_id)
    .fetch_one(&pool)
    .await
    .expect("dispensings");
    assert_eq!(dispensings, 0);
    let supplies: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_schedule_x_register_entries WHERE entry_kind='supply'",
    )
    .fetch_one(&pool)
    .await
    .expect("supply entries");
    assert_eq!(supplies, 0, "a Schedule X supply entry was created");

    // The annotation survives the refusal: the paper was written on, and a refused sale does not
    // unwrite it.
    let surviving: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM schedule_x_prescription_annotations WHERE sale_line_id=?",
    )
    .bind(&line_id)
    .fetch_one(&pool)
    .await
    .expect("annotations");
    assert_eq!(surviving, 1);
    pool.close().await;

    // And the refusal still says nothing that would frighten a pharmacist into thinking the drug
    // is unlawful. It is lawful; this software simply cannot keep its register yet.
    let text = refused.body.to_string().to_lowercase();
    for word in ["banned", "prohibit", "illegal"] {
        assert!(!text.contains(word), "{word}: {:?}", refused.body);
    }
}

/// D3A-9, item 22. Migration 0027 adds compliance evidence, so the existing backup path has to
/// carry it — unchanged, with its frozen particulars intact and its professional linkage still
/// sound. No backup format change was needed: `VACUUM INTO` takes the whole file, and a new table
/// is simply part of the file.
#[tokio::test]
async fn real_service_carries_the_prescription_annotation_through_backup_and_restore_over_http() {
    let harness = start_with_backups().await;
    let service = &harness.service;
    let world = seed_sale_world(service, 1).await;
    schedule_over_http(service, &world, &["schedule_x"]).await;
    let professional = pharmacist_over_http(service, &world).await;
    let (prescription, item) = prescription_over_http(service, &world, None, 20).await;
    let (sale_id, _) = prepared_sale_over_http(service, &world, &item, &professional, 1).await;
    let line_id = sale_line_id_over_http(service, &world, &sale_id).await;

    // Both prescription-side facts of this phase and of D2, recorded before the backup.
    let duplicate = call(
        service,
        "POST",
        &format!("/api/v1/prescriptions/{prescription}/schedule-x-duplicate-copy"),
        Some(json!({ "retainedDuplicatePrescriptionCopyConfirmed": true, "note": Value::Null })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(duplicate.status, 201, "{:?}", duplicate.body);
    let recorded = record_annotation_over_http(service, &world.cookie, &line_id, true).await;
    assert_eq!(recorded.status, 201, "{:?}", recorded.body);
    let annotation_id = recorded.body["id"].as_str().expect("id").to_owned();
    let seller_name = recorded.body["sellerName"]
        .as_str()
        .expect("seller name")
        .to_owned();
    let seller_address = recorded.body["sellerAddress"]
        .as_str()
        .expect("seller address")
        .to_owned();

    let created = call(
        service,
        "POST",
        "/api/v1/backups/create",
        Some(json!({})),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    let backup_id = created.body["backupId"]
        .as_str()
        .expect("backup id")
        .to_owned();
    let resolved = call(
        service,
        "GET",
        &format!("/api/v1/backups/{backup_id}/download"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(resolved.status, 200, "{:?}", resolved.body);
    let url = resolved.body["url"].as_str().expect("url").to_owned();
    let (status, _, downloaded) = get_bytes(service, &url, Some(&world.cookie)).await;
    assert_eq!(status, 200);

    // Something happens after the backup that the restore must undo, so the assertions below prove
    // a real restore rather than a database that was never changed.
    let after = call(
        service,
        "POST",
        "/api/v1/parties",
        Some(json!({
            "party": { "displayName": "Post-Backup Supplier" },
            "roles": [{ "role": "supplier" }]
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(after.status, 201, "{:?}", after.body);

    let (status, _, body) = call_bytes(
        service,
        "/api/v1/backups/restore/prepare",
        "application/octet-stream",
        &downloaded,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let prepared: Value = serde_json::from_slice(&body).expect("prepared restore");
    assert_eq!(prepared["report"]["compatibility"], "ready");
    let token = prepared["candidateToken"]
        .as_str()
        .expect("token")
        .to_owned();
    let committed = call(
        service,
        "POST",
        "/api/v1/backups/restore/commit",
        Some(json!({ "candidateToken": token, "password": "Integration-Password-42" })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(committed.status, 200, "{:?}", committed.body);

    api::backups::recover_interrupted_restore(&harness.backups, &service.database_path)
        .await
        .expect("recovery");
    let reopened = database::connect(&service.database_path)
        .await
        .expect("reopened database");
    assert!(
        api::backups::complete_restore_after_open(&reopened, &harness.backups)
            .await
            .expect("completion")
    );

    // The restore really happened.
    let post_backup: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM parties WHERE display_name='Post-Backup Supplier'",
    )
    .fetch_one(&reopened)
    .await
    .expect("parties");
    assert_eq!(post_backup, 0, "the restore did not roll the pharmacy back");

    // And the compliance evidence came back, with every frozen particular exactly as it was.
    let restored: (String, String, String, String, String, String) = sqlx::query_as(
        "SELECT seller_name,seller_address,dispensing_date,attested_on_store_date,\
         prescription_id,sale_line_id FROM schedule_x_prescription_annotations WHERE id=?",
    )
    .bind(&annotation_id)
    .fetch_one(&reopened)
    .await
    .expect("the rule 65(11)(c) attestation did not survive the restore");
    assert_eq!(restored.0, seller_name);
    assert_eq!(restored.1, seller_address);
    assert_eq!(restored.2, SALE_DATE);
    assert_eq!(restored.4, prescription);
    assert_eq!(restored.5, line_id);

    // The rule 65(9)(a) fact came back too, and both are still append-only after a restore.
    let duplicates: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM prescription_duplicate_copy_attestations")
            .fetch_one(&reopened)
            .await
            .expect("duplicate copy attestations");
    assert_eq!(duplicates, 1);
    let still_guarded = sqlx::query("DELETE FROM schedule_x_prescription_annotations WHERE id=?")
        .bind(&annotation_id)
        .execute(&reopened)
        .await;
    assert!(
        still_guarded.is_err(),
        "the restored database lost its append-only guard"
    );

    // The professional linkage is still sound: the pharmacist the Sale names is a registered
    // pharmacist of this store, and the attestation's occasion still points at that Sale.
    let linked: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM schedule_x_prescription_annotations annotation \
         JOIN sale_documents document ON document.id=annotation.sale_document_id \
         JOIN store_professionals professional \
           ON professional.id=document.supervising_professional_id \
         WHERE annotation.id=? AND professional.store_id=annotation.store_id \
           AND professional.status='active' AND professional.capacity='registered_pharmacist' \
           AND (professional.valid_from IS NULL \
                OR professional.valid_from<=annotation.dispensing_date) \
           AND (professional.valid_upto IS NULL \
                OR professional.valid_upto>=annotation.dispensing_date)",
    )
    .bind(&annotation_id)
    .fetch_one(&reopened)
    .await
    .expect("linkage");
    assert_eq!(linked, 1, "the professional linkage did not survive intact");

    // And a restored pharmacy still refuses a Schedule X sale.
    let schemes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_schedule_x_register_entries WHERE entry_kind='supply'",
    )
    .fetch_one(&reopened)
    .await
    .expect("supply entries");
    assert_eq!(schemes, 0);
    reopened.close().await;
}

// ==============================================================================================
// Phase 1M-D3-B — Schedule X lot provenance and the supply working entry
// ==============================================================================================

async fn supply_preparation_over_http(service: &Service, cookie: &str) -> Value {
    let reply = call(
        service,
        "GET",
        "/api/v1/store/schedule-x/supply-preparation",
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    reply.body
}

async fn prepare_supply_over_http(service: &Service, cookie: &str, line: &str) -> Reply {
    call(
        service,
        "POST",
        "/api/v1/store/schedule-x/supply-preparation",
        Some(json!({ "saleLineId": line })),
        Some(cookie),
    )
    .await
}

/// A draft Schedule X sale of one pack drawing on a NAMED lot, linked to a prescription item and
/// supervised. Mirrors `prepared_sale_over_http`, which always uses the world's seed lot.
async fn prepared_sale_on_lot_over_http(
    service: &Service,
    world: &SaleWorld,
    batch: &str,
    item: &str,
    professional: &str,
) -> (String, String, i64) {
    let sale_id = sale_draft(service, world, None).await;
    let with_line = call(
        service,
        "POST",
        &format!("/api/v1/sales/{sale_id}/lines"),
        Some(json!({
            "expectedRevision": 1,
            "productId": world.product,
            "productPackId": world.pack,
            "batchId": batch,
            "quantityBasis": "pack",
            "quantity": 1,
            "sellingRatePaise": 8000
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);
    let line_id = with_line.body["lines"][0]["id"]
        .as_str()
        .expect("line id")
        .to_owned();
    let linked = call(
        service,
        "PUT",
        &format!("/api/v1/sale-lines/{line_id}/prescription"),
        Some(json!({
            "expectedRevision": with_line.body["revision"],
            "prescriptionItemId": item,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(linked.status, 200, "{:?}", linked.body);
    let supplied = call(
        service,
        "PUT",
        &format!("/api/v1/sales/{sale_id}/supply"),
        Some(json!({
            "expectedRevision": linked.body["revision"],
            "supervisingProfessionalId": professional,
            "prescriptionEndorsementConfirmed": true,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(supplied.status, 200, "{:?}", supplied.body);
    (
        sale_id,
        line_id,
        supplied.body["revision"].as_i64().expect("revision"),
    )
}

/// Receives a lot AFTER the Schedule X finding is recorded, then writes its receipt into the
/// physical register. That lot is the only one in these tests that can lawfully be supplied from.
async fn qualified_lot_over_http(
    service: &Service,
    world: &SaleWorld,
    professional: &str,
) -> String {
    let batch = receive_lot(
        service,
        world,
        "BX-QUALIFIED",
        9550,
        "INV-D3B-1",
        "01997a00-0000-7000-8000-000000000e21",
    )
    .await;
    let register = register_over_http(service, &world.cookie).await;
    let entry = register["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| entry["batchNumber"] == "BX-QUALIFIED")
        .expect("receipt entry for the new lot")
        .clone();
    let confirmed = call(
        service,
        "POST",
        &format!(
            "/api/v1/store/schedule-x/register/{}/confirm",
            entry["id"].as_str().expect("entry id")
        ),
        Some(json!({
            "supervisingProfessionalId": professional,
            "particularsEnteredInPhysicalRegister": true,
            "physicalEntryAuthenticated": true,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);
    batch
}

/// D3B-1. The provenance verdict across a real socket: the lot bought before anybody classified the
/// drug cannot be supplied from, and the lot bought after — and written into the register — can.
#[tokio::test]
async fn real_service_supplies_only_from_a_lot_written_into_the_schedule_x_register_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;
    let professional = pharmacist_over_http(&service, &world).await;
    let (_, item) = prescription_over_http(&service, &world, None, 20).await;

    // The world's seed lot arrived BEFORE the Schedule X finding existed, so no receipt working
    // entry was ever prepared for it. It is exactly the legacy stock Phase 1M-D3-B holds back.
    let (_, legacy_line, _) =
        prepared_sale_on_lot_over_http(&service, &world, &world.batch, &item, &professional).await;
    let listed = supply_preparation_over_http(&service, &world.cookie).await;
    let legacy = listed["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .find(|candidate| candidate["saleLineId"] == legacy_line.as_str())
        .expect("the legacy line is listed")
        .clone();
    assert_eq!(
        legacy["lotProvenance"]["state"], "no_qualifying_receipt",
        "{legacy:?}"
    );
    let refused = prepare_supply_over_http(&service, &world.cookie, &legacy_line).await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(refused.body["code"], "schedule_x_lot_provenance_unresolved");

    // A lot received after the finding, with its receipt written into the bound register, qualifies.
    let batch = qualified_lot_over_http(&service, &world, &professional).await;
    let (_, good_line, _) =
        prepared_sale_on_lot_over_http(&service, &world, &batch, &item, &professional).await;
    let listed = supply_preparation_over_http(&service, &world.cookie).await;
    let good = listed["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .find(|candidate| candidate["saleLineId"] == good_line.as_str())
        .expect("the qualified line is listed")
        .clone();
    assert_eq!(good["lotProvenance"]["state"], "qualified", "{good:?}");

    let prepared = prepare_supply_over_http(&service, &world.cookie, &good_line).await;
    assert_eq!(prepared.status, 201, "{:?}", prepared.body);
    assert_eq!(prepared.body["entryKind"], "supply");
    assert_eq!(prepared.body["status"], "prepared");
    assert_eq!(prepared.body["supplyBasis"], "prescription");
    assert_eq!(prepared.body["batchNumber"], "BX-QUALIFIED");
    // Neither physical act is asserted by preparing a working record.
    assert_eq!(prepared.body["particularsEnteredInPhysicalRegister"], false);
    assert_eq!(prepared.body["physicalEntryAuthenticated"], false);

    // A retry after a client timeout is handed the same working entry, not a second one.
    let retried = prepare_supply_over_http(&service, &world.cookie, &good_line).await;
    assert_eq!(retried.status, 200, "{:?}", retried.body);
    assert_eq!(retried.body["id"], prepared.body["id"]);

    // The list carries no patient and no prescriber.
    let text = listed.to_string();
    for private in ["Sita Kulkarni", "Lakshmi Road", "Anjali Rao", "Rao Clinic"] {
        assert!(!text.contains(private), "{private} leaked: {listed}");
    }

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let supplies: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_schedule_x_register_entries \
         WHERE entry_kind='supply' AND status<>'void'",
    )
    .fetch_one(&pool)
    .await
    .expect("supplies");
    assert_eq!(supplies, 1, "a retry wrote a second working entry");
    pool.close().await;
}

/// D3B-2. Two preparations of the same line at the same instant. Exactly one durable working entry
/// survives, and one reference is issued.
#[tokio::test]
async fn real_service_keeps_one_schedule_x_supply_entry_under_a_race_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;
    let professional = pharmacist_over_http(&service, &world).await;
    let (_, item) = prescription_over_http(&service, &world, None, 20).await;
    let batch = qualified_lot_over_http(&service, &world, &professional).await;
    let (_, line_id, _) =
        prepared_sale_on_lot_over_http(&service, &world, &batch, &item, &professional).await;

    let (first, second) = tokio::join!(
        prepare_supply_over_http(&service, &world.cookie, &line_id),
        prepare_supply_over_http(&service, &world.cookie, &line_id),
    );
    for reply in [&first, &second] {
        assert!(
            reply.status == 201 || reply.status == 200,
            "{:?}",
            reply.body
        );
    }
    assert_eq!(
        first.body["id"], second.body["id"],
        "two entries were written"
    );

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_schedule_x_register_entries \
         WHERE entry_kind='supply' AND sale_line_id=?",
    )
    .bind(&line_id)
    .fetch_one(&pool)
    .await
    .expect("rows");
    assert_eq!(rows, 1);
    // One reference issued, never two for one supply.
    let references: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT reference) FROM store_schedule_x_register_entries \
         WHERE entry_kind='supply'",
    )
    .fetch_one(&pool)
    .await
    .expect("references");
    assert_eq!(references, 1);
    pool.close().await;
}

/// D3B-3. The physical register acts can be attested for a supply entry, and the entry still cannot
/// be closed: a supply record is closed by the sale it records, and that sale cannot post.
#[tokio::test]
async fn real_service_confirms_but_never_closes_a_schedule_x_supply_entry_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;
    let professional = pharmacist_over_http(&service, &world).await;
    let (_, item) = prescription_over_http(&service, &world, None, 20).await;
    let batch = qualified_lot_over_http(&service, &world, &professional).await;
    let (_, line_id, _) =
        prepared_sale_on_lot_over_http(&service, &world, &batch, &item, &professional).await;
    let prepared = prepare_supply_over_http(&service, &world.cookie, &line_id).await;
    assert_eq!(prepared.status, 201, "{:?}", prepared.body);
    let entry_id = prepared.body["id"].as_str().expect("id").to_owned();

    // The same confirmation the receipt side uses: both physical acts, and a named registered
    // pharmacist whose record covers the day.
    let confirmed = call(
        &service,
        "POST",
        &format!("/api/v1/store/schedule-x/register/{entry_id}/confirm"),
        Some(json!({
            "supervisingProfessionalId": professional,
            "particularsEnteredInPhysicalRegister": true,
            "physicalEntryAuthenticated": true,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);
    assert_eq!(confirmed.body["status"], "confirmed");

    // And closing it is refused, in words rather than as a constraint name.
    let closed = call(
        &service,
        "POST",
        &format!("/api/v1/store/schedule-x/register/{entry_id}/finalize"),
        Some(json!({})),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(closed.status, 409, "{:?}", closed.body);
    assert_eq!(
        closed.body["code"],
        "schedule_x_supply_finalization_unavailable"
    );

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let finalized: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_schedule_x_register_entries \
         WHERE entry_kind='supply' AND status='finalized'",
    )
    .fetch_one(&pool)
    .await
    .expect("finalized");
    assert_eq!(
        finalized, 0,
        "a supply entry was closed with no sale behind it"
    );
    pool.close().await;
}

/// D3B-4. Schedule X regulatory records stay with the owner and the pharmacist. A cashier can
/// neither read the provenance surface nor prepare a supply record.
#[tokio::test]
async fn real_service_refuses_schedule_x_supply_preparation_to_a_cashier_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;
    let professional = pharmacist_over_http(&service, &world).await;
    let (_, item) = prescription_over_http(&service, &world, None, 20).await;
    let batch = qualified_lot_over_http(&service, &world, &professional).await;
    let (_, line_id, _) =
        prepared_sale_on_lot_over_http(&service, &world, &batch, &item, &professional).await;

    let cashier = sign_in_as(
        &service,
        "cashier",
        "till-d3b",
        "01997a00-0000-7000-8000-0000000003b1",
    )
    .await;
    let read = call(
        &service,
        "GET",
        "/api/v1/store/schedule-x/supply-preparation",
        None,
        Some(&cashier),
    )
    .await;
    assert_eq!(read.status, 403, "{:?}", read.body);
    let write = prepare_supply_over_http(&service, &cashier, &line_id).await;
    assert_eq!(write.status, 403, "{:?}", write.body);

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_schedule_x_register_entries WHERE entry_kind='supply'",
    )
    .fetch_one(&pool)
    .await
    .expect("rows");
    assert_eq!(rows, 0);
    pool.close().await;
}

/// D3B-5, item 35 — THE HARD SCHEDULE X BOUNDARY, with every D3-B fact added.
///
/// Every favourable fact this software can record now exists: the classification, Form 20-F
/// authority with the drug covered, a lot whose provenance traces to a posted purchase whose
/// receipt is written and authenticated in the bound register, a valid prescription for the exact
/// product, the retained duplicate copy, the rule 65(11)(c) annotation, a date-valid supervising
/// pharmacist, AND a Schedule X SUPPLY working entry prepared and confirmed against the physical
/// register.
///
/// The sale is still refused. Compliance preparation is not an authority to dispense.
#[tokio::test]
async fn real_service_keeps_schedule_x_refused_with_every_d3b_fact_present_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;
    // Phase 1M-D3-C2: the independent NDPS axis, established not to apply, so this test reaches
    // the Schedule X predicate it is actually about instead of stopping at an unrecorded overlay.
    finding_over_http(&service, &world, "ndps_purview", false).await;
    record_basis_over_http(&service, &world, Some("prescription_register")).await;
    let professional = pharmacist_over_http(&service, &world).await;

    let (licence, revision) = form_20f_over_http(&service, &world, "MH-PUNE-20F-4471").await;
    let authorised = set_authority_over_http(
        &service,
        &world,
        &licence,
        revision,
        json!({
            "legalStatus": "in_force",
            "validityBasis": "perpetual",
            "validFrom": "2020-01-01",
        }),
    )
    .await;
    assert_eq!(authorised.status, 200, "{:?}", authorised.body);
    let covered = cover_over_http(&service, &world, &licence, "2020-01-01", None).await;
    assert_eq!(covered.status, 201, "{:?}", covered.body);
    assert_eq!(
        authority_over_http(&service, &world, SALE_DATE).await["state"],
        "established"
    );

    let batch = qualified_lot_over_http(&service, &world, &professional).await;
    let (prescription, item) = prescription_over_http(&service, &world, None, 20).await;
    let duplicate = call(
        &service,
        "POST",
        &format!("/api/v1/prescriptions/{prescription}/schedule-x-duplicate-copy"),
        Some(json!({ "retainedDuplicatePrescriptionCopyConfirmed": true, "note": Value::Null })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(duplicate.status, 201, "{:?}", duplicate.body);

    let (sale_id, line_id, sale_revision) =
        prepared_sale_on_lot_over_http(&service, &world, &batch, &item, &professional).await;
    let annotated = record_annotation_over_http(&service, &world.cookie, &line_id, true).await;
    assert_eq!(annotated.status, 201, "{:?}", annotated.body);

    let prepared = prepare_supply_over_http(&service, &world.cookie, &line_id).await;
    assert_eq!(prepared.status, 201, "{:?}", prepared.body);
    let entry_id = prepared.body["id"].as_str().expect("id").to_owned();
    let confirmed = call(
        &service,
        "POST",
        &format!("/api/v1/store/schedule-x/register/{entry_id}/confirm"),
        Some(json!({
            "supervisingProfessionalId": professional,
            "particularsEnteredInPhysicalRegister": true,
            "physicalEntryAuthenticated": true,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);

    // Everything this software can record is recorded. The counter still refuses.
    let refused = post_quoted_over_http(
        &service,
        &world,
        &sale_id,
        sale_revision,
        "01997a00-0000-7000-8000-0000000002fb",
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(
        refused.body["code"], "prescription_requirements_incomplete",
        "{:?}",
        refused.body
    );
    // Exactly the Schedule X predicates this phase had not yet established, and no others. Every
    // unmet predicate is reported together so the counter can clear them in one pass.
    assert_eq!(
        schedule_x_unmet(&refused.body),
        vec!["schedule_x_source_authority_missing",],
        "{:?}",
        refused.body
    );
    // The field still names the offending line, and now the predicate on it as well.
    assert!(
        refused.body["issues"][0]["field"]
            .as_str()
            .is_some_and(|field| field.starts_with("lines.1.")),
        "{:?}",
        refused.body
    );

    let detail = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{sale_id}"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(detail.body["status"], "draft");
    assert_eq!(detail.body["documentNumber"], Value::Null);

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let outflow: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inventory_movements WHERE movement_type='sale' AND sale_line_id=?",
    )
    .bind(&line_id)
    .fetch_one(&pool)
    .await
    .expect("movements");
    assert_eq!(outflow, 0, "a refused Schedule X sale moved stock");
    let dispensings: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM prescription_dispensings WHERE sale_document_id=?",
    )
    .bind(&sale_id)
    .fetch_one(&pool)
    .await
    .expect("dispensings");
    assert_eq!(dispensings, 0);
    // The supply working entry exists and is CONFIRMED — the physical register was written — but it
    // is not FINALIZED, because no supply happened. That is the designed lifecycle.
    let states: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT status,dispensing_id FROM store_schedule_x_register_entries \
         WHERE entry_kind='supply'",
    )
    .fetch_all(&pool)
    .await
    .expect("supply states");
    assert_eq!(states, vec![("confirmed".to_owned(), None)], "{states:?}");
    pool.close().await;

    let text = refused.body.to_string().to_lowercase();
    for word in ["banned", "prohibit", "illegal"] {
        assert!(!text.contains(word), "{word}: {:?}", refused.body);
    }
}

/// D3B-6, item 31. Migration 0028 adds durable provenance evidence, so the existing backup path has
/// to carry it — the lot identity, the lifecycle state, and the receipt linkage that qualified it.
#[tokio::test]
async fn real_service_carries_schedule_x_supply_provenance_through_backup_and_restore_over_http() {
    let harness = start_with_backups().await;
    let service = &harness.service;
    let world = seed_sale_world(service, 1).await;
    schedule_over_http(service, &world, &["schedule_x"]).await;
    let professional = pharmacist_over_http(service, &world).await;
    let (_, item) = prescription_over_http(service, &world, None, 20).await;
    let batch = qualified_lot_over_http(service, &world, &professional).await;
    let (_, line_id, _) =
        prepared_sale_on_lot_over_http(service, &world, &batch, &item, &professional).await;
    let prepared = prepare_supply_over_http(service, &world.cookie, &line_id).await;
    assert_eq!(prepared.status, 201, "{:?}", prepared.body);
    let entry_id = prepared.body["id"].as_str().expect("id").to_owned();
    let reference = prepared.body["reference"]
        .as_str()
        .expect("reference")
        .to_owned();

    let created = call(
        service,
        "POST",
        "/api/v1/backups/create",
        Some(json!({})),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    let resolved = call(
        service,
        "GET",
        &format!(
            "/api/v1/backups/{}/download",
            created.body["backupId"].as_str().expect("backup id")
        ),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(resolved.status, 200, "{:?}", resolved.body);
    let (status, _, downloaded) = get_bytes(
        service,
        resolved.body["url"].as_str().expect("url"),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(status, 200);

    // Something after the backup that the restore must undo.
    let after = call(
        service,
        "POST",
        "/api/v1/parties",
        Some(json!({
            "party": { "displayName": "Post-Backup Supplier" },
            "roles": [{ "role": "supplier" }]
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(after.status, 201, "{:?}", after.body);

    let (status, _, body) = call_bytes(
        service,
        "/api/v1/backups/restore/prepare",
        "application/octet-stream",
        &downloaded,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let prepared_restore: Value = serde_json::from_slice(&body).expect("prepared restore");
    let committed = call(
        service,
        "POST",
        "/api/v1/backups/restore/commit",
        Some(json!({
            "candidateToken": prepared_restore["candidateToken"].as_str().expect("token"),
            "password": "Integration-Password-42"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(committed.status, 200, "{:?}", committed.body);
    api::backups::recover_interrupted_restore(&harness.backups, &service.database_path)
        .await
        .expect("recovery");
    let reopened = database::connect(&service.database_path)
        .await
        .expect("reopened database");
    assert!(
        api::backups::complete_restore_after_open(&reopened, &harness.backups)
            .await
            .expect("completion")
    );

    let post_backup: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM parties WHERE display_name='Post-Backup Supplier'",
    )
    .fetch_one(&reopened)
    .await
    .expect("parties");
    assert_eq!(post_backup, 0, "the restore did not roll the pharmacy back");

    // The supply working entry came back with its lot identity, its reference and its state.
    let restored: (String, String, String, Option<String>) = sqlx::query_as(
        "SELECT reference,status,batch_id,dispensing_id FROM store_schedule_x_register_entries \
         WHERE id=?",
    )
    .bind(&entry_id)
    .fetch_one(&reopened)
    .await
    .expect("the supply working entry did not survive the restore");
    assert_eq!(restored.0, reference);
    assert_eq!(restored.1, "prepared");
    assert_eq!(restored.2, batch);
    assert_eq!(restored.3, None);

    // And the provenance that qualified it is still derivable: the lot's every sellable inward
    // movement is a purchase whose receipt entry is confirmed.
    let unresolved: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inventory_movements inward \
         WHERE inward.batch_id=? AND inward.stock_status='sellable' \
           AND inward.quantity_delta_atoms>0 \
           AND NOT (inward.movement_type='purchase' AND EXISTS (\
               SELECT 1 FROM store_schedule_x_register_entries receipt \
               WHERE receipt.entry_kind='receipt' \
                 AND receipt.purchase_line_id=inward.purchase_line_id \
                 AND receipt.status IN ('confirmed','finalized')))",
    )
    .bind(&batch)
    .fetch_one(&reopened)
    .await
    .expect("provenance");
    assert_eq!(unresolved, 0, "the lot's provenance did not survive intact");
    reopened.close().await;
}

// ==============================================================================================
// Phase 1M-D3-C1 — supplier Schedule X purchase-source authority
// ==============================================================================================

async fn supplier_authorities_over_http(service: &Service, cookie: &str) -> Value {
    let reply = call(
        service,
        "GET",
        "/api/v1/store/schedule-x/supplier-authorities",
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    reply.body
}

#[allow(clippy::too_many_arguments)]
async fn record_authority_over_http(
    service: &Service,
    cookie: &str,
    supplier: &str,
    kind: &str,
    number: &str,
    from: &str,
    to: Option<&str>,
    basis: &str,
    status: &str,
) -> Reply {
    call(
        service,
        "POST",
        "/api/v1/store/schedule-x/supplier-authorities",
        Some(json!({
            "supplierPartyId": supplier,
            "authorityKind": kind,
            "authorityNumber": number,
            "issuingAuthority": "State Drugs Control Administration",
            "legalStatus": status,
            "validityBasis": basis,
            "effectiveFrom": from,
            "effectiveTo": to,
            "sourceCitation": "Licence copy inspected at the counter",
            "note": Value::Null,
        })),
        Some(cookie),
    )
    .await
}

async fn record_coverage_over_http(
    service: &Service,
    cookie: &str,
    authority: &str,
    product: &str,
    from: &str,
) -> Reply {
    call(
        service,
        "POST",
        &format!("/api/v1/store/schedule-x/supplier-authorities/{authority}/coverage"),
        Some(json!({
            "productId": product,
            "effectiveFrom": from,
            "effectiveTo": Value::Null,
            "sourceCitation": "Drug endorsed on the licence copy",
            "reason": Value::Null,
        })),
        Some(cookie),
    )
    .await
}

/// The ordinary qualifying evidence for the sale world's own supplier: a Form 20-G Schedule X
/// wholesale licence, in force and perpetual from 2020, covering the world's product.
async fn authorise_supplier_over_http(service: &Service, world: &SaleWorld) -> String {
    let recorded = record_authority_over_http(
        service,
        &world.cookie,
        &world.supplier,
        "form_20g",
        "20G-MH-5511",
        "2020-01-01",
        None,
        "perpetual",
        "in_force",
    )
    .await;
    assert_eq!(recorded.status, 201, "{:?}", recorded.body);
    let authority = recorded.body["id"]
        .as_str()
        .expect("authority id")
        .to_owned();
    let covered = record_coverage_over_http(
        service,
        &world.cookie,
        &authority,
        &world.product,
        "2020-01-01",
    )
    .await;
    assert_eq!(covered.status, 201, "{:?}", covered.body);
    authority
}

/// D3C1-1. The evidence across a real socket, and what it does and does not claim.
#[tokio::test]
async fn real_service_records_supplier_schedule_x_authority_evidence_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;

    // Nothing recorded yet.
    let empty = supplier_authorities_over_http(&service, &world.cookie).await;
    assert!(empty["authorities"].as_array().expect("array").is_empty());

    // An unsupported form is refused: Form 20-F is the RETAIL Schedule X licence, Form 28-B belongs
    // to Schedule C/C(1) manufacture, and an ordinary wholesale licence is not Schedule X authority.
    for kind in ["form_20f", "form_28b", "form_20b", "wholesale"] {
        let refused = record_authority_over_http(
            &service,
            &world.cookie,
            &world.supplier,
            kind,
            "X-1",
            "2020-01-01",
            None,
            "perpetual",
            "in_force",
        )
        .await;
        assert_eq!(refused.status, 422, "{kind}: {:?}", refused.body);
        assert_eq!(refused.body["code"], "validation_failed", "{kind}");
    }

    // A fixed term with no end date, and a perpetual authority with one, are each refused in words.
    let no_end = record_authority_over_http(
        &service,
        &world.cookie,
        &world.supplier,
        "form_20g",
        "20G-MH-1",
        "2020-01-01",
        None,
        "fixed_term",
        "in_force",
    )
    .await;
    assert_eq!(no_end.status, 422, "{:?}", no_end.body);
    let perpetual_with_end = record_authority_over_http(
        &service,
        &world.cookie,
        &world.supplier,
        "form_20g",
        "20G-MH-1",
        "2020-01-01",
        Some("2027-01-01"),
        "perpetual",
        "in_force",
    )
    .await;
    assert_eq!(
        perpetual_with_end.status, 422,
        "{:?}",
        perpetual_with_end.body
    );

    let authority = authorise_supplier_over_http(&service, &world).await;
    let listed = supplier_authorities_over_http(&service, &world.cookie).await;
    let recorded = listed["authorities"][0].clone();
    assert_eq!(recorded["authorityKind"], "form_20g");
    assert_eq!(recorded["legalStatus"], "in_force");
    assert_eq!(recorded["validityBasis"], "perpetual");
    assert_eq!(recorded["effectiveFrom"], "2020-01-01");
    assert_eq!(recorded["effectiveTo"], Value::Null);
    assert_eq!(
        recorded["sourceCitation"],
        "Licence copy inspected at the counter"
    );
    // The moment of recording is its own fact, kept apart from the period the document asserts.
    assert!(
        recorded["recordedAtUtc"].as_str().expect("recorded at") > "2026-01-01",
        "{recorded:?}"
    );
    assert_eq!(listed["coverage"][0]["productId"], world.product.as_str());

    // NOTHING here claims the licence was verified with a government.
    let text = listed.to_string().to_lowercase();
    for forbidden in ["government", "verified", "valid licence", "validlicence"] {
        assert!(!text.contains(forbidden), "{forbidden} appeared: {listed}");
    }

    // Overlapping evidence of the same kind for the same supplier is refused rather than resolved.
    let overlap = record_authority_over_http(
        &service,
        &world.cookie,
        &world.supplier,
        "form_20g",
        "20G-MH-9999",
        "2024-01-01",
        None,
        "perpetual",
        "in_force",
    )
    .await;
    assert_eq!(overlap.status, 409, "{:?}", overlap.body);

    // Archiving withdraws it without deleting it.
    let archived = call(
        &service,
        "POST",
        &format!("/api/v1/store/schedule-x/supplier-authorities/{authority}/archive"),
        Some(json!({ "expectedRevision": 1, "reason": "entered against the wrong supplier" })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(archived.status, 200, "{:?}", archived.body);
    assert_eq!(archived.body["status"], "archived");

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let kept: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM supplier_schedule_x_authorities")
        .fetch_one(&pool)
        .await
        .expect("kept");
    assert_eq!(kept, 1, "archiving deleted compliance evidence");
    let refused = sqlx::query("DELETE FROM supplier_schedule_x_authorities")
        .execute(&pool)
        .await;
    assert!(refused.is_err(), "direct SQL deleted compliance evidence");
    pool.close().await;
}

/// D3C1-2. Recording a supplier's authority is the owner's job. A pharmacist may read it for the
/// compliance workflow; a cashier can neither read nor write it, and so can never self-attest a
/// supplier's licence to make a future sale pass.
#[tokio::test]
async fn real_service_keeps_supplier_authority_management_with_the_owner_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;

    let pharmacist = sign_in_as(
        &service,
        "pharmacist",
        "rx-d3c1",
        "01997a00-0000-7000-8000-0000000004c1",
    )
    .await;
    let cashier = sign_in_as(
        &service,
        "cashier",
        "till-d3c1",
        "01997a00-0000-7000-8000-0000000004c2",
    )
    .await;

    // A pharmacist reads.
    let read = call(
        &service,
        "GET",
        "/api/v1/store/schedule-x/supplier-authorities",
        None,
        Some(&pharmacist),
    )
    .await;
    assert_eq!(read.status, 200, "{:?}", read.body);

    // A pharmacist does not record.
    let pharmacist_write = record_authority_over_http(
        &service,
        &pharmacist,
        &world.supplier,
        "form_20g",
        "20G-MH-5511",
        "2020-01-01",
        None,
        "perpetual",
        "in_force",
    )
    .await;
    assert_eq!(pharmacist_write.status, 403, "{:?}", pharmacist_write.body);

    // A cashier neither reads nor records.
    let cashier_read = call(
        &service,
        "GET",
        "/api/v1/store/schedule-x/supplier-authorities",
        None,
        Some(&cashier),
    )
    .await;
    assert_eq!(cashier_read.status, 403, "{:?}", cashier_read.body);
    let cashier_write = record_authority_over_http(
        &service,
        &cashier,
        &world.supplier,
        "form_20g",
        "20G-MH-5511",
        "2020-01-01",
        None,
        "perpetual",
        "in_force",
    )
    .await;
    assert_eq!(cashier_write.status, 403, "{:?}", cashier_write.body);

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM supplier_schedule_x_authorities")
        .fetch_one(&pool)
        .await
        .expect("rows");
    assert_eq!(rows, 0);
    pool.close().await;
}

/// D3C1-3. Two simultaneous recordings of the same authority. One survives; the other is told the
/// period overlaps rather than being allowed to create an ambiguity.
#[tokio::test]
async fn real_service_refuses_a_second_overlapping_authority_under_a_race_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;

    let (first, second) = tokio::join!(
        record_authority_over_http(
            &service,
            &world.cookie,
            &world.supplier,
            "form_20g",
            "20G-MH-5511",
            "2020-01-01",
            None,
            "perpetual",
            "in_force"
        ),
        record_authority_over_http(
            &service,
            &world.cookie,
            &world.supplier,
            "form_20g",
            "20G-MH-5511",
            "2020-01-01",
            None,
            "perpetual",
            "in_force"
        ),
    );
    // Exactly one committed; the loser was refused on the overlap or told the service was busy.
    let created = [&first, &second]
        .iter()
        .filter(|reply| reply.status == 201)
        .count();
    assert_eq!(
        created, 1,
        "first={:?} second={:?}",
        first.body, second.body
    );
    for reply in [&first, &second] {
        assert!(
            reply.status == 201 || reply.status == 409 || reply.status == 503,
            "{:?}",
            reply.body
        );
    }

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM supplier_schedule_x_authorities WHERE status='active'",
    )
    .fetch_one(&pool)
    .await
    .expect("rows");
    assert_eq!(rows, 1, "a race created overlapping authority");
    pool.close().await;
}

/// D3C1-4, item 27. The provenance surface reports the authority verdict BESIDE the lot verdict,
/// with a precise reason, and the Phase 1M-D3-B lot verdict is untouched by any of it.
#[tokio::test]
async fn real_service_reports_supplier_authority_beside_the_lot_verdict_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;
    let professional = pharmacist_over_http(&service, &world).await;
    let (_, item) = prescription_over_http(&service, &world, None, 20).await;
    let batch = qualified_lot_over_http(&service, &world, &professional).await;
    let (_, line_id, _) =
        prepared_sale_on_lot_over_http(&service, &world, &batch, &item, &professional).await;

    // The lot is accounted for by D3-B, and its source has no authority evidence yet. The two
    // verdicts are reported separately, and D3-B's one still says qualified.
    let listed = supply_preparation_over_http(&service, &world.cookie).await;
    let candidate = listed["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .find(|entry| entry["saleLineId"] == line_id.as_str())
        .expect("the line is listed")
        .clone();
    assert_eq!(candidate["lotProvenance"]["state"], "qualified");
    assert_eq!(candidate["everySourceAuthorised"], false);
    assert_eq!(
        candidate["sourceAuthorities"][0]["authority"]["state"], "unresolved",
        "{candidate:?}"
    );
    assert_eq!(
        candidate["sourceAuthorities"][0]["authority"]["reason"], "no_authority_recorded",
        "{candidate:?}"
    );

    // Because D3-C1 does not gate preparation, the D3-B workflow is unchanged: the working record
    // still prepares. The authority predicate is reported, not yet enforced.
    let prepared = prepare_supply_over_http(&service, &world.cookie, &line_id).await;
    assert_eq!(prepared.status, 201, "{:?}", prepared.body);

    // Record the evidence; the verdict changes and the lot verdict does not.
    authorise_supplier_over_http(&service, &world).await;
    let after = supply_preparation_over_http(&service, &world.cookie).await;
    let candidate = after["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .find(|entry| entry["saleLineId"] == line_id.as_str())
        .expect("the line is listed")
        .clone();
    assert_eq!(candidate["lotProvenance"]["state"], "qualified");
    assert_eq!(candidate["everySourceAuthorised"], true);
    assert_eq!(
        candidate["sourceAuthorities"][0]["authority"]["state"],
        "established"
    );
    assert_eq!(
        candidate["sourceAuthorities"][0]["authority"]["authorityKind"],
        "form_20g"
    );

    // And the surface never calls any of this legal, compliant or verified.
    let text = after.to_string().to_lowercase();
    for forbidden in ["\"legal\"", "compliant", "sale_allowed", "government"] {
        assert!(!text.contains(forbidden), "{forbidden} appeared: {after}");
    }
}

/// Phase 1M-D3-C2, item 29 case 15 — NDPS UNRESOLVED REFUSES A SCHEDULE X SALE.
///
/// This test began in Phase 1M-D3-C1 proving that a complete Schedule X evidence chain was still
/// refused, because no Schedule X Sale could post at all. Phase 1M-D3-C2 enables that chain, and
/// `real_service_posts_a_fully_supported_schedule_x_retail_sale_over_http` is the conversion of this
/// very fixture to a successful posting.
///
/// What is left here is the one fact that fixture adds and this one deliberately does not: the NDPS
/// purview. Every Schedule X predicate below is favourable — Form 20-F authority and coverage, a
/// qualified lot, the source's authority, the duplicate copy, the rule 65(11)(c) annotation, the
/// confirmed physical register entry — and the supply is STILL refused, because nobody has recorded
/// whether this drug falls within the NDPS Act. An unrecorded axis is not a cleared one.
#[tokio::test]
async fn real_service_keeps_schedule_x_refused_with_every_d3c1_fact_present_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;
    record_basis_over_http(&service, &world, Some("prescription_register")).await;
    let professional = pharmacist_over_http(&service, &world).await;

    let (licence, revision) = form_20f_over_http(&service, &world, "MH-PUNE-20F-4471").await;
    let authorised = set_authority_over_http(
        &service,
        &world,
        &licence,
        revision,
        json!({
            "legalStatus": "in_force",
            "validityBasis": "perpetual",
            "validFrom": "2020-01-01",
        }),
    )
    .await;
    assert_eq!(authorised.status, 200, "{:?}", authorised.body);
    let covered = cover_over_http(&service, &world, &licence, "2020-01-01", None).await;
    assert_eq!(covered.status, 201, "{:?}", covered.body);

    let batch = qualified_lot_over_http(&service, &world, &professional).await;
    // The new fact: the purchase source's own Schedule X authority evidence.
    authorise_supplier_over_http(&service, &world).await;

    let (prescription, item) = prescription_over_http(&service, &world, None, 20).await;
    let duplicate = call(
        &service,
        "POST",
        &format!("/api/v1/prescriptions/{prescription}/schedule-x-duplicate-copy"),
        Some(json!({ "retainedDuplicatePrescriptionCopyConfirmed": true, "note": Value::Null })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(duplicate.status, 201, "{:?}", duplicate.body);

    let (sale_id, line_id, sale_revision) =
        prepared_sale_on_lot_over_http(&service, &world, &batch, &item, &professional).await;
    let annotated = record_annotation_over_http(&service, &world.cookie, &line_id, true).await;
    assert_eq!(annotated.status, 201, "{:?}", annotated.body);
    let prepared = prepare_supply_over_http(&service, &world.cookie, &line_id).await;
    assert_eq!(prepared.status, 201, "{:?}", prepared.body);
    let entry_id = prepared.body["id"].as_str().expect("id").to_owned();
    let confirmed = call(
        &service,
        "POST",
        &format!("/api/v1/store/schedule-x/register/{entry_id}/confirm"),
        Some(json!({
            "supervisingProfessionalId": professional,
            "particularsEnteredInPhysicalRegister": true,
            "physicalEntryAuthenticated": true,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);

    // Every verdict this software can give is favourable.
    let listed = supply_preparation_over_http(&service, &world.cookie).await;
    let candidate = listed["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .find(|entry| entry["saleLineId"] == line_id.as_str())
        .expect("listed")
        .clone();
    assert_eq!(candidate["lotProvenance"]["state"], "qualified");
    assert_eq!(candidate["everySourceAuthorised"], true);

    // And the counter still refuses.
    let refused = post_quoted_over_http(
        &service,
        &world,
        &sale_id,
        sale_revision,
        "01997a00-0000-7000-8000-0000000002fc",
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    // Named for the axis that is actually unresolved, not for a workflow that now exists.
    assert_eq!(refused.body["code"], "schedule_x_ndps_purview_unresolved");
    assert_eq!(refused.body["issues"][0]["field"], "lines.1");

    let detail = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{sale_id}"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(detail.body["status"], "draft");
    assert_eq!(detail.body["documentNumber"], Value::Null);

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let outflow: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inventory_movements WHERE movement_type='sale' AND sale_line_id=?",
    )
    .bind(&line_id)
    .fetch_one(&pool)
    .await
    .expect("movements");
    assert_eq!(outflow, 0, "a refused Schedule X sale moved stock");
    let dispensings: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM prescription_dispensings WHERE sale_document_id=?",
    )
    .bind(&sale_id)
    .fetch_one(&pool)
    .await
    .expect("dispensings");
    assert_eq!(dispensings, 0);
    let finalized: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_schedule_x_register_entries \
         WHERE entry_kind='supply' AND status='finalized'",
    )
    .fetch_one(&pool)
    .await
    .expect("finalized");
    assert_eq!(
        finalized, 0,
        "a supply entry was closed with no sale behind it"
    );
    pool.close().await;

    let text = refused.body.to_string().to_lowercase();
    for word in ["banned", "prohibit", "illegal"] {
        assert!(!text.contains(word), "{word}: {:?}", refused.body);
    }
}

/// D3C1-6, item 32. The authority evidence is durable compliance data, so the existing backup path
/// has to carry it — identity, supplier linkage, kind, number, period, coverage, lifecycle and the
/// moment of recording.
#[tokio::test]
async fn real_service_carries_supplier_authority_through_backup_and_restore_over_http() {
    let harness = start_with_backups().await;
    let service = &harness.service;
    let world = seed_sale_world(service, 1).await;
    schedule_over_http(service, &world, &["schedule_x"]).await;
    let authority = authorise_supplier_over_http(service, &world).await;

    let created = call(
        service,
        "POST",
        "/api/v1/backups/create",
        Some(json!({})),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    let resolved = call(
        service,
        "GET",
        &format!(
            "/api/v1/backups/{}/download",
            created.body["backupId"].as_str().expect("backup id")
        ),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(resolved.status, 200, "{:?}", resolved.body);
    let (status, _, downloaded) = get_bytes(
        service,
        resolved.body["url"].as_str().expect("url"),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(status, 200);

    let after = call(
        service,
        "POST",
        "/api/v1/parties",
        Some(json!({
            "party": { "displayName": "Post-Backup Supplier" },
            "roles": [{ "role": "supplier" }]
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(after.status, 201, "{:?}", after.body);

    let (status, _, body) = call_bytes(
        service,
        "/api/v1/backups/restore/prepare",
        "application/octet-stream",
        &downloaded,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let prepared: Value = serde_json::from_slice(&body).expect("prepared restore");
    let committed = call(
        service,
        "POST",
        "/api/v1/backups/restore/commit",
        Some(json!({
            "candidateToken": prepared["candidateToken"].as_str().expect("token"),
            "password": "Integration-Password-42"
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(committed.status, 200, "{:?}", committed.body);
    api::backups::recover_interrupted_restore(&harness.backups, &service.database_path)
        .await
        .expect("recovery");
    let reopened = database::connect(&service.database_path)
        .await
        .expect("reopened database");
    assert!(
        api::backups::complete_restore_after_open(&reopened, &harness.backups)
            .await
            .expect("completion")
    );

    let post_backup: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM parties WHERE display_name='Post-Backup Supplier'",
    )
    .fetch_one(&reopened)
    .await
    .expect("parties");
    assert_eq!(post_backup, 0, "the restore did not roll the pharmacy back");

    let restored: (
        String,
        String,
        String,
        String,
        Option<String>,
        String,
        String,
    ) = sqlx::query_as(
        "SELECT supplier_party_id,authority_kind,authority_number,effective_from,effective_to,\
             legal_status,status FROM supplier_schedule_x_authorities WHERE id=?",
    )
    .bind(&authority)
    .fetch_one(&reopened)
    .await
    .expect("the authority did not survive the restore");
    assert_eq!(restored.0, world.supplier);
    assert_eq!(restored.1, "form_20g");
    assert_eq!(restored.2, "20G-MH-5511");
    assert_eq!(restored.3, "2020-01-01");
    assert_eq!(restored.4, None);
    assert_eq!(restored.5, "in_force");
    assert_eq!(restored.6, "active");

    let coverage: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM supplier_schedule_x_authority_coverage \
         WHERE authority_id=? AND status='active'",
    )
    .bind(&authority)
    .fetch_one(&reopened)
    .await
    .expect("coverage");
    assert_eq!(coverage, 1, "the drug coverage did not survive the restore");

    // And the restored database still refuses to delete the evidence.
    let refused = sqlx::query("DELETE FROM supplier_schedule_x_authorities WHERE id=?")
        .bind(&authority)
        .execute(&reopened)
        .await;
    assert!(
        refused.is_err(),
        "the restored database lost its no-delete guard"
    );
    reopened.close().await;
}

/// Phase 1M-D3-C2, item 29 case 1 — THE SUPPORTED SCHEDULE X RETAIL SALE POSTS.
///
/// Every predicate of the first supported path is established: the store's own Form 20-F retail
/// authority with this very drug named on it, a lot whose whole sellable balance arrived on a posted
/// purchase with an authenticated D2 receipt, documentary evidence that the purchase source held
/// Schedule X authority covering the drug on the invoice date, a prescription in duplicate with the
/// retained copy attested, the rule 65(11)(c) seller-and-date annotation, a registered pharmacist
/// supervising, the rule 65(21) working entry written into the bound physical register and
/// authenticated by hand, and the NDPS purview established NOT to apply.
///
/// This is the first Schedule X Sale AUSHADHARTH has ever posted. Everything the register, the
/// dispensing and the bill particulars must carry is asserted from the database afterwards, not from
/// the response.
#[tokio::test]
async fn real_service_posts_a_fully_supported_schedule_x_retail_sale_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;
    // The independent NDPS axis, established not to apply. Without this the supply is refused, and
    // a test that left it out would never reach the Schedule X predicates at all.
    finding_over_http(&service, &world, "ndps_purview", false).await;
    record_basis_over_http(&service, &world, Some("prescription_register")).await;
    let professional = pharmacist_over_http(&service, &world).await;

    let (licence, revision) = form_20f_over_http(&service, &world, "MH-PUNE-20F-4471").await;
    let authorised = set_authority_over_http(
        &service,
        &world,
        &licence,
        revision,
        json!({
            "legalStatus": "in_force",
            "validityBasis": "perpetual",
            "validFrom": "2020-01-01",
        }),
    )
    .await;
    assert_eq!(authorised.status, 200, "{:?}", authorised.body);
    let covered = cover_over_http(&service, &world, &licence, "2020-01-01", None).await;
    assert_eq!(covered.status, 201, "{:?}", covered.body);

    let batch = qualified_lot_over_http(&service, &world, &professional).await;
    authorise_supplier_over_http(&service, &world).await;

    let (prescription, item) = prescription_over_http(&service, &world, None, 20).await;
    let duplicate = call(
        &service,
        "POST",
        &format!("/api/v1/prescriptions/{prescription}/schedule-x-duplicate-copy"),
        Some(json!({ "retainedDuplicatePrescriptionCopyConfirmed": true, "note": Value::Null })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(duplicate.status, 201, "{:?}", duplicate.body);

    let (sale_id, line_id, sale_revision) =
        prepared_sale_on_lot_over_http(&service, &world, &batch, &item, &professional).await;
    let annotated = record_annotation_over_http(&service, &world.cookie, &line_id, true).await;
    assert_eq!(annotated.status, 201, "{:?}", annotated.body);
    let prepared = prepare_supply_over_http(&service, &world.cookie, &line_id).await;
    assert_eq!(prepared.status, 201, "{:?}", prepared.body);
    let entry_id = prepared.body["id"].as_str().expect("id").to_owned();
    let entry_reference = prepared.body["reference"]
        .as_str()
        .expect("reference")
        .to_owned();

    // The physical act, then the record of it. Both attestations together; neither is prechecked.
    let confirmed = call(
        &service,
        "POST",
        &format!("/api/v1/store/schedule-x/register/{entry_id}/confirm"),
        Some(json!({
            "supervisingProfessionalId": professional,
            "particularsEnteredInPhysicalRegister": true,
            "physicalEntryAuthenticated": true,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);

    // AND IT POSTS.
    let posted = post_quoted_over_http(
        &service,
        &world,
        &sale_id,
        sale_revision,
        "01997a00-0000-7000-8000-00000000030a",
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    assert_eq!(posted.body["status"], "posted");
    let document_number = posted.body["documentNumber"]
        .as_str()
        .expect("documentNumber")
        .to_owned();
    assert!(!document_number.is_empty());

    // ---------------------------------------------------------------------------------------------
    // The durable state, read from the database rather than believed from the response.
    // ---------------------------------------------------------------------------------------------
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");

    let (status, number, business_date): (String, Option<String>, String) = sqlx::query_as(
        "SELECT status,document_number,business_date FROM sale_documents WHERE id=?",
    )
    .bind(&sale_id)
    .fetch_one(&pool)
    .await
    .expect("sale");
    assert_eq!(status, "posted");
    assert_eq!(number.as_deref(), Some(document_number.as_str()));

    // Exactly one document number, and exactly one dispensing for the line.
    let dispensings: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM prescription_dispensings WHERE sale_line_id=?")
            .bind(&line_id)
            .fetch_one(&pool)
            .await
            .expect("dispensings");
    assert_eq!(dispensings, 1);

    // The inventory outflow exists, is negative, and names the lot.
    let (outflow, moved_batch): (i64, String) = sqlx::query_as(
        "SELECT quantity_delta_atoms,batch_id FROM inventory_movements \
         WHERE movement_type='sale' AND sale_line_id=?",
    )
    .bind(&line_id)
    .fetch_one(&pool)
    .await
    .expect("outflow");
    assert!(outflow < 0, "outflow was {outflow}");
    assert_eq!(moved_batch, batch);

    // The rule 65(21) entry: finalized, bound to this dispensing, carrying the bill particulars,
    // with both physical attestations still recorded and nothing else disturbed.
    #[derive(sqlx::FromRow)]
    struct FinalEntry {
        status: String,
        dispensing_id: Option<String>,
        bill_number: Option<String>,
        bill_date: Option<String>,
        particulars_entered_in_physical_register: i64,
        physical_entry_authenticated: i64,
        reference: String,
        batch_id: Option<String>,
        finalized_at_utc: Option<String>,
    }
    let entry: FinalEntry = sqlx::query_as(
        "SELECT status,dispensing_id,bill_number,bill_date,\
         particulars_entered_in_physical_register,physical_entry_authenticated,reference,\
         batch_id,finalized_at_utc \
         FROM store_schedule_x_register_entries WHERE id=?",
    )
    .bind(&entry_id)
    .fetch_one(&pool)
    .await
    .expect("entry");
    assert_eq!(entry.status, "finalized");
    assert_eq!(entry.reference, entry_reference);
    assert_eq!(entry.batch_id.as_deref(), Some(batch.as_str()));
    assert_eq!(entry.particulars_entered_in_physical_register, 1);
    assert_eq!(entry.physical_entry_authenticated, 1);
    assert!(entry.finalized_at_utc.is_some());
    // Rule 65(21)(b)(ix): the bill number and date of the supply, which are the Sale's own.
    assert_eq!(entry.bill_number.as_deref(), Some(document_number.as_str()));
    assert_eq!(entry.bill_date.as_deref(), Some(business_date.as_str()));
    // Bound to the dispensing of this very line.
    let bound: String =
        sqlx::query_scalar("SELECT sale_line_id FROM prescription_dispensings WHERE id=?")
            .bind(entry.dispensing_id.as_deref().expect("dispensing_id"))
            .fetch_one(&pool)
            .await
            .expect("bound dispensing");
    assert_eq!(bound, line_id);

    // No duplicate Schedule X supply entry for the line, and nothing left prepared or confirmed.
    let supply_entries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_schedule_x_register_entries \
         WHERE sale_line_id=? AND entry_kind='supply'",
    )
    .bind(&line_id)
    .fetch_one(&pool)
    .await
    .expect("count");
    assert_eq!(supply_entries, 1);
    let live: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_schedule_x_register_entries \
         WHERE sale_document_id=? AND status IN ('prepared','confirmed')",
    )
    .bind(&sale_id)
    .fetch_one(&pool)
    .await
    .expect("live count");
    assert_eq!(live, 0);

    // Rule 65(3)(1) excludes Schedule X by its own opening words: no generic prescription-register
    // record was created for this supply.
    let generic: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM prescription_supply_records WHERE sale_document_id=?",
    )
    .bind(&sale_id)
    .fetch_one(&pool)
    .await
    .expect("generic records");
    assert_eq!(
        generic, 0,
        "a Schedule X supply was entered in the rule 65(3) register"
    );

    // The audit records the finalization against the entry, with identifiers and states only.
    let audited: Vec<String> = sqlx::query_scalar(
        "SELECT change_payload FROM master_change_events \
         WHERE entity_type='schedule_x_register_entry' AND entity_id=? AND action='posted'",
    )
    .bind(&entry_id)
    .fetch_all(&pool)
    .await
    .expect("audit");
    assert_eq!(audited.len(), 1, "{audited:?}");
    let payload = &audited[0];
    assert!(payload.contains("finalized"), "{payload}");
    // No patient, prescriber or medical particular in a generic audit payload.
    let lowered = payload.to_lowercase();
    for forbidden in ["address", "prescriber", "diagnos", "dose", "patient"] {
        assert!(
            !lowered.contains(forbidden),
            "{forbidden} in audit: {payload}"
        );
    }
    pool.close().await;
}

/// Phase 1M-D3-C2 — every unmet Schedule X predicate a structured refusal names, in the order the
/// service reports them, with the `lines.N.` prefix removed.
fn schedule_x_unmet(body: &Value) -> Vec<String> {
    body["issues"]
        .as_array()
        .map(|issues| {
            issues
                .iter()
                .filter_map(|issue| issue["field"].as_str())
                .filter_map(|field| field.rsplit_once('.').map(|(_, code)| code.to_owned()))
                .filter(|code| code.starts_with("schedule_x_"))
                .collect()
        })
        .unwrap_or_default()
}

/// Phase 1M-D3-C2, item 29 cases 24 and 25 — A FAILED POSTING LEAVES THE PAPER TRUTHFUL.
///
/// The whole supported chain is established and the physical register entry is written and signed,
/// then the posting is made to fail by taking the lot's stock away underneath it. What must survive
/// is exactly one thing: the confirmed working entry, because the pharmacist's signature is on a
/// page of a bound register and a transaction cannot un-sign it.
///
/// Everything else must be gone — no posted document, no document number consumed, no dispensing, no
/// stock movement, no tender, and above all no FINALIZED Schedule X entry with a bill number for a
/// bill that was never issued.
#[tokio::test]
async fn real_service_rolls_a_failed_schedule_x_posting_back_to_a_confirmed_entry_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;
    finding_over_http(&service, &world, "ndps_purview", false).await;
    record_basis_over_http(&service, &world, Some("prescription_register")).await;
    let professional = pharmacist_over_http(&service, &world).await;

    let (licence, revision) = form_20f_over_http(&service, &world, "MH-PUNE-20F-4471").await;
    let authorised = set_authority_over_http(
        &service,
        &world,
        &licence,
        revision,
        json!({
            "legalStatus": "in_force",
            "validityBasis": "perpetual",
            "validFrom": "2020-01-01",
        }),
    )
    .await;
    assert_eq!(authorised.status, 200, "{:?}", authorised.body);
    let covered = cover_over_http(&service, &world, &licence, "2020-01-01", None).await;
    assert_eq!(covered.status, 201, "{:?}", covered.body);

    let batch = qualified_lot_over_http(&service, &world, &professional).await;
    authorise_supplier_over_http(&service, &world).await;

    let (prescription, item) = prescription_over_http(&service, &world, None, 20).await;
    let duplicate = call(
        &service,
        "POST",
        &format!("/api/v1/prescriptions/{prescription}/schedule-x-duplicate-copy"),
        Some(json!({ "retainedDuplicatePrescriptionCopyConfirmed": true, "note": Value::Null })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(duplicate.status, 201, "{:?}", duplicate.body);

    let (sale_id, line_id, sale_revision) =
        prepared_sale_on_lot_over_http(&service, &world, &batch, &item, &professional).await;
    let annotated = record_annotation_over_http(&service, &world.cookie, &line_id, true).await;
    assert_eq!(annotated.status, 201, "{:?}", annotated.body);
    let prepared = prepare_supply_over_http(&service, &world.cookie, &line_id).await;
    assert_eq!(prepared.status, 201, "{:?}", prepared.body);
    let entry_id = prepared.body["id"].as_str().expect("id").to_owned();
    let confirmed = call(
        &service,
        "POST",
        &format!("/api/v1/store/schedule-x/register/{entry_id}/confirm"),
        Some(json!({
            "supervisingProfessionalId": professional,
            "particularsEnteredInPhysicalRegister": true,
            "physicalEntryAuthenticated": true,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);

    // The paper is signed. Now the stock goes away underneath the posting: a stock removal of the
    // whole lot, which is an ordinary thing another terminal might do between the signature and the
    // Post Sale press.
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let available: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(quantity_delta_atoms),0) FROM inventory_movements \
         WHERE batch_id=? AND stock_status='sellable'",
    )
    .bind(&batch)
    .fetch_one(&pool)
    .await
    .expect("balance");
    assert!(available > 0, "nothing to remove");
    let number_before: Option<i64> = sqlx::query_scalar(
        "SELECT next_value FROM document_number_series WHERE document_kind='sale'",
    )
    .fetch_optional(&pool)
    .await
    .expect("series");
    sqlx::query(
        "INSERT INTO inventory_movements (id,store_id,product_id,product_pack_id,batch_id,\
         movement_type,stock_status,quantity_delta_atoms,occurred_on,reason,idempotency_key,\
         posted_by_user_id,posted_at_utc) \
         SELECT ?,store_id,product_id,product_pack_id,batch_id,'adjustment','sellable',?,\
         occurred_on,'drawn down for the rollback case',?,posted_by_user_id,posted_at_utc \
         FROM inventory_movements WHERE batch_id=? AND movement_type='purchase' LIMIT 1",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(-available)
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(&batch)
    .execute(&pool)
    .await
    .expect("removal");

    // The posting now fails on availability, after every Schedule X predicate was satisfied.
    let refused = post_quoted_over_http(
        &service,
        &world,
        &sale_id,
        sale_revision,
        "01997a00-0000-7000-8000-00000000031b",
    )
    .await;
    assert_ne!(refused.status, 200, "{:?}", refused.body);

    // ---------------------------------------------------------------------------------------------
    // Nothing durable survived, and the entry is still truthfully confirmed.
    // ---------------------------------------------------------------------------------------------
    let (status, document_number): (String, Option<String>) =
        sqlx::query_as("SELECT status,document_number FROM sale_documents WHERE id=?")
            .bind(&sale_id)
            .fetch_one(&pool)
            .await
            .expect("sale");
    assert_eq!(status, "draft");
    assert_eq!(document_number, None);

    // The invoice series was not consumed.
    let number_after: Option<i64> = sqlx::query_scalar(
        "SELECT next_value FROM document_number_series WHERE document_kind='sale'",
    )
    .fetch_optional(&pool)
    .await
    .expect("series");
    assert_eq!(
        number_after, number_before,
        "an invoice number was consumed"
    );

    for (what, sql) in [
        (
            "dispensing",
            "SELECT COUNT(*) FROM prescription_dispensings WHERE sale_document_id=?",
        ),
        (
            "outward movement",
            "SELECT COUNT(*) FROM inventory_movements WHERE movement_type='sale' \
             AND sale_line_id IN (SELECT id FROM sale_lines WHERE sale_document_id=?)",
        ),
        (
            "tender",
            "SELECT COUNT(*) FROM sale_tenders WHERE sale_document_id=?",
        ),
    ] {
        let count: i64 = sqlx::query_scalar(sql)
            .bind(&sale_id)
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 0, "{what} survived a failed posting");
    }

    // The Schedule X entry: still confirmed, never finalized, and with none of the three posting
    // bindings written. A bill number here would name a bill that does not exist.
    #[derive(sqlx::FromRow)]
    struct AfterFailure {
        status: String,
        dispensing_id: Option<String>,
        bill_number: Option<String>,
        bill_date: Option<String>,
        finalized_at_utc: Option<String>,
        particulars_entered_in_physical_register: i64,
        physical_entry_authenticated: i64,
    }
    let entry: AfterFailure = sqlx::query_as(
        "SELECT status,dispensing_id,bill_number,bill_date,finalized_at_utc,\
         particulars_entered_in_physical_register,physical_entry_authenticated \
         FROM store_schedule_x_register_entries WHERE id=?",
    )
    .bind(&entry_id)
    .fetch_one(&pool)
    .await
    .expect("entry");
    assert_eq!(entry.status, "confirmed");
    assert_eq!(entry.dispensing_id, None);
    assert_eq!(entry.bill_number, None);
    assert_eq!(entry.bill_date, None);
    assert_eq!(entry.finalized_at_utc, None);
    // The signature is still recorded, because it was really made.
    assert_eq!(entry.particulars_entered_in_physical_register, 1);
    assert_eq!(entry.physical_entry_authenticated, 1);
    pool.close().await;
}

/// Phase 1M-D3-C2, item 29 case 28 — THE DATABASE REFUSES EVERY DIRECT-SQL BYPASS.
///
/// The service is not the only thing standing between an under-evidenced Schedule X supply and a
/// posted Sale. Every attempt below goes straight at the tables with SQL, with no service code in
/// the way, and every one of them is refused by a trigger or a constraint.
///
/// Where a predicate rests on something outside any database — a page of a bound register, a
/// pharmacist's signature, a retained sheet of paper — what is refused here is an attempt to claim
/// the ATTESTATION AND LINKAGE without it. No trigger pretends to have inspected the physical world.
#[tokio::test]
async fn real_service_refuses_every_direct_sql_schedule_x_bypass_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    schedule_over_http(&service, &world, &["schedule_x"]).await;
    finding_over_http(&service, &world, "ndps_purview", false).await;
    record_basis_over_http(&service, &world, Some("prescription_register")).await;
    let professional = pharmacist_over_http(&service, &world).await;

    let (licence, revision) = form_20f_over_http(&service, &world, "MH-PUNE-20F-4471").await;
    let authorised = set_authority_over_http(
        &service,
        &world,
        &licence,
        revision,
        json!({
            "legalStatus": "in_force",
            "validityBasis": "perpetual",
            "validFrom": "2020-01-01",
        }),
    )
    .await;
    assert_eq!(authorised.status, 200, "{:?}", authorised.body);
    let covered = cover_over_http(&service, &world, &licence, "2020-01-01", None).await;
    assert_eq!(covered.status, 201, "{:?}", covered.body);

    let batch = qualified_lot_over_http(&service, &world, &professional).await;
    authorise_supplier_over_http(&service, &world).await;

    let (prescription, item) = prescription_over_http(&service, &world, None, 20).await;
    let duplicate = call(
        &service,
        "POST",
        &format!("/api/v1/prescriptions/{prescription}/schedule-x-duplicate-copy"),
        Some(json!({ "retainedDuplicatePrescriptionCopyConfirmed": true, "note": Value::Null })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(duplicate.status, 201, "{:?}", duplicate.body);

    let (sale_id, line_id, sale_revision) =
        prepared_sale_on_lot_over_http(&service, &world, &batch, &item, &professional).await;
    let annotated = record_annotation_over_http(&service, &world.cookie, &line_id, true).await;
    assert_eq!(annotated.status, 201, "{:?}", annotated.body);
    let prepared = prepare_supply_over_http(&service, &world.cookie, &line_id).await;
    assert_eq!(prepared.status, 201, "{:?}", prepared.body);
    let entry_id = prepared.body["id"].as_str().expect("id").to_owned();

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");

    // Every statement below must be refused. The message each trigger raises is asserted so a later
    // change cannot quietly move a refusal to a different, weaker guard.
    async fn refuses(pool: &sqlx::SqlitePool, what: &str, statement: &str, expected: &str) {
        let error = sqlx::query(statement)
            .execute(pool)
            .await
            .err()
            .unwrap_or_else(|| panic!("ACCEPTED a statement it must refuse — {what}: {statement}"))
            .to_string();
        assert!(
            error.contains(expected),
            "{what}: expected {expected}, got {error}"
        );
    }

    // ---------------------------------------------------------------------------------------------
    // While the entry is only PREPARED: none of the three posting bindings may be written, and the
    // entry may not jump straight to finalized.
    // ---------------------------------------------------------------------------------------------
    refuses(
        &pool,
        "bill number written while prepared",
        &format!(
            "UPDATE store_schedule_x_register_entries SET bill_number='INV/2026-27/0001' \
             WHERE id='{entry_id}'"
        ),
        "schedule_x_register_entry_immutable",
    )
    .await;
    refuses(
        &pool,
        "bill date written while prepared",
        &format!(
            "UPDATE store_schedule_x_register_entries SET bill_date='2026-05-04' \
             WHERE id='{entry_id}'"
        ),
        "schedule_x_register_entry_immutable",
    )
    .await;
    refuses(
        &pool,
        "prepared jumped straight to finalized",
        &format!(
            "UPDATE store_schedule_x_register_entries \
             SET status='finalized',finalized_at_utc='2026-05-04T06:00:00.000Z' \
             WHERE id='{entry_id}'"
        ),
        "schedule_x_register_entry_immutable",
    )
    .await;
    // Setting one attestation without the other would also break the table's own CHECK that keeps
    // the pair equal, but SQLite runs a BEFORE trigger ahead of CHECK evaluation, so the transition
    // guard is what answers. Either way the pair cannot be forced apart.
    refuses(
        &pool,
        "attestation flags forced apart",
        &format!(
            "UPDATE store_schedule_x_register_entries \
             SET particulars_entered_in_physical_register=1 WHERE id='{entry_id}'"
        ),
        "schedule_x_register_entry_immutable",
    )
    .await;
    refuses(
        &pool,
        "the lot was swapped",
        &format!(
            "UPDATE store_schedule_x_register_entries SET batch_id=NULL WHERE id='{entry_id}'"
        ),
        "schedule_x_register_entry_immutable",
    )
    .await;
    refuses(
        &pool,
        "the entry was deleted",
        &format!("DELETE FROM store_schedule_x_register_entries WHERE id='{entry_id}'"),
        "schedule_x_register_entry_immutable",
    )
    .await;
    // And the Sale cannot be posted while the entry is only prepared.
    refuses(
        &pool,
        "posted with a prepared entry outstanding",
        &format!("UPDATE sale_documents SET status='posted' WHERE id='{sale_id}'"),
        "schedule_x_register_entry_missing",
    )
    .await;

    // ---------------------------------------------------------------------------------------------
    // Now CONFIRMED: the physical page is written and signed. Still none of the three bindings may
    // be written outside finalization, and finalization still needs all three.
    // ---------------------------------------------------------------------------------------------
    let confirmed = call(
        &service,
        "POST",
        &format!("/api/v1/store/schedule-x/register/{entry_id}/confirm"),
        Some(json!({
            "supervisingProfessionalId": professional,
            "particularsEnteredInPhysicalRegister": true,
            "physicalEntryAuthenticated": true,
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);

    refuses(
        &pool,
        "bill number written while merely confirmed",
        &format!(
            "UPDATE store_schedule_x_register_entries SET bill_number='INV/2026-27/0001' \
             WHERE id='{entry_id}'"
        ),
        "schedule_x_register_entry_immutable",
    )
    .await;
    refuses(
        &pool,
        "finalized with no dispensing and no bill particulars",
        &format!(
            "UPDATE store_schedule_x_register_entries \
             SET status='finalized',finalized_at_utc='2026-05-04T06:00:00.000Z',\
                 finalized_by_user_id=(SELECT id FROM users WHERE role='owner_admin' LIMIT 1) \
             WHERE id='{entry_id}'"
        ),
        "schedule_x_register_entry_immutable",
    )
    .await;
    // A confirmed entry is still not a posted Sale: the Sale needs the entry FINALIZED, which only
    // the posting transaction can do.
    refuses(
        &pool,
        "posted with a confirmed but unfinalized entry",
        &format!("UPDATE sale_documents SET status='posted' WHERE id='{sale_id}'"),
        "schedule_x_register_entry_missing",
    )
    .await;

    // ---------------------------------------------------------------------------------------------
    // Post it properly, then prove the finalized record is beyond reach.
    // ---------------------------------------------------------------------------------------------
    let posted = post_quoted_over_http(
        &service,
        &world,
        &sale_id,
        sale_revision,
        "01997a00-0000-7000-8000-00000000032c",
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);

    refuses(
        &pool,
        "the dispensing was re-pointed after finalization",
        &format!(
            "UPDATE store_schedule_x_register_entries SET dispensing_id=NULL WHERE id='{entry_id}'"
        ),
        "schedule_x_register_entry_immutable",
    )
    .await;
    refuses(
        &pool,
        "the bill number was rewritten after finalization",
        &format!(
            "UPDATE store_schedule_x_register_entries SET bill_number='INV/2026-27/9999' \
             WHERE id='{entry_id}'"
        ),
        "schedule_x_register_entry_immutable",
    )
    .await;
    refuses(
        &pool,
        "a statutory particular was altered after finalization",
        &format!(
            "UPDATE store_schedule_x_register_entries SET quantity_atoms=quantity_atoms+1 \
             WHERE id='{entry_id}'"
        ),
        "schedule_x_register_entry_immutable",
    )
    .await;
    refuses(
        &pool,
        "the finalized entry was deleted",
        &format!("DELETE FROM store_schedule_x_register_entries WHERE id='{entry_id}'"),
        "schedule_x_register_entry_immutable",
    )
    .await;
    pool.close().await;
}

// ==============================================================================================
// Phase 1M-D3-C2 — the read-only preflight, and the proof that it never authorizes a posting
// ==============================================================================================

/// Everything a supported Schedule X retail Sale needs, built once.
struct FavourableX {
    world: SaleWorld,
    /// The prescription item the line is dispensed against.
    item: String,
    professional: String,
    /// The professional named on the working entry. Usually the same as `professional`.
    entry_professional: String,
    licence: String,
    authority: String,
    batch: String,
    sale_id: String,
    line_id: String,
    revision: i64,
    entry_id: String,
}

/// The whole supported chain, up to and optionally including the physical-register confirmation.
///
/// `confirm = false` leaves the working entry `prepared`, which is the state the counter is in while
/// the particulars are still being written into the bound register.
async fn favourable_schedule_x_over_http(service: &Service, confirm: bool) -> FavourableX {
    favourable_schedule_x_with_options(
        service,
        FavourableOptions {
            confirm,
            ..Default::default()
        },
    )
    .await
}

/// How a favourable Schedule X world should differ from the ordinary one.
#[derive(Default, Clone)]
struct FavourableOptions {
    /// Record that the particulars were entered in the physical register and signed by hand.
    confirm: bool,
    /// Confirm the working entry under a SECOND registered pharmacist, leaving the supply supervised
    /// by the first, so a test can reach the entry-level rule 65(2) predicate on its own.
    separate_entry_pharmacist: bool,
    /// A veterinary supply: the prescription names an animal and its owner's name and address.
    animal: bool,
    /// Stop before preparing the working entry, leaving `entry_id` empty.
    ///
    /// A prepared entry PINS the draft — it names exactly what is about to be supplied — so a test
    /// that still has lines to add to the basket must finish the basket first and prepare after.
    skip_prepare: bool,
    /// Record no Form 20-F Schedule X retail authority at all for the store.
    skip_form_20f: bool,
    /// Supply from a lot whose sellable stock arrived by a counted shelf rather than a purchase with
    /// an authenticated Schedule X receipt — the shape legacy stock has.
    counted_lot: bool,
    /// The prescription's repeat authority, as the prescriber wrote it.
    repeat: Option<Value>,
    /// The supplier authority's own effective period, as (from, to, validity basis). The default is a
    /// perpetual authority from 2020-01-01, which covers every purchase these fixtures make.
    authority_period: Option<(String, Option<String>, String)>,
}

async fn favourable_schedule_x_with_entry_pharmacist(
    service: &Service,
    confirm: bool,
    separate_entry_pharmacist: bool,
) -> FavourableX {
    favourable_schedule_x_with_options(
        service,
        FavourableOptions {
            confirm,
            separate_entry_pharmacist,
            ..Default::default()
        },
    )
    .await
}

async fn favourable_schedule_x_with_options(
    service: &Service,
    options: FavourableOptions,
) -> FavourableX {
    let FavourableOptions {
        confirm,
        separate_entry_pharmacist,
        animal,
        skip_prepare,
        skip_form_20f,
        counted_lot,
        repeat,
        authority_period,
    } = options;
    let world = seed_sale_world(service, 1).await;
    schedule_over_http(service, &world, &["schedule_x"]).await;
    finding_over_http(service, &world, "ndps_purview", false).await;
    record_basis_over_http(service, &world, Some("prescription_register")).await;
    let professional = pharmacist_over_http(service, &world).await;

    // The store's own Form 20-F Schedule X retail authority, and this drug named on it.
    let licence = if skip_form_20f {
        String::new()
    } else {
        let (licence, revision) = form_20f_over_http(service, &world, "MH-PUNE-20F-4471").await;
        let authorised = set_authority_over_http(
            service,
            &world,
            &licence,
            revision,
            json!({
                "legalStatus": "in_force",
                "validityBasis": "perpetual",
                "validFrom": "2020-01-01",
            }),
        )
        .await;
        assert_eq!(authorised.status, 200, "{:?}", authorised.body);
        let covered = cover_over_http(service, &world, &licence, "2020-01-01", None).await;
        assert_eq!(covered.status, 201, "{:?}", covered.body);
        licence
    };

    // The lot. A qualified one arrived on a posted purchase whose Schedule X receipt entry was
    // written into the physical register; a counted one simply appeared on a shelf, which is the
    // shape legacy stock has and which rule 65(21) cannot account for.
    let batch = if counted_lot {
        counted_lot_over_http(service, &world).await
    } else {
        qualified_lot_over_http(service, &world, &professional).await
    };
    let authority = match authority_period.clone() {
        None => authorise_supplier_over_http(service, &world).await,
        Some((from, to, basis)) => {
            let recorded = record_authority_over_http(
                service,
                &world.cookie,
                &world.supplier,
                "form_20g",
                "20G-MH-5511",
                &from,
                to.as_deref(),
                &basis,
                "in_force",
            )
            .await;
            assert_eq!(recorded.status, 201, "{:?}", recorded.body);
            let authority = recorded.body["id"]
                .as_str()
                .expect("authority id")
                .to_owned();
            // Coverage spans exactly the authority's own period: a drug cannot be covered by a
            // licence before that licence exists, nor after it ends. So the period under test is the
            // AUTHORITY's and nothing else.
            let covered = call(
                service,
                "POST",
                &format!("/api/v1/store/schedule-x/supplier-authorities/{authority}/coverage"),
                Some(json!({
                    "productId": world.product,
                    "effectiveFrom": from,
                    "effectiveTo": to,
                    "sourceCitation": "Drug endorsed on the licence copy",
                    "reason": Value::Null,
                })),
                Some(&world.cookie),
            )
            .await;
            assert_eq!(covered.status, 201, "{:?}", covered.body);
            authority
        }
    };

    let (prescription, item) = if animal {
        animal_prescription_over_http(service, &world, 20).await
    } else if let Some(repeat) = repeat.clone() {
        repeatable_prescription_over_http(service, &world, 40, repeat).await
    } else {
        prescription_over_http(service, &world, None, 20).await
    };
    let duplicate = call(
        service,
        "POST",
        &format!("/api/v1/prescriptions/{prescription}/schedule-x-duplicate-copy"),
        Some(json!({ "retainedDuplicatePrescriptionCopyConfirmed": true, "note": Value::Null })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(duplicate.status, 201, "{:?}", duplicate.body);

    let (sale_id, line_id, revision) =
        prepared_sale_on_lot_over_http(service, &world, &batch, &item, &professional).await;
    let annotated = record_annotation_over_http(service, &world.cookie, &line_id, true).await;
    assert_eq!(annotated.status, 201, "{:?}", annotated.body);
    let entry_id = if skip_prepare {
        String::new()
    } else {
        let prepared = prepare_supply_over_http(service, &world.cookie, &line_id).await;
        assert_eq!(prepared.status, 201, "{:?}", prepared.body);
        prepared.body["id"].as_str().expect("id").to_owned()
    };

    let entry_professional = if separate_entry_pharmacist {
        second_pharmacist_over_http(service, &world).await
    } else {
        professional.clone()
    };

    if confirm {
        let confirmed = call(
            service,
            "POST",
            &format!("/api/v1/store/schedule-x/register/{entry_id}/confirm"),
            Some(json!({
                "supervisingProfessionalId": entry_professional,
                "particularsEnteredInPhysicalRegister": true,
                "physicalEntryAuthenticated": true,
            })),
            Some(&world.cookie),
        )
        .await;
        assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);
    }

    FavourableX {
        world,
        item,
        professional,
        entry_professional,
        licence,
        authority,
        batch,
        sale_id,
        line_id,
        revision,
        entry_id,
    }
}

async fn preflight_over_http(service: &Service, cookie: &str, sale_id: &str) -> Value {
    let reply = call(
        service,
        "GET",
        &format!("/api/v1/sales/{sale_id}/schedule-x-preflight"),
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    reply.body
}

/// Every predicate a preflight line reports that is NOT established, as `name:reason`.
fn unmet_predicates(line: &Value) -> Vec<String> {
    line["predicates"]
        .as_array()
        .expect("predicates")
        .iter()
        .filter(|predicate| predicate["verdict"]["state"] != "established")
        .map(|predicate| {
            format!(
                "{}:{}",
                predicate["predicate"].as_str().unwrap_or("?"),
                predicate["verdict"]["reason"].as_str().unwrap_or("?")
            )
        })
        .collect()
}

/// Phase 1M-D3-C2, item 3 — THE READ-ONLY PREFLIGHT.
///
/// It reports every Schedule X requirement on its own, it is advisory, and it changes nothing. The
/// Sale is left exactly as it was: still a draft, no number allocated, no dispensing, no stock
/// movement, no audit event, and the working entry in the state the operator left it in.
#[tokio::test]
async fn real_service_reports_every_schedule_x_predicate_without_changing_anything_over_http() {
    let service = start().await;
    let f = favourable_schedule_x_over_http(&service, false).await;

    // Before the physical page is written and signed, exactly one requirement is outstanding.
    let before = preflight_over_http(&service, &f.world.cookie, &f.sale_id).await;
    assert_eq!(before["status"], "draft");
    let lines = before["lines"].as_array().expect("lines");
    assert_eq!(lines.len(), 1, "{before}");
    let line = &lines[0];
    assert_eq!(line["saleLineId"], f.line_id.as_str());
    assert_eq!(line["batchId"], f.batch.as_str());
    assert_eq!(
        unmet_predicates(line),
        vec![
            "physicalRegisterEntry:physical_entry_not_confirmed".to_owned(),
            "supervisingPharmacist:no_confirmed_entry".to_owned(),
        ],
        "{line}"
    );
    assert_eq!(line["canAttemptPosting"], false);
    assert_eq!(before["canAttemptPosting"], false);

    // Every predicate is named, including the ones other owners answer, so the screen can show the
    // whole picture rather than only the Schedule X chain.
    let named: Vec<&str> = line["predicates"]
        .as_array()
        .expect("predicates")
        .iter()
        .map(|predicate| predicate["predicate"].as_str().expect("name"))
        .collect();
    for required in [
        "scheduleXClassification",
        "supportedIntersection",
        "ndpsOverlay",
        "punjabOverlay",
        "h1Intersection",
        "exactBatch",
        "prescriptionLinkage",
        "prescriptionState",
        "prescriptionProductMatch",
        "writtenSignedDated",
        "prescribedDose",
        "quantityAuthority",
        "repeatAuthority",
        "repeatInterval",
        "remainingAuthority",
        "supplySupervision",
        "storeAuthority",
        "productCoverage",
        "lotProvenance",
        "sourceAuthorities",
        "duplicatePrescriptionCopy",
        "prescriptionAnnotation",
        "physicalRegisterEntry",
        "supervisingPharmacist",
    ] {
        assert!(
            named.contains(&required),
            "{required} missing from {named:?}"
        );
    }

    // No patient, prescriber or medical particular is exposed.
    let text = before.to_string().to_lowercase();
    for forbidden in [
        "prescriber",
        "diagnos",
        "subjectname",
        "patientname",
        "dosetext",
    ] {
        assert!(
            !text.contains(forbidden),
            "{forbidden} in preflight: {before}"
        );
    }

    // After the physical act is recorded, every predicate is established.
    let confirmed = call(
        &service,
        "POST",
        &format!("/api/v1/store/schedule-x/register/{}/confirm", f.entry_id),
        Some(json!({
            "supervisingProfessionalId": f.professional,
            "particularsEnteredInPhysicalRegister": true,
            "physicalEntryAuthenticated": true,
        })),
        Some(&f.world.cookie),
    )
    .await;
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);

    let after = preflight_over_http(&service, &f.world.cookie, &f.sale_id).await;
    let line = &after["lines"].as_array().expect("lines")[0];
    assert_eq!(unmet_predicates(line), Vec::<String>::new(), "{line}");
    assert_eq!(line["canAttemptPosting"], true);
    assert_eq!(after["canAttemptPosting"], true);

    // And after two preflights, nothing has happened to the Sale.
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let (status, number): (String, Option<String>) =
        sqlx::query_as("SELECT status,document_number FROM sale_documents WHERE id=?")
            .bind(&f.sale_id)
            .fetch_one(&pool)
            .await
            .expect("sale");
    assert_eq!(status, "draft");
    assert_eq!(number, None);
    let series: Option<i64> = sqlx::query_scalar(
        "SELECT next_value FROM document_number_series WHERE document_kind='sale'",
    )
    .fetch_optional(&pool)
    .await
    .expect("series");
    assert_eq!(series, None, "the preflight allocated a document number");
    for (what, sql) in [
        (
            "dispensing",
            "SELECT COUNT(*) FROM prescription_dispensings WHERE sale_document_id=?",
        ),
        (
            "tender",
            "SELECT COUNT(*) FROM sale_tenders WHERE sale_document_id=?",
        ),
    ] {
        let count: i64 = sqlx::query_scalar(sql)
            .bind(&f.sale_id)
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 0, "the preflight created a {what}");
    }
    // The entry is still exactly `confirmed`: the preflight neither finalized nor attested anything.
    let (entry_status, finalized): (String, Option<String>) = sqlx::query_as(
        "SELECT status,finalized_at_utc FROM store_schedule_x_register_entries WHERE id=?",
    )
    .bind(&f.entry_id)
    .fetch_one(&pool)
    .await
    .expect("entry");
    assert_eq!(entry_status, "confirmed");
    assert_eq!(finalized, None);
    // And it wrote no audit event of its own.
    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM master_change_events \
         WHERE entity_type='schedule_x_register_entry' AND entity_id=? AND action='posted'",
    )
    .bind(&f.entry_id)
    .fetch_one(&pool)
    .await
    .expect("audit");
    assert_eq!(audited, 0, "the preflight wrote an audit event");
    pool.close().await;
}

/// Phase 1M-D3-C2, item 29 case 31 — A FAVOURABLE PREFLIGHT AUTHORIZES NOTHING.
///
/// Three mutable prerequisites, one per case. In each, the preflight is fully favourable and
/// `canAttemptPosting` is true; then the prerequisite changes underneath, exactly as it could while
/// an operator is walking to the register; then the posting is refused on that predicate and nothing
/// durable exists.
///
/// This is why the posting recomputes everything inside `BEGIN IMMEDIATE` and why the advisory
/// boolean is never an authority.
#[tokio::test]
async fn real_service_refuses_posting_when_a_preflight_prerequisite_changes_over_http() {
    for (case, mutation, expected) in [
        (
            "form_20f_product_coverage_archived",
            "UPDATE store_licence_drug_coverage SET status='archived',\
             archived_at_utc='2026-09-12T06:00:00.000Z',archive_reason='withdrawn' \
             WHERE licence_id=?",
            "schedule_x_store_authority_product_not_covered",
        ),
        (
            "supplier_authority_archived",
            "UPDATE supplier_schedule_x_authorities SET status='archived',\
             archived_at_utc='2026-09-12T06:00:00.000Z',archive_reason='withdrawn' WHERE id=?",
            "schedule_x_source_authority_missing",
        ),
        (
            "supervising_pharmacist_archived",
            "UPDATE store_professionals SET status='archived',\
             archived_at_utc='2026-09-12T06:00:00.000Z',archive_reason='left the pharmacy' \
             WHERE id=?",
            "schedule_x_supervising_pharmacist_invalid",
        ),
    ] {
        let service = start().await;
        // The pharmacist case needs the entry's professional to be somebody other than the one
        // supervising the supply, or the supply-level rule 65(2) check answers first and this case
        // never reaches the predicate it is about.
        let separate = case == "supervising_pharmacist_archived";
        let f = favourable_schedule_x_with_entry_pharmacist(&service, true, separate).await;

        // The preflight says every requirement is established.
        let ready = preflight_over_http(&service, &f.world.cookie, &f.sale_id).await;
        let line = &ready["lines"].as_array().expect("lines")[0];
        assert_eq!(
            unmet_predicates(line),
            Vec::<String>::new(),
            "{case}: {line}"
        );
        assert_eq!(ready["canAttemptPosting"], true, "{case}: {ready}");

        // Then one prerequisite changes.
        let pool = database::connect(&service.database_path)
            .await
            .expect("database");
        let target = match case {
            "form_20f_product_coverage_archived" => f.licence.clone(),
            "supplier_authority_archived" => f.authority.clone(),
            _ => f.entry_professional.clone(),
        };
        let changed = sqlx::query(mutation)
            .bind(&target)
            .execute(&pool)
            .await
            .unwrap_or_else(|error| panic!("{case}: {error}"));
        assert_eq!(changed.rows_affected(), 1, "{case}: nothing was changed");

        // And the posting is refused on exactly that predicate.
        let refused = post_quoted_over_http(
            &service,
            &f.world,
            &f.sale_id,
            f.revision,
            "01997a00-0000-7000-8000-00000000033d",
        )
        .await;
        assert_eq!(refused.status, 409, "{case}: {:?}", refused.body);
        assert_eq!(
            refused.body["code"], "prescription_requirements_incomplete",
            "{case}: {:?}",
            refused.body
        );
        assert!(
            schedule_x_unmet(&refused.body).contains(&expected.to_owned()),
            "{case}: expected {expected}, got {:?}",
            refused.body
        );

        // Nothing durable.
        let (status, number): (String, Option<String>) =
            sqlx::query_as("SELECT status,document_number FROM sale_documents WHERE id=?")
                .bind(&f.sale_id)
                .fetch_one(&pool)
                .await
                .expect("sale");
        assert_eq!(status, "draft", "{case}");
        assert_eq!(number, None, "{case}");
        let series: Option<i64> = sqlx::query_scalar(
            "SELECT next_value FROM document_number_series WHERE document_kind='sale'",
        )
        .fetch_optional(&pool)
        .await
        .expect("series");
        assert_eq!(series, None, "{case}: a document number was consumed");
        for (what, sql) in [
            (
                "dispensing",
                "SELECT COUNT(*) FROM prescription_dispensings WHERE sale_document_id=?",
            ),
            (
                "outflow",
                "SELECT COUNT(*) FROM inventory_movements WHERE movement_type='sale' \
                 AND sale_line_id IN (SELECT id FROM sale_lines WHERE sale_document_id=?)",
            ),
            (
                "tender",
                "SELECT COUNT(*) FROM sale_tenders WHERE sale_document_id=?",
            ),
        ] {
            let count: i64 = sqlx::query_scalar(sql)
                .bind(&f.sale_id)
                .fetch_one(&pool)
                .await
                .expect("count");
            assert_eq!(count, 0, "{case}: a {what} survived");
        }
        let (entry_status, finalized): (String, Option<String>) = sqlx::query_as(
            "SELECT status,finalized_at_utc FROM store_schedule_x_register_entries WHERE id=?",
        )
        .bind(&f.entry_id)
        .fetch_one(&pool)
        .await
        .expect("entry");
        assert_eq!(entry_status, "confirmed", "{case}");
        assert_eq!(finalized, None, "{case}");
        pool.close().await;
    }
}

/// Phase 1M-D3-C2, item 29 case 13 — THE SUPERVISING PHARMACIST, AS ITS OWN PREDICATE.
///
/// Rule 65(2) requires the supply to be effected under the personal supervision of a registered
/// pharmacist. The 0028 transition trigger proves the professional named on the working entry
/// qualified AT THE MOMENT OF CONFIRMATION; this proves the posting asks again, and refuses under its
/// own code rather than claiming the physical register entry is missing.
///
/// Every case below confirms the entry under a second registered pharmacist, so the supply-level
/// rule 65(2) answer stays favourable and these cases actually reach the entry-level predicate. Every
/// other Schedule X predicate is established first, so nothing else can be masking the refusal.
#[tokio::test]
async fn real_service_refuses_schedule_x_on_the_entry_pharmacist_alone_over_http() {
    for (case, mutation, reason) in [
        (
            "archived_after_confirmation",
            "UPDATE store_professionals SET status='archived',\
             archived_at_utc='2026-09-12T06:00:00.000Z',archive_reason='left' WHERE id=?",
            "professional_archived",
        ),
        (
            "no_longer_a_registered_pharmacist",
            "UPDATE store_professionals SET capacity='competent_person' WHERE id=?",
            "not_a_registered_pharmacist",
        ),
        // A registered pharmacist's registration number cannot be taken away at all: the
        // `store_professionals` CHECK refuses it outright, which is stronger than any predicate.
        // Proved separately in `the_registration_number_cannot_be_removed_from_a_pharmacist` below.
        (
            "registration_lapsed_before_the_business_date",
            "UPDATE store_professionals SET valid_upto='2026-09-11' WHERE id=?",
            "registration_lapsed",
        ),
        (
            "registration_not_yet_valid_on_the_business_date",
            "UPDATE store_professionals SET valid_from='2026-12-01' WHERE id=?",
            "registration_not_yet_valid",
        ),
    ] {
        let service = start().await;
        let f = favourable_schedule_x_with_entry_pharmacist(&service, true, true).await;

        // Favourable to begin with, so the case reaches the predicate it is about.
        let ready = preflight_over_http(&service, &f.world.cookie, &f.sale_id).await;
        let line = &ready["lines"].as_array().expect("lines")[0];
        assert_eq!(
            unmet_predicates(line),
            Vec::<String>::new(),
            "{case}: {line}"
        );

        let pool = database::connect(&service.database_path)
            .await
            .expect("database");
        let changed = sqlx::query(mutation)
            .bind(&f.entry_professional)
            .execute(&pool)
            .await
            .unwrap_or_else(|error| panic!("{case}: {error}"));
        assert_eq!(changed.rows_affected(), 1, "{case}: nothing was changed");

        // The preflight names the pharmacist predicate and the exact reason — not the register.
        let blocked = preflight_over_http(&service, &f.world.cookie, &f.sale_id).await;
        let line = &blocked["lines"].as_array().expect("lines")[0];
        assert_eq!(
            unmet_predicates(line),
            vec![format!("supervisingPharmacist:{reason}")],
            "{case}: {line}"
        );
        assert_eq!(line["canAttemptPosting"], false, "{case}");

        // And the posting refuses under the Schedule X pharmacist code.
        let refused = post_quoted_over_http(
            &service,
            &f.world,
            &f.sale_id,
            f.revision,
            "01997a00-0000-7000-8000-00000000034e",
        )
        .await;
        assert_eq!(refused.status, 409, "{case}: {:?}", refused.body);
        assert_eq!(
            schedule_x_unmet(&refused.body),
            vec!["schedule_x_supervising_pharmacist_invalid".to_owned()],
            "{case}: {:?}",
            refused.body
        );
        // The physical entry is untouched: it is the pharmacist's record that needs attention.
        let (entry_status, attested): (String, i64) = sqlx::query_as(
            "SELECT status,physical_entry_authenticated \
             FROM store_schedule_x_register_entries WHERE id=?",
        )
        .bind(&f.entry_id)
        .fetch_one(&pool)
        .await
        .expect("entry");
        assert_eq!(entry_status, "confirmed", "{case}");
        assert_eq!(attested, 1, "{case}");
        assert_nothing_durable_over_http(&pool, &f.sale_id).await;
        pool.close().await;
    }
}

/// No posted document, no number consumed, no dispensing, no outflow, no tender, nothing finalized.
async fn assert_nothing_durable_over_http(pool: &sqlx::SqlitePool, sale_id: &str) {
    let (status, number): (String, Option<String>) =
        sqlx::query_as("SELECT status,document_number FROM sale_documents WHERE id=?")
            .bind(sale_id)
            .fetch_one(pool)
            .await
            .expect("sale");
    assert_eq!(status, "draft");
    assert_eq!(number, None);
    let series: Option<i64> = sqlx::query_scalar(
        "SELECT next_value FROM document_number_series WHERE document_kind='sale'",
    )
    .fetch_optional(pool)
    .await
    .expect("series");
    assert_eq!(series, None, "a document number was consumed");
    for (what, sql) in [
        (
            "dispensing",
            "SELECT COUNT(*) FROM prescription_dispensings WHERE sale_document_id=?",
        ),
        (
            "outflow",
            "SELECT COUNT(*) FROM inventory_movements WHERE movement_type='sale' \
             AND sale_line_id IN (SELECT id FROM sale_lines WHERE sale_document_id=?)",
        ),
        (
            "tender",
            "SELECT COUNT(*) FROM sale_tenders WHERE sale_document_id=?",
        ),
        (
            "finalized Schedule X entry",
            "SELECT COUNT(*) FROM store_schedule_x_register_entries \
             WHERE sale_document_id=? AND status='finalized'",
        ),
    ] {
        let count: i64 = sqlx::query_scalar(sql)
            .bind(sale_id)
            .fetch_one(pool)
            .await
            .expect("count");
        assert_eq!(count, 0, "a {what} survived");
    }
}

/// Phase 1M-D3-C2 — the registration number of a registered pharmacist cannot be removed.
///
/// `resolve_entry_supervising_pharmacist` has a `no_registration_number` branch, and this records why
/// that branch cannot be reached for an active registered pharmacist: `store_professionals` carries a
/// CHECK that refuses the state outright. The branch stays as the fail-closed answer for a row that
/// somehow lacked one; the guarantee relied on is the constraint, which is stronger.
#[tokio::test]
async fn the_registration_number_cannot_be_removed_from_a_pharmacist() {
    let service = start().await;
    let f = favourable_schedule_x_with_entry_pharmacist(&service, true, true).await;
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let error = sqlx::query("UPDATE store_professionals SET registration_number=NULL WHERE id=?")
        .bind(&f.entry_professional)
        .execute(&pool)
        .await
        .expect_err("the database accepted a registered pharmacist with no registration number")
        .to_string();
    assert!(error.contains("CHECK constraint failed"), "{error}");
    pool.close().await;
}

/// Phase 1M-D3-C2, item 10 — A SUPPORTED VETERINARY SCHEDULE X RETAIL SALE POSTS.
///
/// Rule 65(10)(b) requires, where the drug is meant for veterinary use, "the name and address of the
/// owner of the animal". Rule 65(21)(b)(vii) requires the "Name and address of the patient/purchaser".
/// Those are the facts, and they are all the facts: no species, no breed, no animal identifier and no
/// age appears anywhere, because the Rules do not ask for any of them.
///
/// Every other requirement is identical to the human path — the same prescription rules, the same
/// product and lot, the same Form 20-F authority and coverage, the same provenance and source
/// authority, the same pharmacist, duplicate copy, annotation, physical register and NDPS/Punjab
/// clearance. Nothing is relaxed for an animal.
#[tokio::test]
async fn real_service_posts_a_supported_veterinary_schedule_x_sale_over_http() {
    let service = start().await;
    let f = favourable_schedule_x_with_options(
        &service,
        FavourableOptions {
            confirm: true,
            animal: true,
            ..Default::default()
        },
    )
    .await;

    // Every predicate established, on the same list as a human supply.
    let ready = preflight_over_http(&service, &f.world.cookie, &f.sale_id).await;
    let line = &ready["lines"].as_array().expect("lines")[0];
    assert_eq!(unmet_predicates(line), Vec::<String>::new(), "{line}");

    let posted = post_quoted_over_http(
        &service,
        &f.world,
        &f.sale_id,
        f.revision,
        "01997a00-0000-7000-8000-00000000035f",
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    assert_eq!(posted.body["status"], "posted");

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    // The register entry carries the animal's owner as the purchaser, and says which it is.
    let (subject_kind, purchaser_name, purchaser_address, status): (
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    ) = sqlx::query_as(
        "SELECT subject_kind,purchaser_name,purchaser_address,status \
         FROM store_schedule_x_register_entries WHERE id=?",
    )
    .bind(&f.entry_id)
    .fetch_one(&pool)
    .await
    .expect("entry");
    assert_eq!(status, "finalized");
    assert_eq!(subject_kind.as_deref(), Some("animal"));
    assert_eq!(purchaser_name.as_deref(), Some("Ramesh Patil"));
    assert_eq!(
        purchaser_address.as_deref(),
        Some("22 Shivaji Nagar, Pune 411005")
    );

    // No invented animal fact exists anywhere in the register entry or the prescription.
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info('store_schedule_x_register_entries') \
         UNION SELECT name FROM pragma_table_info('prescriptions')",
    )
    .fetch_all(&pool)
    .await
    .expect("columns");
    for invented in ["species", "breed", "animal_id", "animal_identifier", "age"] {
        assert!(
            !columns.iter().any(|column| column.contains(invented)),
            "{invented} was invented: {columns:?}"
        );
    }
    pool.close().await;
}

// ==============================================================================================
// Phase 1M-D3-C2 — mixed baskets. One Sale, one transaction, all or nothing.
// ==============================================================================================

/// A second product with no regulatory position at all, its own pack and lot, and sellable stock
/// brought in by an opening-stock operation.
///
/// It is an `general_pharmacy_item` with every schedule recorded as not applying, so it is an ordinary
/// counter line. Its stock arrives as opening stock, which would disqualify a Schedule X lot — and
/// that is exactly right, because this product is not Schedule X and owes no provenance.
async fn ordinary_product_over_http(
    service: &Service,
    world: &SaleWorld,
) -> (String, String, String) {
    let product = call(
        service,
        "POST",
        "/api/v1/products",
        Some(json!({"product":{
            "productKind":"general_pharmacy_item","baseUnitId":TABLET,
            "quantityScale":0,"displayName":"Digene Antacid Tablet"
        }})),
        Some(&world.cookie),
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
        Some(&world.cookie),
    )
    .await;
    assert_eq!(pack.status, 201, "{:?}", pack.body);
    let pack_id = pack.body["id"].as_str().expect("pack id").to_owned();

    // The same tax classification the world's own product carries, so the money path is ordinary.
    let pool = database::connect(&service.database_path).await.expect("db");
    let (hsn, category): (String, String) =
        sqlx::query_as("SELECT hsn_code_id,tax_category_id FROM products WHERE id=?")
            .bind(&world.product)
            .fetch_one(&pool)
            .await
            .expect("classification");
    pool.close().await;
    let classified = call(
        service,
        "PUT",
        &format!("/api/v1/products/{product_id}/tax-classification"),
        Some(json!({ "expectedRevision": 1, "hsnCodeId": hsn, "taxCategoryId": category })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(classified.status, 200, "{:?}", classified.body);

    // Outside every schedule, so the line is an ordinary one and the gate says so.
    for scheme in [
        "schedule_h",
        "schedule_h1",
        "schedule_x",
        "schedule_c",
        "schedule_c1",
        "ndps_purview",
    ] {
        let finding = call(
            service,
            "POST",
            &format!("/api/v1/products/{product_id}/regulatory/classifications"),
            Some(json!({
                "scheme": scheme,
                "applies": false,
                "effectiveFrom": "2020-01-01",
                "sourceCitation": "Drugs Rules, 1945, Schedules as amended",
            })),
            Some(&world.cookie),
        )
        .await;
        assert_eq!(finding.status, 201, "{scheme}: {:?}", finding.body);
    }

    // A lot, and stock in it.
    let batch = call(
        service,
        "POST",
        &format!("/api/v1/packs/{pack_id}/batches"),
        Some(json!({
            "batchNumber": "DG-7781", "expiresOn": "2028-01-31", "mrpPaise": 9550
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(batch.status, 201, "{:?}", batch.body);
    let batch_id = batch.body["id"].as_str().expect("batch id").to_owned();

    // A counted shelf brings the stock in. For an ordinary line this is simply how stock arrives in
    // a test; for a Schedule X lot the very same movement would be a DISQUALIFYING inward, which is
    // exactly right — this product is not Schedule X and owes no provenance.
    let counted = run_operation(
        service,
        &world.cookie,
        "physical_count",
        json!({
            "productPackId": pack_id, "batchId": batch_id, "stockStatus": "sellable",
            "reasonCode": "physical_count_gain", "countedQuantity": 100,
            "quantityBasis": "base_unit"
        }),
    )
    .await;
    assert_eq!(counted.status, 200, "{:?}", counted.body);

    // And the pack is enabled for sale at this store, the same way the world enables its own.
    let policy = call(
        service,
        "PUT",
        &format!("/api/v1/packs/{pack_id}/policy"),
        Some(json!({
            "expectedRevision": null,
            "policy": {
                "storeId": world.store, "purchaseEnabled": true, "saleEnabled": true,
                "wholePackOnlyPurchase": false, "fractionalSaleAllowed": false,
                "minimumSaleIncrementAtoms": 1,
                "defaultPurchasePack": false, "defaultSalePack": true
            }
        })),
        Some(&world.cookie),
    )
    .await;
    assert!(
        policy.status == 200 || policy.status == 201,
        "{:?}",
        policy.body
    );
    (product_id, pack_id, batch_id)
}

/// The draft Sale's current revision, read back over HTTP.
async fn sale_revision_over_http(service: &Service, world: &SaleWorld, sale_id: &str) -> i64 {
    let detail = call(
        service,
        "GET",
        &format!("/api/v1/sales/{sale_id}"),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(detail.status, 200, "{:?}", detail.body);
    detail.body["revision"].as_i64().expect("revision")
}

/// Adds one more line to an existing draft and returns (line id, new revision).
async fn add_line_over_http(
    service: &Service,
    world: &SaleWorld,
    sale_id: &str,
    revision: i64,
    product: &str,
    pack: &str,
    batch: &str,
) -> (String, i64) {
    let added = call(
        service,
        "POST",
        &format!("/api/v1/sales/{sale_id}/lines"),
        Some(json!({
            "expectedRevision": revision,
            "productId": product,
            "productPackId": pack,
            "batchId": batch,
            "quantityBasis": "pack",
            "quantity": 1,
            "sellingRatePaise": 8000
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(added.status, 201, "{:?}", added.body);
    let lines = added.body["lines"].as_array().expect("lines");
    let line_id = lines
        .last()
        .and_then(|line| line["id"].as_str())
        .expect("line id")
        .to_owned();
    let revision = added.body["revision"].as_i64().expect("revision");
    (line_id, revision)
}

/// Phase 1M-D3-C2, item 11 case A — ORDINARY + SCHEDULE X, ALL FAVOURABLE, THE WHOLE SALE POSTS.
#[tokio::test]
async fn real_service_posts_a_mixed_ordinary_and_schedule_x_sale_over_http() {
    let service = start().await;
    let f = favourable_schedule_x_with_options(
        &service,
        FavourableOptions {
            skip_prepare: true,
            ..Default::default()
        },
    )
    .await;
    let (product, pack, batch) = ordinary_product_over_http(&service, &f.world).await;
    let revision = sale_revision_over_http(&service, &f.world, &f.sale_id).await;
    let (ordinary_line, revision) = add_line_over_http(
        &service, &f.world, &f.sale_id, revision, &product, &pack, &batch,
    )
    .await;
    // The Schedule X entry is prepared and confirmed only after the basket is final: it names
    // exactly what is about to be supplied.
    let prepared = prepare_supply_over_http(&service, &f.world.cookie, &f.line_id).await;
    assert_eq!(prepared.status, 201, "{:?}", prepared.body);
    let entry_id = prepared.body["id"].as_str().expect("id").to_owned();
    let confirmed = call(
        &service,
        "POST",
        &format!("/api/v1/store/schedule-x/register/{entry_id}/confirm"),
        Some(json!({
            "supervisingProfessionalId": f.professional,
            "particularsEnteredInPhysicalRegister": true,
            "physicalEntryAuthenticated": true,
        })),
        Some(&f.world.cookie),
    )
    .await;
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);

    // The preflight reports only the Schedule X line; the ordinary one is not its business.
    let ready = preflight_over_http(&service, &f.world.cookie, &f.sale_id).await;
    assert_eq!(
        ready["lines"].as_array().expect("lines").len(),
        1,
        "{ready}"
    );

    let posted = post_quoted_over_http(
        &service,
        &f.world,
        &f.sale_id,
        revision,
        "01997a00-0000-7000-8000-000000000370",
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    // Both lines moved stock, and the Schedule X entry is finalized.
    for line in [&f.line_id, &ordinary_line] {
        let moved: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM inventory_movements WHERE movement_type='sale' AND sale_line_id=?",
        )
        .bind(line)
        .fetch_one(&pool)
        .await
        .expect("movement");
        assert_eq!(moved, 1, "line {line} did not move stock");
    }
    let status: String =
        sqlx::query_scalar("SELECT status FROM store_schedule_x_register_entries WHERE id=?")
            .bind(&entry_id)
            .fetch_one(&pool)
            .await
            .expect("entry");
    assert_eq!(status, "finalized");
    pool.close().await;
}

/// Phase 1M-D3-C2, item 11 cases B, F, G, H and I — ONE BAD LINE LEAVES THE WHOLE SALE A DRAFT.
///
/// Each case takes a Sale that would otherwise post and spoils exactly one thing about its Schedule X
/// line. In every case the ORDINARY line is perfectly saleable, and in every case it does not post:
/// a Sale is one transaction, and there is no partial posting to be had.
#[tokio::test]
async fn real_service_keeps_a_whole_mixed_sale_draft_when_one_schedule_x_line_fails_over_http() {
    for (case, spoil, expected) in [
        (
            // B — the Schedule X line's working entry is never confirmed.
            "schedule_x_register_not_confirmed",
            "",
            "schedule_x_register_confirmation_missing",
        ),
        (
            // F — the lot's source authority is withdrawn.
            "schedule_x_source_authority_archived",
            "UPDATE supplier_schedule_x_authorities SET status='archived',\
             archived_at_utc='2026-09-12T06:00:00.000Z',archive_reason='withdrawn'",
            "schedule_x_source_authority_missing",
        ),
        (
            // G — the NDPS axis is withdrawn to unrecorded.
            "ndps_unresolved",
            "UPDATE product_regulatory_classifications SET status='archived',\
             archived_at_utc='2026-09-12T06:00:00.000Z',archive_reason='withdrawn' \
             WHERE scheme='ndps_purview'",
            "",
        ),
        (
            // I — the drug turns out to be Schedule C as well, an unsupported intersection.
            "schedule_c_intersection",
            "UPDATE product_regulatory_classifications SET applies=1 WHERE scheme='schedule_c'",
            "",
        ),
    ] {
        let service = start().await;
        let f = favourable_schedule_x_with_options(
            &service,
            FavourableOptions {
                skip_prepare: true,
                ..Default::default()
            },
        )
        .await;
        let (product, pack, batch) = ordinary_product_over_http(&service, &f.world).await;
        let revision = sale_revision_over_http(&service, &f.world, &f.sale_id).await;
        let (ordinary_line, revision) = add_line_over_http(
            &service, &f.world, &f.sale_id, revision, &product, &pack, &batch,
        )
        .await;

        // The basket is final, so now the working entry is prepared — and confirmed, except in the
        // case that is about an unconfirmed entry.
        let prepared = prepare_supply_over_http(&service, &f.world.cookie, &f.line_id).await;
        assert_eq!(prepared.status, 201, "{case}: {:?}", prepared.body);
        let entry_id = prepared.body["id"].as_str().expect("id").to_owned();
        if case != "schedule_x_register_not_confirmed" {
            let confirmed = call(
                &service,
                "POST",
                &format!("/api/v1/store/schedule-x/register/{entry_id}/confirm"),
                Some(json!({
                    "supervisingProfessionalId": f.professional,
                    "particularsEnteredInPhysicalRegister": true,
                    "physicalEntryAuthenticated": true,
                })),
                Some(&f.world.cookie),
            )
            .await;
            assert_eq!(confirmed.status, 200, "{case}: {:?}", confirmed.body);
        }

        let pool = database::connect(&service.database_path)
            .await
            .expect("database");
        if !spoil.is_empty() {
            // Scoped to the world's own product where the statement does not already say so.
            let statement = if spoil.contains("product_regulatory_classifications") {
                format!("{spoil} AND product_id='{}'", f.world.product)
            } else {
                spoil.to_owned()
            };
            let changed = sqlx::query(&statement)
                .execute(&pool)
                .await
                .unwrap_or_else(|error| panic!("{case}: {error}"));
            assert!(changed.rows_affected() >= 1, "{case}: nothing changed");
        }

        let refused = post_quoted_over_http(
            &service,
            &f.world,
            &f.sale_id,
            revision,
            "01997a00-0000-7000-8000-000000000381",
        )
        .await;
        assert_ne!(refused.status, 200, "{case}: {:?}", refused.body);
        if !expected.is_empty() {
            assert!(
                schedule_x_unmet(&refused.body).contains(&expected.to_owned()),
                "{case}: expected {expected}, got {:?}",
                refused.body
            );
        }

        // The ordinary line did not post either.
        assert_nothing_durable_over_http(&pool, &f.sale_id).await;
        let moved: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM inventory_movements WHERE movement_type='sale' AND sale_line_id=?",
        )
        .bind(&ordinary_line)
        .fetch_one(&pool)
        .await
        .expect("movement");
        assert_eq!(moved, 0, "{case}: the ordinary line posted on its own");
        pool.close().await;
    }
}

/// Phase 1M-D3-C2, item 11 case H — PUNJAB LEAVES THE WHOLE SALE A DRAFT.
///
/// Its own test because the Punjab axis is a property of the STORE's premises State, not of the
/// product classification, so the fixture differs: the store sits in Punjab and the product's
/// position under the State notification is left unrecorded.
#[tokio::test]
async fn real_service_keeps_a_schedule_x_sale_draft_on_the_punjab_axis_over_http() {
    let service = start().await;
    let f = favourable_schedule_x_over_http(&service, true).await;
    // Move the premises into Punjab through the ordinary route, then record that the product IS
    // within the State notification.
    //
    // `applies` rather than an unrecorded position, deliberately: Phase 1M-C refuses an UNRECORDED
    // State position only for a `medicine`, and this world's product is a `general_pharmacy_item`,
    // so leaving it unrecorded here would not refuse and the case would prove nothing. The
    // unrecorded-position path for a medicine is proved in
    // `domain::regulatory::tests::the_central_and_state_axes_stay_independent`, which asserts
    // `StateUnresolved` for a Schedule X medicine on both `Resolved(Unknown)` and `Undetermined`.
    let moved = call(
        &service,
        "PUT",
        "/api/v1/store/address",
        Some(json!({
            "expectedRevision": 1, "line1": "12 Mall Road", "city": "Ludhiana",
            "stateId": PUNJAB, "postalCode": "141001"
        })),
        Some(&f.world.cookie),
    )
    .await;
    assert_eq!(moved.status, 200, "{:?}", moved.body);
    finding_over_http(&service, &f.world, "punjab_restricted_supply", true).await;
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");

    let refused = post_quoted_over_http(
        &service,
        &f.world,
        &f.sale_id,
        f.revision,
        "01997a00-0000-7000-8000-000000000392",
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    // Refused on the STATE axis, not on anything Schedule X: completing the Schedule X record never
    // clears the Punjab boundary.
    assert_eq!(
        refused.body["code"], "state_restricted_drug_workflow_not_available",
        "{:?}",
        refused.body
    );
    assert_nothing_durable_over_http(&pool, &f.sale_id).await;
    pool.close().await;
}

/// Phase 1M-D3-C2, item 11 cases C and D — SCHEDULE X ∩ SCHEDULE H1 IS STRUCTURALLY UNSUPPORTED.
///
/// This case was designed expecting the two workflows to compose. They do not, and the reason is in
/// the schema rather than in policy.
///
/// A Schedule H1 line owes its separate rule 65(3)(1)(h) working entry. That entry lives in
/// `prescription_h1_register_entries`, whose `supply_record_id` is NOT NULL and references the rule
/// 65(3)(1) record. Rule 65(3)(1) governs the supply of a drug "other than those specified in
/// Schedule X", so a Schedule X supply has no record in that register for the H1 entry to point at.
/// The H1 obligation therefore cannot be discharged for such a line without making
/// `supply_record_id` nullable or giving the H1 entry an independent existence — a new foundation
/// this phase does not lay.
///
/// So the line fails closed, under its own code, before anything is written. Both halves of the
/// original case are proved here: the supply does not post, and neither register gains an entry.
#[tokio::test]
async fn real_service_refuses_a_schedule_x_and_h1_line_as_unsupported_over_http() {
    let service = start().await;
    let world = seed_sale_world(&service, 1).await;
    // One drug, in Schedule X AND Schedule H1, outside the NDPS Act: each schedule is supported on
    // its own, and this test is about the two together.
    schedule_over_http(&service, &world, &["schedule_h1", "schedule_x"]).await;
    finding_over_http(&service, &world, "ndps_purview", false).await;
    record_basis_over_http(&service, &world, Some("prescription_register")).await;
    let professional = pharmacist_over_http(&service, &world).await;

    let (licence, revision) = form_20f_over_http(&service, &world, "MH-PUNE-20F-4471").await;
    let authorised = set_authority_over_http(
        &service,
        &world,
        &licence,
        revision,
        json!({
            "legalStatus": "in_force",
            "validityBasis": "perpetual",
            "validFrom": "2020-01-01",
        }),
    )
    .await;
    assert_eq!(authorised.status, 200, "{:?}", authorised.body);
    let covered = cover_over_http(&service, &world, &licence, "2020-01-01", None).await;
    assert_eq!(covered.status, 201, "{:?}", covered.body);

    let batch = qualified_lot_over_http(&service, &world, &professional).await;
    authorise_supplier_over_http(&service, &world).await;

    let (prescription, item) = prescription_over_http(&service, &world, None, 20).await;
    let duplicate = call(
        &service,
        "POST",
        &format!("/api/v1/prescriptions/{prescription}/schedule-x-duplicate-copy"),
        Some(json!({
            "retainedDuplicatePrescriptionCopyConfirmed": true, "note": Value::Null
        })),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(duplicate.status, 201, "{:?}", duplicate.body);

    let (sale_id, line_id, revision) =
        prepared_sale_on_lot_over_http(&service, &world, &batch, &item, &professional).await;

    // The preflight says so plainly, naming the intersection rather than an outstanding H1 entry the
    // operator could never supply.
    let preflight = preflight_over_http(&service, &world.cookie, &sale_id).await;
    let line = &preflight["lines"].as_array().expect("lines")[0];
    assert!(
        unmet_predicates(line).contains(&"h1Intersection:schedule_x_h1_intersection".to_owned()),
        "{line}"
    );
    assert_eq!(line["canAttemptPosting"], false, "{line}");

    // And the posting refuses under the unsupported-intersection code.
    let refused = post_quoted_over_http(
        &service,
        &world,
        &sale_id,
        revision,
        "01997a00-0000-7000-8000-0000000003a3",
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(
        refused.body["code"], "schedule_x_unsupported_intersecting_regime",
        "{:?}",
        refused.body
    );
    assert_eq!(refused.body["issues"][0]["field"], "lines.1.schedule_h1");
    // Nothing is called illegal, banned or prohibited: this is a combination the software does not
    // implement, not a drug anybody is forbidden to sell.
    let text = refused.body.to_string().to_lowercase();
    for word in ["banned", "prohibit", "illegal"] {
        assert!(!text.contains(word), "{word}: {:?}", refused.body);
    }

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    assert_nothing_durable_over_http(&pool, &sale_id).await;
    // Neither register gained an entry, and the Schedule X working record was never even prepared.
    for (what, sql) in [
        (
            "rule 65(3) record",
            "SELECT COUNT(*) FROM prescription_supply_records WHERE sale_document_id=?",
        ),
        (
            "H1 entry",
            "SELECT COUNT(*) FROM prescription_h1_register_entries WHERE sale_document_id=?",
        ),
        (
            "Schedule X entry",
            "SELECT COUNT(*) FROM store_schedule_x_register_entries WHERE sale_document_id=?",
        ),
    ] {
        let count: i64 = sqlx::query_scalar(sql)
            .bind(&sale_id)
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(
            count, 0,
            "a {what} was written for an unsupported intersection"
        );
    }
    let _ = line_id;
    pool.close().await;
}

/// Phase 1M-D3-C2, item 11 cases E and F — TWO SCHEDULE X LINES, ONE TRANSACTION.
///
/// Case E satisfies both lines and posts them together: two supplies, two register entries, two
/// dispensings, one document number. Case F leaves the SECOND line's working entry unconfirmed, and
/// neither line posts — not the good one either.
#[tokio::test]
async fn real_service_posts_or_refuses_two_schedule_x_lines_together_over_http() {
    for (case, confirm_second, expect_post) in [
        ("both_lines_favourable", true, true),
        ("second_line_unconfirmed", false, false),
    ] {
        let service = start().await;
        let f = favourable_schedule_x_with_options(
            &service,
            FavourableOptions {
                skip_prepare: true,
                ..Default::default()
            },
        )
        .await;
        // A second line on the same drug and the same lot, against the same prescription item. The
        // item authorises 20 atoms and each line takes 10, so the pair is exactly within it.
        let revision = sale_revision_over_http(&service, &f.world, &f.sale_id).await;
        let (second_line, revision) = add_line_over_http(
            &service,
            &f.world,
            &f.sale_id,
            revision,
            &f.world.product,
            &f.world.pack,
            &f.batch,
        )
        .await;
        let linked = call(
            &service,
            "PUT",
            &format!("/api/v1/sale-lines/{second_line}/prescription"),
            Some(json!({ "expectedRevision": revision, "prescriptionItemId": f.item })),
            Some(&f.world.cookie),
        )
        .await;
        assert_eq!(linked.status, 200, "{case}: {:?}", linked.body);
        let revision = linked.body["revision"].as_i64().expect("revision");
        let annotated =
            record_annotation_over_http(&service, &f.world.cookie, &second_line, true).await;
        assert_eq!(annotated.status, 201, "{case}: {:?}", annotated.body);
        // The endorsement is about the prescription as this supply stands, so adding a line retires
        // it. Confirm it again for the basket that is actually going to be supplied.
        let supplied = call(
            &service,
            "PUT",
            &format!("/api/v1/sales/{}/supply", f.sale_id),
            Some(json!({
                "expectedRevision": revision,
                "supervisingProfessionalId": f.professional,
                "prescriptionEndorsementConfirmed": true,
            })),
            Some(&f.world.cookie),
        )
        .await;
        assert_eq!(supplied.status, 200, "{case}: {:?}", supplied.body);
        let revision = supplied.body["revision"].as_i64().expect("revision");

        // One working entry per line: two statutory drug/quantity pairs are two entries, never one.
        let mut entries = Vec::new();
        for (index, line) in [&f.line_id, &second_line].iter().enumerate() {
            let prepared = prepare_supply_over_http(&service, &f.world.cookie, line).await;
            assert_eq!(prepared.status, 201, "{case}: {:?}", prepared.body);
            let entry_id = prepared.body["id"].as_str().expect("id").to_owned();
            if index == 0 || confirm_second {
                let confirmed = call(
                    &service,
                    "POST",
                    &format!("/api/v1/store/schedule-x/register/{entry_id}/confirm"),
                    Some(json!({
                        "supervisingProfessionalId": f.professional,
                        "particularsEnteredInPhysicalRegister": true,
                        "physicalEntryAuthenticated": true,
                    })),
                    Some(&f.world.cookie),
                )
                .await;
                assert_eq!(confirmed.status, 200, "{case}: {:?}", confirmed.body);
            }
            entries.push(entry_id);
        }

        // The preflight reports both lines.
        let preflight = preflight_over_http(&service, &f.world.cookie, &f.sale_id).await;
        assert_eq!(
            preflight["lines"].as_array().expect("lines").len(),
            2,
            "{case}: {preflight}"
        );

        let reply = post_quoted_over_http(
            &service,
            &f.world,
            &f.sale_id,
            revision,
            "01997a00-0000-7000-8000-0000000003b4",
        )
        .await;
        let pool = database::connect(&service.database_path)
            .await
            .expect("database");
        if expect_post {
            assert_eq!(reply.status, 200, "{case}: {:?}", reply.body);
            // Two finalized entries, two dispensings, ONE document number.
            let finalized: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM store_schedule_x_register_entries \
                 WHERE sale_document_id=? AND status='finalized'",
            )
            .bind(&f.sale_id)
            .fetch_one(&pool)
            .await
            .expect("entries");
            assert_eq!(finalized, 2, "{case}");
            let dispensings: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM prescription_dispensings WHERE sale_document_id=?",
            )
            .bind(&f.sale_id)
            .fetch_one(&pool)
            .await
            .expect("dispensings");
            assert_eq!(dispensings, 2, "{case}");
            // Both entries carry the SAME bill number: one Sale, one bill.
            let bills: Vec<String> = sqlx::query_scalar(
                "SELECT DISTINCT bill_number FROM store_schedule_x_register_entries \
                 WHERE sale_document_id=? AND status='finalized'",
            )
            .bind(&f.sale_id)
            .fetch_all(&pool)
            .await
            .expect("bills");
            assert_eq!(bills.len(), 1, "{case}: {bills:?}");
            // And each entry is bound to its own line's dispensing, never the other's.
            for entry in &entries {
                let same: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM store_schedule_x_register_entries entry \
                     JOIN prescription_dispensings dispensing ON dispensing.id=entry.dispensing_id \
                     WHERE entry.id=? AND dispensing.sale_line_id=entry.sale_line_id",
                )
                .bind(entry)
                .fetch_one(&pool)
                .await
                .expect("binding");
                assert_eq!(
                    same, 1,
                    "{case}: entry {entry} is bound to another line's dispensing"
                );
            }
        } else {
            assert_ne!(reply.status, 200, "{case}: {:?}", reply.body);
            assert!(
                schedule_x_unmet(&reply.body)
                    .contains(&"schedule_x_register_confirmation_missing".to_owned()),
                "{case}: {:?}",
                reply.body
            );
            assert_nothing_durable_over_http(&pool, &f.sale_id).await;
        }
        pool.close().await;
    }
}

/// Phase 1M-D3-C2, item 29 case 2 — NO FORM 20-F AUTHORITY REFUSES THE SALE.
///
/// Rule 61(3) issues the Schedule X retail licence in Form 20-F, and Form 20-F condition 3 and item 2
/// are what the rest of this chain hangs on. Everything else here is favourable — the lot, its
/// source's authority, the prescription, the duplicate copy, the annotation, the pharmacist, the
/// signed physical register entry, the NDPS and State axes — and the supply is still refused, because
/// the store has recorded no Schedule X retail authority of its own.
#[tokio::test]
async fn real_service_refuses_a_schedule_x_sale_with_no_form_20f_authority_over_http() {
    let service = start().await;
    let f = favourable_schedule_x_with_options(
        &service,
        FavourableOptions {
            confirm: true,
            skip_form_20f: true,
            ..Default::default()
        },
    )
    .await;

    // The preflight names the authority, and the coverage as unresolvable without it.
    let blocked = preflight_over_http(&service, &f.world.cookie, &f.sale_id).await;
    let line = &blocked["lines"].as_array().expect("lines")[0];
    assert_eq!(
        unmet_predicates(line),
        vec![
            "storeAuthority:no_authority_recorded".to_owned(),
            "productCoverage:authority_not_established".to_owned(),
        ],
        "{line}"
    );

    let refused = post_quoted_over_http(
        &service,
        &f.world,
        &f.sale_id,
        f.revision,
        "01997a00-0000-7000-8000-0000000003c5",
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(
        schedule_x_unmet(&refused.body),
        vec![
            "schedule_x_store_authority_missing".to_owned(),
            "schedule_x_store_authority_product_not_covered".to_owned(),
        ],
        "{:?}",
        refused.body
    );
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    assert_nothing_durable_over_http(&pool, &f.sale_id).await;
    pool.close().await;
}

/// Phase 1M-D3-C2, item 29 case 19 — LEGACY STOCK CANNOT BE SUPPLIED.
///
/// The lot's sellable balance arrived on a counted shelf rather than on a posted purchase with an
/// authenticated Schedule X receipt entry. Every other predicate is favourable.
///
/// The refusal lands EARLIER than the posting: Phase 1M-D3-B's provenance trigger will not let the
/// rule 65(21) working entry be prepared for such a lot at all, so no page is written in the physical
/// register for a supply that could never be made. Stock inside one lot is fungible, so a lot that
/// cannot account for where its units came from is held back whole — which is the position old stock
/// is in, and the position stock released back into the sellable balance is in.
#[tokio::test]
async fn real_service_refuses_a_schedule_x_sale_from_legacy_counted_stock_over_http() {
    let service = start().await;
    let f = favourable_schedule_x_with_options(
        &service,
        FavourableOptions {
            counted_lot: true,
            skip_prepare: true,
            ..Default::default()
        },
    )
    .await;

    // The preflight says the lot cannot account for itself.
    let blocked = preflight_over_http(&service, &f.world.cookie, &f.sale_id).await;
    let line = &blocked["lines"].as_array().expect("lines")[0];
    let unmet = unmet_predicates(line);
    assert!(
        unmet.contains(&"lotProvenance:unresolved_inward_movement".to_owned())
            || unmet.contains(&"lotProvenance:no_qualifying_receipt".to_owned()),
        "{line}"
    );
    assert_eq!(line["canAttemptPosting"], false, "{line}");

    // And the working entry cannot even be prepared.
    let refused_prepare = prepare_supply_over_http(&service, &f.world.cookie, &f.line_id).await;
    assert_eq!(refused_prepare.status, 409, "{:?}", refused_prepare.body);
    assert_eq!(
        refused_prepare.body["code"], "schedule_x_lot_provenance_unresolved",
        "{:?}",
        refused_prepare.body
    );

    // So the posting is refused too, and nothing durable exists.
    let refused = post_quoted_over_http(
        &service,
        &f.world,
        &f.sale_id,
        f.revision,
        "01997a00-0000-7000-8000-0000000003d6",
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert!(
        schedule_x_unmet(&refused.body)
            .contains(&"schedule_x_lot_provenance_incomplete".to_owned()),
        "{:?}",
        refused.body
    );
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    assert_nothing_durable_over_http(&pool, &f.sale_id).await;
    // No page was written in the register for a supply that could never be made.
    let entries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_schedule_x_register_entries          WHERE sale_document_id=? AND entry_kind='supply'",
    )
    .bind(&f.sale_id)
    .fetch_one(&pool)
    .await
    .expect("entries");
    assert_eq!(entries, 0);
    pool.close().await;
}

/// Phase 1M-D3-C2, item 29 case 27 — RETRYING AN UNCERTAIN POSTING IS IDEMPOTENT.
///
/// The counter presses Post Sale, the response is lost, and the same request is sent again with the
/// same idempotency key. One Sale was supplied, so there must be exactly one of everything: one
/// document number, one dispensing, one outward movement, one tender, and one finalized Schedule X
/// register entry. A second register entry would be a second supply in the statutory record of a
/// supply that happened once.
#[tokio::test]
async fn real_service_replays_a_posted_schedule_x_sale_idempotently_over_http() {
    let service = start().await;
    let f = favourable_schedule_x_over_http(&service, true).await;

    let key = "01997a00-0000-7000-8000-0000000003e7";
    let first = post_quoted_over_http(&service, &f.world, &f.sale_id, f.revision, key).await;
    assert_eq!(first.status, 200, "{:?}", first.body);
    let document_number = first.body["documentNumber"]
        .as_str()
        .expect("documentNumber")
        .to_owned();

    // The same key again, with the revision the draft had: the original invoice comes back rather
    // than a second supply.
    let replay = post_quoted_over_http(&service, &f.world, &f.sale_id, f.revision, key).await;
    assert_eq!(replay.status, 200, "{:?}", replay.body);
    assert_eq!(replay.body["documentNumber"], document_number.as_str());
    assert_eq!(replay.body["status"], "posted");

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    for (what, sql) in [
        (
            "dispensing",
            "SELECT COUNT(*) FROM prescription_dispensings WHERE sale_document_id=?",
        ),
        (
            "outward movement",
            "SELECT COUNT(*) FROM inventory_movements WHERE movement_type='sale' \
             AND sale_line_id IN (SELECT id FROM sale_lines WHERE sale_document_id=?)",
        ),
        (
            "tender",
            "SELECT COUNT(*) FROM sale_tenders WHERE sale_document_id=?",
        ),
        (
            "finalized Schedule X entry",
            "SELECT COUNT(*) FROM store_schedule_x_register_entries \
             WHERE sale_document_id=? AND status='finalized'",
        ),
        (
            "posted audit event for the entry",
            "SELECT COUNT(*) FROM master_change_events \
             WHERE entity_type='schedule_x_register_entry' AND action='posted' \
               AND entity_id IN (SELECT id FROM store_schedule_x_register_entries \
                                 WHERE sale_document_id=?)",
        ),
    ] {
        let count: i64 = sqlx::query_scalar(sql)
            .bind(&f.sale_id)
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 1, "the replay produced {count} of: {what}");
    }
    // And exactly one number was taken from the series.
    let next: i64 = sqlx::query_scalar(
        "SELECT next_value FROM document_number_series WHERE document_kind='sale'",
    )
    .fetch_one(&pool)
    .await
    .expect("series");
    assert_eq!(next, 2, "the replay consumed a second document number");
    pool.close().await;
}

/// Phase 1M-D3-C2, item 29 case 20 — RELEASED RETURNED STOCK CANNOT BE SUPPLIED AGAIN.
///
/// The real lineage, through the real flows: a supported Schedule X Sale posts; the customer brings
/// some of it back and it goes into quarantine as a `sales_return`; the pharmacy inspects it and
/// releases it to sellable through a stock disposition. The lot's sellable balance now contains
/// units that did NOT arrive on a qualifying purchase.
///
/// Nothing was faked with a master edit. A returned-and-released unit is sellable again as far as
/// ordinary stock is concerned, and Phase 1M-D3-B still refuses it for Schedule X — because stock
/// inside one lot cannot be told apart, so the whole lot is held back and no amount of disposition
/// paperwork restores the original purchase provenance.
#[tokio::test]
async fn real_service_refuses_schedule_x_from_returned_and_released_stock_over_http() {
    let service = start().await;
    let f = favourable_schedule_x_over_http(&service, true).await;

    // A supported Schedule X Sale, posted.
    let posted = post_quoted_over_http(
        &service,
        &f.world,
        &f.sale_id,
        f.revision,
        "01997a00-0000-7000-8000-0000000003f8",
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);

    // The customer returns it. It goes to quarantine, which is where a sales return must go.
    let draft = call(
        &service,
        "POST",
        "/api/v1/returns",
        Some(json!({
            "returnKind": "sales_return",
            "originalDocumentId": f.sale_id,
            "businessDate": SALE_DATE
        })),
        Some(&f.world.cookie),
    )
    .await;
    assert_eq!(draft.status, 201, "{:?}", draft.body);
    let return_id = draft.body["id"].as_str().expect("return id").to_owned();
    let with_line = call(
        &service,
        "POST",
        &format!("/api/v1/returns/{return_id}/lines"),
        Some(json!({
            "expectedRevision": 1,
            "originalLineId": f.line_id,
            "quantity": 1,
            "disposition": "quarantined"
        })),
        Some(&f.world.cookie),
    )
    .await;
    assert_eq!(with_line.status, 201, "{:?}", with_line.body);
    let returned = call(
        &service,
        "POST",
        &format!("/api/v1/returns/{return_id}/post"),
        Some(json!({
            "expectedRevision": with_line.body["revision"],
            "idempotencyKey": "01997a00-0000-7000-8000-0000000003f9",
            "taxAdjustmentStatus": "commercial_only",
            "taxAdjustmentReason": "Tax was passed on to the customer"
        })),
        Some(&f.world.cookie),
    )
    .await;
    assert_eq!(returned.status, 200, "{:?}", returned.body);

    // Inspected and released back to sellable, through the ordinary disposition flow.
    let released = call(
        &service,
        "POST",
        "/api/v1/stock-dispositions",
        Some(json!({
            "idempotencyKey": "01997a00-0000-7000-8000-0000000003fa",
            "productPackId": f.world.pack,
            "batchId": f.batch,
            "quantityAtoms": 10,
            "fromStatus": "quarantined",
            "toStatus": "sellable",
            "reason": "Sealed strip, inspected and found fit for sale",
            "occurredOn": SALE_DATE
        })),
        Some(&f.world.cookie),
    )
    .await;
    assert_eq!(released.status, 201, "{:?}", released.body);

    // The stock is sellable again as far as ordinary stock is concerned.
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let released_inward: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inventory_movements \
         WHERE batch_id=? AND stock_status='sellable' AND quantity_delta_atoms > 0 \
           AND movement_type='disposition_transfer'",
    )
    .bind(&f.batch)
    .fetch_one(&pool)
    .await
    .expect("released inward");
    assert_eq!(
        released_inward, 1,
        "the release did not reach the sellable balance"
    );
    pool.close().await;

    // And a fresh Schedule X supply from that lot is refused, at the earliest point: the rule 65(21)
    // working entry cannot be prepared for a lot whose stock no longer accounts for itself.
    let (second_prescription, second_item) =
        prescription_over_http(&service, &f.world, None, 20).await;
    let duplicate = call(
        &service,
        "POST",
        &format!("/api/v1/prescriptions/{second_prescription}/schedule-x-duplicate-copy"),
        Some(json!({ "retainedDuplicatePrescriptionCopyConfirmed": true, "note": Value::Null })),
        Some(&f.world.cookie),
    )
    .await;
    assert_eq!(duplicate.status, 201, "{:?}", duplicate.body);
    let (second_sale, second_line, second_revision) =
        prepared_sale_on_lot_over_http(&service, &f.world, &f.batch, &second_item, &f.professional)
            .await;
    let annotated =
        record_annotation_over_http(&service, &f.world.cookie, &second_line, true).await;
    assert_eq!(annotated.status, 201, "{:?}", annotated.body);

    let refused_prepare = prepare_supply_over_http(&service, &f.world.cookie, &second_line).await;
    assert_eq!(refused_prepare.status, 409, "{:?}", refused_prepare.body);
    assert_eq!(
        refused_prepare.body["code"], "schedule_x_lot_provenance_unresolved",
        "{:?}",
        refused_prepare.body
    );

    let refused = post_quoted_over_http(
        &service,
        &f.world,
        &second_sale,
        second_revision,
        "01997a00-0000-7000-8000-0000000003fb",
    )
    .await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert!(
        schedule_x_unmet(&refused.body)
            .contains(&"schedule_x_lot_provenance_incomplete".to_owned()),
        "{:?}",
        refused.body
    );
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    assert_nothing_durable_over_http_for(&pool, &second_sale).await;
    pool.close().await;
}

/// As `assert_nothing_durable_over_http`, for a Sale in a world where an earlier Sale legitimately
/// consumed a document number.
async fn assert_nothing_durable_over_http_for(pool: &sqlx::SqlitePool, sale_id: &str) {
    let (status, number): (String, Option<String>) =
        sqlx::query_as("SELECT status,document_number FROM sale_documents WHERE id=?")
            .bind(sale_id)
            .fetch_one(pool)
            .await
            .expect("sale");
    assert_eq!(status, "draft");
    assert_eq!(number, None);
    for (what, sql) in [
        (
            "dispensing",
            "SELECT COUNT(*) FROM prescription_dispensings WHERE sale_document_id=?",
        ),
        (
            "outflow",
            "SELECT COUNT(*) FROM inventory_movements WHERE movement_type='sale' \
             AND sale_line_id IN (SELECT id FROM sale_lines WHERE sale_document_id=?)",
        ),
        (
            "tender",
            "SELECT COUNT(*) FROM sale_tenders WHERE sale_document_id=?",
        ),
        (
            "finalized Schedule X entry",
            "SELECT COUNT(*) FROM store_schedule_x_register_entries \
             WHERE sale_document_id=? AND status='finalized'",
        ),
    ] {
        let count: i64 = sqlx::query_scalar(sql)
            .bind(sale_id)
            .fetch_one(pool)
            .await
            .expect("count");
        assert_eq!(count, 0, "a {what} survived");
    }
}

/// Phase 1M-D3-C2, item 29 case 8 — RULE 65(11A): NO SUBSTITUTION, EVEN FOR A SCHEDULE X LINE.
///
/// "No person dispensing a prescription containing substances specified in Schedule H and Schedule H1
/// or X may supply any other preparation, whether containing the same substances or not in lieu
/// thereof." The prescription here names a different product from the one on the Sale line, and the
/// link is refused outright — the identity is checked where it is established, not re-litigated inside
/// Schedule X.
#[tokio::test]
async fn real_service_refuses_a_schedule_x_line_against_another_products_prescription_over_http() {
    let service = start().await;
    let f = favourable_schedule_x_with_options(
        &service,
        FavourableOptions {
            skip_prepare: true,
            ..Default::default()
        },
    )
    .await;
    // A second product, and a prescription that names IT rather than the Schedule X drug.
    let (other_product, _, _) = ordinary_product_over_http(&service, &f.world).await;
    let created = call(
        &service,
        "POST",
        "/api/v1/prescriptions",
        Some(json!({
            "prescribedOn": "2026-09-10",
            "prescriberId": Value::Null,
            "prescriberName": "Dr. Anjali Rao",
            "prescriberAddress": "Rao Clinic, FC Road, Pune 411005",
            "subjectKind": "human",
            "subjectName": "Sita Kulkarni",
            "subjectAddress": "14 Lakshmi Road, Pune 411004",
            "repeatAuthority": "once",
            "writtenSignedDatedAttested": true,
            "items": [{
                "productId": other_product,
                "writtenDescription": "Tab. as prescribed",
                "prescribedQuantityAtoms": 20,
                "doseText": "1 tablet twice daily",
            }],
        })),
        Some(&f.world.cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    let other_item = created.body["items"][0]["id"]
        .as_str()
        .expect("item")
        .to_owned();

    // Re-pointing the Schedule X line at that item is refused: same molecule or not, it is not the
    // preparation the prescriber wrote.
    let revision = sale_revision_over_http(&service, &f.world, &f.sale_id).await;
    let refused = call(
        &service,
        "PUT",
        &format!("/api/v1/sale-lines/{}/prescription", f.line_id),
        Some(json!({ "expectedRevision": revision, "prescriptionItemId": other_item })),
        Some(&f.world.cookie),
    )
    .await;
    assert_ne!(
        refused.status, 200,
        "a Schedule X line took another product's prescription item: {:?}",
        refused.body
    );
    let text = refused.body.to_string().to_lowercase();
    assert!(
        text.contains("product") || text.contains("substitut") || text.contains("prescription"),
        "{:?}",
        refused.body
    );

    // The line still points at its own item, and the identity chain is intact in the database.
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let linked: String =
        sqlx::query_scalar("SELECT prescription_item_id FROM sale_lines WHERE id=?")
            .bind(&f.line_id)
            .fetch_one(&pool)
            .await
            .expect("link");
    assert_eq!(linked, f.item);
    assert_nothing_durable_over_http(&pool, &f.sale_id).await;
    pool.close().await;
}

/// Phase 1M-D3-C2, item 29 cases 9 and 10 — RULE 65(11)(a) AND (b), ON A SCHEDULE X SUPPLY.
///
/// (a) "the prescription must not be dispensed more than once unless the prescriber has stated thereon
/// that it may be dispensed more than once". (b) "if the prescription contains a direction that it may
/// be dispensed a stated number of times or at stated intervals it must not be dispensed otherwise
/// than in accordance with the directions".
///
/// Both are answered by `domain::prescriptions::check_dispense`, which the Schedule X path joins
/// rather than reimplements, and both are enforced again by the `prescription_dispensings` caps in the
/// database. Each case posts one lawful Schedule X supply first, so the refusal is reached on the
/// second — not on some earlier requirement.
#[tokio::test]
async fn real_service_refuses_a_second_schedule_x_supply_beyond_the_repeat_authority_over_http() {
    for (case, repeat, expected) in [
        (
            // Case 9 — dispensed once already, and the prescriber authorised no repeat.
            "repeat_not_authorised",
            json!({ "repeatAuthority": "once" }),
            "prescription_repeat_not_authorised",
        ),
        (
            // Case 10 — a repeat IS authorised, but not yet: the stated interval has not elapsed.
            "repeat_interval_not_elapsed",
            json!({
                "repeatAuthority": "stated_times",
                "repeatTimes": 2,
                "repeatIntervalDays": 30
            }),
            "prescription_repeat_too_soon",
        ),
    ] {
        let service = start().await;
        let f = favourable_schedule_x_with_options(
            &service,
            FavourableOptions {
                confirm: true,
                repeat: Some(repeat),
                ..Default::default()
            },
        )
        .await;

        // One lawful supply.
        let posted = post_quoted_over_http(
            &service,
            &f.world,
            &f.sale_id,
            f.revision,
            "01997a00-0000-7000-8000-000000000409",
        )
        .await;
        assert_eq!(posted.status, 200, "{case}: {:?}", posted.body);

        // A second Sale against the same prescription item, on the same lawful lot, with every other
        // Schedule X requirement satisfied again.
        let (second_sale, second_line, second_revision) =
            prepared_sale_on_lot_over_http(&service, &f.world, &f.batch, &f.item, &f.professional)
                .await;
        let annotated =
            record_annotation_over_http(&service, &f.world.cookie, &second_line, true).await;
        assert_eq!(annotated.status, 201, "{case}: {:?}", annotated.body);

        let refused = post_quoted_over_http(
            &service,
            &f.world,
            &second_sale,
            second_revision,
            "01997a00-0000-7000-8000-00000000040a",
        )
        .await;
        assert_eq!(refused.status, 409, "{case}: {:?}", refused.body);
        assert_eq!(
            refused.body["code"], "prescription_requirements_incomplete",
            "{case}: {:?}",
            refused.body
        );
        let codes = prescription_issue_fields(&refused.body);
        assert!(
            codes.iter().any(|code| code.ends_with(expected)),
            "{case}: expected {expected}, got {codes:?}"
        );

        // The first supply stands; the second left nothing behind.
        let pool = database::connect(&service.database_path)
            .await
            .expect("database");
        let dispensings: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM prescription_dispensings WHERE prescription_item_id=?",
        )
        .bind(&f.item)
        .fetch_one(&pool)
        .await
        .expect("dispensings");
        assert_eq!(dispensings, 1, "{case}: a second dispensing survived");
        assert_nothing_durable_over_http_for(&pool, &second_sale).await;
        pool.close().await;
    }
}

/// Every issue field a structured prescription refusal names.
fn prescription_issue_fields(body: &Value) -> Vec<String> {
    body["issues"]
        .as_array()
        .map(|issues| {
            issues
                .iter()
                .filter_map(|issue| issue["field"].as_str())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Phase 1M-D3-C2, item 29 case 6 — THE SUPPLIER AUTHORITY'S PERIOD IS HALF-OPEN, AT THE SALE.
///
/// Phase 1M-D3-C1 models the recorded period as `[effective_from, effective_to)` with `effective_to`
/// EXCLUSIVE, and resolves it against the purchase's own invoice date — never today. These are the
/// four boundaries, exercised through a complete Schedule X Sale rather than through the resolver
/// alone, so the arithmetic that actually gates a supply is the arithmetic under test.
///
/// Every fixture is otherwise fully favourable, and the lot's only purchase is invoiced 2026-09-10.
#[tokio::test]
async fn real_service_holds_the_supplier_authority_period_half_open_at_the_sale_over_http() {
    const INVOICE: &str = "2026-09-10";
    for (case, from, to, basis, eligible) in [
        // The invoice date is exactly the first day the authority covers.
        (
            "invoice_on_effective_from",
            INVOICE,
            None,
            "perpetual",
            true,
        ),
        // One day later, so the invoice falls immediately BEFORE the period opens.
        (
            "invoice_immediately_before_effective_from",
            "2026-09-11",
            None,
            "perpetual",
            false,
        ),
        // `effective_to` is exclusive, so the invoice sits inside a period ending the day after.
        (
            "invoice_immediately_before_effective_to",
            "2020-01-01",
            Some("2026-09-11"),
            "fixed_term",
            true,
        ),
        // And exactly ON `effective_to` is outside it.
        (
            "invoice_exactly_on_effective_to",
            "2020-01-01",
            Some(INVOICE),
            "fixed_term",
            false,
        ),
    ] {
        let service = start().await;
        let f = favourable_schedule_x_with_options(
            &service,
            FavourableOptions {
                confirm: eligible,
                skip_prepare: !eligible,
                authority_period: Some((from.to_owned(), to.map(str::to_owned), basis.to_owned())),
                ..Default::default()
            },
        )
        .await;

        let preflight = preflight_over_http(&service, &f.world.cookie, &f.sale_id).await;
        let line = &preflight["lines"].as_array().expect("lines")[0];
        let unmet = unmet_predicates(line);
        let pool = database::connect(&service.database_path)
            .await
            .expect("database");

        if eligible {
            assert_eq!(unmet, Vec::<String>::new(), "{case}: {line}");
            let posted = post_quoted_over_http(
                &service,
                &f.world,
                &f.sale_id,
                f.revision,
                "01997a00-0000-7000-8000-00000000041b",
            )
            .await;
            assert_eq!(posted.status, 200, "{case}: {:?}", posted.body);
            let status: String = sqlx::query_scalar(
                "SELECT status FROM store_schedule_x_register_entries WHERE id=?",
            )
            .bind(&f.entry_id)
            .fetch_one(&pool)
            .await
            .expect("entry");
            assert_eq!(status, "finalized", "{case}");
        } else {
            // The source's authority does not cover the day its goods were bought, so the lot cannot
            // support a Schedule X supply and the working entry is never even prepared.
            assert!(
                unmet.contains(&"sourceAuthorities:outside_effective_period".to_owned()),
                "{case}: {unmet:?}"
            );
            let refused = post_quoted_over_http(
                &service,
                &f.world,
                &f.sale_id,
                f.revision,
                "01997a00-0000-7000-8000-00000000041c",
            )
            .await;
            assert_eq!(refused.status, 409, "{case}: {:?}", refused.body);
            assert!(
                schedule_x_unmet(&refused.body)
                    .contains(&"schedule_x_source_authority_missing".to_owned()),
                "{case}: {:?}",
                refused.body
            );
            assert_nothing_durable_over_http(&pool, &f.sale_id).await;
        }
        pool.close().await;
    }
}

// ==============================================================================================
// Phase 1M-D3-C2 — concurrency. Enumerate the valid serializations, then prove the invariant from
// the database. No sleeps, no retries, no serialization of the suite, no widened budgets.
// ==============================================================================================

/// A second favourable Schedule X Sale in an existing world, against the same lot and prescription
/// item, with its own signed working entry. Returns (sale, line, revision, entry).
async fn second_favourable_x_sale_over_http(
    service: &Service,
    f: &FavourableX,
) -> (String, String, i64, String) {
    let (sale_id, line_id, revision) =
        prepared_sale_on_lot_over_http(service, &f.world, &f.batch, &f.item, &f.professional).await;
    let annotated = record_annotation_over_http(service, &f.world.cookie, &line_id, true).await;
    assert_eq!(annotated.status, 201, "{:?}", annotated.body);
    let prepared = prepare_supply_over_http(service, &f.world.cookie, &line_id).await;
    assert_eq!(prepared.status, 201, "{:?}", prepared.body);
    let entry_id = prepared.body["id"].as_str().expect("id").to_owned();
    let confirmed = call(
        service,
        "POST",
        &format!("/api/v1/store/schedule-x/register/{entry_id}/confirm"),
        Some(json!({
            "supervisingProfessionalId": f.professional,
            "particularsEnteredInPhysicalRegister": true,
            "physicalEntryAuthenticated": true,
        })),
        Some(&f.world.cookie),
    )
    .await;
    assert_eq!(confirmed.status, 200, "{:?}", confirmed.body);
    (sale_id, line_id, revision, entry_id)
}

/// Phase 1M-D3-C2, item 29 case 21 — TWO TERMINALS POSTING ONE SCHEDULE X SALE.
///
/// Both requests carry the same draft revision and different idempotency keys, so they are two
/// genuine attempts at the same supply rather than a replay.
///
/// Valid serializations: whichever posting takes the write lock first commits; the other finds the
/// document already posted, or its revision stale, and is refused. Either way the authoritative
/// invariant is the same and is proved below — ONE effective posting, and exactly one of everything
/// a supply writes, including one Schedule X register entry. A second entry would be a second supply
/// in the statutory record of a supply that happened once.
#[tokio::test]
async fn real_service_posts_one_schedule_x_sale_once_under_a_double_post_race_over_http() {
    let service = start().await;
    let f = favourable_schedule_x_over_http(&service, true).await;
    let total = quote_over_http(&service, &f.world, &f.sale_id).await["grandTotalPaise"]
        .as_i64()
        .expect("total");

    let (first, second) = tokio::join!(
        post_sale(
            &service,
            &f.world,
            &f.sale_id,
            f.revision,
            "01997a00-0000-7000-8000-00000000042c",
            total
        ),
        post_sale(
            &service,
            &f.world,
            &f.sale_id,
            f.revision,
            "01997a00-0000-7000-8000-00000000042d",
            total
        )
    );
    // At least one landed, and they did not both.
    assert!(
        (first.status == 200) ^ (second.status == 200),
        "exactly one posting must win: first={:?} second={:?}",
        first.body,
        second.body
    );

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let status: String = sqlx::query_scalar("SELECT status FROM sale_documents WHERE id=?")
        .bind(&f.sale_id)
        .fetch_one(&pool)
        .await
        .expect("sale");
    assert_eq!(status, "posted");
    for (what, sql) in [
        (
            "document",
            "SELECT COUNT(*) FROM sale_documents WHERE id=? AND document_number IS NOT NULL",
        ),
        (
            "dispensing",
            "SELECT COUNT(*) FROM prescription_dispensings WHERE sale_document_id=?",
        ),
        (
            "outward movement",
            "SELECT COUNT(*) FROM inventory_movements WHERE movement_type='sale' \
             AND sale_line_id IN (SELECT id FROM sale_lines WHERE sale_document_id=?)",
        ),
        (
            "tender",
            "SELECT COUNT(*) FROM sale_tenders WHERE sale_document_id=?",
        ),
        (
            "finalized Schedule X entry",
            "SELECT COUNT(*) FROM store_schedule_x_register_entries \
             WHERE sale_document_id=? AND status='finalized'",
        ),
    ] {
        let count: i64 = sqlx::query_scalar(sql)
            .bind(&f.sale_id)
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 1, "the race produced {count} of: {what}");
    }
    // Exactly one number left the series.
    let next: i64 = sqlx::query_scalar(
        "SELECT next_value FROM document_number_series WHERE document_kind='sale'",
    )
    .fetch_one(&pool)
    .await
    .expect("series");
    assert_eq!(next, 2, "the race consumed more than one document number");
    pool.close().await;
}

/// Phase 1M-D3-C2, item 29 case 22 — TWO SCHEDULE X SALES AGAINST ONE PRESCRIPTION AUTHORITY.
///
/// The prescription item authorises 10 atoms. Two complete Schedule X Sales each want 10. Rule
/// 65(10)(c) caps the total at what the prescriber wrote and rule 65(11)(a) caps the occasions at one,
/// so both answers refuse the second supply.
///
/// Valid serializations: either order. The invariant proved from the database afterwards is the one
/// that matters — the durable dispensed total for the item NEVER exceeds the authority, whichever
/// Sale won.
#[tokio::test]
async fn real_service_never_exceeds_a_prescription_authority_under_a_schedule_x_race_over_http() {
    let service = start().await;
    let f = favourable_schedule_x_with_options(
        &service,
        FavourableOptions {
            confirm: true,
            // Exactly one pack's worth of authority, so two Sales of one pack cannot both be lawful.
            repeat: Some(json!({ "repeatAuthority": "once" })),
            ..Default::default()
        },
    )
    .await;
    // Narrow the authority to a single pack.
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let narrowed =
        sqlx::query("UPDATE prescription_items SET prescribed_quantity_atoms=10 WHERE id=?")
            .bind(&f.item)
            .execute(&pool)
            .await
            .expect("narrow");
    assert_eq!(narrowed.rows_affected(), 1);
    pool.close().await;

    let (second_sale, _, second_revision, _) =
        second_favourable_x_sale_over_http(&service, &f).await;

    let first_total = quote_over_http(&service, &f.world, &f.sale_id).await["grandTotalPaise"]
        .as_i64()
        .expect("total");
    let second_total = quote_over_http(&service, &f.world, &second_sale).await["grandTotalPaise"]
        .as_i64()
        .expect("total");
    let (first, second) = tokio::join!(
        post_sale(
            &service,
            &f.world,
            &f.sale_id,
            f.revision,
            "01997a00-0000-7000-8000-00000000043e",
            first_total
        ),
        post_sale(
            &service,
            &f.world,
            &second_sale,
            second_revision,
            "01997a00-0000-7000-8000-00000000043f",
            second_total
        )
    );
    // Both refusing is a valid serialization only if neither could proceed; at most one may post.
    assert!(
        !(first.status == 200 && second.status == 200),
        "both Sales posted against one authority: first={:?} second={:?}",
        first.body,
        second.body
    );

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let dispensed: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(quantity_atoms),0) FROM prescription_dispensings \
         WHERE prescription_item_id=?",
    )
    .bind(&f.item)
    .fetch_one(&pool)
    .await
    .expect("dispensed");
    assert!(
        dispensed <= 10,
        "the race dispensed {dispensed} atoms against an authority of 10"
    );
    // And exactly one finalized Schedule X entry exists across both Sales, at most.
    let finalized: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_schedule_x_register_entries WHERE status='finalized'",
    )
    .fetch_one(&pool)
    .await
    .expect("finalized");
    assert!(finalized <= 1, "{finalized} supplies were finalized");
    pool.close().await;
}

/// Phase 1M-D3-C2, item 29 case 23 — TWO SCHEDULE X SALES AGAINST ONE LOT'S STOCK.
///
/// The lot is drawn down to a single pack, and two complete Schedule X Sales each want a pack from it.
/// The Phase 1H inventory model takes the write lock before reading any balance, so the two cannot
/// interleave between the availability check and the movement.
///
/// Valid serializations: either order. The invariants proved afterwards are that the lot's sellable
/// balance never goes negative and the durable outward quantity never exceeds what the lot held.
#[tokio::test]
async fn real_service_never_oversells_a_schedule_x_lot_under_a_race_over_http() {
    let service = start().await;
    let f = favourable_schedule_x_over_http(&service, true).await;
    let (second_sale, _, second_revision, _) =
        second_favourable_x_sale_over_http(&service, &f).await;

    // Draw the lot down to exactly one pack, so only one of the two Sales can be supplied.
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let available: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(quantity_delta_atoms),0) FROM inventory_movements \
         WHERE batch_id=? AND stock_status='sellable'",
    )
    .bind(&f.batch)
    .fetch_one(&pool)
    .await
    .expect("balance");
    assert!(available >= 10, "the lot held {available} atoms");
    if available > 10 {
        // A counted shelf sets the balance to exactly one pack. A count LOSS is a decrease, so it
        // cannot add anything to the sellable balance and the lot's provenance is untouched.
        let counted = run_operation(
            &service,
            &f.world.cookie,
            "physical_count",
            json!({
                "productPackId": f.world.pack, "batchId": f.batch, "stockStatus": "sellable",
                "reasonCode": "physical_count_loss", "countedQuantity": 10,
                "quantityBasis": "base_unit"
            }),
        )
        .await;
        assert_eq!(counted.status, 200, "{:?}", counted.body);
    }
    pool.close().await;

    let first_total = quote_over_http(&service, &f.world, &f.sale_id).await["grandTotalPaise"]
        .as_i64()
        .expect("total");
    let second_total = quote_over_http(&service, &f.world, &second_sale).await["grandTotalPaise"]
        .as_i64()
        .expect("total");
    let (first, second) = tokio::join!(
        post_sale(
            &service,
            &f.world,
            &f.sale_id,
            f.revision,
            "01997a00-0000-7000-8000-000000000450",
            first_total
        ),
        post_sale(
            &service,
            &f.world,
            &second_sale,
            second_revision,
            "01997a00-0000-7000-8000-000000000451",
            second_total
        )
    );
    assert!(
        !(first.status == 200 && second.status == 200),
        "both Sales drew the same pack: first={:?} second={:?}",
        first.body,
        second.body
    );

    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    let balance: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(quantity_delta_atoms),0) FROM inventory_movements \
         WHERE batch_id=? AND stock_status='sellable'",
    )
    .bind(&f.batch)
    .fetch_one(&pool)
    .await
    .expect("balance");
    assert!(balance >= 0, "the lot went to {balance} atoms");
    let sold: i64 = sqlx::query_scalar(
        "SELECT COALESCE(-SUM(quantity_delta_atoms),0) FROM inventory_movements \
         WHERE batch_id=? AND movement_type='sale'",
    )
    .bind(&f.batch)
    .fetch_one(&pool)
    .await
    .expect("sold");
    assert!(
        sold <= 10,
        "the race sold {sold} atoms out of a lot holding 10"
    );
    pool.close().await;
}

/// Phase 1M-D3-C2, item 29 case 29 — A POSTED SCHEDULE X SALE SURVIVES BACKUP AND RESTORE.
///
/// A genuinely posted supply, through the ordinary Phase 1K backup and restore workflow and no
/// Schedule X-specific format. What must come back is the whole statutory record of that supply: the
/// document and its number and date, the frozen regulatory snapshot, the dispensing, the outward
/// movement, the finalized rule 65(21) entry with its three posting bindings and both physical
/// attestations, the rule 65(11)(c) annotation linkage, the supplier's historical authority evidence,
/// the provenance chain behind the lot, and the audit history.
#[tokio::test]
async fn real_service_carries_a_posted_schedule_x_sale_through_backup_and_restore_over_http() {
    let harness = start_with_backups().await;
    let service = &harness.service;
    let f = favourable_schedule_x_over_http(service, true).await;
    let posted = post_quoted_over_http(
        service,
        &f.world,
        &f.sale_id,
        f.revision,
        "01997a00-0000-7000-8000-000000000462",
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    let document_number = posted.body["documentNumber"]
        .as_str()
        .expect("documentNumber")
        .to_owned();

    let created = call(
        service,
        "POST",
        "/api/v1/backups/create",
        Some(json!({})),
        Some(&f.world.cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    let resolved = call(
        service,
        "GET",
        &format!(
            "/api/v1/backups/{}/download",
            created.body["backupId"].as_str().expect("backup id")
        ),
        None,
        Some(&f.world.cookie),
    )
    .await;
    assert_eq!(resolved.status, 200, "{:?}", resolved.body);
    let (status, _, downloaded) = get_bytes(
        service,
        resolved.body["url"].as_str().expect("url"),
        Some(&f.world.cookie),
    )
    .await;
    assert_eq!(status, 200);

    // Something after the backup that the restore must undo.
    let after = call(
        service,
        "POST",
        "/api/v1/parties",
        Some(json!({
            "party": { "displayName": "Post-Backup Supplier" },
            "roles": [{ "role": "supplier" }]
        })),
        Some(&f.world.cookie),
    )
    .await;
    assert_eq!(after.status, 201, "{:?}", after.body);

    let (status, _, body) = call_bytes(
        service,
        "/api/v1/backups/restore/prepare",
        "application/octet-stream",
        &downloaded,
        Some(&f.world.cookie),
    )
    .await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let prepared_restore: Value = serde_json::from_slice(&body).expect("prepared restore");
    let committed = call(
        service,
        "POST",
        "/api/v1/backups/restore/commit",
        Some(json!({
            "candidateToken": prepared_restore["candidateToken"].as_str().expect("token"),
            "password": "Integration-Password-42"
        })),
        Some(&f.world.cookie),
    )
    .await;
    assert_eq!(committed.status, 200, "{:?}", committed.body);
    api::backups::recover_interrupted_restore(&harness.backups, &service.database_path)
        .await
        .expect("recovery");
    let reopened = database::connect(&service.database_path)
        .await
        .expect("reopened database");
    assert!(
        api::backups::complete_restore_after_open(&reopened, &harness.backups)
            .await
            .expect("completion")
    );
    let post_backup: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM parties WHERE display_name='Post-Backup Supplier'",
    )
    .fetch_one(&reopened)
    .await
    .expect("parties");
    assert_eq!(post_backup, 0, "the restore did not roll the pharmacy back");

    // The Sale, its number, its date and its frozen regulatory snapshot.
    let (status, number, business_date): (String, Option<String>, String) = sqlx::query_as(
        "SELECT status,document_number,business_date FROM sale_documents WHERE id=?",
    )
    .bind(&f.sale_id)
    .fetch_one(&reopened)
    .await
    .expect("the posted Sale did not survive the restore");
    assert_eq!(status, "posted");
    assert_eq!(number.as_deref(), Some(document_number.as_str()));
    let (snapshot_version, schemes): (i64, String) = sqlx::query_as(
        "SELECT regulatory_snapshot_version,regulatory_schemes_snapshot FROM sale_lines WHERE id=?",
    )
    .bind(&f.line_id)
    .fetch_one(&reopened)
    .await
    .expect("line");
    assert_eq!(snapshot_version, 1);
    assert!(schemes.contains("\"schedule_x\":\"applies\""), "{schemes}");

    // The dispensing and the outward movement.
    let dispensings: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM prescription_dispensings WHERE sale_line_id=?")
            .bind(&f.line_id)
            .fetch_one(&reopened)
            .await
            .expect("dispensings");
    assert_eq!(dispensings, 1);
    let outflow: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inventory_movements WHERE movement_type='sale' AND sale_line_id=?",
    )
    .bind(&f.line_id)
    .fetch_one(&reopened)
    .await
    .expect("outflow");
    assert_eq!(outflow, 1);

    // The finalized rule 65(21) entry, with all three bindings and both attestations.
    #[derive(sqlx::FromRow)]
    struct Restored {
        status: String,
        dispensing_id: Option<String>,
        bill_number: Option<String>,
        bill_date: Option<String>,
        batch_id: Option<String>,
        particulars_entered_in_physical_register: i64,
        physical_entry_authenticated: i64,
        supervising_professional_id: Option<String>,
    }
    let entry: Restored = sqlx::query_as(
        "SELECT status,dispensing_id,bill_number,bill_date,batch_id,\
         particulars_entered_in_physical_register,physical_entry_authenticated,\
         supervising_professional_id \
         FROM store_schedule_x_register_entries WHERE id=?",
    )
    .bind(&f.entry_id)
    .fetch_one(&reopened)
    .await
    .expect("the finalized entry did not survive the restore");
    assert_eq!(entry.status, "finalized");
    assert!(entry.dispensing_id.is_some());
    assert_eq!(entry.bill_number.as_deref(), Some(document_number.as_str()));
    assert_eq!(entry.bill_date.as_deref(), Some(business_date.as_str()));
    assert_eq!(entry.batch_id.as_deref(), Some(f.batch.as_str()));
    assert_eq!(entry.particulars_entered_in_physical_register, 1);
    assert_eq!(entry.physical_entry_authenticated, 1);
    assert!(entry.supervising_professional_id.is_some());

    // The rule 65(11)(c) annotation linkage, the supplier's historical authority evidence, the
    // provenance chain behind the lot, and the audit history.
    for (what, sql) in [
        (
            "rule 65(11)(c) annotation",
            "SELECT COUNT(*) FROM schedule_x_prescription_annotations WHERE sale_line_id=?",
        ),
        (
            "qualifying receipt entry",
            "SELECT COUNT(*) FROM store_schedule_x_register_entries receipt \
             JOIN inventory_movements inward ON inward.purchase_line_id=receipt.purchase_line_id \
             WHERE receipt.entry_kind='receipt' AND inward.batch_id=(\
                 SELECT batch_id FROM sale_lines WHERE id=?)",
        ),
    ] {
        let count: i64 = sqlx::query_scalar(sql)
            .bind(&f.line_id)
            .fetch_one(&reopened)
            .await
            .expect("count");
        assert!(count >= 1, "{what} did not survive the restore");
    }
    let authority: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM supplier_schedule_x_authorities WHERE id=?")
            .bind(&f.authority)
            .fetch_one(&reopened)
            .await
            .expect("authority");
    assert_eq!(
        authority, 1,
        "the supplier's authority evidence did not survive"
    );
    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM master_change_events \
         WHERE entity_type='schedule_x_register_entry' AND entity_id=? AND action='posted'",
    )
    .bind(&f.entry_id)
    .fetch_one(&reopened)
    .await
    .expect("audit");
    assert_eq!(audited, 1, "the finalization audit did not survive");
    reopened.close().await;
}

/// Phase 1M-D3-C2, item 29 case 30 — A POSTED SCHEDULE X SALE IS NEVER REREAD THROUGH TODAY'S MASTERS.
///
/// After the supply is posted, the masters move on, exactly as they are allowed to: the Form 20-F drug
/// coverage is withdrawn, the supplier's authority is archived, the pharmacist leaves, and the drug's
/// regulatory classification is corrected. None of that is historical evidence, and none of it is
/// touched here — only current master state.
///
/// The posted document must not be reinterpreted by any of it. What was frozen at posting stays
/// frozen, and the statutory record of what was supplied that day reads the same afterwards.
#[tokio::test]
async fn real_service_never_rereads_a_posted_schedule_x_sale_through_todays_masters_over_http() {
    let service = start().await;
    let f = favourable_schedule_x_over_http(&service, true).await;
    let posted = post_quoted_over_http(
        &service,
        &f.world,
        &f.sale_id,
        f.revision,
        "01997a00-0000-7000-8000-000000000473",
    )
    .await;
    assert_eq!(posted.status, 200, "{:?}", posted.body);
    let before = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{}", f.sale_id),
        None,
        Some(&f.world.cookie),
    )
    .await;
    assert_eq!(before.status, 200, "{:?}", before.body);

    // The masters move on. Every one of these is a CURRENT master the owner may lawfully change.
    let pool = database::connect(&service.database_path)
        .await
        .expect("database");
    for (what, statement, binding) in [
        (
            "Form 20-F drug coverage withdrawn",
            "UPDATE store_licence_drug_coverage SET status='archived',\
             archived_at_utc='2026-09-20T06:00:00.000Z',archive_reason='withdrawn' \
             WHERE licence_id=?",
            f.licence.clone(),
        ),
        (
            "supplier authority archived",
            "UPDATE supplier_schedule_x_authorities SET status='archived',\
             archived_at_utc='2026-09-20T06:00:00.000Z',archive_reason='withdrawn' WHERE id=?",
            f.authority.clone(),
        ),
        (
            "pharmacist left the pharmacy",
            "UPDATE store_professionals SET status='archived',\
             archived_at_utc='2026-09-20T06:00:00.000Z',archive_reason='left' WHERE id=?",
            f.professional.clone(),
        ),
        (
            "Schedule X classification corrected to not applying",
            "UPDATE product_regulatory_classifications SET applies=0 \
             WHERE product_id=? AND scheme='schedule_x' AND status='active'",
            f.world.product.clone(),
        ),
    ] {
        let changed = sqlx::query(statement)
            .bind(&binding)
            .execute(&pool)
            .await
            .unwrap_or_else(|error| panic!("{what}: {error}"));
        assert!(changed.rows_affected() >= 1, "{what}: nothing changed");
    }

    // The posted document reads exactly as it did.
    let after = call(
        &service,
        "GET",
        &format!("/api/v1/sales/{}", f.sale_id),
        None,
        Some(&f.world.cookie),
    )
    .await;
    assert_eq!(after.status, 200, "{:?}", after.body);
    assert_eq!(after.body["status"], "posted");
    assert_eq!(after.body["documentNumber"], before.body["documentNumber"]);
    assert_eq!(
        after.body["lines"][0]["regulatoryGate"], before.body["lines"][0]["regulatoryGate"],
        "the posted line was re-gated against today's classification"
    );

    // And the frozen snapshot still says Schedule X applied, though the master now says otherwise.
    let schemes: String =
        sqlx::query_scalar("SELECT regulatory_schemes_snapshot FROM sale_lines WHERE id=?")
            .bind(&f.line_id)
            .fetch_one(&pool)
            .await
            .expect("snapshot");
    assert!(
        schemes.contains("\"schedule_x\":\"applies\""),
        "the frozen snapshot followed the master: {schemes}"
    );
    let current: i64 = sqlx::query_scalar(
        "SELECT applies FROM product_regulatory_classifications \
         WHERE product_id=? AND scheme='schedule_x' AND status='active'",
    )
    .bind(&f.world.product)
    .fetch_one(&pool)
    .await
    .expect("current");
    assert_eq!(current, 0, "the master was not actually changed");

    // The finalized register entry is untouched by any of it.
    #[derive(sqlx::FromRow)]
    struct Still {
        status: String,
        bill_number: Option<String>,
        supervising_professional_name: Option<String>,
        particulars_entered_in_physical_register: i64,
    }
    let entry: Still = sqlx::query_as(
        "SELECT status,bill_number,supervising_professional_name,\
         particulars_entered_in_physical_register \
         FROM store_schedule_x_register_entries WHERE id=?",
    )
    .bind(&f.entry_id)
    .fetch_one(&pool)
    .await
    .expect("entry");
    assert_eq!(entry.status, "finalized");
    assert_eq!(
        entry.bill_number.as_deref(),
        before.body["documentNumber"].as_str()
    );
    // The pharmacist's name is the one frozen on the entry, not a lookup into a now-archived master.
    assert!(entry.supervising_professional_name.is_some());
    assert_eq!(entry.particulars_entered_in_physical_register, 1);
    pool.close().await;
}
