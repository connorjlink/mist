use std::ffi::c_void;
use std::ptr::null_mut;
use windows::{
    Win32::{
        Foundation::*,
        System::{
            Diagnostics::{Debug::*, ToolHelp::*}, ProcessStatus::*, Threading::*, Memory::*
        },
    },
    core::PCWSTR,
};

use crate::utilities::*;

// Mist debugger.rs
// (c) Connor J. Link. All Rights Reserved.

macro_rules! string {
    ($s:expr) => {
        $s.into()
    };
}

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

pub fn attach_debugger(name: PCWSTR) -> std::result::Result<Debugger, DebuggerError> {
    let process_handle = attach_to_process(name)
        .ok_or(DebuggerError(string!("Failed to attach to process")))?;

    let thread_id = await_get_thread_id()
        .ok_or(DebuggerError(string!("Failed to get thread ID")))?;

    let thread_handle = unsafe { OpenThread(THREAD_ALL_ACCESS, false, thread_id) }
        .map_err(|e| DebuggerError(format!("Failed to open thread: {e}")))?;
    if thread_handle.is_invalid() {
        return Err(DebuggerError(string!("Failed to open thread")));
    }

    let image_base = resolve_image_base(process_handle);
    if image_base.is_null() {
        return Err(DebuggerError(string!("Failed to resolve image base")));
    }

    return Ok(Debugger {
        toolhelp_snapshot: HANDLE(null_mut()),
        thread_id: Some(thread_id),
        thread_handle: Some(thread_handle),
        process_handle: Some(process_handle),
        image_base: Some(image_base),
    });
}

/// Snapshot a process by handle and resolve thread and module information
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

/// Retrieves a handle to a process by its name.
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

/// Resolves the executable image base of a process that is already running (after ASLR has applied).
pub fn resolve_image_base(process: HANDLE) -> *mut std::ffi::c_void {
    unsafe {
        // this is probably overkil, but this function only gets called once per debug attach, so it is probably okay
        let mut modules = [HMODULE(null_mut()); 1024];
        let mut bytes = 0u32;

        let modules_result = EnumProcessModules(
            process,
            modules.as_mut_ptr(),
            std::mem::size_of_val(&modules) as u32,
            &mut bytes,
        );
        if modules_result.is_err() {
            return null_mut();
        }

        let mut mod_info = MODULEINFO::default();
        // always assuming the first module is the main executable
        let information_result = GetModuleInformation(
            process,
            modules[0],
            &mut mod_info,
            std::mem::size_of::<MODULEINFO>() as u32,
        );
        if information_result.is_err() {
            return null_mut();
        }

        return mod_info.lpBaseOfDll;
    }
}

/// Attaches the debugger to a running process by its name
pub fn attach_to_process(name: PCWSTR) -> Option<HANDLE> {
    let process_handle = get_process_handle(name, PROCESS_ALL_ACCESS.0);
    if process_handle.is_invalid() {
        return None;
    }

    let attach_result = unsafe { 
        DebugActiveProcess(GetProcessId(process_handle))
    };
    if attach_result.is_err() {
        unsafe { 
            _ = CloseHandle(process_handle);
        };
        return None;
    }

    return Some(process_handle);
}

/// Loop indefinitely until a thread is spawned within the target process and its triggers a debug event
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

            _ = ContinueDebugEvent(
                debug_event.dwProcessId,
                debug_event.dwThreadId,
                DBG_CONTINUE,
            );
        }
    }
}

/// Use int3 to inject a breakpoint into the target process
pub fn inject_software_breakpoint_at(relative_virtual_address: u32, original: &mut u8) -> bool {
    const INT3: u8 = 0xCC;

    unsafe {
        let image_base = resolve_image_base(GetCurrentProcess());
        let target_address = (image_base as usize + relative_virtual_address as usize) as *mut u8;

        let mut old = PAGE_PROTECTION_FLAGS::default();
        if VirtualProtect(target_address as _, 1, PAGE_EXECUTE_READWRITE, &mut old).is_err() {
            return false;
        }

        let mut bytes_read = 0usize;
        if ReadProcessMemory(GetCurrentProcess(), target_address as *const c_void, original as *mut u8 as *mut c_void, 1, Some(&mut bytes_read)).is_err() || bytes_read != 1 {
            return false;
        }

        let mut bytes_written = 0usize;
        if WriteProcessMemory(GetCurrentProcess(), target_address as *mut c_void, &INT3 as *const u8 as *const c_void, 1, Some(&mut bytes_written)).is_err() || bytes_written != 1 {
            return false;
        }

        let mut dummy = PAGE_PROTECTION_FLAGS::default();
        if VirtualProtect(target_address as _, 1, old, &mut dummy).is_err() {
            return false;
        }

        // must flush cache to ensure breakpoint is active
        _ = FlushInstructionCache(GetCurrentProcess(), Some(target_address as *const c_void), 1);

        return true;
    }
}

/// Use DR0 and DR7 to inject an x86 hardware breakpoint
pub fn inject_hardware_breakpoint_at(relative_virtual_address: u32) -> bool {
    unsafe {
        let mut context = CONTEXT::default();
        context.ContextFlags = CONTEXT_DEBUG_REGISTERS_X86;
        if GetThreadContext(GetCurrentThread(), &mut context).is_err() {
            return false;
        }

        let image_base = resolve_image_base(GetCurrentProcess());
        let target_address = (image_base as usize + relative_virtual_address as usize) as u64;

        context.Dr0 = target_address;
        // thread local enable for DR0 hardware breakpoint
        context.Dr7 = (context.Dr7 & !0xF) | 0x1;

        if SetThreadContext(GetCurrentThread(), &context).is_err() {
            return false;
        }

        return true;
    }
}

/// Enable single-stepping on the target thread handle using the x86 trap flag
pub fn enable_single_step(thread: HANDLE) -> bool {
    unsafe {
        let mut context = CONTEXT::default();
        context.ContextFlags = CONTEXT_ALL_X86;

        let get_result = GetThreadContext(thread, &mut context);
        if get_result.is_err() {
            return false;
        }

        context.EFlags |= 0x100;

        let set_result = SetThreadContext(thread, &context);
        _ = ContinueDebugEvent(GetCurrentProcessId(), GetCurrentThreadId(), DBG_CONTINUE);
        
        return set_result.is_ok();
    }
}
