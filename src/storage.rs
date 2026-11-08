//! Protected local snapshots. All writers share one persistent lock inode.
use crate::{
    contracts::Diagnostic,
    environment::{MAX_PACK_BYTES, Pack, PackData},
};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?
            .sync_all()
    }
    #[cfg(not(windows))]
    {
        File::open(path)?.sync_all()
    }
}

fn io(context: &str, e: std::io::Error) -> Diagnostic {
    Diagnostic::error("LOCAL_IO", format!("{context}: {e}"))
}
pub fn read_bytes(path: &Path, limit: u64) -> Result<Vec<u8>, Diagnostic> {
    let meta = fs::symlink_metadata(path).map_err(|e| io("Cannot inspect input", e))?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(Diagnostic::error(
            "LOCAL_FILE_TYPE",
            "Input must be a regular file, not a symlink.",
        ));
    }
    if meta.len() > limit {
        return Err(Diagnostic::error(
            "LOCAL_SIZE",
            "File exceeds the configured byte budget.",
        ));
    }
    let f = File::open(path).map_err(|e| io("Cannot open input", e))?;
    let mut out = Vec::new();
    f.take(limit + 1)
        .read_to_end(&mut out)
        .map_err(|e| io("Cannot read input", e))?;
    if out.len() as u64 > limit {
        return Err(Diagnostic::error(
            "LOCAL_SIZE",
            "Input grew beyond the byte budget.",
        ));
    }
    Ok(out)
}
pub fn read_pack(path: &Path) -> Result<Pack, Diagnostic> {
    let data = read_bytes(path, MAX_PACK_BYTES)?;
    let p: Pack = serde_json::from_slice(&data)
        .map_err(|e| Diagnostic::error("PACK_JSON", format!("Invalid pack JSON: {e}")))?;
    p.validate()?;
    Ok(p)
}

