//! Does the certificate we generate actually validate?
//!
//! Every requirement Apple imposes on a server certificate fails *silently* —
//! a missing SAN, a missing `serverAuth` EKU and a validity period over 398
//! days all present to the user as "it didn't work", with nothing in any log to
//! say which. Reading the generating code proves nothing, because the code
//! looks correct in all three failure modes.
//!
//! So these tests do the only thing that establishes anything: they run a real
//! TLS client against a real listener and make it validate the chain. `rustls`
//! and `webpki` enforce SAN matching, EKU and expiry, so a handshake that
//! completes has cleared the same checks a browser applies.
//!
//! **What this does not establish.** iOS is not rustls. These tests prove the
//! certificate is well-formed and that a strict client accepts it; they cannot
//! prove Apple's trust-store UI behaves as documented, and nothing short of a
//! real iPhone can. Where a claim here stops and a claim about iOS would begin
//! is stated in the PR rather than blurred.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use coyote_bridge::auth::Token;
use coyote_bridge::certs::LocalCa;
use coyote_bridge::state::PlayerSnapshot;
use coyote_bridge::{http, install, tls};
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use tokio_rustls::TlsConnector;

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "coyote-bridge-tls-it-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Decode the PEM chain the CA hands out into something rustls can trust.
fn ca_roots(ca_pem: &str) -> RootCertStore {
    let mut body = String::new();
    for line in ca_pem.lines() {
        if !line.starts_with("-----") {
            body.push_str(line.trim());
        }
    }
    let der = base64_decode(&body).expect("the CA certificate must be valid PEM");

    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(der))
        .expect("the CA must be usable as a trust anchor");
    roots
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (i, c) in TABLE.iter().enumerate() {
        lookup[*c as usize] = i as u8;
    }
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for byte in input.bytes() {
        if byte == b'=' || byte.is_ascii_whitespace() {
            continue;
        }
        let v = lookup[byte as usize];
        if v == 255 {
            return None;
        }
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Start the real HTTPS listener on a loopback port, with a real leaf.
///
/// The leaf is issued for `127.0.0.1` alongside the usual names so the client
/// can actually connect to it; the hostname SAN is exercised separately.
async fn serve_https(tag: &str) -> (SocketAddr, String, Token) {
    let dir = scratch(tag);
    let ca = LocalCa::load_or_generate(&dir).expect("CA");
    let material = ca
        .issue_leaf(&[IpAddr::V4(Ipv4Addr::LOCALHOST)])
        .expect("leaf");
    let ca_pem = material.ca_cert_pem.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let (_snap_tx, snapshot_rx) = watch::channel(PlayerSnapshot::new(String::new()));
    let (cmd_tx, _cmd_rx) = mpsc::channel(4);
    let token = Token::generate();

    let public = Arc::new(install::TlsPublicInfo {
        ca_cert_pem: ca_pem.clone(),
        ca_display_name: material.ca_display_name.clone(),
        hostname: "coyote.local".to_string(),
        http_port: 8787,
        https_port: addr.port(),
        ip: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
    });

    let ctx = Arc::new(http::Ctx {
        snapshot_rx,
        cmd_tx,
        static_dir: None,
        pairing_url: format!("http://127.0.0.1:8787/install?t={}", token.as_str()),
        token: std::sync::RwLock::new(token.clone()),
        allowed_hosts: tls::browser_origins(
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            8787,
            addr.port(),
        ),
        on_token_rotated: None,
        tls: Some(public),
    });

    let (certs_tx, certs_rx) = watch::channel(material);
    // Held so the channel stays open for the life of the listener.
    std::mem::forget(certs_tx);
    tokio::spawn(tls::run(listener, ctx, certs_rx));

    (addr, ca_pem, token)
}

async fn https_get(addr: SocketAddr, server_name: &str, ca_pem: &str, path: &str) -> String {
    let config = ClientConfig::builder()
        .with_root_certificates(ca_roots(ca_pem))
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let name = ServerName::try_from(server_name.to_string()).expect("server name");

    let tcp = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let mut stream = connector.connect(name, tcp).await.expect(
        "the TLS handshake must succeed — a failure here means the certificate would be \
         rejected by a browser too",
    );

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {server_name}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut body = Vec::new();
    let _ = stream.read_to_end(&mut body).await;
    String::from_utf8_lossy(&body).to_string()
}

#[tokio::test]
async fn a_strict_client_completes_the_handshake_and_gets_a_response() {
    let (addr, ca_pem, _token) = serve_https("handshake").await;

    // Validated against the IP SAN. Reaching `/trustcheck` at all is exactly
    // the signal the install page's second stage relies on.
    let response = https_get(addr, "127.0.0.1", &ca_pem, "/trustcheck").await;
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "expected 200, got: {response}"
    );
    assert!(response.contains("trusted"));
}

#[tokio::test]
async fn the_trust_check_is_readable_cross_origin() {
    // The install page is on a different origin by necessity — plain HTTP,
    // different port — so without this header the check cannot read its own
    // answer and would report "not trusted" for a perfectly trusted
    // certificate. That would be worse than having no check at all.
    let (addr, ca_pem, _token) = serve_https("cors").await;
    let response = https_get(addr, "127.0.0.1", &ca_pem, "/trustcheck").await;
    assert!(
        response.to_lowercase().contains("access-control-allow-origin"),
        "the trust check must be readable from the install page's origin"
    );
}

