//! Real HTTP/TLS exchanges over a loopback CONNECT proxy. The request URL
//! keeps the production publisher hostname, scheme and port throughout.
use super::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};

const CERT: &[u8] = include_bytes!("../../test/fixtures/frontiers/test-cert.pem");
const KEY: &[u8] = include_bytes!("../../test/fixtures/frontiers/test-key.pem");
const URL: &str = "https://www.frontiersin.org/resource.png";

thread_local! {
    static OVERRIDE: std::cell::RefCell<Option<reqwest::Client>> = const { std::cell::RefCell::new(None) };
}

// This non-Send guard confines fixture trust to one test thread and restores
// prior ownership on drop. Public-run tests use a separate process and a
// current-thread runtime, so no concurrent task can inherit this override.
pub(crate) struct OverrideGuard(Option<reqwest::Client>, std::marker::PhantomData<std::rc::Rc<()>>);
pub(crate) fn override_client(client: reqwest::Client) -> OverrideGuard {
    OverrideGuard(OVERRIDE.with(|slot| slot.replace(Some(client))), std::marker::PhantomData)
}
pub(crate) fn current_client() -> Option<reqwest::Client> {
    OVERRIDE.with(|slot| slot.borrow().clone())
}
impl Drop for OverrideGuard {
    fn drop(&mut self) { OVERRIDE.with(|slot| { slot.replace(self.0.take()); }); }
}

pub(crate) enum Script { Reply(Vec<u8>), Delay, DelayBody, OversizeStream }

fn read_headers(stream: &mut impl Read) -> String {
    let mut bytes = Vec::new();
    let mut byte = [0];
    while !bytes.ends_with(b"\r\n\r\n") && bytes.len() < 16384 {
        if stream.read(&mut byte).expect("read fixture request") == 0 { break; }
        bytes.push(byte[0]);
    }
    String::from_utf8_lossy(&bytes).into()
}

pub(crate) fn fixture(scripts: Vec<Script>, timeout: Duration) -> (reqwest::Client, std::thread::JoinHandle<usize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth().with_single_cert(
            vec![CertificateDer::from_pem_slice(CERT).unwrap()],
            PrivateKeyDer::from_pem_slice(KEY).unwrap(),
        ).unwrap();
    let thread = std::thread::spawn(move || {
        let mut count = 0;
        for script in scripts {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            let mut socket: TcpStream = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
                    _ => return count,
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            socket.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
            let connect = read_headers(&mut socket);
            assert!(connect.starts_with("CONNECT www.frontiersin.org:443 ") || connect.starts_with("CONNECT public-pages-files-2025.frontiersin.org:443 "), "{connect}");
            socket.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").unwrap();
            let connection = rustls::ServerConnection::new(Arc::new(config.clone())).unwrap();
            let mut tls = rustls::StreamOwned::new(connection, socket);
            assert!(read_headers(&mut tls).starts_with("GET "));
            count += 1;
            match script {
                Script::Reply(bytes) => { let _ = tls.write_all(&bytes); let _ = tls.flush(); }
                Script::Delay => std::thread::sleep(Duration::from_millis(150)),
                Script::DelayBody => {
                    tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\npartial").unwrap();
                    tls.flush().unwrap();
                    std::thread::sleep(Duration::from_millis(150));
                }
                Script::OversizeStream => {
                    if tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").is_ok() {
                        let chunk = vec![b'x'; 1024 * 1024];
                        for _ in 0..51 {
                            if tls.write_all(b"100000\r\n").and_then(|_| tls.write_all(&chunk)).and_then(|_| tls.write_all(b"\r\n")).is_err() { break; }
                        }
                        let _ = tls.write_all(b"0\r\n\r\n");
                    }
                }
            }
        }
        count
    });
    let client = publisher_client_builder(timeout)
        .add_root_certificate(reqwest::Certificate::from_pem(CERT).unwrap())
        .proxy(reqwest::Proxy::all(format!("http://{address}")).unwrap())
        .build().unwrap();
    (client, thread)
}

pub(crate) fn reply(status: &str, extra: &str, body: &[u8]) -> Script {
    let mut response = format!("HTTP/1.1 {status}\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n", body.len()).into_bytes();
    response.extend_from_slice(body);
    Script::Reply(response)
}

pub(crate) async fn fetch(client: &reqwest::Client, url: &str) -> Result<PublisherResponse, Box<dyn std::error::Error>> {
    publisher_get(client, url, 1).await
}

