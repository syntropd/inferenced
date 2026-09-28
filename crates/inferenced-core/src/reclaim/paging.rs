use crate::error::{Error, Result};
use crate::fd_lease::{create_sealed_memfd, recv_fd_from_unix, send_fd_over_unix};
use crate::madvise::{
    advise_dontneed, advise_hugepage, advise_random, advise_sequential, advise_willneed,
};
use rustix::fd::{AsFd, OwnedFd};
use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use std::fs;
use std::path::Path;

pub type PagingError = Error;

/// Real-time metrics for Linux kernel zswap compressed page cache.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ZswapMetrics {
    pub enabled: bool,
    pub compressor: String,
    pub zswap_bytes: u64,
    pub zswapped_bytes: u64,
    pub max_pool_percent: u32,
    pub accept_threshold_percent: u32,
}

impl ZswapMetrics {
    /// Read zswap metrics from `/proc/meminfo` and `/sys/module/zswap/parameters`.
    pub fn read_current() -> Self {
        let meminfo = fs::read_to_string("/proc/meminfo").unwrap_or_default();
        Self::parse_metrics(&meminfo, "/sys/module/zswap/parameters")
    }

    /// Parse zswap metrics from supplied meminfo content and zswap sysfs parameter path.
    pub fn parse_metrics(meminfo: &str, sysfs_params_dir: &str) -> Self {
        let mut zswap_bytes = 0;
        let mut zswapped_bytes = 0;

        for line in meminfo.lines() {
            if let Some(rest) = line.strip_prefix("Zswap:") {
                zswap_bytes = crate::topology::cpu::parse_kb(rest);
            } else if let Some(rest) = line.strip_prefix("Zswapped:") {
                zswapped_bytes = crate::topology::cpu::parse_kb(rest);
            }
        }

        let sysfs = Path::new(sysfs_params_dir);
        let enabled = fs::read_to_string(sysfs.join("enabled"))
            .map(|s| s.trim() == "Y" || s.trim() == "1")
            .unwrap_or(false);

        let compressor = fs::read_to_string(sysfs.join("compressor"))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "zstd".to_string());

        let max_pool_percent = fs::read_to_string(sysfs.join("max_pool_percent"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(20);

        let accept_threshold_percent = fs::read_to_string(sysfs.join("accept_threshold_percent"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(90);

        Self {
            enabled,
            compressor,
            zswap_bytes,
            zswapped_bytes,
            max_pool_percent,
            accept_threshold_percent,
        }
    }
}

/// Zero-Copy Paging and Kernel Memory Reclamation Manager.
/// Provides sealed memfd creation, SCM_RIGHTS model passing, madvise hints, and zswap metrics.
pub struct MemfdPaging;

impl MemfdPaging {
    /// Creates a sealed anonymous memory file descriptor containing model weights.
    pub fn create_sealed_model(name: &str, data: &[u8]) -> Result<OwnedFd> {
        create_sealed_memfd(name, data.len() as u64, Some(data))
    }

    /// Send a model's sealed file descriptor along with its model ID string over a Unix socket.
    pub fn send_model_fd<S: AsFd, F: AsFd>(socket: S, fd: F, model_id: &str) -> Result<usize> {
        send_fd_over_unix(socket, fd, model_id.as_bytes())
    }

    /// Receive a model descriptor string and passed sealed file descriptor from a Unix socket.
    pub fn recv_model_fd<S: AsFd>(socket: S) -> Result<(String, OwnedFd)> {
        let mut buf = [0u8; 1024];
        let (bytes_read, fd_opt) = recv_fd_from_unix(socket, &mut buf)?;
        let fd = fd_opt.ok_or_else(|| {
            Error::Fd("No file descriptor received in SCM_RIGHTS message".into())
        })?;
        let model_id = String::from_utf8_lossy(&buf[..bytes_read]).to_string();
        Ok((model_id, fd))
    }

    /// Advise kernel to reclaim pages using `Advice::LinuxDontNeed` (delegates to zswap / host reclaim).
    pub fn reclaim_pages(addr: *mut c_void, len: usize) -> Result<()> {
        advise_dontneed(addr, len)
    }

    /// Advise kernel to prefetch pages into RAM using `Advice::WillNeed`.
    pub fn prefetch_pages(addr: *mut c_void, len: usize) -> Result<()> {
        advise_willneed(addr, len)
    }

    /// Advise kernel to back pages with Transparent Huge Pages (THP).
    pub fn advise_hugepages(addr: *mut c_void, len: usize) -> Result<()> {
        advise_hugepage(addr, len)
    }

    /// Advise kernel of sequential access pattern.
    pub fn advise_sequential(addr: *mut c_void, len: usize) -> Result<()> {
        advise_sequential(addr, len)
    }

    /// Advise kernel of random access pattern.
    pub fn advise_random(addr: *mut c_void, len: usize) -> Result<()> {
        advise_random(addr, len)
    }

    /// Advise kernel to exclude pages from core dumps using `Advice::LinuxDontDump`.
    /// Crucial for systemd-coredump: prevents multi-gigabyte models from thrashing storage on crash.
    pub fn advise_exclude_coredump(addr: *mut c_void, len: usize) -> Result<()> {
        crate::madvise::advise_dontdump(addr, len)
    }

    /// Maps a received memfd into the process address space with PROT_READ and MAP_SHARED,
    /// and automatically invokes `MADV_DONTDUMP` to protect systemd-coredump from multi-GB dumps.
    pub fn map_and_protect_memfd<F: AsFd>(fd: &F, len: usize) -> Result<*mut c_void> {
        let ptr = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                len,
                rustix::mm::ProtFlags::READ,
                rustix::mm::MapFlags::SHARED,
                fd,
                0,
            )
        }
        .map_err(Error::SystemCall)?;

        crate::madvise::advise_dontdump(ptr, len)?;
        Ok(ptr)
    }

    /// Unmaps a previously mapped memory region.
    ///
    /// # Safety
    /// The caller must ensure that `addr` and `len` correspond to a valid mapped region
    /// and that the memory is no longer referenced.
    pub unsafe fn unmap_memfd(addr: *mut c_void, len: usize) -> Result<()> {
        rustix::mm::munmap(addr, len).map_err(Error::SystemCall)
    }

    /// Read live system zswap metrics.
    pub fn read_zswap_metrics() -> ZswapMetrics {
        ZswapMetrics::read_current()
    }
}