pub struct Writer {
    _lock: File,
    path: PathBuf,
    parent: PathBuf,
}
impl Drop for Writer {
    fn drop(&mut self) {
        // A forked child can retain the shared open-file description until exec.
        let _ = self._lock.unlock();
    }
}
pub(crate) fn writer_lock_path(path: &Path) -> Result<PathBuf, Diagnostic> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| Diagnostic::error("LOCAL_PATH", "Pack destination needs a filename."))?
        .to_string_lossy();
    Ok(parent.join(format!(".{name}.lock")))
}
impl Writer {
    pub fn acquire(path: &Path) -> Result<Self, Diagnostic> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| io("Cannot create pack directory", e))?;
            #[cfg(unix)]
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .map_err(|e| io("Cannot protect pack directory", e))?;
        }
        #[cfg(unix)]
        {
            let m = fs::metadata(parent).map_err(|e| io("Cannot inspect pack directory", e))?;
            if m.permissions().mode() & 0o022 != 0 {
                return Err(Diagnostic::error(
                    "LOCAL_DIRECTORY_PERMISSIONS",
                    "Pack directory must not be writable by group or others.",
                ));
            }
        }
        if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink() || !m.is_file()) {
            return Err(Diagnostic::error(
                "LOCAL_FILE_TYPE",
                "Pack destination must not be a symlink or non-file.",
            ));
        }
        let lock_path = writer_lock_path(path)?;
        if fs::symlink_metadata(&lock_path)
            .is_ok_and(|m| m.file_type().is_symlink() || !m.is_file())
        {
            return Err(Diagnostic::error(
                "LOCAL_FILE_TYPE",
                "Pack lock must be a regular file.",
            ));
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        options.mode(0o600);
        let lock = options
            .open(&lock_path)
            .map_err(|e| io("Cannot open pack lock", e))?;
        lock.try_lock().map_err(|_|Diagnostic::error("PACK_BUSY","Another writer holds this pack. Readers may continue using the committed snapshot."))?;
        Ok(Self {
            _lock: lock,
            path: path.to_path_buf(),
            parent: parent.to_path_buf(),
        })
    }
    pub fn existing(&self) -> Result<Option<Pack>, Diagnostic> {
        if self.path.exists() {
            Ok(Some(read_pack(&self.path)?))
        } else {
            Ok(None)
        }
    }
    pub fn commit(&self, pack: &Pack) -> Result<(), Diagnostic> {
        self.commit_with(pack, |_| Ok(()), sync_directory)
    }
    fn commit_with(
        &self,
        pack: &Pack,
        stage: impl FnMut(&str) -> Result<(), Diagnostic>,
        sync_parent: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<(), Diagnostic> {
        pack.validate()?;
        let bytes = serde_json::to_vec_pretty(pack)
            .map_err(|_| Diagnostic::error("PACK_SERIALIZATION", "Cannot serialize pack."))?;
        if bytes.len() as u64 > MAX_PACK_BYTES {
            return Err(Diagnostic::error(
                "PACK_SIZE",
                "Pack exceeds the 64 MiB storage budget.",
            ));
        }
        self.commit_bytes_with(&bytes, stage, sync_parent)
    }
    pub fn commit_bytes(&self, bytes: &[u8], limit: u64) -> Result<(), Diagnostic> {
        if bytes.len() as u64 > limit {
            return Err(Diagnostic::error(
                "LOCAL_SIZE",
                "State exceeds its byte budget.",
            ));
        }
        self.commit_bytes_with(bytes, |_| Ok(()), sync_directory)
    }
    pub fn create_new_bytes(&self, bytes: &[u8], limit: u64) -> Result<(), Diagnostic> {
        if bytes.len() as u64 > limit {
            return Err(Diagnostic::error(
                "LOCAL_SIZE",
                "State exceeds its byte budget.",
            ));
        }
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut file = opts
            .open(&self.path)
            .map_err(|e| io("Cannot create new state", e))?;
        let result = file
            .write_all(bytes)
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.sync_all())
            .and_then(|_| sync_directory(&self.parent))
            .map_err(|e| io("Cannot durably initialize state", e));
        if result.is_err() {
            drop(file);
            let _ = fs::remove_file(&self.path);
        }
        result
    }
    /// Commit a previously frozen payload, including its final newline, unchanged.
    pub(crate) fn commit_frozen_bytes(&self, bytes: &[u8], limit: u64) -> Result<(), Diagnostic> {
        if bytes.len() as u64 > limit {
            return Err(Diagnostic::error(
                "LOCAL_SIZE",
                "Snapshot exceeds its byte budget.",
            ));
        }
        self.commit_exact_bytes_with(bytes, |_| Ok(()), sync_directory)
    }
    /// Reconfirm durability after a previous rename became visible without a confirmed sync.
    pub(crate) fn confirm_durable(&self) -> Result<(), Diagnostic> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
            .map_err(|e| io("Cannot open committed snapshot", e))?;
        file.sync_all()
            .and_then(|_| sync_directory(&self.parent))
            .map_err(|e| {
                Diagnostic::error(
                    "PACK_DURABILITY",
                    format!("Snapshot is visible, but durability could not be confirmed: {e}"),
                )
            })
    }
    fn commit_bytes_with(
        &self,
        bytes: &[u8],
        stage: impl FnMut(&str) -> Result<(), Diagnostic>,
        sync_parent: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<(), Diagnostic> {
        let mut persisted = Vec::with_capacity(bytes.len() + 1);
        persisted.extend_from_slice(bytes);
        persisted.push(b'\n');
        self.commit_exact_bytes_with(&persisted, stage, sync_parent)
    }
    fn commit_exact_bytes_with(
        &self,
        bytes: &[u8],
        mut stage: impl FnMut(&str) -> Result<(), Diagnostic>,
        sync_parent: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<(), Diagnostic> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let temp = self
            .parent
            .join(format!(".cqlmatic-{}-{n}.tmp", std::process::id()));
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut file = opts
            .open(&temp)
            .map_err(|e| io("Cannot create candidate snapshot", e))?;
        let result = (|| {
            file.write_all(bytes)
                .map_err(|e| io("Cannot write candidate snapshot", e))?;
            file.sync_all()
                .map_err(|e| io("Cannot sync candidate snapshot", e))?;
            stage("before-rename")?;
            snapshot_fault(&self.path, "before-rename")?;
            fs::rename(&temp, &self.path).map_err(|e| io("Cannot activate snapshot", e))?;
            stage("after-rename")?;
            snapshot_fault(&self.path, "directory-sync")?;
            sync_parent(&self.parent).map_err(|e| {
                Diagnostic::error(
                    "PACK_DURABILITY",
                    format!(
                        "Snapshot activated, but directory durability could not be confirmed: {e}"
                    ),
                )
            })?;
            Ok(())
        })();
        if temp.exists() {
            let _ = fs::remove_file(temp);
        }
        result
    }
}

