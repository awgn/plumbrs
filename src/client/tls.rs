use std::io;
use std::sync::{Arc, OnceLock};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_util::either::Either;

pub type MaybeTlsStream = Either<TcpStream, TlsStream<TcpStream>>;

static TLS_H1: OnceLock<TlsConnector> = OnceLock::new();
static TLS_H2: OnceLock<TlsConnector> = OnceLock::new();

pub fn init(insecure: bool) {
    let _ = TLS_H1.get_or_init(|| build_connector(false, insecure));
    let _ = TLS_H2.get_or_init(|| build_connector(true, insecure));
}

pub async fn connect(
    tcp: TcpStream,
    server_name: &str,
    http2: bool,
) -> io::Result<TlsStream<TcpStream>> {
    let connector = connector(http2);
    let name = ServerName::try_from(server_name.to_owned())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    connector.connect(name, tcp).await
}

fn connector(http2: bool) -> &'static TlsConnector {
    let slot = if http2 { &TLS_H2 } else { &TLS_H1 };
    slot.get().expect("TLS not initialized")
}

fn build_connector(http2: bool, insecure: bool) -> TlsConnector {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("valid TLS versions");

    let mut config = if insecure {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth()
    } else {
        let mut root_store = rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        builder
            .with_root_certificates(root_store)
            .with_no_client_auth()
    };

    config.alpn_protocols = if http2 {
        vec![b"h2".to_vec()]
    } else {
        vec![b"http/1.1".to_vec()]
    };

    TlsConnector::from(Arc::new(config))
}

#[derive(Debug)]
struct NoVerifier;

impl ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