#[tokio::test]
async fn a_client_that_does_not_know_the_ca_is_refused() {
    // The other half of the claim: the certificate is trusted *because* the CA
    // is, not because it is accepted by anything that asks. If this passed, the
    // install step would be pointless and the security story fictional.
    let (addr, _ca_pem, _token) = serve_https("stranger").await;

    let config = ClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let name = ServerName::try_from("127.0.0.1".to_string()).unwrap();
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();

    assert!(
        connector.connect(name, tcp).await.is_err(),
        "a client with an empty trust store must reject this certificate"
    );
}

#[tokio::test]
async fn the_install_page_is_reachable_without_a_token() {
    // The bootstrap property. A phone cannot present a credential it has not
    // been given, so if this ever starts returning 401 the entire flow
    // deadlocks — and it would deadlock silently, at the one moment the user
    // has no way to diagnose it.
    let (addr, ca_pem, _token) = serve_https("ungated").await;

    let page = https_get(addr, "127.0.0.1", &ca_pem, "/install").await;
    assert!(page.starts_with("HTTP/1.1 200"), "got: {}", &page[..60.min(page.len())]);
    assert!(page.contains("Certificate&nbsp;Trust&nbsp;Settings"));

    let cert = https_get(addr, "127.0.0.1", &ca_pem, "/ca.crt").await;
    assert!(cert.contains("BEGIN CERTIFICATE"));
    assert!(
        !cert.contains("PRIVATE KEY"),
        "the install endpoint must never serve key material"
    );
}

#[tokio::test]
async fn the_state_relay_is_reachable_as_wss_with_a_token_and_an_origin() {
    // The acceptance-critical path, and the one that would fail last and most
    // confusingly. A page served over HTTPS is forbidden from opening `ws://`,
    // so the relay has to work over TLS or the app loads and then cannot talk
    // to the bridge at all — which looks like a bridge fault rather than a
    // transport rule.
    //
    // The Origin sent here is the HTTPS one the phone will actually present.
    // If `tls::browser_origins` ever stops including it, this fails here rather
    // than on someone's phone at the last step.
    let (addr, ca_pem, token) = serve_https("wss").await;

    let config = ClientConfig::builder()
        .with_root_certificates(ca_roots(&ca_pem))
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let name = ServerName::try_from("127.0.0.1".to_string()).unwrap();
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let tls_stream = connector.connect(name, tcp).await.expect("handshake");

    let url = format!("wss://127.0.0.1:{}/ws?t={}", addr.port(), token.as_str());
    let request = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
        url.as_str(),
    )
    .map(|mut req| {
        req.headers_mut().insert(
            "Origin",
            format!("https://127.0.0.1:{}", addr.port()).parse().unwrap(),
        );
        req
    })
    .unwrap();

    let (mut ws, _response) = tokio_tungstenite::client_async(request, tls_stream)
        .await
        .expect("the relay must accept a wss connection carrying a valid token");

    // The hello frame proves the relay is actually running, not merely that the
    // upgrade was accepted.
    use futures::StreamExt;
    let first = ws.next().await.expect("a frame").expect("not an error");
    assert!(
        first.to_text().unwrap().contains("hello"),
        "expected the hello frame, got: {first:?}"
    );
}

#[tokio::test]
async fn the_state_relay_still_refuses_a_bad_token_over_tls() {
    // TLS must not become an accidental bypass. Encrypting the transport
    // changes nothing about authorization, and this is the assertion that says
    // so in code rather than in a comment.
    let (addr, ca_pem, _token) = serve_https("wss-refused").await;

    let config = ClientConfig::builder()
        .with_root_certificates(ca_roots(&ca_pem))
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let name = ServerName::try_from("127.0.0.1".to_string()).unwrap();
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let tls_stream = connector.connect(name, tcp).await.expect("handshake");

    let url = format!("wss://127.0.0.1:{}/ws?t=not-the-token", addr.port());
    let request = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
        url.as_str(),
    )
    .unwrap();

    assert!(
        tokio_tungstenite::client_async(request, tls_stream)
            .await
            .is_err(),
        "a secure transport must not confer authorization"
    );
}

#[tokio::test]
async fn the_certificate_covers_the_mdns_hostname() {
    // `coyote.local` is what the QR advertises and what the phone will ask for,
    // so a leaf that only covered the IP would fail on the one address that is
    // meant to be stable. Checked by making rustls validate against that name —
    // it enforces SAN matching, so this passing means the SAN is present and
    // correct.
    let (addr, ca_pem, _token) = serve_https("hostname").await;

    let config = ClientConfig::builder()
        .with_root_certificates(ca_roots(&ca_pem))
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let name = ServerName::try_from("coyote.local".to_string()).unwrap();
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();

    assert!(
        connector.connect(name, tcp).await.is_ok(),
        "the leaf must be valid for coyote.local, which is the name on the QR"
    );
}

#[tokio::test]
async fn a_name_the_certificate_does_not_cover_is_rejected() {
    // Guards against the lazy fix for the previous test — a wildcard, or SAN
    // matching quietly disabled. The certificate should be valid for the names
    // we chose and no others.
    let (addr, ca_pem, _token) = serve_https("wrongname").await;

    let config = ClientConfig::builder()
        .with_root_certificates(ca_roots(&ca_pem))
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let name = ServerName::try_from("not-the-bridge.local".to_string()).unwrap();
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();

    assert!(
        connector.connect(name, tcp).await.is_err(),
        "the leaf must not validate for a name it was never issued for"
    );
}