#[cfg(test)]
thread_local! {
    static SNAPSHOT_FAULT: std::cell::RefCell<Option<(PathBuf, &'static str)>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(crate) fn inject_snapshot_fault(path: &Path, stage: &'static str) {
    SNAPSHOT_FAULT.with(|fault| *fault.borrow_mut() = Some((path.to_path_buf(), stage)));
}
fn snapshot_fault(_path: &Path, _stage: &str) -> Result<(), Diagnostic> {
    #[cfg(test)]
    {
        let triggered = SNAPSHOT_FAULT.with(|fault| {
            let mut fault = fault.borrow_mut();
            if fault
                .as_ref()
                .is_some_and(|(path, stage)| path == _path && *stage == _stage)
            {
                fault.take();
                return true;
            }
            false
        });
        if triggered {
            return Err(Diagnostic::error(
                if _stage == "directory-sync" {
                    "PACK_DURABILITY"
                } else {
                    "TEST_INTERRUPTED"
                },
                "Injected snapshot interruption.",
            ));
        }
    }
    Ok(())
}

pub fn legacy_data(directory: &Path) -> Result<PackData, Diagnostic> {
    let dir = if directory.join("sources.jsonl").exists() {
        directory.to_path_buf()
    } else {
        directory.join("environment")
    };
    let mut total = 0usize;
    let mut load = |name: &str, optional: bool| -> Result<Vec<serde_json::Value>, Diagnostic> {
        let path = dir.join(format!("{name}.jsonl"));
        if optional && !path.exists() {
            return Ok(vec![]);
        }
        let raw = read_bytes(&path, MAX_PACK_BYTES)?;
        total += raw.len();
        if total as u64 > MAX_PACK_BYTES {
            return Err(Diagnostic::error(
                "PACK_SIZE",
                "Combined legacy files exceed 64 MiB.",
            ));
        }
        let text = std::str::from_utf8(&raw)
            .map_err(|_| Diagnostic::error("PACK_UTF8", "Legacy data must be UTF-8."))?;
        text.lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(i, l)| {
                serde_json::from_str(l).map_err(|e| {
                    Diagnostic::error(
                        "PACK_JSON",
                        format!("Invalid {name}.jsonl line {}: {e}", i + 1),
                    )
                })
            })
            .collect()
    };
    let data = PackData {
        sources: load("sources", false)?,
        fields: load("fields", false)?,
        values: load("values", true)?,
        provenance: load("provenance", false)?,
        annotations: load("annotations", true)?,
    };
    data.validate()?;
    Ok(data)
}

