//! Explorer, the parent console and process memory.

use std::io;
use std::path::Path;

use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::GetCurrentProcess;

pub(crate) fn open_folder(folder: &Path) -> io::Result<()> {
    std::process::Command::new("explorer.exe")
        .arg(folder)
        .spawn()
        .map(drop)
}

/// Started from a terminal, a windowed build can still print there and be stopped with Ctrl+C.
pub(crate) fn attach_parent_console() {
    // SAFETY: fails harmlessly when there is no parent console.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// (working set, private bytes) of this process.
pub(crate) fn process_memory() -> Option<(usize, usize)> {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: the pseudo handle from GetCurrentProcess is always valid, and `counters` is a
    // correctly sized PROCESS_MEMORY_COUNTERS_EX (its prefix is PROCESS_MEMORY_COUNTERS).
    let ok = unsafe {
        GetProcessMemoryInfo(GetCurrentProcess(), (&raw mut counters).cast(), counters.cb)
    };
    ok.is_ok()
        .then_some((counters.WorkingSetSize, counters.PrivateUsage))
}
