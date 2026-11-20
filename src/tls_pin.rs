//! A TLS client that only talks to a server whose certificate it was told in advance: the SHA-256 fingerprint of the certificate is **pinned** and the handshake is refused
//! unless the certificate the server shows has it. This is how the server reaches a Bambu Lab printer in LAN mode, which shows a certificate that it signed itself, so there is
//! no authority to ask and no name that could be checked.
//!
//! Nothing here turns the verification off. The certificate is not taken because it is there: its fingerprint is compared with the pinned one in constant time, a connection with
//! no fingerprint pinned (or a pinned one that is not 64 hexadecimal digits) is refused before any byte of the conversation is written, and the proof that the server holds the
//! key of that certificate (the signature of the handshake) is checked by the library with its normal algorithms. What is not checked, on purpose, is the chain of authorities,
//! the name and the dates of the certificate: a certificate that is pinned is the identity, and a printer that is on the LAN has none of them.
//!
//! When the certificate is not the pinned one, the refusal says the fingerprint that was shown, so that the person who owns the printer can compare it with what the screen of the
//! printer says and pin it; the server never pins it by itself. The connection is a plain blocking socket with time limits.

use std::fmt;
use std::io;
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, Error as TlsError, SignatureScheme,
    StreamOwned,
};
use sha2::{Digest, Sha256};

use crate::bambu::{trust_check, TrustError};
use crate::sync::Locked;

/// Why a connection was not made.
#[derive(Debug, Clone, PartialEq)]
pub enum PinError {
    /// The certificate is not trusted: none is pinned, another is, or the pin is malformed. Nothing was sent.
    Untrusted(TrustError),
    /// The server could not be reached or the handshake failed for another reason; the text says why.
    Down(String),
}

impl PinError {
    /// A sentence for the person who reads the refusal.
    pub fn text(&self) -> String {
        match self {
            PinError::Untrusted(why) => why.text(),
            PinError::Down(why) => why.clone(),
        }
    }
}

/// The checks of the certificate, which also write down the fingerprint of the certificate that was shown.
struct PinVerifier {
    pinned: Option<String>,
    shown: Arc<Mutex<Option<String>>>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl fmt::Debug for PinVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PinVerifier")
    }
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let fingerprint = hex::encode(Sha256::digest(end_entity.as_ref()));
        *self.shown.locked() = Some(fingerprint.clone());
        match trust_check(self.pinned.as_deref(), &fingerprint) {
            Ok(()) => Ok(ServerCertVerified::assertion()),
            Err(_) => Err(TlsError::General(
                "the certificate of the server is not the pinned one".into(),
            )),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// A connection whose server showed the pinned certificate.
pub struct PinnedStream {
    /// The TLS stream: read and write on it as on a socket.
    pub stream: StreamOwned<ClientConnection, TcpStream>,
    /// The fingerprint of the certificate the server showed (the pinned one).
    pub fingerprint: String,
}

fn io_text(e: &io::Error) -> String {
    format!("{e}")
}

fn address(host: &str, port: u16) -> Result<SocketAddr, PinError> {
    (host, port)
        .to_socket_addrs()
        .map_err(|e| {
            PinError::Down(format!(
                "the address {host}:{port} cannot be resolved: {}",
                io_text(&e)
            ))
        })?
        .next()
        .ok_or_else(|| PinError::Down(format!("the address {host}:{port} has no IP address")))
}

fn server_name(host: &str) -> Result<ServerName<'static>, PinError> {
    match host.parse::<IpAddr>() {
        Ok(ip) => Ok(ServerName::IpAddress(ip.into())),
        Err(_) => ServerName::try_from(host.to_string())
            .map_err(|_| PinError::Down(format!("{host} is not a name or an address"))),
    }
}

