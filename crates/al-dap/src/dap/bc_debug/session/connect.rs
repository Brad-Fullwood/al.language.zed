//! The SignalR negotiate and WebSocket handshake.
//!
//! `accept_invalid_certs` builds a connector that skips certificate
//! verification; every other path uses the platform verifier. The handshake
//! response can already carry coalesced SignalR frames, so those are taken
//! from the buffer rather than waited for again.

use std::sync::atomic::AtomicI64;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use reqwest::header::AUTHORIZATION;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, warn};

use crate::dap::{DapError, Result};

use super::super::session_config::BcDebugConfig;
use super::super::wire::{
    percent_encode_url, redact_connection_token, resolve_negotiate_connection,
    validate_signalr_handshake_response, SignalRMessage,
};
use super::{
    route_signalr_message, BcDebugSession, COMPLETION_CHANNEL_CAPACITY, EVENT_CHANNEL_CAPACITY,
};

#[derive(Debug)]
struct AcceptInvalidCertVerifier {
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

fn websocket_connector(accept_invalid_certs: bool) -> Result<Option<tokio_tungstenite::Connector>> {
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

fn websocket_host_header(ws_url: &str) -> Result<String> {
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

fn validate_handshake_and_take_frames(text: &str) -> Result<Vec<String>> {
    let mut parts = text.split('\x1e');
    let handshake = parts.next().unwrap_or_default();
    validate_signalr_handshake_response(handshake)?;
    Ok(parts
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect())
}

impl BcDebugSession {
    pub async fn connect(config: &BcDebugConfig, access_token: &str) -> Result<Self> {
        if config.accept_invalid_certs {
            // Match the warn-on-construction parity from BcClient::new at
            // bc_client.rs:99. Without this the DAP path silently
            // disables TLS certificate validation when launch.json sets
            // accept_invalid_certs=true.
            al_bc::http_auth::warn_insecure_tls("DAP SignalR debug");
        }
        let hub_url = config.debug_hub_url();

        let negotiate_url = format!("{hub_url}/negotiate?negotiateVersion=1");
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(config.accept_invalid_certs)
            .build()
            .map_err(|e| DapError::ConnectionFailed(format!("HTTP client error: {e}")))?;

        info!("SignalR negotiate: {negotiate_url}");
        let negotiate_resp = http
            .post(&negotiate_url)
            .header(AUTHORIZATION, format!("Bearer {access_token}"))
            .header("Content-Length", "0")
            .body("")
            .send()
            .await
            .map_err(|e| DapError::ConnectionFailed(format!("SignalR negotiate failed: {e}")))?;

        let status = negotiate_resp.status();
        let resp_text = negotiate_resp.text().await.map_err(|e| {
            DapError::ConnectionFailed(format!("Failed to read negotiate response: {e}"))
        })?;
        let redacted = redact_connection_token(&resp_text);
        debug!(
            "Negotiate response (HTTP {}): {}",
            status,
            &redacted[..redacted.len().min(500)]
        );

        if !status.is_success() {
            return Err(DapError::ConnectionFailed(format!(
                "SignalR negotiate failed (HTTP {status}): {}",
                redact_connection_token(&resp_text)
            )));
        }

        let negotiate: serde_json::Value = serde_json::from_str(&resp_text).map_err(|e| {
            let redacted = redact_connection_token(&resp_text);
            DapError::ConnectionFailed(format!(
                "Bad negotiate JSON: {e}: {}",
                &redacted[..redacted.len().min(200)]
            ))
        })?;

        // Resolve the WebSocket connection identifier from the negotiate
        // response, validating the version the server actually negotiated.
        // We request `negotiateVersion=1`; a spec-compliant
        // server echoes the version it agreed to and, for v1, returns a
        // `connectionToken` distinct from `connectionId`. A server that
        // negotiates down to v0 returns no `connectionToken` and the
        // `connectionId` doubles as the `?id=` value.
        let resolved = resolve_negotiate_connection(&negotiate)?;
        let connection_id = resolved.connection_id;

        let ws_url = hub_url
            .replace("https://", "wss://")
            .replace("http://", "ws://");
        let ws_url = format!("{ws_url}?id={}", percent_encode_url(&resolved.ws_id));
        // Redact the connection_token from the log line — it grants access to
        // the active debug session and must not appear in plaintext logs.
        let log_url = ws_url.split('?').next().unwrap_or(&ws_url);
        info!("SignalR WebSocket: {log_url}?id=<redacted>");

        let request = tokio_tungstenite::tungstenite::http::Request::builder()
            .uri(&ws_url)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tokio_tungstenite::tungstenite::handshake::client::generate_key(),
            )
            .header("Host", websocket_host_header(&ws_url)?)
            .body(())
            .map_err(|e| DapError::ConnectionFailed(format!("WS request build error: {e}")))?;

        let connector = websocket_connector(config.accept_invalid_certs)?;
        let (ws_stream, _) =
            tokio_tungstenite::connect_async_tls_with_config(request, None, false, connector)
                .await
                .map_err(|e| {
                    DapError::ConnectionFailed(format!("WebSocket connect failed: {e}"))
                })?;

        let (mut ws_sink, mut ws_source) = ws_stream.split();

        let handshake = "{\"protocol\":\"json\",\"version\":1}\x1e";
        ws_sink
            .send(tokio_tungstenite::tungstenite::Message::Text(
                handshake.into(),
            ))
            .await
            .map_err(|e| DapError::ConnectionFailed(format!("SignalR handshake failed: {e}")))?;

        // Read handshake response. A SignalR server signals a
        // protocol/version mismatch here via `{"error":...}`; validate it so a
        // rejected handshake fails loudly instead of limping on against an
        // adapter that will misbehave on every later invoke.
        let initial_frames = if let Some(msg) = ws_source.next().await {
            let msg = msg.map_err(|e| DapError::ConnectionFailed(format!("WS read error: {e}")))?;
            debug!("SignalR handshake response: {:?}", msg);
            if let tokio_tungstenite::tungstenite::Message::Text(text) = &msg {
                validate_handshake_and_take_frames(text)?
            } else {
                Vec::new()
            }
        } else {
            return Err(DapError::ConnectionFailed(
                "WebSocket closed before SignalR handshake response".to_string(),
            ));
        };

        info!("SignalR connection established");

        let (ws_tx, mut ws_rx) = mpsc::channel::<String>(32);
        // Deep-but-bounded event channel. Break events take a dedicated
        // unbounded path below to preserve the "Break must never be lost"
        // invariant; everything else drops with a warn on overflow so a
        // hostile or misbehaving server can't OOM the daemon.
        let (event_tx, event_rx) = mpsc::channel::<SignalRMessage>(EVENT_CHANNEL_CAPACITY);
        let (completion_tx, completion_rx) =
            mpsc::channel::<SignalRMessage>(COMPLETION_CHANNEL_CAPACITY);
        // Dedicated channel for Break/Detached/FatalError notifications.
        // Using an unbounded channel ensures events buffered before wait_for_break_event
        // is called are never lost. Each entry is one `bool`, so even a
        // pathological 1M-deep backlog is ~1 MB — bounded in practice by
        // the number of break events the BC server emits in one session.
        let (break_event_tx, break_event_rx) = mpsc::unbounded_channel::<bool>();

        tokio::spawn(async move {
            while let Some(msg) = ws_rx.recv().await {
                let framed = format!("{msg}\x1e"); // SignalR record separator
                if let Err(e) = ws_sink
                    .send(tokio_tungstenite::tungstenite::Message::Text(framed.into()))
                    .await
                {
                    error!("BC SignalR send failed: {e} — debug stream dead");
                    break;
                }
            }
        });

        tokio::spawn(async move {
            for part in initial_frames {
                match serde_json::from_str::<SignalRMessage>(&part) {
                    Ok(msg) => {
                        if !route_signalr_message(msg, &event_tx, &completion_tx, &break_event_tx)
                            .await
                        {
                            return;
                        }
                    }
                    Err(e) => warn!("Failed to parse coalesced SignalR message: {e}: {part}"),
                }
            }

            while let Some(msg) = ws_source.next().await {
                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => {
                        error!("WS read error: {e}");
                        break;
                    }
                };

                let text = match msg {
                    tokio_tungstenite::tungstenite::Message::Text(t) => t,
                    tokio_tungstenite::tungstenite::Message::Ping(_) => continue,
                    tokio_tungstenite::tungstenite::Message::Close(_) => break,
                    _ => continue,
                };

                for part in text.split('\x1e') {
                    let part = part.trim();
                    if part.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<SignalRMessage>(part) {
                        Ok(msg) => {
                            if !route_signalr_message(
                                msg,
                                &event_tx,
                                &completion_tx,
                                &break_event_tx,
                            )
                            .await
                            {
                                return;
                            }
                        }
                        Err(e) => {
                            warn!("Failed to parse SignalR message: {e}: {part}");
                        }
                    }
                }
            }
        });

        Ok(Self {
            ws_tx,
            event_rx: Mutex::new(event_rx),
            completion_rx: Mutex::new(completion_rx),
            next_id: AtomicI64::new(1),
            connection_id,
            is_attached: Mutex::new(false),
            is_stopped: Mutex::new(false),
            expecting_step: Mutex::new(false),
            break_event_rx: Mutex::new(break_event_rx),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn websocket_host_header_preserves_port_and_ipv6_brackets() {
        assert_eq!(
            websocket_host_header("wss://bc.example.test:7049/debug?id=x").unwrap(),
            "bc.example.test:7049"
        );
        assert_eq!(
            websocket_host_header("ws://[2001:db8::1]:8080/debug?id=x").unwrap(),
            "[2001:db8::1]:8080"
        );
        assert_eq!(
            websocket_host_header("wss://bc.example.test/debug?id=x").unwrap(),
            "bc.example.test"
        );
    }

    #[test]
    fn handshake_retains_coalesced_signalr_frames() {
        let frames = validate_handshake_and_take_frames(
            "{}\x1e{\"type\":1,\"target\":\"Break\"}\x1e{\"type\":3,\"invocationId\":\"1\"}\x1e",
        )
        .unwrap();
        assert_eq!(frames.len(), 2);
        assert!(frames[0].contains("Break"));
        assert!(frames[1].contains("invocationId"));
    }

    #[test]
    fn invalid_cert_option_builds_only_the_explicit_insecure_connector() {
        assert!(websocket_connector(false).unwrap().is_none());
        assert!(websocket_connector(true).unwrap().is_some());
    }
}
