//! Captured subprocesses used by the daemon and CLI helpers.
use std::{ffi::OsStr, process::Command};

pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    // Captured helpers never consume a terminal. Explicit pipe/null handles set
    // by each caller still work; Windows must not allocate a console for them.
    command.stdin(std::process::Stdio::null());
    command
}
