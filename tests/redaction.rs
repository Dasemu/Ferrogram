//! Proves secrets never reach formatted log output (architecture §6).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use ferrogram::auth::secrets::Secret;
use ferrogram::error::{Phone, Redacted};
use ferrogram::logging::ScrubMakeWriter;
use ferrogram::model::{Chat, ChatKind, Message};

const PHONE: &str = "+34 600 123 412";
const BODY: &str = "my bank pin is 4321";
const TOKEN: &str = "AUTHKEY-0xDEADBEEF";

#[test]
fn log_output_contains_no_secret_fixtures() {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(ScrubMakeWriter::buffer(buf.clone()))
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let phone = Phone::new(PHONE);
        let msg = Message::text(1, 2, false, 0, BODY);
        let chat = Chat::new(1, ChatKind::User, "Alice Secret-Name");
        let secret = Secret::new(TOKEN.as_bytes().to_vec());
        tracing::info!(phone = %phone, "requesting code");
        tracing::info!(?phone, ?msg, ?chat, ?secret, "debug dump");
        tracing::info!(body = %Redacted(BODY), "sending");
        // Even a careless raw phone number is scrubbed by the writer.
        tracing::warn!("raw number {}", "+15550109999");
    });
    let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(out.contains("requesting code"), "{out}");
    for needle in [
        "600 123",
        "600123",
        BODY,
        "4321",
        TOKEN,
        "DEADBEEF",
        "Secret-Name",
        "5550109999",
    ] {
        assert!(!out.contains(needle), "leaked {needle:?} in:\n{out}");
    }
    assert!(out.contains("+34•••••••12"));
    assert!(out.contains("[REDACTED]"));
}
