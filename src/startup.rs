use anyhow::Result;
use std::{io, os::windows::ffi::OsStrExt, ptr::null_mut};
use windows_sys::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
        System::Registry::{
            HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW,
            RegSetKeyValueW,
        },
    },
    core::w,
};

const RUN: *const u16 = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const ENTRY: *const u16 = w!("WfZiUiBp");

pub fn enabled() -> bool {
    let mut size = 0;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN,
            ENTRY,
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            &mut size,
        ) == ERROR_SUCCESS
    }
}

pub fn set(enabled: bool) -> Result<()> {
    let status = if enabled {
        let path = std::env::current_exe()?;
        let mut command = vec![b'"' as u16];
        command.extend(path.as_os_str().encode_wide());
        command.extend("\" --hide".encode_utf16());
        command.push(0);
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN,
                ENTRY,
                REG_SZ,
                command.as_ptr().cast(),
                (command.len() * 2) as u32,
            )
        }
    } else {
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, ENTRY) }
    };
    if status == ERROR_SUCCESS || !enabled && status == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status as i32).into())
    }
}
