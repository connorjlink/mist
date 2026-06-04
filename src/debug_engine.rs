use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, c_void};
use std::os::raw::c_char;
use std::ptr::null_mut;
use std::sync::{Mutex, OnceLock};

use windows::{
    Win32::{
        Foundation::*,
        System::{Diagnostics::Debug::*, Memory::*, Threading::*},
    },
    core::PCSTR,
};

use crate::debug_controller::{DebugCommand, StopReason, controller};
use crate::dap::{DebuggerError, DebuggerResult};

// Mist debug_engine.rs
// (c) Connor J. Link. All Rights Reserved.

// IMPORTANT NOTE: compiler and this debugger must be built x64 and debug x86 targets, thus the following are safe assumptions:
type Address = u32;

const INT3: u8 = 0xCC;

const EXCEPTION_BREAKPOINT_CODE: NTSTATUS = NTSTATUS(0x8000_0003u32 as i32);
const EXCEPTION_SINGLE_STEP_CODE: NTSTATUS = NTSTATUS(0x8000_0004u32 as i32);


#[derive(Debug, Clone, Copy)]
struct SoftwareBreakpoint {
    address: Address,
    original: u8,
    temporary: bool,
}

const DR0: usize = 0;
const DR1: usize = 1;
const DR2: usize = 2;
const DR3: usize = 3;

#[derive(Debug, Clone, Copy)]
struct HardwareBreakpoint {
    address: Address,
    slot: usize
}


#[derive(Debug, Clone, Copy)]
enum PendingReinsert {
    None,
    At(Address),
}

pub struct DebugEngine {
    process: HANDLE,
    threads: HashMap<u32, HANDLE>,
    breakpoints: HashMap<Address, SoftwareBreakpoint>,
    pending_reinsert: PendingReinsert,
    hardware_breakpoints: [Option<Address>; 4],
    requested_function_breakpoints: Vec<String>,
    image_base: Option<Address>,
    function_symbols_rva: HashMap<String, Address>,
    function_symbols_va: HashMap<String, Address>,
    hardware_breakpoints_dirty: bool,
}

// NOTE: DebugEngine is not thread safe can only submit debug syscalls from the original launching thread
unsafe impl Send for DebugEngine {}
unsafe impl Sync for DebugEngine {}

static ENGINE: OnceLock<Mutex<DebugEngine>> = OnceLock::new();
pub fn engine() -> &'static Mutex<DebugEngine> {
    ENGINE.get_or_init(|| Mutex::new(DebugEngine::new()))
}

impl DebugEngine {
    fn new() -> Self {
        return Self {
            process: HANDLE(null_mut()),
            threads: HashMap::new(),
            breakpoints: HashMap::new(),
            pending_reinsert: PendingReinsert::None,
            hardware_breakpoints: [None, None, None, None],
            requested_function_breakpoints: Vec::new(),
            image_base: None,
            function_symbols_rva: HashMap::new(),
            function_symbols_va: HashMap::new(),
            hardware_breakpoints_dirty: true,
        };
    }

    fn mark_dirty(&mut self) {
        self.hardware_breakpoints_dirty = true;
    }

    fn resolve_function_address(&self, name: &str) -> Option<Address> {
        if let Some(&va) = self.function_symbols_va.get(name) {
            return Some(va);
        }
        if let Some(&rva) = self.function_symbols_rva.get(name) {
            let base = self.image_base?;
            return Some(base.wrapping_add(rva));
        }
        return parse_address_literal(name);
    }

    fn thread_handle(&self, thread_id: u32) -> Option<HANDLE> {
        return self.threads.get(&thread_id).copied();
    }

    // TODO: refactor and use only for function-level breakpoints?
    // fn inject_hardware_breakpoint_at(target_address: u32) -> bool {
    //     unsafe {
    //         let current_process = GetCurrentProcess();

    //         let mut context = WOW64_CONTEXT::default();
    //         context.ContextFlags = WOW64_CONTEXT_DEBUG_REGISTERS;
    //         if Wow64GetThreadContext(current_process, &mut context).is_err() {
    //             return false;
    //         }

    //         context.Dr0 = target_address;
    //         // thread local enable for DR0 hardware breakpoint
    //         context.Dr7 = (context.Dr7 & !0xF) | 0x1;

