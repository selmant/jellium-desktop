//! Official CEF Linux builds compile `MallocDumpProvider` against a glibc 2.31
//! sysroot, so they call 32-bit `mallinfo()` rather than `mallinfo2()`.
//! `mallinfo` fields wrap above ~2GiB and Chromium `CHECK`s the wrapped values
//! (`ud2` / SIGILL on the MemoryInfra thread). See
//! <https://github.com/chromiumembedded/cef/issues/3963>.
//!
//! ELF symbol preemption (`--export-dynamic`) lets this definition shadow
//! libc's `mallinfo` for `libcef.so`.

#[repr(C)]
#[derive(Clone, Copy)]
struct Mallinfo {
    arena: i32,
    ordblks: i32,
    smblks: i32,
    hblks: i32,
    hblkhd: i32,
    usmblks: i32,
    fsmblks: i32,
    uordblks: i32,
    fordblks: i32,
    keepcost: i32,
}

#[repr(C)]
struct Mallinfo2 {
    arena: usize,
    ordblks: usize,
    smblks: usize,
    hblks: usize,
    hblkhd: usize,
    usmblks: usize,
    fsmblks: usize,
    uordblks: usize,
    fordblks: usize,
    keepcost: usize,
}

unsafe extern "C" {
    fn mallinfo2() -> Mallinfo2;
}

fn sat(v: usize) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

fn from_mallinfo2(m2: Mallinfo2) -> Mallinfo {
    let arena = sat(m2.arena);
    // Chromium does `checked_cast<size_t>(info.arena + info.hblkhd)`.
    let hblkhd = sat(m2.hblkhd).min(i32::MAX.saturating_sub(arena));
    let virt = arena.saturating_add(hblkhd);
    let uordblks = sat(m2.uordblks).min(virt);
    Mallinfo {
        arena,
        ordblks: sat(m2.ordblks),
        smblks: sat(m2.smblks),
        hblks: sat(m2.hblks),
        hblkhd,
        usmblks: sat(m2.usmblks),
        fsmblks: sat(m2.fsmblks),
        uordblks,
        fordblks: sat(m2.fordblks),
        keepcost: sat(m2.keepcost),
    }
}

#[unsafe(no_mangle)]
extern "C" fn mallinfo() -> Mallinfo {
    from_mallinfo2(unsafe { mallinfo2() })
}

/// Keep the interposer in the rlib so lld cannot GC it before `--export-dynamic`
/// puts it in the binary's dynamic symbol table.
pub fn keep() {
    let _ = mallinfo as extern "C" fn() -> Mallinfo;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapped_sizes_stay_in_signed_i32_and_consistent() {
        let m = from_mallinfo2(Mallinfo2 {
            arena: 3 * 1024 * 1024 * 1024,
            ordblks: 1,
            smblks: 0,
            hblks: 2,
            hblkhd: 2 * 1024 * 1024 * 1024,
            usmblks: 0,
            fsmblks: 0,
            uordblks: 4 * 1024 * 1024 * 1024,
            fordblks: 1,
            keepcost: 1,
        });
        assert!(m.arena >= 0);
        assert!(m.hblkhd >= 0);
        assert!(m.uordblks >= 0);
        m.arena
            .checked_add(m.hblkhd)
            .expect("arena+hblkhd must not overflow i32");
        assert!(m.uordblks <= m.arena + m.hblkhd);
    }
}
