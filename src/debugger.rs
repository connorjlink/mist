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

use crate::dap::*;

// Mist debugger.rs
// (c) Connor J. Link. All Rights Reserved.

pub struct Debugger {
    // debugger information
    toolhelp_snapshot: HANDLE,

    // debugee information
    process_id: Option<u32>,
    thread_id: Option<u32>,
    thread_handle: Option<HANDLE>,
    process_handle: Option<HANDLE>,
    image_base: Option<*mut c_void>,
}

impl Drop for Debugger {
    fn drop(&mut self) {
        if !self.toolhelp_snapshot.is_invalid() {
            unsafe { _ = CloseHandle(self.toolhelp_snapshot) };
        }
    }
}

impl Debugger {
    pub fn snapshot_process(process_id: u32) -> DebuggerResult<HANDLE> {
        // use proper flags to capture modules and threads for the specified process
        let flags = TH32CS_SNAPPROCESS | TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32 | TH32CS_SNAPTHREAD;
        
        let toolhelp_snapshot = unsafe { CreateToolhelp32Snapshot(flags, process_id) }
            .map_err(|e| DebuggerError(format!("CreateToolhelp32Snapshot failed: {e}")))?;

        if toolhelp_snapshot.is_invalid() {
            return Err(DebuggerError("Toolhelp snapshot handle is invalid".to_string()));
        }

        Ok(toolhelp_snapshot)
    }

    pub fn attach_debugger(name: PCWSTR) -> DebuggerResult<Debugger> {
        let (process_id, process_handle) = Self::attach_to_process(name)?;
        let thread_id = Self::await_get_thread_id()?;

        let thread_handle = unsafe { OpenThread(THREAD_ALL_ACCESS, false, thread_id) }
            .map_err(|e| DebuggerError(format!("Failed to open thread: {e}")))?;
        
        if thread_handle.is_invalid() {
            return Err(DebuggerError("Opened thread handle is invalid".to_string()));
        }

        let image_base = Self::resolve_image_base(process_handle)?;
        let toolhelp_snapshot = Self::snapshot_process(process_id)?;

        return Ok(Debugger {
            toolhelp_snapshot,
            process_id: Some(process_id),
            thread_id: Some(thread_id),
            thread_handle: Some(thread_handle),
            process_handle: Some(process_handle),
            image_base: Some(image_base),
        });
    }

    pub fn get_process_handle(name: PCWSTR, desired_access: u32) -> DebuggerResult<(u32, HANDLE)> {
        let snapshot = Self::snapshot_process(0)?;
    
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
    
        if unsafe { Process32FirstW(snapshot, &mut entry) }.is_err() {
            unsafe { _ = CloseHandle(snapshot) };
            return Err(DebuggerError("Process32FirstW failed".to_string()));
        }
    
        let mut result = None;
        loop {
            let exe_name = PCWSTR(entry.szExeFile.as_ptr());
            if Self::compare_pcwstr_case_insensitive(exe_name, name) {
                let access_rights = PROCESS_ACCESS_RIGHTS(desired_access);
    
                match unsafe { OpenProcess(access_rights, false, entry.th32ProcessID) } {
                    Ok(handle) => {
                        result = Some((entry.th32ProcessID, handle));
                        break;
                    }
                    Err(error) => {
                        unsafe { _ = CloseHandle(snapshot) };
                        return Err(DebuggerError(format!("Failed to open process: {error}")));
                    }
                }
            }
    
            if unsafe { Process32NextW(snapshot, &mut entry) }.is_err() {
                break;
            }
        }
    
        unsafe { _ = CloseHandle(snapshot) };
        
        match result {
            Some(res) => return Ok(res),
            None => {
                let name_str = unsafe { name.to_string() }.unwrap_or_else(|_| "Unknown process".to_string());
                return Err(DebuggerError(format!("Failed to find process matching name: {}", name_str)));
            }
        }
    }
    
    pub fn resolve_image_base(process: HANDLE) -> DebuggerResult<*mut std::ffi::c_void> {
        let mut modules = [HMODULE(null_mut()); 1024];
        let mut bytes = 0u32;
    
        unsafe { EnumProcessModulesEx(process, modules.as_mut_ptr(), std::mem::size_of_val(&modules) as u32, &mut bytes, LIST_MODULES_32BIT) }
            .map_err(|e| DebuggerError(format!("EnumProcessModulesEx failed: {e}")))?;
        
        // this isn't actually possible if the executable is valid??
        if bytes == 0 {
            return Err(DebuggerError("No modules loaded in the target process".to_string()));
        }
    
        let mut module_info = MODULEINFO::default();
        unsafe { GetModuleInformation(process, modules[0], &mut module_info, std::mem::size_of::<MODULEINFO>() as u32) }
            .map_err(|e| DebuggerError(format!("GetModuleInformation failed: {e}")))?;
    
        return Ok(module_info.lpBaseOfDll);
    }
    
    pub fn attach_to_process(name: PCWSTR) -> DebuggerResult<(u32, HANDLE)> {
        let (process_id, process_handle) = Self::get_process_handle(name, PROCESS_ALL_ACCESS.0)?;
        if process_handle.is_invalid() {
            return Err(DebuggerError("Retrieved process handle is invalid".to_string()));
        }
    
        unsafe { DebugActiveProcess(process_id) }
            .map_err(|e| {
                unsafe { _ = CloseHandle(process_handle) };
                return DebuggerError(format!("DebugActiveProcess failed: {e}"));
            })?;
    
        return Ok((process_id, process_handle));
    }
    
    pub fn await_get_thread_id() -> DebuggerResult<u32> {
        let mut debug_event = DEBUG_EVENT::default();
    
        loop {
            unsafe { WaitForDebugEvent(&mut debug_event, INFINITE) }
                .map_err(|e| DebuggerError(format!("WaitForDebugEvent failed: {e}")))?;
    
            if debug_event.dwDebugEventCode == EXCEPTION_DEBUG_EVENT && 
               unsafe { debug_event.u.Exception.ExceptionRecord.ExceptionCode } == EXCEPTION_BREAKPOINT {
                return Ok(debug_event.dwThreadId);
            }
    
            unsafe { ContinueDebugEvent(debug_event.dwProcessId, debug_event.dwThreadId, DBG_CONTINUE) }
                .map_err(|e| DebuggerError(format!("ContinueDebugEvent failed: {e}")))?;
        }
    }
}

fn compare_pcwstr_case_insensitive(a: PCWSTR, b: PCWSTR) -> bool {
    let a_string = unsafe { a.to_string() };
    let b_string = unsafe { b.to_string() };

    match (a_string, b_string) {
        (Ok(ok_a), Ok(ok_b)) => ok_a.to_lowercase() == ok_b.to_lowercase(),
        _ => false,
    }
}