    //         if Wow64SetThreadContext(current_process, &context).is_err() {
    //             return false;
    //         }

    //         return true;
    //     }
    // }

    fn set_breakpoint(&mut self, address: Address, temporary: bool) -> DebuggerResult<()> {
        if self.process.is_invalid() {
            return Err(DebuggerError("set_breakpoint: no process handle".to_string()));
        }
        if self.breakpoints.contains_key(&address) {
            return Ok(());
        }

        let mut old_protect = PAGE_PROTECTION_FLAGS::default();
        unsafe { VirtualProtectEx(self.process, address as usize as *const c_void, 1, PAGE_EXECUTE_READWRITE, &mut old_protect) }
            .map_err(|e| DebuggerError(format!("VirtualProtectEx failed: {e}")))?;

        let mut original = 0u8;
        let mut bytes_read = 0usize;
        unsafe { ReadProcessMemory(self.process, address as usize as *const c_void, &mut original as *mut u8 as *mut c_void, 1, Some(&mut bytes_read)) }
            .map_err(|e| DebuggerError(format!("ReadProcessMemory failed: {e}")))?;
        if bytes_read != 1 {
            return Err(DebuggerError("ReadProcessMemory: short read".to_string()));
        }

        let mut bytes_written = 0usize;
        unsafe { WriteProcessMemory(self.process, address as usize as *mut c_void, &INT3 as *const u8 as *const c_void, 1, Some(&mut bytes_written)) }
            .map_err(|e| DebuggerError(format!("WriteProcessMemory failed: {e}")))?;
        if bytes_written != 1 {
            return Err(DebuggerError("WriteProcessMemory: short write".to_string()));
        }

        let mut dummy = PAGE_PROTECTION_FLAGS::default();
        unsafe { VirtualProtectEx(self.process, address as usize as *const c_void, 1, old_protect, &mut dummy) }
            .map_err(|e| DebuggerError(format!("VirtualProtectEx restore failed: {e}")))?;

        unsafe { _ = FlushInstructionCache(self.process, Some(address as usize as *const c_void), 1); }

        self.breakpoints.insert(
            address,
            SoftwareBreakpoint {
                address: address,
                original,
                temporary,
            },
        );

        return Ok(());
    }

    fn clear_breakpoint(&mut self, address: Address) -> DebuggerResult<()> {
        let Some(bp) = self.breakpoints.remove(&address) else {
            return Ok(());
        };

        let mut old_protect = PAGE_PROTECTION_FLAGS::default();
        unsafe { VirtualProtectEx(self.process, address as usize as *const c_void, 1, PAGE_EXECUTE_READWRITE, &mut old_protect) }
            .map_err(|e| DebuggerError(format!("VirtualProtectEx failed: {e}")))?;

        let mut bytes_written = 0usize;
        unsafe { WriteProcessMemory(self.process, address as usize as *mut c_void, &bp.original as *const u8 as *const c_void, 1, Some(&mut bytes_written)) }
            .map_err(|e| DebuggerError(format!("WriteProcessMemory failed: {e}")))?;
        if bytes_written != 1 {
            return Err(DebuggerError("WriteProcessMemory: short write".to_string()));
        }

        let mut dummy = PAGE_PROTECTION_FLAGS::default();
        unsafe { VirtualProtectEx(self.process, address as usize as *const c_void, 1, old_protect, &mut dummy) }
            .map_err(|e| DebuggerError(format!("VirtualProtectEx restore failed: {e}")))?;

        unsafe { _ = FlushInstructionCache(self.process, Some(address as usize as *const c_void), 1); }

        return Ok(());
    }

    fn reinsert_breakpoint(&mut self, address: Address) -> DebuggerResult<()> {
        if !self.breakpoints.contains_key(&address) {
            return Ok(());
        }
        // restore persistent breakpoint: original byte is already in the map
        let mut old_protect = PAGE_PROTECTION_FLAGS::default();
        unsafe { VirtualProtectEx(self.process, address as usize as *const c_void, 1, PAGE_EXECUTE_READWRITE, &mut old_protect) }
            .map_err(|e| DebuggerError(format!("VirtualProtectEx failed: {e}")))?;
        
        let mut bytes_written = 0usize;
        unsafe { WriteProcessMemory(self.process, address as usize as *mut c_void, &INT3 as *const u8 as *const c_void, 1, Some(&mut bytes_written)) }
            .map_err(|e| DebuggerError(format!("WriteProcessMemory failed: {e}")))?;

        let mut dummy = PAGE_PROTECTION_FLAGS::default();
        unsafe { VirtualProtectEx(self.process, address as usize as *const c_void, 1, old_protect, &mut dummy) }
            .map_err(|e| DebuggerError(format!("VirtualProtectEx restore failed: {e}")))?;

        unsafe { _ = FlushInstructionCache(self.process, Some(address as usize as *const c_void), 1); }

        return Ok(());
    }

