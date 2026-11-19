#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Opt-in test against a REAL Secret Service (gnome-keyring / KWallet).
//!
//! Ignored by default. Run with a running, unlocked Secret Service:
//!
//! ```text
//! FERRO_REAL_KEYRING=1 dbus-run-session -- sh -c \
//!   'echo | gnome-keyring-daemon --unlock --components=secrets >/dev/null; \
//!    cargo test --test real_keyring -- --ignored'
//! ```
//!
//! The keyring credential store is initialised once per process, on the first
//! `Entry::new`, so every calling context runs in its own child process.

use std::process::Command;
use std::sync::Arc;

use ferrogram::auth::secrets::{KeyringStore, Secret, SecretStore};
use ferrogram::auth::vault::Vault;

const CASES: &[&str] = &[
    "main_before_runtime",
    "main_after_runtime",
    "runtime_worker",
    "current_thread_worker",
    "spawn_blocking",
    "app_helpers",
];

fn roundtrip(store: &dyn SecretStore, key: &str) {
    store.delete(key).expect("pre-delete");
    assert!(store.get(key).expect("get missing").is_none());
    store
        .set(key, &Secret::new(b"s3cret".to_vec()))
        .expect("set");
    assert_eq!(
        store.get(key).expect("get").expect("some").expose(),
        b"s3cret"
    );
    store
        .set(key, &Secret::new(b"s4cret".to_vec()))
        .expect("overwrite");
    assert_eq!(
        store.get(key).expect("get2").expect("some").expose(),
        b"s4cret"
    );
    store.delete(key).expect("delete");
    assert!(store.get(key).expect("get after delete").is_none());
    store.delete(key).expect("delete missing is ok");
}

fn run_case(case: &str) {
    let key = format!("real-keyring-test-{case}");
    let rt = || {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime")
    };
    match case {
        "main_before_runtime" => {
            roundtrip(&KeyringStore, &key);
            let _rt = rt();
        }
        "main_after_runtime" => {
            let rt = rt();
            let _guard = rt.enter();
            roundtrip(&KeyringStore, &key);
        }
        "runtime_worker" => rt().block_on(async { roundtrip(&KeyringStore, &key) }),
        "current_thread_worker" => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(async { roundtrip(&KeyringStore, &key) }),
        "spawn_blocking" => rt().block_on(async {
            tokio::task::spawn_blocking(move || roundtrip(&KeyringStore, &key))
                .await
                .expect("join");
        }),
        "app_helpers" => rt().block_on(async {
            let store: Arc<dyn SecretStore> = Arc::new(KeyringStore);
            let dir = tempfile::tempdir().expect("tempdir");
            let vault = Vault::new(store, dir.path().join("session.vault"));
            tokio::task::spawn_blocking(move || {
                vault.clear().expect("clear");
                assert!(vault.load().expect("load empty").is_none());
                vault.save(b"blob").expect("save");
                assert_eq!(vault.load().expect("load").expect("some").expose(), b"blob");
                vault.clear().expect("clear2");
            })
            .await
            .expect("join");
        }),
        other => panic!("unknown case {other}"),
    }
}

/// Child entry point: only does something when `FERRO_KR_CASE` is set.
#[test]
#[ignore = "real Secret Service; run via real_keyring_all_contexts"]
fn case_runner() {
    if let Ok(case) = std::env::var("FERRO_KR_CASE") {
        run_case(&case);
    }
}

#[test]
#[ignore = "needs a running Secret Service and FERRO_REAL_KEYRING=1"]
fn real_keyring_all_contexts() {
    if std::env::var_os("FERRO_REAL_KEYRING").is_none() {
        eprintln!("FERRO_REAL_KEYRING not set; skipping");
        return;
    }
    let exe = std::env::current_exe().expect("exe");
    let mut failed = Vec::new();
    for case in CASES {
        let out = Command::new(&exe)
            .args(["case_runner", "--exact", "--ignored", "--nocapture"])
            .env("FERRO_KR_CASE", case)
            .output()
            .expect("spawn child");
        if out.status.success() {
            eprintln!("ok: {case}");
        } else {
            failed.push(format!(
                "{case}: {}",
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .take(8)
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
        }
    }
    assert!(
        failed.is_empty(),
        "keyring failures:\n{}",
        failed.join("\n")
    );
}
