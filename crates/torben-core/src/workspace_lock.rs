use std::{fs::OpenOptions, path::Path};

use fs4::FileExt;
use torben_contracts::{TorbenError, TorbenResult};

pub struct WorkspaceLock {
    file: std::fs::File,
}

impl WorkspaceLock {
    pub fn acquire(path: impl AsRef<Path>) -> TorbenResult<Self> {
        let path = path.as_ref();
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(|error| {
                TorbenError::new(
                    "workspace_lock_open_failed",
                    "Could not open the workspace lock.",
                )
                .with_detail("path", path.display().to_string())
                .with_detail("reason", error.to_string())
            })?;
        FileExt::lock(&file).map_err(|error| {
            TorbenError::new(
                "workspace_locked",
                "Another Torben App process is modifying the workspace.",
            )
            .with_detail("reason", error.to_string())
        })?;
        Ok(Self { file })
    }
}

impl Drop for WorkspaceLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        process::{Command, Stdio},
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    use tempfile::tempdir;

    use super::WorkspaceLock;

    const HELPER_LOCK_PATH: &str = "TORBEN_WORKSPACE_LOCK_HELPER_PATH";

    #[test]
    #[ignore = "subprocess helper for the cross-process workspace lock test"]
    fn holds_workspace_lock_for_parent_process() {
        let Some(lock_path) = std::env::var_os(HELPER_LOCK_PATH) else {
            return;
        };
        let lock_path = std::path::PathBuf::from(lock_path);
        let ready_path = lock_path.with_extension("ready");
        let release_path = lock_path.with_extension("release");
        let _lock = WorkspaceLock::acquire(&lock_path).expect("helper acquires workspace lock");
        std::fs::write(&ready_path, b"ready").expect("signal acquired workspace lock");

        let deadline = Instant::now() + Duration::from_secs(10);
        while !release_path.exists() {
            assert!(
                Instant::now() < deadline,
                "parent did not release the workspace lock helper"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn serializes_mutations_across_processes() {
        let root = tempdir().expect("create isolated lock root");
        let lock_path = root.path().join("workspace.lock");
        let ready_path = lock_path.with_extension("ready");
        let release_path = lock_path.with_extension("release");
        let test_binary = std::env::current_exe().expect("locate current test binary");
        let mut helper = Command::new(test_binary)
            .args([
                "--exact",
                "workspace_lock::tests::holds_workspace_lock_for_parent_process",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(HELPER_LOCK_PATH, &lock_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start workspace lock helper process");

        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready_path.exists() {
            assert!(
                helper.try_wait().expect("poll lock helper").is_none(),
                "workspace lock helper exited before acquiring the lock"
            );
            assert!(
                Instant::now() < deadline,
                "workspace lock helper did not acquire the lock"
            );
            thread::sleep(Duration::from_millis(10));
        }

        let contender_path = lock_path.clone();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let contender = thread::spawn(move || {
            let lock = WorkspaceLock::acquire(contender_path)
                .expect("second process lock is released to contender");
            acquired_tx.send(lock).expect("report lock acquisition");
        });
        assert!(
            matches!(
                acquired_rx.recv_timeout(Duration::from_millis(150)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "a concurrent mutation acquired the workspace lock too early"
        );

        std::fs::write(&release_path, b"release").expect("release lock helper");
        let status = helper.wait().expect("wait for lock helper");
        assert!(status.success(), "workspace lock helper failed: {status}");
        let acquired = acquired_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("contender acquires lock after helper exits");
        drop(acquired);
        contender.join().expect("join lock contender");
    }
}