    fn get_context_flags(
        &self,
        thread: HANDLE,
        flags: WOW64_CONTEXT_FLAGS,
    ) -> DebuggerResult<WOW64_CONTEXT> {
        let mut context = WOW64_CONTEXT::default();
        context.ContextFlags = flags;

        unsafe { Wow64GetThreadContext(thread, &mut context) }
            .map_err(|e| DebuggerError(format!("Wow64GetThreadContext failed: {e}")))?;

        return Ok(context);
    }

    fn get_context(&self, thread: HANDLE) -> DebuggerResult<WOW64_CONTEXT> {
        return self.get_context_flags(thread, WOW64_CONTEXT_CONTROL);
    }

    fn get_context_with_debug(&self, thread: HANDLE) -> DebuggerResult<WOW64_CONTEXT> {
        return self.get_context_flags(
            thread,
            WOW64_CONTEXT_CONTROL | WOW64_CONTEXT_DEBUG_REGISTERS,
        );
    }

    fn set_context(&self, thread: HANDLE, context: &WOW64_CONTEXT) -> DebuggerResult<()> {
        unsafe { Wow64SetThreadContext(thread, context) }
            .map_err(|e| DebuggerError(format!("Wow64SetThreadContext failed: {e}")))?;

        return Ok(());
    }

    fn enable_trap_flag(&self, thread: HANDLE) -> DebuggerResult<()> {
        let mut context = self.get_context(thread)?;
        context.EFlags |= 0x100;
        return self.set_context(thread, &context);
    }

    fn clear_trap_flag(&self, thread: HANDLE) -> DebuggerResult<()> {
        let mut context = self.get_context(thread)?;
        context.EFlags &= !0x100;
        return self.set_context(thread, &context);
    }

    fn adjust_ip_back_after_int3(&self, thread: HANDLE) -> DebuggerResult<()> {
        let mut context = self.get_context(thread)?;
        let ip = context.Eip;
        if ip > 0 {
            context.Eip = ip - 1;
        }
        return self.set_context(thread, &context);
    }

    fn read_u8(&self, address: Address) -> DebuggerResult<u8> {
        let mut value = 0u8;
        let mut bytes_read = 0usize;

        unsafe { ReadProcessMemory(self.process, address as usize as *const c_void, &mut value as *mut u8 as *mut c_void, 1, Some(&mut bytes_read)) }
            .map_err(|e| DebuggerError(format!("ReadProcessMemory failed: {e}")))?;
        if bytes_read != 1 {
            return Err(DebuggerError("ReadProcessMemory: short read".to_string()));
        }

        return Ok(value);
    }

    fn read_u32(&self, address: Address) -> DebuggerResult<u32> {
        let mut value: u32 = 0;
        let mut bytes_read = 0usize;
        let size = std::mem::size_of::<u32>();

        unsafe { ReadProcessMemory(self.process, address as usize as *const c_void, &mut value as *mut u32 as *mut c_void, size, Some(&mut bytes_read)) }
            .map_err(|e| DebuggerError(format!("ReadProcessMemory failed: {e}")))?;

        if bytes_read != size {
            return Err(DebuggerError("ReadProcessMemory: short read".to_string()));
        }

        return Ok(value);
    }

    fn step_in(&self, thread: HANDLE) -> DebuggerResult<()> {
        self.enable_trap_flag(thread)
    }

    fn step_over(&mut self, thread: HANDLE) -> DebuggerResult<()> {
        let context = self.get_context(thread)?;
        let ip = context.Eip;

        // NOTE: the Haze compiler is hardcoded to produce only E8 calls, length 5
        let opcode = self.read_u8(ip)?;
        if opcode == 0xE8 {
            let next_ip = ip.wrapping_add(5);
            self.set_breakpoint(next_ip, true)?;
            return Ok(());
        }

        return self.step_in(thread);
    }