#[tokio::test]
async fn publisher_transport_body_timeout_reports_url_and_timeout() {
    let (client, server) = fixture(vec![Script::DelayBody], Duration::from_millis(75));
    let error = publisher_get(&client, URL, 1).await.unwrap_err().to_string();
    assert!(error.contains(URL) && error.contains("timeout"), "{error}");
    assert_eq!(server.join().unwrap(), 1);
}

#[tokio::test]
async fn publisher_transport_follows_scoped_redirect_and_records_final_url() {
    let (client, server) = fixture(vec![reply("302 Found", "Location: https://public-pages-files-2025.frontiersin.org/final.png\r\n", b""), reply("200 OK", "", b"\x89PNG\r\n\x1a\nimage")], Duration::from_secs(2));
    let response = publisher_get(&client, URL, 1).await.unwrap();
    assert_eq!(response.final_url, "https://public-pages-files-2025.frontiersin.org/final.png");
    assert_eq!(response.bytes, b"\x89PNG\r\n\x1a\nimage");
    assert_eq!(server.join().unwrap(), 2);
}

#[tokio::test]
async fn publisher_transport_rejects_external_redirect_without_contacting_target() {
    let (client, server) = fixture(vec![reply("302 Found", "Location: https://example.invalid/file\r\n", b"")], Duration::from_secs(2));
    assert!(publisher_get(&client, URL, 1).await.unwrap_err().to_string().contains("redirect"));
    assert_eq!(server.join().unwrap(), 1);
}

#[tokio::test]
async fn publisher_transport_stops_at_redirect_limit() {
    let scripts = (0..10).map(|_| reply("302 Found", "Location: https://www.frontiersin.org/again\r\n", b"")).collect();
    let (client, server) = fixture(scripts, Duration::from_secs(2));
    assert!(publisher_get(&client, URL, 1).await.unwrap_err().to_string().contains("redirect"));
    assert_eq!(server.join().unwrap(), 10);
}

#[tokio::test]
async fn publisher_transport_retries_rate_limit_and_preserves_final_success() {
    let (client, server) = fixture(vec![reply("429 Too Many Requests", "Retry-After: 0\r\n", b"rate limited"), reply("200 OK", "", b"complete")], Duration::from_secs(2));
    let response = publisher_get(&client, URL, 2).await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.bytes, b"complete");
    assert_eq!(server.join().unwrap(), 2);
}

#[tokio::test]
async fn publisher_transport_preserves_denial_response() {
    let (client, server) = fixture(vec![reply("403 Forbidden", "", b"denied")], Duration::from_secs(2));
    let response = publisher_get(&client, URL, 1).await.unwrap();
    assert_eq!(response.status, 403);
    assert_eq!(response.bytes, b"denied");
    assert_eq!(server.join().unwrap(), 1);
}

#[tokio::test]
async fn publisher_transport_timeout_reports_url_and_budget() {
    let (client, server) = fixture(vec![Script::Delay], Duration::from_millis(75));
    let error = publisher_get(&client, URL, 1).await.unwrap_err().to_string();
    assert!(error.contains(URL) && error.contains("1 attempts") && error.contains("timeout"), "{error}");
    assert_eq!(server.join().unwrap(), 1);
}

#[tokio::test]
async fn publisher_transport_rejects_declared_and_streamed_oversize_with_url() {
    for script in [Script::Reply(b"HTTP/1.1 200 OK\r\nContent-Length: 52428801\r\nConnection: close\r\n\r\n".to_vec()), Script::OversizeStream] {
        let (client, server) = fixture(vec![script], Duration::from_secs(2));
        let error = publisher_get(&client, URL, 1).await.unwrap_err().to_string();
        assert!(error.contains("50 MB") && error.contains(URL), "{error}");
        assert_eq!(server.join().unwrap(), 1);
    }
}

#[tokio::test]
async fn publisher_transport_rejects_truncated_success_without_returning_partial_bytes() {
    let response = b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 100\r\nConnection: close\r\n\r\n\x89PNG\r\n\x1a\n";
    let (client, server) = fixture(vec![Script::Reply(response.to_vec())], Duration::from_secs(2));
    let error = publisher_get(&client, URL, 1).await.unwrap_err().to_string();
    assert!(error.contains(URL), "{error}");
    assert_eq!(server.join().unwrap(), 1);
}
