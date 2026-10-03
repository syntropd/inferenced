use inferenced_core::madvise::{
    advise_dontneed, advise_hugepage, advise_random, advise_sequential, advise_willneed,
};
use rustix::mm::{mmap_anonymous, munmap, MapFlags, ProtFlags};
use std::ptr::null_mut;

#[test]
fn test_madvise_hints_on_anonymous_mmap() {
    let page_size = 4096 * 64; // 256 KB
    let addr = unsafe {
        mmap_anonymous(
            null_mut(),
            page_size,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE,
        )
    }
    .expect("mmap_anonymous should succeed");

    unsafe {
        std::ptr::write_bytes(addr, 0x42, page_size);
    }

    advise_sequential(addr, page_size).expect("advise_sequential should succeed");
    advise_willneed(addr, page_size).expect("advise_willneed should succeed");
    advise_random(addr, page_size).expect("advise_random should succeed");
    let _ = advise_hugepage(addr, page_size);
    advise_dontneed(addr, page_size).expect("advise_dontneed should succeed");

    unsafe {
        munmap(addr, page_size).expect("munmap should succeed");
    }
}

#[test]
fn test_madvise_dontneed_zeros_anonymous_memory() {
    let size = 4096 * 16;
    let addr = unsafe {
        mmap_anonymous(
            null_mut(),
            size,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE,
        )
    }
    .expect("mmap");

    unsafe {
        // Fill memory with 0xAA
        std::ptr::write_bytes(addr, 0xAA, size);
        let slice = std::slice::from_raw_parts(addr as *const u8, size);
        assert_eq!(slice[0], 0xAA);

        // Tell kernel we don't need this memory (zswap / page reclaim)
        advise_dontneed(addr, size).expect("advise_dontneed");

        // Linux private anonymous pages revert to zeroed pages on next read
        let post_slice = std::slice::from_raw_parts(addr as *const u8, size);
        assert_eq!(post_slice[0], 0x00, "DontNeed on anon pages should zero content on read");

        munmap(addr, size).expect("munmap");
    }
}

#[test]
fn test_madvise_willneed_prefetch_cycle() {
    let size = 4096 * 8;
    let addr = unsafe {
        mmap_anonymous(
            null_mut(),
            size,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE,
        )
    }
    .unwrap();

    // Sequence of operations: willneed before write, then write, then dontneed
    advise_willneed(addr, size).expect("prefetch");
    unsafe {
        std::ptr::write_bytes(addr, 0x77, size);
    }
    advise_dontneed(addr, size).expect("reclaim");

    unsafe {
        munmap(addr, size).unwrap();
    }
}

#[test]
fn test_madvise_multi_page_chunking() {
    let size = 2 * 1024 * 1024; // 2MB
    let addr = unsafe {
        mmap_anonymous(
            null_mut(),
            size,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE,
        )
    }
    .unwrap();

    advise_sequential(addr, size).unwrap();
    advise_dontneed(addr, size).unwrap();

    unsafe {
        munmap(addr, size).unwrap();
    }
}

#[test]
fn test_madvise_null_pointer_error() {
    // Calling madvise with null pointer must return SystemCall error
    let res = advise_dontneed(null_mut(), 4096);
    assert!(res.is_err(), "Null pointer madvise must return an error");
}

#[test]
fn test_madvise_dontdump_excludes_from_coredump() {
    let size = 4096 * 4;
    let addr = unsafe {
        mmap_anonymous(
            null_mut(),
            size,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE,
        )
    }
    .unwrap();

    let res = inferenced_core::madvise::advise_dontdump(addr, size);
    assert!(res.is_ok(), "advise_dontdump should succeed on valid anonymous mmap");

    unsafe {
        munmap(addr, size).unwrap();
    }
}

#[test]
fn test_map_and_protect_memfd_lifecycle() {
    let initial_data = b"model_weights_test_tensor";
    let memfd = inferenced_core::fd_lease::create_sealed_memfd(
        "test_weights",
        initial_data.len() as u64,
        Some(initial_data),
    )
    .expect("create_sealed_memfd should succeed");

    let ptr = inferenced_core::paging::MemfdPaging::map_and_protect_memfd(
        &memfd,
        initial_data.len(),
    )
    .expect("map_and_protect_memfd should succeed");

    assert!(!ptr.is_null());
    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, initial_data.len()) };
    assert_eq!(slice, initial_data);

    unsafe {
        inferenced_core::paging::MemfdPaging::unmap_memfd(ptr, initial_data.len())
            .expect("unmap_memfd should succeed");
    }
}