    fn step_out(&mut self, thread: HANDLE) -> DebuggerResult<()> {
        let context = self.get_context(thread)?;
        let esp = context.Esp;
        let return_addr = self.read_u32(esp)?;
        return self.set_breakpoint(return_addr, true);
    }

    fn set_hw_breakpoint_slot(
        &self,
        thread: HANDLE,
        slot: usize,
        address: Option<Address>,
    ) -> DebuggerResult<()> {
        if slot >= 4 {
            return Err(DebuggerError("set_hw_breakpoint_slot: slot out of range".to_string()));
        }

        let mut context = self.get_context_flags(thread, WOW64_CONTEXT_DEBUG_REGISTERS)?;

        let address_or = address.unwrap_or(0);
        match slot {
            DR0 => context.Dr0 = address_or,
            DR1 => context.Dr1 = address_or,
            DR2 => context.Dr2 = address_or,
            DR3 => context.Dr3 = address_or,
            _ => {}
        }

        // set RW/LEN and local enables
        let enable_bit = 1u32 << (slot * 2);
        if address.is_some() {
            context.Dr7 |= enable_bit;
        } else {
            context.Dr7 &= !enable_bit;
        }

        // clear RW/LEN for this slot (force execute, len=1)
        let rwlen_shift = 16 + (slot * 4);
        context.Dr7 &= !(0xFu32 << rwlen_shift);
        context.Dr6 = 0;

        unsafe { Wow64SetThreadContext(thread, &context) }
            .map_err(|e| DebuggerError(format!("Wow64SetThreadContext failed: {e}")))?;

        return Ok(());
    }

    fn apply_hw_breakpoints_to_thread(&self, thread: HANDLE) -> DebuggerResult<()> {
        for (slot, address) in self.hardware_breakpoints.iter().copied().enumerate() {
            self.set_hw_breakpoint_slot(thread, slot, address)?;
        }

        return Ok(());
    }

    fn sync_hw_breakpoints_from_registry(&mut self) -> DebuggerResult<()> {
        if !self.hardware_breakpoints_dirty {
            return Ok(());
        }
        self.hardware_breakpoints_dirty = false;

        let mut out = Vec::new();
        let mut seen = HashSet::<Address>::new();
        for name in &self.requested_function_breakpoints {
            if let Some(address) = self.resolve_function_address(name) {
                if seen.insert(address) {
                    out.push(address);
                    if out.len() == 4 {
                        break;
                    }
                }
            }
        }

        self.hardware_breakpoints = [None, None, None, None];
        for (i, address) in out.into_iter().enumerate().take(4) {
            self.hardware_breakpoints[i] = Some(address);
        }

        // commit breakpoint changes to all threads in case code is re-entrant
        for (_, thread) in self.threads.iter() {
            if !thread.is_invalid() {
                let _ = self.apply_hw_breakpoints_to_thread(*thread);
            }
        }

        return Ok(());
    }

    fn is_hw_breakpoint_exception(&self, thread: HANDLE) -> DebuggerResult<bool> {
        let context = self.get_context_flags(thread, WOW64_CONTEXT_DEBUG_REGISTERS)?;
        let dr6 = context.Dr6;
        return Ok((dr6 & 0xF) != 0);
    }

    fn clear_hw_breakpoint_status(&self, thread: HANDLE) -> DebuggerResult<()> {
        let mut context = self.get_context_flags(thread, WOW64_CONTEXT_DEBUG_REGISTERS)?;
        context.Dr6 = 0;
        unsafe { Wow64SetThreadContext(thread, &context) }
            .map_err(|e| DebuggerError(format!("Wow64SetThreadContext failed: {e}")))?;
        
        return Ok(());
    }
}

pub fn set_requested_function_breakpoints(names: Vec<String>) -> Vec<bool> {
    let mut e = engine().lock().unwrap();
    e.requested_function_breakpoints = names;
    e.mark_dirty();

    return e.requested_function_breakpoints
        .iter()
        .map(|name| {
            e.function_symbols_va.contains_key(name)
                || e.function_symbols_rva.contains_key(name)
                || parse_address_literal(name).is_some()
        })
        .collect();
}

