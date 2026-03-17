use std::ffi::c_void;
use std::ptr::null_mut;
use windows::{
    Win32::{
        Foundation::*,
        System::{
            Diagnostics::{Debug::*, ToolHelp::*},
            ProcessStatus::*,
            Threading::*,
        },
    },
    core::PCWSTR,
};

// Mist debugger.rs
// (c) Connor J. Link. All Rights Reserved.

pub struct Debugger {
    // debugger information
    toolhelp_snapshot: HANDLE,

    // debugee information
    thread_id: Option<u32>,
    thread_handle: Option<HANDLE>,
    process_handle: Option<HANDLE>,
    image_base: Option<*mut c_void>,
}

impl Drop for Debugger {
    fn drop(&mut self) {
        unsafe {
            if !self.toolhelp_snapshot.is_invalid() {
                _ = CloseHandle(self.toolhelp_snapshot);
            }
        }
    }
}

#[derive(Debug)]
pub struct DebuggerError(String);

pub fn attach_debugger(name: PCWSTR) -> Result<Debugger, DebuggerError> {
    let process_handle = unsafe { attach_to_process(name)
        .ok_or(DebuggerError(format!("Failed to attach to process: {}", name.to_string().unwrap_or(format!("Unknown")))))? };

    let thread_id = await_get_thread_id()
        .ok_or(DebuggerError(format!("Failed to get thread ID")))?;

    let thread_handle = unsafe { OpenThread(THREAD_ALL_ACCESS, false, thread_id) }
        .map_err(|e| DebuggerError(format!("Failed to open thread: {e}")))?;
    if thread_handle.is_invalid() {
        return Err(DebuggerError(format!("Failed to open thread")));
    }

    let image_base = resolve_image_base(process_handle);
    if image_base.is_null() {
        return Err(DebuggerError(format!("Failed to resolve image base")));
    }

    return Ok(Debugger {
        toolhelp_snapshot: HANDLE(null_mut()),
        thread_id: Some(thread_id),
        thread_handle: Some(thread_handle),
        process_handle: Some(process_handle),
        image_base: Some(image_base),
    });
}

pub fn snapshot_process() -> Option<HANDLE> {
    let toolhelp_snapshot_result = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if toolhelp_snapshot_result.is_err() {
        return None;
    }
    let toolhelp_snapshot = toolhelp_snapshot_result.unwrap();
    if toolhelp_snapshot.is_invalid() {
        return None;
    }

    return Some(toolhelp_snapshot);
}

pub fn get_process_handle(name: PCWSTR, desired_access: u32) -> HANDLE {
    unsafe {
        let snapshot_result = snapshot_process();
        if snapshot_result.is_none() {
            return HANDLE(null_mut());
        }
        let snapshot = snapshot_result.unwrap();

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        let process = Process32FirstW(snapshot, &mut entry);
        if process.is_err() {
            _ = CloseHandle(snapshot);
            return HANDLE(null_mut());
        }

        let mut process_handle = HANDLE(null_mut());
        loop {
            let exe_name = PCWSTR(entry.szExeFile.as_ptr());
            if compare_pcwstr_case_insensitive(exe_name, name) {
                let access_rights = PROCESS_ACCESS_RIGHTS(desired_access);

                let opened_process = OpenProcess(access_rights, false, entry.th32ProcessID);
                if opened_process.is_err() {
                    _ = CloseHandle(snapshot);
                    return HANDLE(null_mut());
                }

                process_handle = opened_process.unwrap();
                break;
            }

            let next_process = Process32NextW(snapshot, &mut entry);
            if next_process.is_err() {
                break;
            }
        }

        _ = CloseHandle(snapshot);
        return process_handle;
    }
}

pub fn resolve_image_base(process: HANDLE) -> *mut std::ffi::c_void {
    unsafe {
        // this is probably overkil, but this function only gets called once per debug attach, so it is probably okay
        let mut modules = [HMODULE(null_mut()); 1024];
        let mut bytes = 0u32;

        let modules_result = EnumProcessModulesEx(process, modules.as_mut_ptr(), std::mem::size_of_val(&modules) as u32, &mut bytes, LIST_MODULES_32BIT);
        if modules_result.is_err() || bytes == 0 {
            return null_mut();
        }

        let mut mod_info = MODULEINFO::default();
        // always assuming the first module is the main executable
        let information_result = GetModuleInformation(process, modules[0], &mut mod_info, std::mem::size_of::<MODULEINFO>() as u32);
        if information_result.is_err() {
            return null_mut();
        }

        return mod_info.lpBaseOfDll;
    }
}

pub fn attach_to_process(name: PCWSTR) -> Option<HANDLE> {
    let process_handle = get_process_handle(name, PROCESS_ALL_ACCESS.0);
    if process_handle.is_invalid() {
        return None;
    }

    let attach_result = unsafe { DebugActiveProcess(GetProcessId(process_handle)) };
    if attach_result.is_err() {
        unsafe {
            _ = CloseHandle(process_handle);
        };
        return None;
    }

    return Some(process_handle);
}

pub fn await_get_thread_id() -> Option<u32> {
    unsafe {
        let mut debug_event = DEBUG_EVENT::default();

        loop {
            let wait_result = WaitForDebugEvent(&mut debug_event, INFINITE);
            if wait_result.is_err() {
                return None;
            }

            if debug_event.dwDebugEventCode == EXCEPTION_DEBUG_EVENT && 
               debug_event.u.Exception.ExceptionRecord.ExceptionCode == EXCEPTION_BREAKPOINT {
                return Some(debug_event.dwThreadId);
            }

            _ = ContinueDebugEvent(debug_event.dwProcessId, debug_event.dwThreadId, DBG_CONTINUE);
        }
    }
}


fn compare_pcwstr_case_insensitive(a: PCWSTR, b: PCWSTR) -> bool {
    unsafe {
        match (a.to_string(), b.to_string()) {
            (Ok(sa), Ok(sb)) => sa.to_lowercase() == sb.to_lowercase(),
            _ => false,
        }
    }
}
