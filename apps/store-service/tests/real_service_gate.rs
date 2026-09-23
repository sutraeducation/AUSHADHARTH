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
        (&["schedule_x"][..], "schedule_x_workflow_not_available"),
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
