//! Embeds Ferrogram's own Telegram API credentials into release builds.
//!
//! Set `FERROGRAM_API_ID` and `FERROGRAM_API_HASH` in the environment of the
//! build (the release workflow takes them from the repository secrets
//! `TELEGRAM_API_ID` / `TELEGRAM_API_HASH`). Nothing is ever read from a
//! file in the repository. Without them the build still succeeds and the app
//! falls back to runtime env vars or the "own API credentials" setup screen.
//! Malformed values fail the build instead of shipping a broken binary.

fn main() {
    println!("cargo:rerun-if-env-changed=FERROGRAM_API_ID");
    println!("cargo:rerun-if-env-changed=FERROGRAM_API_HASH");
    println!("cargo:rerun-if-changed=build.rs");

    let id = std::env::var("FERROGRAM_API_ID")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let hash = std::env::var("FERROGRAM_API_HASH")
        .ok()
        .filter(|v| !v.trim().is_empty());
    match (id, hash) {
        (None, None) => {}
        (Some(_), None) | (None, Some(_)) => {
            panic!(
                "ferrogram build: set both FERROGRAM_API_ID and FERROGRAM_API_HASH \
                 (or neither) to embed Telegram API credentials"
            );
        }
        (Some(id), Some(hash)) => {
            let id = id.trim();
            let hash = hash.trim();
            match id.parse::<i32>() {
                Ok(n) if n > 0 => {}
                _ => panic!("ferrogram build: FERROGRAM_API_ID must be a positive 32-bit integer"),
            }
            if hash.len() != 32 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
                panic!("ferrogram build: FERROGRAM_API_HASH must be exactly 32 hex characters");
            }
            println!("cargo:rustc-env=FERROGRAM_EMBEDDED_API_ID={id}");
            println!(
                "cargo:rustc-env=FERROGRAM_EMBEDDED_API_HASH={}",
                hash.to_ascii_lowercase()
            );
            // Never print the values themselves in build output.
            println!(
                "cargo:warning=ferrogram: embedding Telegram API credentials (id/hash redacted)"
            );
        }
    }
}
