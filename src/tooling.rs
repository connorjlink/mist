use std::{ptr::null_mut};
use windows::{
    Win32::{
        Foundation::*,
        System::{
            Diagnostics::{Debug::*, ToolHelp::*}, ProcessStatus::*, Threading::*
        },
    }, core::*
};

use crate::utilities::*;

/// Retrieves a handle to a process by its name.
pub fn get_process_handle(name: PCWSTR, desired_access: u32) -> HANDLE {
    unsafe {
        let snapshot_result = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot_result.is_err() {
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
        ContinueDebugEvent(GetCurrentProcessId(), GetCurrentThreadId(), DBG_CONTINUE);
        
        return set_result.is_ok();
    }
}

