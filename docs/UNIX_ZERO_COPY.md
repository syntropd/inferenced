# Zero-Copy Unix Memory Fabric: `memfd`, `SCM_RIGHTS`, and `madvise`

`systemd-inferenced` bridges Linux Unix system architecture with modern tensor workloads.
Rather than copying gigabytes of neural weights across userland processes or duplicating model parameters in RAM, the daemon delegates memory residency and sharing directly to the Linux virtual memory manager.

---

## 1. Zero-Copy Architecture Overview

```
 [ systemd-inferenced ]  --- creates sealed memfd --->  [ Linux Kernel SHM ]
         |                                                       ^
   SCM_RIGHTS fd                                                 |
         v                                                       |
 [ Client Inference Engine ]  ==== mmap(PROT_READ) ==============+
   (runtimed / llama.cpp)
```

1. **Weight Staging**: Model tensors are loaded into an anonymous, in-memory file descriptor via `memfd_create(2)` with `MFD_ALLOW_SEALING | MFD_CLOEXEC`.
2. **Immutable Sealing**: `fcntl(F_ADD_SEALS)` applies `F_SEAL_GROW`, `F_SEAL_SHRINK`, and `F_SEAL_WRITE`. This guarantees to consumer processes that weights cannot be mutated or truncated underneath them.
3. **Descriptor Passing**: The sealed `memfd` is transferred across a local Unix domain socket (`/run/systemd-inferenced/fd.sock`) using `SCM_RIGHTS` ancillary messages (`sendmsg`/`recvmsg`).
4. **Direct Mapping**: Consumers `mmap(2)` the received descriptor directly into their virtual address space with zero memory copies.

---

## 2. Kernel Virtual Memory Delegation (`madvise` & `zswap`)

Rather than developing a custom userland paging mechanism, `systemd-inferenced` delegates page lifecycle management to the Linux kernel VM subsystem:

### Asynchronous Read-Ahead (`Advice::WillNeed`)
```rust
advise_willneed(ptr, length)?;
```
Informs the kernel page cache that tensor pages will be accessed imminently. The kernel prefetches the physical pages into RAM without blocking userland execution.

### Cold Weight Eviction (`Advice::LinuxDontNeed`)
```rust
advise_dontneed(ptr, length)?;
```
Informs the kernel that pages are dormant. On Linux, dirty or clean pages are immediately reclaimed or compressed into `zswap` without unmapping the virtual address range. When the model is invoked again, pages fault back in transparently.

### Transparent Huge Pages (`Advice::LinuxHugepage`)
```rust
advise_hugepage(ptr, length)?;
```
Advises the kernel to collapse 4KB pages into 2MB huge pages (THP), drastically reducing TLB cache misses during high-throughput matrix multiplication.

---

## 3. Protocol Wire Interaction

A client connects to `/run/systemd-inferenced/fd.sock` and requests a weight descriptor:

```json
{"action": "create", "name": "qwen2.5-coder:7b", "size_bytes": 4294967296}
```

The daemon responds with `SCM_RIGHTS` ancillary control data containing the newly sealed file descriptor.
