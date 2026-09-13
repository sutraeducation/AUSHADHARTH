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