fn cstr_to_string(pointer: *const c_char) -> Option<String> {
    if pointer.is_null() { return None; }
    let cstr = unsafe { CStr::from_ptr(pointer) };
    return Some(cstr.to_string_lossy().into_owned());
}

#[unsafe(no_mangle)]
pub extern "C" fn mist_clear_function_symbols() -> bool {
    let mut e = engine().lock().unwrap();
    e.function_symbols_rva.clear();
    e.function_symbols_va.clear();
    e.mark_dirty();
    return true;
}

#[unsafe(no_mangle)]
pub extern "C" fn mist_register_function_symbol_rva(name: *const c_char, rva: Address) -> bool {
    let Some(name) = cstr_to_string(name) else { return false; };
    let mut e = engine().lock().unwrap();
    e.function_symbols_rva.insert(name, rva);
    e.mark_dirty();
    return true;
}

#[unsafe(no_mangle)]
pub extern "C" fn mist_register_function_symbol_va(name: *const c_char, va: Address) -> bool {
    let Some(name) = cstr_to_string(name) else { return false; };
    let mut e = engine().lock().unwrap();
    e.function_symbols_va.insert(name, va);
    e.mark_dirty();
    return true;
}

#[unsafe(no_mangle)]
pub extern "C" fn mist_launch_target(target_path: *const c_char) {
    if target_path.is_null() {
        eprintln!("launch_target: target_path was null");
        return;
    }

    let target_path = unsafe { CStr::from_ptr(target_path) };
    let target_path = target_path.to_string_lossy().into_owned();

    std::thread::spawn(move || {
        if let Err(err) = launch_and_debug(&target_path) {
            eprintln!("launch_target: failed because {err:?}");
        }
    });
}

