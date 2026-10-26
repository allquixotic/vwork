//! Spawn a shared daemon without inheriting the MCP client's pipe handles.
#[cfg(unix)]
use {anyhow::Result, std::path::Path};

#[cfg(windows)]
#[path = "background_windows.rs"]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{Child, spawn};

#[cfg(unix)]
pub(crate) struct Child(std::process::Child);

#[cfg(unix)]
impl Child {
    pub(crate) fn try_wait(&mut self) -> Result<Option<String>> {
        Ok(self.0.try_wait()?.map(|s| s.to_string()))
    }
}

#[cfg(unix)]
pub(crate) fn spawn(home: &Path, executable: &Path) -> Result<Child> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let log = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(home.join("daemon.log"))?;
    let mut command = Command::new(executable);
    command
        .arg("--home")
        .arg(home)
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // SAFETY: setsid is async-signal-safe; no allocation or locks after fork.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    Ok(Child(command.spawn()?))
}
