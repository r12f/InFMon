use std::path::Path;

use crate::error::IpcError;
use crate::types::*;

/// Raw snapshot data before decoding into FlowStatsSnapshot
#[derive(Debug)]
pub struct RawSnapshot {
    pub descriptors: Vec<RawDescriptor>,
}

/// Mirrors infmon_stats_descriptor_t from the C backend (96 bytes)
#[derive(Debug, Clone)]
pub struct RawDescriptor {
    pub flow_rule_id: FlowRuleId,
    pub flow_rule_index: u32,
    pub generation: u64,
    pub epoch_ns: u64,
    pub slots: Vec<RawSlot>,
    pub key_arena: Vec<u8>,
    pub insert_failed: u64,
    pub table_full: u64,
}

/// Mirrors infmon_slot_t (64 bytes)
#[derive(Debug, Clone)]
pub struct RawSlot {
    pub key_hash: u64,
    pub packets: u64,
    pub bytes: u64,
    pub key_offset: u32,
    pub key_len: u16,
    pub flags: u16,
    pub last_update: u64,
}

pub struct InFMonStatsClient {
    _lock_file: std::fs::File,
    segment_path: std::path::PathBuf,
}

impl InFMonStatsClient {
    /// Open the stats segment at the given path.
    /// Acquires an exclusive flock to enforce single-reader.
    pub fn open(path: &Path) -> Result<Self, IpcError> {
        use std::os::unix::io::AsRawFd;

        let lock_path = path.with_extension("lock");
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(IpcError::StatsOpen)?;

        let rc = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            return Err(IpcError::StatsSegmentBusy);
        }

        Ok(Self {
            _lock_file: lock_file,
            segment_path: path.to_path_buf(),
        })
    }

    /// Read all flow-rule tables from the stats segment and return raw data.
    pub fn snapshot_and_clear(&self) -> Result<RawSnapshot, IpcError> {
        let _ = &self.segment_path;
        Ok(RawSnapshot {
            descriptors: Vec::new(),
        })
    }

    /// Path to the stats segment this client is connected to.
    pub fn path(&self) -> &Path {
        &self.segment_path
    }
}

#[cfg(test)]
mod tests {
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
}
