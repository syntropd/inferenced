use crate::error::{Error, Result};
use rustix::mm::{madvise, Advice};
use std::ffi::c_void;

/// Page management hints using Linux madvise() and kernel zswap.
/// Delegates page residency directly to the kernel page cache rather than userland paging.

/// Advise the kernel that the specified address range will be accessed soon.
/// Triggers asynchronous read-ahead into host RAM.
pub fn advise_willneed(addr: *mut c_void, len: usize) -> Result<()> {
    // Safety: Caller must provide valid memory pointer and length
    unsafe {
        madvise(addr, len, Advice::WillNeed).map_err(Error::SystemCall)?;
    }
    Ok(())
}

/// Advise the kernel that the specified address range is no longer needed immediately.
/// Uses Advice::LinuxDontNeed so pages can be immediately reclaimed or pushed to zswap
/// without unmapping the virtual address space.
pub fn advise_dontneed(addr: *mut c_void, len: usize) -> Result<()> {
    // Safety: Caller must provide valid memory pointer and length
    unsafe {
        madvise(addr, len, Advice::LinuxDontNeed).map_err(Error::SystemCall)?;
    }
    Ok(())
}

/// Advise the kernel to back the memory range with Transparent Huge Pages (THP)
/// to reduce TLB misses during high-throughput tensor operations.
pub fn advise_hugepage(addr: *mut c_void, len: usize) -> Result<()> {
    // Safety: Caller must provide valid memory pointer and length
    unsafe {
        madvise(addr, len, Advice::LinuxHugepage).map_err(Error::SystemCall)?;
    }
    Ok(())
}

/// Advise the kernel that access pattern will be sequential.
pub fn advise_sequential(addr: *mut c_void, len: usize) -> Result<()> {
    // Safety: Caller must provide valid memory pointer and length
    unsafe {
        madvise(addr, len, Advice::Sequential).map_err(Error::SystemCall)?;
    }
    Ok(())
}

/// Advise the kernel that access pattern will be random.
pub fn advise_random(addr: *mut c_void, len: usize) -> Result<()> {
    // Safety: Caller must provide valid memory pointer and length
    unsafe {
        madvise(addr, len, Advice::Random).map_err(Error::SystemCall)?;
    }
    Ok(())
}

/// Advise the kernel to exclude the memory range from core dumps (MADV_DONTDUMP).
/// Crucial for systemd-coredump: prevents multi-gigabyte zero-copy model weight
/// buffers from exhausting disk space and stalling crash supervisor analysis.
pub fn advise_dontdump(addr: *mut c_void, len: usize) -> Result<()> {
    // Safety: Caller must provide valid memory pointer and length
    unsafe {
        madvise(addr, len, Advice::LinuxDontDump).map_err(Error::SystemCall)?;
    }
    Ok(())
}

