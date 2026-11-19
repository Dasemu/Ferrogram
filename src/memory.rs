//! Give freed heap back to the OS after the window closes into the tray.
//!
//! glibc keeps freed pages in its arenas, so closing the window (dropping
//! the renderer surfaces and the decoded images) does not by itself lower
//! the resident size. `malloc_trim` hands the free pages back.

/// Return unused heap pages to the OS (glibc only; a no-op elsewhere).
pub fn release_to_os() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: `malloc_trim` has no preconditions; it only walks the
        // allocator's own free lists, and is safe to call from any thread.
        unsafe {
            libc::malloc_trim(0);
        }
    }
}
