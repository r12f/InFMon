use super::*;
use tempfile::TempDir;

#[test]
fn open_and_lock() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("stats.sock");
    std::fs::write(&sock, b"").unwrap();

    let client = InFMonStatsClient::open(&sock).unwrap();
    assert_eq!(client.path(), sock);

    // Second open should fail (lock held)
    let result = InFMonStatsClient::open(&sock);
    assert!(matches!(result, Err(IpcError::StatsSegmentBusy)));
}

#[test]
fn snapshot_returns_empty() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("stats.sock");
    std::fs::write(&sock, b"").unwrap();

    let client = InFMonStatsClient::open(&sock).unwrap();
    let snap = client.snapshot_and_clear().unwrap();
    assert!(snap.descriptors.is_empty());
}

#[test]
fn open_missing_segment_fails() {
    let dir = TempDir::new().unwrap();
    let sock = dir.path().join("nonexistent.sock");
    let result = InFMonStatsClient::open(&sock);
    assert!(matches!(result, Err(IpcError::StatsOpen(_))));
}