fn launch_and_debug(target_path: &str) -> DebuggerResult<()> {
    let app_path = CString::new(target_path)
        .map_err(|_| DebuggerError(format!("Invalid debugee path: {}", target_path)))?;

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
        .map_err(|e| DebuggerError(format!("Could not spawn debugee: {e}")))?;

        controller().set_session_active(true);

        {
            let mut guard = engine().lock().unwrap();
            guard.process = process_info.hProcess;
            guard.threads.insert(process_info.dwThreadId, process_info.hThread);
        }

        let mut debug_event = DEBUG_EVENT::default();

        loop {
            if WaitForDebugEvent(&mut debug_event, u32::MAX).is_err() {
                break;
            }

            let mut e = engine().lock().unwrap();
            e.sync_hw_breakpoints_from_registry()?;

            let pid = debug_event.dwProcessId;
            let tid = debug_event.dwThreadId;

            match debug_event.dwDebugEventCode {
                CREATE_PROCESS_DEBUG_EVENT => {
                    let base = debug_event.u.CreateProcessInfo.lpBaseOfImage as usize as u32;
                    e.image_base = Some(base);
                    e.mark_dirty();

                    let file = debug_event.u.CreateProcessInfo.hFile;
                    if !file.is_invalid() {
                        _ = CloseHandle(file);
                    }

                    if let Some(thread) = e.thread_handle(tid) {
                        e.apply_hw_breakpoints_to_thread(thread)?;
                    }
                }
                CREATE_THREAD_DEBUG_EVENT => {
                    let h_thread = debug_event.u.CreateThread.hThread;
                    if !h_thread.is_invalid() {
                        e.threads.insert(tid, h_thread);
                        let _ = e.apply_hw_breakpoints_to_thread(h_thread);
                    } else {
                        if let Ok(opened) = OpenThread(THREAD_ALL_ACCESS, false, tid) {
                            if !opened.is_invalid() {
                                e.threads.insert(tid, opened);
                                e.apply_hw_breakpoints_to_thread(opened)?;
                            }
                        }
                    }
                }
                EXIT_THREAD_DEBUG_EVENT => {
                    if let Some(h) = e.threads.remove(&tid) {
                        if !h.is_invalid() {
                            _ = CloseHandle(h);
                        }
                    }
                }
                EXCEPTION_DEBUG_EVENT => {
                    let code = debug_event.u.Exception.ExceptionRecord.ExceptionCode;
                    if code == EXCEPTION_BREAKPOINT_CODE {
                        if let Some(thread) = e.thread_handle(tid) {
                            let context = e.get_context(thread)?;
                            let breakpoint_address = context.Eip.wrapping_sub(1);

                            if let Some(bp) = e.breakpoints.get(&breakpoint_address).copied() {
                                e.clear_breakpoint(breakpoint_address)?;
                                e.adjust_ip_back_after_int3(thread)?;

                                if !bp.temporary {
                                    e.pending_reinsert = PendingReinsert::At(breakpoint_address);
                                    e.enable_trap_flag(thread)?;
                                    drop(e);
                                    _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
                                    continue;
                                }

                                controller().notify_stop(StopReason::Breakpoint, tid);
                                drop(e);
                                let command = controller().wait_for_command();
                                e = engine().lock().unwrap();
                                apply_command(&mut e, tid, command)?;
                                e.sync_hw_breakpoints_from_registry()?;
                                drop(e);
                                _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
                                continue;
                            }

                            controller().notify_stop(StopReason::Breakpoint, tid);
                            drop(e);
                            let command = controller().wait_for_command();
                            e = engine().lock().unwrap();
                            apply_command(&mut e, tid, command)?;
                            e.sync_hw_breakpoints_from_registry()?;
                            drop(e);
                            _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
                            continue;
                        }
                    } else if code == EXCEPTION_SINGLE_STEP_CODE {
                        if let Some(thread) = e.thread_handle(tid) {
                            if let PendingReinsert::At(address) = e.pending_reinsert {
                                e.pending_reinsert = PendingReinsert::None;
                                e.clear_trap_flag(thread)?;
                                e.reinsert_breakpoint(address)?;

                                if let Some(command) = controller().try_take_command() {
                                    apply_command(&mut e, tid, command)?;
                                }

                                drop(e);
                                _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
                                continue;
                            }

                            if e.is_hw_breakpoint_exception(thread)? {
                                e.clear_hw_breakpoint_status(thread)?;
                                controller().notify_stop(StopReason::Breakpoint, tid);
                                drop(e);
                                let command = controller().wait_for_command();
                                e = engine().lock().unwrap();
                                apply_command(&mut e, tid, command)?;
                                e.sync_hw_breakpoints_from_registry()?;
                                drop(e);
                                _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
                                continue;
                            }

                            e.clear_trap_flag(thread)?;
                            controller().notify_stop(StopReason::SingleStep, tid);
                            drop(e);
                            let command = controller().wait_for_command();
                            e = engine().lock().unwrap();
                            apply_command(&mut e, tid, command)?;
                            let _ = e.sync_hw_breakpoints_from_registry();
                            drop(e);
                            _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
                            continue;
                        }
                    }
                }
                EXIT_PROCESS_DEBUG_EVENT => {
                    controller().notify_stop(StopReason::ProcessExit, tid);
                    break;
                }
                _ => {}
            }

            drop(e);
            _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
        }

        controller().set_session_active(false);

        let mut final_guard = engine().lock().unwrap();
        for (_, handle) in final_guard.threads.drain() {
            if !handle.is_invalid() {
                _ = CloseHandle(handle);
            }
        }
        if !process_info.hThread.is_invalid() {
            _ = CloseHandle(process_info.hThread);
        }
        if !process_info.hProcess.is_invalid() {
            _ = CloseHandle(process_info.hProcess);
        }
        final_guard.process = HANDLE(null_mut());
    }

    return Ok(());
}

unsafe fn apply_command(
    engine: &mut DebugEngine,
    thread_id: u32,
    command: DebugCommand,
) -> DebuggerResult<()> {
    let Some(thread) = engine.thread_handle(thread_id) else {
        return Err(DebuggerError(format!(
            "apply_command: missing thread handle for thread {}",
            thread_id
        )));
    };

    return match command {
        DebugCommand::Continue => Ok(()),
        DebugCommand::StepIn => engine.step_in(thread),
        DebugCommand::StepOver => engine.step_over(thread),
        DebugCommand::StepOut => engine.step_out(thread),
    };
}

pub fn parse_address_literal(name: &str) -> Option<Address> {
    let trimmed = name.trim();
    if trimmed.is_empty() { return None; }
    if let Some(hex) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        return Address::from_str_radix(hex, 16).ok();
    }
    if trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Address::from_str_radix(trimmed, 16).ok();
    }
    return trimmed.parse::<Address>().ok();
}
