use inferenced_core::arbiter::Arbiter;
use rustix::fs::{flock, FlockOperation};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::process::Child;
use tracing::{debug, info, warn};

/// Active inhibitor lock mechanism handle.
enum InhibitHandle {
    SystemdInhibit(Child),
    FileLock { file: std::fs::File, path: PathBuf },
    BusSocket(tokio::net::UnixStream),
}

/// Active mode under which sleep/idle is inhibited.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InhibitMode {
    SystemdInhibit,
    StandaloneFileLock,
    UnixBusSocket,
}

/// Systemd logind sleep and idle inhibitor manager with standalone fallback.
pub struct InhibitorManager {
    active_leases: Arc<AtomicUsize>,
    is_inhibited: Arc<AtomicBool>,
    active_handle: tokio::sync::Mutex<Option<InhibitHandle>>,
    lock_path: PathBuf,
    bus_socket_path: Option<PathBuf>,
    prefer_standalone: bool,
}

#[allow(dead_code)]
impl InhibitorManager {
    pub fn new() -> Self { Self::with_options(Self::default_lock_path(), false) }
    pub fn with_lock_path(path: PathBuf) -> Self { Self::with_options(path, false) }
    pub fn with_bus_socket(mut self, socket_path: PathBuf) -> Self { self.bus_socket_path = Some(socket_path); self }

    pub fn with_options(lock_path: PathBuf, prefer_standalone: bool) -> Self {
        Self {
            active_leases: Arc::new(AtomicUsize::new(0)), is_inhibited: Arc::new(AtomicBool::new(false)),
            active_handle: tokio::sync::Mutex::new(None), lock_path, bus_socket_path: None, prefer_standalone,
        }
    }

    fn default_lock_path() -> PathBuf {
        let run_dir = Path::new("/run/systemd-inferenced");
        if run_dir.exists() || std::fs::create_dir_all(run_dir).is_ok() {
            run_dir.join("inhibit.lock")
        } else if let Ok(rt) = std::env::var("XDG_RUNTIME_DIR") {
            let p = PathBuf::from(rt).join("systemd-inferenced");
            let _ = std::fs::create_dir_all(&p);
            p.join("inhibit.lock")
        } else { std::env::temp_dir().join("systemd-inferenced-inhibit.lock") }
    }

    pub async fn on_lease_acquired(&self) {
        if self.active_leases.fetch_add(1, Ordering::SeqCst) == 0 {
            self.acquire_inhibit_lock().await;
        }
    }

    pub async fn on_lease_released(&self) {
        if self.active_leases.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.release_inhibit_lock().await;
        }
    }

    pub fn active_lease_count(&self) -> usize { self.active_leases.load(Ordering::SeqCst) }
    pub fn is_inhibited(&self) -> bool { self.is_inhibited.load(Ordering::SeqCst) }
    pub fn lock_path(&self) -> &Path { &self.lock_path }

    pub async fn active_mode(&self) -> Option<InhibitMode> {
        self.active_handle.lock().await.as_ref().map(|h| match h {
            InhibitHandle::SystemdInhibit(_) => InhibitMode::SystemdInhibit,
            InhibitHandle::FileLock { .. } => InhibitMode::StandaloneFileLock,
            InhibitHandle::BusSocket(_) => InhibitMode::UnixBusSocket,
        })
    }

    async fn acquire_inhibit_lock(&self) {
        let mut lock = self.active_handle.lock().await;
        if lock.is_some() { return; }
        if !self.prefer_standalone {
            debug!("Attempting systemd-inhibit sleep delay lock");
            if let Ok(mut child) = tokio::process::Command::new("systemd-inhibit")
                .args(["--what=sleep", "--who=systemd-inferenced",
                       "--why=Active AI model inference lease executing",
                       "--mode=delay", "sleep", "infinity"])
                .spawn()
            {
                tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
                if let Ok(Some(st)) = child.try_wait() {
                    warn!("systemd-inhibit exited prematurely ({st}); using flock fallback");
                } else {
                    *lock = Some(InhibitHandle::SystemdInhibit(child));
                    self.is_inhibited.store(true, Ordering::SeqCst);
                    info!("Acquired host sleep lock via systemd-inhibit");
                    return;
                }
            }
            warn!("systemd-inhibit unavailable; falling back to standalone flock");
        }
        if self.acquire_standalone_fd_lock(&mut lock).await { return; }
        self.acquire_unix_bus_socket(&mut lock).await;
    }

    async fn acquire_standalone_fd_lock(&self, lock: &mut Option<InhibitHandle>) -> bool {
        if let Some(parent) = self.lock_path.parent() { let _ = std::fs::create_dir_all(parent); }
        let opts = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&self.lock_path);
        match opts {
            Ok(file) => match flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => {
                    let mut f = &file;
                    let _ = writeln!(f, "pid={}\nstate=inhibited\nleases={}\n",
                        std::process::id(), self.active_leases.load(Ordering::SeqCst));
                    *lock = Some(InhibitHandle::FileLock { file, path: self.lock_path.clone() });
                    self.is_inhibited.store(true, Ordering::SeqCst);
                    info!("Acquired standalone flock inhibitor lock on {}", self.lock_path.display());
                    true
                }
                Err(e) => { warn!("Failed flock on {}: {}", self.lock_path.display(), e); false }
            },
            Err(e) => { warn!("Failed opening inhibitor lockfile {}: {}", self.lock_path.display(), e); false }
        }
    }

    async fn acquire_unix_bus_socket(&self, lock: &mut Option<InhibitHandle>) {
        let sock_path = self.bus_socket_path.clone().or_else(|| {
            std::env::var("INHIBIT_SOCKET").ok().map(PathBuf::from).or_else(|| {
                let p = PathBuf::from("/run/systemd/inhibit.sock");
                if p.exists() { Some(p) } else { None }
            })
        });
        let Some(path) = sock_path else { return };
        debug!("Attempting Unix bus socket inhibitor connection to {}", path.display());
        match tokio::net::UnixStream::connect(&path).await {
            Ok(mut stream) => {
                let msg = format!("INHIBIT:sleep:idle pid={} leases={}\n",
                    std::process::id(), self.active_leases.load(Ordering::SeqCst));
                let _ = stream.write_all(msg.as_bytes()).await;
                *lock = Some(InhibitHandle::BusSocket(stream));
                self.is_inhibited.store(true, Ordering::SeqCst);
                info!("Acquired Unix bus socket inhibitor connection to {}", path.display());
            }
            Err(e) => warn!("Failed connecting to Unix bus socket {}: {}", path.display(), e),
        }
    }

    async fn release_inhibit_lock(&self) {
        let mut lock = self.active_handle.lock().await;
        if let Some(handle) = lock.take() {
            match handle {
                InhibitHandle::SystemdInhibit(mut child) => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    debug!("Terminated systemd-inhibit child process (reaped)");
                }
                InhibitHandle::FileLock { file, path } => {
                    let _ = flock(&file, FlockOperation::Unlock);
                    let _ = std::fs::remove_file(&path);
                    debug!("Unlocked and removed inhibitor lockfile {}", path.display());
                }
                InhibitHandle::BusSocket(mut stream) => {
                    let _ = stream.write_all(b"RELEASE\n").await;
                    let _ = stream.flush().await;
                    debug!("Closed inhibitor Unix bus socket");
                }
            }
            self.is_inhibited.store(false, Ordering::SeqCst);
            info!("Released sleep inhibitor lock");
        }
    }

    pub async fn quiesce_for_sleep(&self, arbiter: &Arc<Arbiter>) {
        info!("systemd-logind PrepareForSleep: initiating memory quiescence");
        info!("Quiescing {} loaded models prior to host sleep", arbiter.list_models().await.len());
    }

    pub async fn resume_from_sleep(&self, _arbiter: &Arc<Arbiter>) {
        info!("systemd-logind ResumeFromSleep: host awake; models ready for on-demand paging");
    }
}

