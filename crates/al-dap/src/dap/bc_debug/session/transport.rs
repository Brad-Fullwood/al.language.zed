//! The WebSocket connection under the SignalR hub: TLS verification, the
//! `Host` header and the handshake.

use super::*;

#[derive(Debug)]
pub(super) struct AcceptInvalidCertVerifier {
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for AcceptInvalidCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

pub(super) fn websocket_connector(
    accept_invalid_certs: bool,
) -> Result<Option<tokio_tungstenite::Connector>> {
    if !accept_invalid_certs {
        return Ok(None);
    }

    let algorithms = rustls::crypto::ring::default_provider().signature_verification_algorithms;
    let provider = rustls::crypto::ring::default_provider();
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| DapError::ConnectionFailed(format!("TLS configuration failed: {e}")))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptInvalidCertVerifier { algorithms }))
        .with_no_client_auth();
    Ok(Some(tokio_tungstenite::Connector::Rustls(Arc::new(config))))
}

pub(super) fn websocket_host_header(ws_url: &str) -> Result<String> {
    let parsed = url::Url::parse(ws_url)
        .map_err(|e| DapError::ConnectionFailed(format!("Invalid WebSocket URL: {e}")))?;
    let host = match parsed.host() {
        Some(url::Host::Domain(domain)) => domain.to_string(),
        Some(url::Host::Ipv4(address)) => address.to_string(),
        Some(url::Host::Ipv6(address)) => format!("[{address}]"),
        None => {
            return Err(DapError::ConnectionFailed(
                "WebSocket URL has no host".to_string(),
            ))
        }
    };
    Ok(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

pub(super) fn validate_handshake_and_take_frames(text: &str) -> Result<Vec<String>> {
    let mut parts = text.split('\x1e');
    let handshake = parts.next().unwrap_or_default();
    validate_signalr_handshake_response(handshake)?;
    Ok(parts
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect())
}
