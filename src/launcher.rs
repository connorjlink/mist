use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use windows::Win32::Foundation::{CloseHandle, DBG_CONTINUE};
use windows::Win32::System::Diagnostics::Debug::{
    ContinueDebugEvent, WaitForDebugEvent, DEBUG_EVENT, CREATE_PROCESS_DEBUG_EVENT,
    EXIT_PROCESS_DEBUG_EVENT,
};
use windows::Win32::System::Threading::{
    CreateProcessA, PROCESS_INFORMATION, STARTUPINFOA, DEBUG_ONLY_THIS_PROCESS,
};
use windows::core::PCSTR;

#[unsafe(no_mangle)]
pub extern "C" fn launch_target(target_path: *const c_char) {
    if target_path.is_null() {
        eprintln!("launch_target: target_path was null");
        return;
    }

    let target_path = unsafe { CStr::from_ptr(target_path) };
    let target_path = target_path.to_string_lossy().into_owned();

    std::thread::spawn(move || {
        if let Err(err) = launch_and_debug(&target_path) {
            eprintln!("launch_target: failed: {err:?}");
        }
    });
}

fn launch_and_debug(target_path: &str) -> Result<(), String> {
    let app_path = CString::new(target_path)
        .map_err(|_| "Invalid target path (contains NUL byte)".to_string())?;

    let mut startup_info = STARTUPINFOA::default();
    startup_info.cb = std::mem::size_of::<STARTUPINFOA>() as u32;

    let mut process_info = PROCESS_INFORMATION::default();

    unsafe {
        CreateProcessA(
            PCSTR(app_path.as_ptr() as *const u8),
            None,
            None,
            None,
            false,
            DEBUG_ONLY_THIS_PROCESS,
            None,
            None,
            &mut startup_info,
            &mut process_info,
        )
        .map_err(|e| format!("CreateProcessA failed: {e}"))?;

        let mut debug_event = DEBUG_EVENT::default();

        loop {
            if WaitForDebugEvent(&mut debug_event, u32::MAX).is_err() {
                break;
            }

            match debug_event.dwDebugEventCode {
                CREATE_PROCESS_DEBUG_EVENT => {
                    // entry point/image base is available via CreateProcessInfo
                    // keep this quiet by default; callers can add logging externally.
                }
                EXIT_PROCESS_DEBUG_EVENT => {
                    break;
                }
                _ => {}
            }

            let _ = ContinueDebugEvent(
                debug_event.dwProcessId,
                debug_event.dwThreadId,
                DBG_CONTINUE,
            );
        }

        if !process_info.hThread.is_invalid() {
            _ = CloseHandle(process_info.hThread);
        }
        if !process_info.hProcess.is_invalid() {
            _ = CloseHandle(process_info.hProcess);
        }
    }

    return Ok(());
}