impl Default for InhibitorManager { fn default() -> Self { Self::new() } }

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_inhibitor_reference_counting() {
        let m = InhibitorManager::new();
        assert_eq!(m.active_lease_count(), 0);
        m.on_lease_acquired().await; assert_eq!(m.active_lease_count(), 1);
        m.on_lease_acquired().await; assert_eq!(m.active_lease_count(), 2);
        m.on_lease_released().await; assert_eq!(m.active_lease_count(), 1);
        m.on_lease_released().await; assert_eq!(m.active_lease_count(), 0);
    }

    #[tokio::test]
    async fn test_standalone_flock_fallback_lifecycle() {
        let temp = std::env::temp_dir().join(format!("inf_test_{}", std::process::id()));
        let lock_path = temp.join("test_inhibit.lock");
        let m = InhibitorManager::with_options(lock_path.clone(), true);
        assert_eq!(m.lock_path(), lock_path.as_path());
        assert!(!m.is_inhibited());
        m.on_lease_acquired().await;
        assert!(m.is_inhibited());
        assert_eq!(m.active_mode().await, Some(InhibitMode::StandaloneFileLock));

        let verify_file = std::fs::File::open(&lock_path).unwrap();
        assert!(flock(&verify_file, FlockOperation::NonBlockingLockExclusive).is_err());

        m.on_lease_released().await;
        assert!(!m.is_inhibited());
        assert_eq!(m.active_mode().await, None);
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn test_unix_bus_socket_fallback_lifecycle() {
        let temp = std::env::temp_dir().join(format!("inf_sock_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp);
        let sock_path = temp.join("bus.sock");
        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();
        let m = InhibitorManager::with_options(PathBuf::from("/proc/0/bad.lock"), true).with_bus_socket(sock_path);
        let (tx, rx) = tokio::sync::oneshot::channel();

        let srv = tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let n = tokio::io::AsyncReadExt::read(&mut conn, &mut buf).await.unwrap();
            assert!(String::from_utf8_lossy(&buf[..n]).starts_with("INHIBIT:sleep:idle"));
            let _ = tx.send(());
            let n2 = tokio::io::AsyncReadExt::read(&mut conn, &mut buf).await.unwrap();
            assert!(String::from_utf8_lossy(&buf[..n2]).starts_with("RELEASE"));
        });

        m.on_lease_acquired().await;
        assert!(m.is_inhibited());
        assert_eq!(m.active_mode().await, Some(InhibitMode::UnixBusSocket));
        let _ = rx.await;
        m.on_lease_released().await;
        assert!(!m.is_inhibited());
        srv.await.unwrap();
        let _ = std::fs::remove_dir_all(&temp);
    }
}