pub fn write_receipt(path: &Path, value: &impl serde::Serialize) -> Result<(), Diagnostic> {
    finish_receipt(reserve_receipt(path)?, value)
}
pub fn reserve_receipt(path: &Path) -> Result<File, Diagnostic> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.exists() {
        fs::create_dir_all(parent).map_err(|e| io("Cannot create run directory", e))?;
        #[cfg(unix)]
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|e| io("Cannot protect run directory", e))?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options
        .open(path)
        .map_err(|e| io("Cannot create a new run receipt", e))?;
    file.sync_all()
        .and_then(|_| sync_directory(parent))
        .map_err(|e| io("Cannot durably reserve the run receipt", e))?;
    Ok(file)
}
pub fn finish_receipt(mut file: File, value: &impl serde::Serialize) -> Result<(), Diagnostic> {
    serde_json::to_writer_pretty(&mut file, value)
        .map_err(|_| Diagnostic::error("LOCAL_SERIALIZATION", "Cannot write run receipt."))?;
    file.write_all(b"\n")
        .and_then(|_| file.sync_all())
        .map_err(|e| io("Cannot sync run receipt", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    pub fn temp() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "cqlmatic-storage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        p
    }
    fn empty(date: &str) -> Pack {
        Pack::new(
            crate::contracts::EnvironmentId {
                cloud: crate::contracts::Cloud::Us1,
                tenant: "0".repeat(32),
                view: "search-all".into(),
            },
            date.into(),
            PackData::default(),
        )
        .unwrap()
    }
    #[test]
    fn lock_contention_and_failed_commit_preserve_snapshot() {
        let dir = temp();
        let path = dir.join("pack.json");
        let w = Writer::acquire(&path).unwrap();
        let a = empty("2026-01-01T00:00:00Z");
        w.commit(&a).unwrap();
        let original = fs::read(&path).unwrap();
        assert!(Writer::acquire(&path).is_err());
        let b = empty("2026-02-01T00:00:00Z");
        assert!(
            w.commit_with(
                &b,
                |_| Err(Diagnostic::error("TEST", "injected failure")),
                sync_directory,
            )
            .is_err()
        );
        assert_eq!(original, fs::read(&path).unwrap());
        drop(w);
        assert!(Writer::acquire(&path).is_ok());
        fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn dropped_writer_unlocks_while_inherited_descriptor_remains_open() {
        let dir = temp();
        let path = dir.join("pack.json");
        let writer = Writer::acquire(&path).unwrap();
        let inherited = writer._lock.try_clone().unwrap();
        assert_eq!(Writer::acquire(&path).err().unwrap().code, "PACK_BUSY");
        drop(writer);
        let replacement = Writer::acquire(&path).unwrap();
        drop(inherited);
        drop(replacement);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn reader_sees_complete_new_snapshot_after_rename_failure() {
        let dir = temp();
        let path = dir.join("pack.json");
        let w = Writer::acquire(&path).unwrap();
        w.commit(&empty("2026-01-01T00:00:00Z")).unwrap();
        let b = empty("2026-02-01T00:00:00Z");
        assert!(
            w.commit_with(
                &b,
                |stage| if stage == "after-rename" {
                    Err(Diagnostic::error("TEST", "injected post-rename failure"))
                } else {
                    Ok(())
                },
                sync_directory,
            )
            .is_err()
        );
        assert_eq!(read_pack(&path).unwrap().content_digest, b.content_digest);
        drop(w);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn directory_sync_failure_reports_activated_snapshot() {
        let dir = temp();
        let path = dir.join("pack.json");
        let w = Writer::acquire(&path).unwrap();
        let original = empty("2026-01-01T00:00:00Z");
        w.commit(&original).unwrap();
        let replacement = empty("2026-02-01T00:00:00Z");
        assert_ne!(original.content_digest, replacement.content_digest);
        let error = w
            .commit_with(
                &replacement,
                |_| Ok(()),
                |parent| {
                    assert_eq!(parent, dir.as_path());
                    Err(std::io::Error::other("injected directory sync failure"))
                },
            )
            .unwrap_err();
        assert_eq!(error.code, "PACK_DURABILITY");
        assert_eq!(
            read_pack(&path).unwrap().content_digest,
            replacement.content_digest
        );
        drop(w);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn commit_replaces_existing_snapshot() {
        let dir = temp();
        let path = dir.join("pack.json");
        let w = Writer::acquire(&path).unwrap();
        let original = empty("2026-01-01T00:00:00Z");
        w.commit(&original).unwrap();
        let replacement = empty("2026-02-01T00:00:00Z");
        assert_ne!(original.content_digest, replacement.content_digest);
        w.commit(&replacement).unwrap();
        assert_eq!(
            read_pack(&path).unwrap().content_digest,
            replacement.content_digest
        );
        drop(w);
        fs::remove_dir_all(dir).unwrap();
    }
}