/// Connects to `host:port` over TLS and only goes on when the certificate the server shows has the fingerprint `pinned`. With no pin, or a malformed one, the handshake is refused
/// and the refusal says the fingerprint that was shown. `timeout` limits the connection and every read and write after it.
pub fn connect(
    host: &str,
    port: u16,
    pinned: Option<&str>,
    timeout: Duration,
) -> Result<PinnedStream, PinError> {
    let pinned = match pinned {
        None => None,
        Some(text) => Some(
            crate::bambu::normalize_fingerprint(text)
                .ok_or(PinError::Untrusted(TrustError::Malformed))?,
        ),
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let shown = Arc::new(Mutex::new(None));
    let verifier = Arc::new(PinVerifier {
        pinned: pinned.clone(),
        shown: shown.clone(),
        algorithms: provider.signature_verification_algorithms,
    });
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| PinError::Down(format!("TLS cannot be set up: {e}")))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    let mut connection = ClientConnection::new(Arc::new(config), server_name(host)?)
        .map_err(|e| PinError::Down(format!("TLS cannot be set up: {e}")))?;
    let mut socket = TcpStream::connect_timeout(&address(host, port)?, timeout)
        .map_err(|e| PinError::Down(format!("{host}:{port} does not answer: {}", io_text(&e))))?;
    let _ = socket.set_read_timeout(Some(timeout));
    let _ = socket.set_write_timeout(Some(timeout));
    let _ = socket.set_nodelay(true);
    while connection.is_handshaking() {
        if let Err(e) = connection.complete_io(&mut socket) {
            // a certificate that was shown and is not the pinned one is a refusal of trust and not of the network
            let seen = shown.locked().clone();
            if let Some(seen) = seen {
                if let Err(why) = trust_check(pinned.as_deref(), &seen) {
                    return Err(PinError::Untrusted(why));
                }
            }
            return Err(PinError::Down(format!(
                "the TLS handshake with {host}:{port} failed: {}",
                io_text(&e)
            )));
        }
    }
    let fingerprint = shown.locked().clone().ok_or_else(|| {
        PinError::Down("the server finished the handshake with no certificate".into())
    })?;
    // the handshake only finishes when the verifier said yes; the pin is checked once more here so that a change of the verifier alone cannot let a connection through
    trust_check(pinned.as_deref(), &fingerprint).map_err(PinError::Untrusted)?;
    Ok(PinnedStream {
        stream: StreamOwned::new(connection, socket),
        fingerprint,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use rustls::{ServerConfig, ServerConnection};

    /// A server with a certificate it made for the test. Each connection it accepts gets the handshake and then the handler.
    pub(crate) struct TestServer {
        pub port: u16,
        pub fingerprint: String,
    }

    impl TestServer {
        /// A server that answers what it reads upside down.
        pub(crate) fn start(connections: usize) -> TestServer {
            TestServer::start_with(connections, |tls| {
                let mut buffer = [0u8; 64];
                if let Ok(n) = tls.read(&mut buffer) {
                    let mut reply = buffer[..n].to_vec();
                    reply.reverse();
                    let _ = tls.write_all(&reply);
                    let _ = tls.flush();
                }
            })
        }

        pub(crate) fn start_with<F>(connections: usize, handler: F) -> TestServer
        where
            F: Fn(&mut StreamOwned<ServerConnection, TcpStream>) + Send + 'static,
        {
            let made = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
            let der = made.cert.der().clone();
            let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(made.key_pair.serialize_der()));
            let fingerprint = hex::encode(Sha256::digest(der.as_ref()));
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = Arc::new(
                ServerConfig::builder_with_provider(provider)
                    .with_safe_default_protocol_versions()
                    .unwrap()
                    .with_no_client_auth()
                    .with_single_cert(vec![der], key)
                    .unwrap(),
            );
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            thread::spawn(move || {
                for _ in 0..connections {
                    let Ok((mut socket, _)) = listener.accept() else {
                        return;
                    };
                    let mut conn = ServerConnection::new(config.clone()).unwrap();
                    // a client that refuses the certificate ends the handshake with an error: that is the end of this connection
                    if conn.complete_io(&mut socket).is_err() {
                        continue;
                    }
                    handler(&mut StreamOwned::new(conn, socket));
                }
            });
            TestServer { port, fingerprint }
        }
    }

    const SECOND: Duration = Duration::from_secs(5);

    #[test]
    fn the_pinned_certificate_is_accepted_and_the_conversation_goes_through_tls() {
        let server = TestServer::start(1);
        let mut tls = connect("127.0.0.1", server.port, Some(&server.fingerprint), SECOND)
            .unwrap_or_else(|e| panic!("{}", e.text()));
        assert_eq!(tls.fingerprint, server.fingerprint);
        tls.stream.write_all(b"abc").unwrap();
        tls.stream.flush().unwrap();
        let mut got = [0u8; 3];
        tls.stream.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"cba");
    }

    #[test]
    fn the_pin_may_be_written_with_colons_and_capitals() {
        let server = TestServer::start(1);
        let spelled = server
            .fingerprint
            .to_ascii_uppercase()
            .as_bytes()
            .chunks(2)
            .map(|c| String::from_utf8(c.to_vec()).unwrap())
            .collect::<Vec<_>>()
            .join(":");
        assert!(connect("127.0.0.1", server.port, Some(&spelled), SECOND).is_ok());
    }

    #[test]
    fn a_certificate_that_is_not_the_pinned_one_is_refused_and_says_what_it_showed() {
        let server = TestServer::start(1);
        let other = hex::encode(Sha256::digest(b"another certificate"));
        match connect("127.0.0.1", server.port, Some(&other), SECOND) {
            Err(PinError::Untrusted(TrustError::Mismatch { pinned, seen })) => {
                assert_eq!(pinned, other);
                assert_eq!(seen, server.fingerprint);
            }
            Err(e) => panic!("another refusal: {e:?}"),
            Ok(_) => panic!("a certificate that is not the pinned one was accepted"),
        }
    }

    #[test]
    fn with_no_pin_nothing_is_accepted_and_the_refusal_names_the_certificate() {
        let server = TestServer::start(1);
        match connect("127.0.0.1", server.port, None, SECOND) {
            Err(PinError::Untrusted(TrustError::NotPinned { seen })) => {
                assert_eq!(seen, server.fingerprint)
            }
            Err(e) => panic!("another refusal: {e:?}"),
            Ok(_) => panic!("a certificate was accepted with no pin"),
        }
    }

    #[test]
    fn a_malformed_pin_is_refused_before_the_network_is_touched() {
        // the port is one nobody listens on: the pin is read first
        for pin in ["", "abc", &"g".repeat(64), &"a".repeat(63)] {
            assert_eq!(
                connect("127.0.0.1", 9, Some(pin), SECOND).err(),
                Some(PinError::Untrusted(TrustError::Malformed)),
                "{pin}"
            );
        }
    }

    #[test]
    fn a_server_that_does_not_answer_is_down_and_not_untrusted() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let pin = "a".repeat(64);
        assert!(matches!(
            connect("127.0.0.1", port, Some(&pin), Duration::from_millis(500)),
            Err(PinError::Down(_))
        ));
        // something that listens and does not speak TLS
        let mute = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = mute.local_addr().unwrap().port();
        let keeper = thread::spawn(move || {
            let _held = mute.accept();
            thread::sleep(Duration::from_millis(900));
        });
        let started = std::time::Instant::now();
        assert!(matches!(
            connect("127.0.0.1", port, Some(&pin), Duration::from_millis(300)),
            Err(PinError::Down(_))
        ));
        assert!(started.elapsed() < Duration::from_secs(3));
        keeper.join().unwrap();
    }

    #[test]
    fn a_host_that_is_not_a_name_is_down() {
        let pin = "a".repeat(64);
        assert!(matches!(
            connect("not a host", 8883, Some(&pin), SECOND),
            Err(PinError::Down(_))
        ));
    }
}
