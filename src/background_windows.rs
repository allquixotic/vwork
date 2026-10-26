use anyhow::{Result, ensure};
use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    io,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr,
};
use windows_sys::Win32::{
    Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle},
    System::Threading::{
        CreateProcessW, DETACHED_PROCESS, DeleteProcThreadAttributeList,
        EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess,
        InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION,
        STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
    },
};

pub(crate) struct Child(OwnedHandle);
impl Child {
    pub(crate) fn try_wait(&mut self) -> Result<Option<String>> {
        let mut code = 0;
        // SAFETY: the owned process handle and output pointer are valid.
        if unsafe { GetExitCodeProcess(self.0.as_raw_handle(), &mut code) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok((code != 259).then(|| format!("with exit code {code}")))
    }
}

fn inheritable(file: &File) -> Result<OwnedHandle> {
    let mut handle = ptr::null_mut();
    // SAFETY: duplicate a live handle in this process; ownership transfers on success.
    if unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            file.as_raw_handle(),
            GetCurrentProcess(),
            &mut handle,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: DuplicateHandle returned a new, uniquely owned handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

struct Attributes(Vec<usize>);
impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: storage was initialized as an attribute list and remains alive.
        unsafe { DeleteProcThreadAttributeList(self.0.as_mut_ptr().cast()) };
    }
}

/// Microsoft C runtime quoting; paths are arguments, never shell commands.
fn quote(value: &OsStr) -> Result<Vec<u16>> {
    let mut result = vec![b'"' as u16];
    let mut slashes = 0;
    for unit in value.encode_wide() {
        ensure!(unit != 0, "NUL in daemon path");
        if unit == b'\\' as u16 {
            slashes += 1;
        } else {
            result.extend(std::iter::repeat_n(
                b'\\' as u16,
                if unit == b'"' as u16 {
                    slashes * 2 + 1
                } else {
                    slashes
                },
            ));
            slashes = 0;
            result.push(unit);
        }
    }
    result.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    result.push(b'"' as u16);
    Ok(result)
}

pub(crate) fn spawn(home: &Path, executable: &Path) -> Result<Child> {
    let input = inheritable(&File::open("NUL")?)?;
    let output = inheritable(
        &OpenOptions::new()
            .append(true)
            .create(true)
            .open(home.join("daemon.log"))?,
    )?;
    let handles = [input.as_raw_handle(), output.as_raw_handle()];
    let mut bytes = 0;
    // SAFETY: the null sizing query writes only the required byte count.
    unsafe {
        InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut bytes);
    }
    ensure!(bytes > 0, "cannot size daemon process attributes");
    let mut storage = vec![0usize; bytes.div_ceil(std::mem::size_of::<usize>())];
    // SAFETY: allocation is sufficiently sized and pointer-aligned; Windows initializes it.
    if unsafe { InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), 1, 0, &mut bytes) }
        == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    let mut attributes = Attributes(storage);
    // SAFETY: the list and two inheritable handles stay alive through CreateProcessW.
    if unsafe {
        UpdateProcThreadAttribute(
            attributes.0.as_mut_ptr().cast(),
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            handles.as_ptr().cast(),
            std::mem::size_of_val(&handles),
            ptr::null_mut(),
            ptr::null(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    let mut application: Vec<u16> = executable.as_os_str().encode_wide().collect();
    ensure!(!application.contains(&0), "NUL in executable path");
    application.push(0);
    let mut command = quote(executable.as_os_str())?;
    command.extend(" --home ".encode_utf16());
    command.extend(quote(home.as_os_str())?);
    command.extend(" serve\0".encode_utf16());
    // SAFETY: Windows POD structures permit zero initialization.
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = input.as_raw_handle();
    startup.StartupInfo.hStdOutput = output.as_raw_handle();
    startup.StartupInfo.hStdError = output.as_raw_handle();
    startup.lpAttributeList = attributes.0.as_mut_ptr().cast();
    // SAFETY: all pointers refer to live, correctly sized Windows structures/UTF-16
    // strings. Only the explicit NUL/log handle list is inherited, never MCP pipes.
    if unsafe {
        CreateProcessW(
            application.as_ptr(),
            command.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            DETACHED_PROCESS | EXTENDED_STARTUPINFO_PRESENT,
            ptr::null(),
            ptr::null(),
            &startup.StartupInfo,
            &mut process,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: successful CreateProcessW transfers unique process/thread handles.
    let handle = unsafe { OwnedHandle::from_raw_handle(process.hProcess) };
    drop(unsafe { OwnedHandle::from_raw_handle(process.hThread) });
    Ok(Child(handle))
}
