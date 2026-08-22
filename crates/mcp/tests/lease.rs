#![cfg(feature = "runtime")]

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use cognee_mcp::layout::StateLayout;
use cognee_mcp::lease::{EngineLease, LeaseError, LeaseRuntime};
use serde_json::Value;
use tempfile::tempdir;

struct FakeRuntime {
    host: String,
    pid: u32,
    now: Mutex<DateTime<Utc>>,
    nonces: Mutex<VecDeque<String>>,
    alive: Mutex<HashSet<u32>>,
}

impl FakeRuntime {
    fn new(host: &str, pid: u32, now: DateTime<Utc>, nonces: &[&str]) -> Self {
        Self {
            host: host.to_owned(),
            pid,
            now: Mutex::new(now),
            nonces: Mutex::new(nonces.iter().map(|value| (*value).to_owned()).collect()),
            alive: Mutex::new(HashSet::new()),
        }
    }

    fn set_now(&self, now: DateTime<Utc>) {
        *self.now.lock().expect("clock lock") = now;
    }

    fn set_alive(&self, pid: u32, alive: bool) {
        let mut processes = self.alive.lock().expect("process lock");
        if alive {
            processes.insert(pid);
        } else {
            processes.remove(&pid);
        }
    }
}

impl LeaseRuntime for FakeRuntime {
    fn hostname(&self) -> String {
        self.host.clone()
    }

    fn process_id(&self) -> u32 {
        self.pid
    }

    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().expect("clock lock")
    }

    fn next_nonce(&self) -> String {
        self.nonces
            .lock()
            .expect("nonce lock")
            .pop_front()
            .expect("fixture nonce")
    }

    fn process_is_alive(&self, pid: u32) -> bool {
        self.alive.lock().expect("process lock").contains(&pid)
    }
}

fn instant(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_777_000_000 + seconds, 0)
        .single()
        .expect("fixture timestamp")
}

fn lease_for(layout: &StateLayout, runtime: Arc<FakeRuntime>) -> EngineLease {
    EngineLease::with_runtime(layout.clone(), Duration::from_secs(180), runtime)
}

#[test]
fn atomic_directory_acquisition_has_one_owner_and_secret_free_metadata() {
    let root = tempdir().expect("temp root");
    let layout = StateLayout::under(root.path().join("cognee"));
    layout.ensure_private().expect("private layout");
    let runtime = Arc::new(FakeRuntime::new(
        "host-a",
        101,
        instant(0),
        &["nonce-one", "nonce-two"],
    ));
    runtime.set_alive(101, true);
    let lease = Arc::new(lease_for(&layout, runtime));
    let start = Arc::new(Barrier::new(2));

    let contenders: Vec<_> = (0..2)
        .map(|_| {
            let lease = lease.clone();
            let start = start.clone();
            thread::spawn(move || {
                start.wait();
                lease
                    .try_acquire("drain")
                    .expect("lease acquisition")
                    .is_some()
            })
        })
        .collect();
    let wins = contenders
        .into_iter()
        .map(|contender| contender.join().expect("contender thread"))
        .filter(|won| *won)
        .count();
    assert_eq!(wins, 1);

    let owner_path = layout.locks.join("engine/owner.json");
    let owner: Value =
        serde_json::from_slice(&fs::read(owner_path).expect("owner bytes")).expect("owner JSON");
    let mut keys: Vec<_> = owner
        .as_object()
        .expect("owner object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "heartbeat_at",
            "host",
            "nonce",
            "operation",
            "pid",
            "started_at"
        ]
    );
    assert_eq!(owner["operation"], "drain");
}

#[test]
fn reclaim_requires_stale_dead_same_host_owner() {
    let root = tempdir().expect("temp root");
    let layout = StateLayout::under(root.path().join("cognee"));
    layout.ensure_private().expect("private layout");
    let owner_runtime = Arc::new(FakeRuntime::new(
        "host-a",
        100,
        instant(0),
        &["owner-nonce"],
    ));
    owner_runtime.set_alive(100, true);
    let owner = lease_for(&layout, owner_runtime.clone())
        .try_acquire("drain")
        .expect("owner acquisition")
        .expect("owner guard");

    let live_runtime = Arc::new(FakeRuntime::new(
        "host-a",
        200,
        instant(181),
        &["live-contender"],
    ));
    live_runtime.set_alive(100, true);
    assert!(
        lease_for(&layout, live_runtime)
            .try_acquire("recall")
            .expect("live contender")
            .is_none()
    );

    let cross_host = Arc::new(FakeRuntime::new(
        "host-b",
        300,
        instant(1_000),
        &["cross-host"],
    ));
    assert!(
        lease_for(&layout, cross_host)
            .try_acquire("recall")
            .expect("cross-host contender")
            .is_none()
    );

    let dead_runtime = Arc::new(FakeRuntime::new(
        "host-a",
        400,
        instant(181),
        &["replacement-nonce", "stale-path-nonce"],
    ));
    dead_runtime.set_alive(100, false);
    let replacement = lease_for(&layout, dead_runtime)
        .try_acquire("recover")
        .expect("dead-owner reclaim")
        .expect("replacement guard");
    assert_eq!(replacement.nonce(), "replacement-nonce");
    assert!(matches!(owner.verify(), Err(LeaseError::LeaseLost)));
    replacement.release().expect("replacement release");
    assert!(!layout.locks.join("engine").exists());
}

#[test]
fn heartbeat_and_release_are_nonce_fenced() {
    let root = tempdir().expect("temp root");
    let layout = StateLayout::under(root.path().join("cognee"));
    layout.ensure_private().expect("private layout");
    let runtime = Arc::new(FakeRuntime::new(
        "host-a",
        500,
        instant(0),
        &["guard-nonce"],
    ));
    runtime.set_alive(500, true);
    let mut guard = lease_for(&layout, runtime.clone())
        .try_acquire("drain")
        .expect("acquisition")
        .expect("guard");

    runtime.set_now(instant(30));
    guard.heartbeat().expect("heartbeat");
    let owner_path = layout.locks.join("engine/owner.json");
    let mut owner: Value =
        serde_json::from_slice(&fs::read(&owner_path).expect("owner bytes")).expect("owner JSON");
    assert_eq!(owner["heartbeat_at"], instant(30).to_rfc3339());

    owner["nonce"] = Value::String("replacement".to_owned());
    fs::write(
        &owner_path,
        serde_json::to_vec(&owner).expect("tampered JSON"),
    )
    .expect("tamper owner nonce");
    assert!(matches!(guard.verify(), Err(LeaseError::LeaseLost)));
    assert!(matches!(guard.release(), Err(LeaseError::LeaseLost)));
    assert!(layout.locks.join("engine").exists());
}
