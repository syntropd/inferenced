//! Memory reclaim: freeze, advise, and page idle weight out.

pub mod drm_watermark;
pub mod freezer;
pub mod madvise;
pub mod paging;
