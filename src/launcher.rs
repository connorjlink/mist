use std::ffi::{CStr, CString, c_void};
use std::os::raw::c_char;
use std::collections::HashMap;
use std::ptr::null_mut;

use windows::{
    Win32::{
        Foundation::*,
        System::{
            Diagnostics::Debug::*,
            Threading::*,
            Memory::*,
        }
    },
    core::PCSTR,
};

use crate::control::{controller, DebugCommand, StopReason};

// Mist launcher.rs
// (c) Connor J. Link. All Rights Reserved.

const INT3: u8 = 0xCC;

const EXCEPTION_BREAKPOINT_CODE: NTSTATUS = NTSTATUS(0x8000_0003u32 as i32);
const EXCEPTION_SINGLE_STEP_CODE: NTSTATUS = NTSTATUS(0x8000_0004u32 as i32);

// IMPORTANT NOTE: compiler and this debugger must be built x64 and debug x86 targets
type Address = u32;
const WOW64_CONTEXT_CONTROL: u32 = 0x0001_0001;

fn get_ip(ctx: &WOW64_CONTEXT) -> Address {
    ctx.Eip
}

fn set_ip(ctx: &mut WOW64_CONTEXT, ip: Address) {
    ctx.Eip = ip;
}

fn get_sp(ctx: &WOW64_CONTEXT) -> Address {
    ctx.Esp
}

#[derive(Debug, Clone, Copy)]
struct SoftwareBreakpoint {
    address: Address,
    original: u8,
    temporary: bool,
}

#[derive(Debug, Clone, Copy)]
enum PendingReinsert {
    None,
    At(Address),
}

struct DebugEngine {
    process: HANDLE,
    threads: HashMap<u32, HANDLE>,
    breakpoints: HashMap<Address, SoftwareBreakpoint>,
    pending_reinsert: PendingReinsert,
}

impl DebugEngine {
    fn new() -> Self {
        Self {
            process: HANDLE(null_mut()),
            threads: HashMap::new(),
            breakpoints: HashMap::new(),
            pending_reinsert: PendingReinsert::None,
        }
    }

    fn thread_handle(&self, thread_id: u32) -> Option<HANDLE> {
        self.threads.get(&thread_id).copied()
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

    fn set_breakpoint(&mut self, addr: Address, temporary: bool) -> Result<(), String> {
        if self.process.is_invalid() {
            return Err("set_breakpoint: no process handle".to_string());
        }
        if self.breakpoints.contains_key(&addr) {
            return Ok(());
        }

        let mut old_protect = PAGE_PROTECTION_FLAGS::default();
        unsafe { VirtualProtectEx(self.process, addr as usize as *const c_void, 1, PAGE_EXECUTE_READWRITE, &mut old_protect) }
            .map_err(|e| format!("VirtualProtectEx failed: {e}"))?;

        let mut original = 0u8;
        let mut bytes_read = 0usize;
        unsafe { ReadProcessMemory(self.process, addr as usize as *const c_void, &mut original as *mut u8 as *mut c_void, 1, Some(&mut bytes_read)) }
            .map_err(|e| format!("ReadProcessMemory failed: {e}"))?;
        if bytes_read != 1 {
            return Err("ReadProcessMemory: short read".to_string());
        }

        let mut bytes_written = 0usize;
        unsafe { WriteProcessMemory(self.process, addr as usize as *mut c_void, &INT3 as *const u8 as *const c_void, 1, Some(&mut bytes_written)) }
            .map_err(|e| format!("WriteProcessMemory failed: {e}"))?;
        if bytes_written != 1 {
            return Err("WriteProcessMemory: short write".to_string());
        }

        let mut dummy = PAGE_PROTECTION_FLAGS::default();
        unsafe { VirtualProtectEx(self.process, addr as usize as *const c_void, 1, old_protect, &mut dummy) }
            .map_err(|e| format!("VirtualProtectEx restore failed: {e}"))?;

        unsafe {
            _ = FlushInstructionCache(self.process, Some(addr as usize as *const c_void), 1);
        }

        self.breakpoints.insert(
            addr,
            SoftwareBreakpoint {
                address: addr,
                original,
                temporary,
            },
        );

        return Ok(());
    }

    fn clear_breakpoint(&mut self, addr: Address) -> Result<(), String> {
        let Some(bp) = self.breakpoints.remove(&addr) else {
            return Ok(());
        };

        let mut old_protect = PAGE_PROTECTION_FLAGS::default();
        unsafe { VirtualProtectEx(self.process, addr as usize as *const c_void, 1, PAGE_EXECUTE_READWRITE, &mut old_protect) }
            .map_err(|e| format!("VirtualProtectEx failed: {e}"))?;

        let mut bytes_written = 0usize;
        unsafe { WriteProcessMemory(self.process, addr as usize as *mut c_void, &bp.original as *const u8 as *const c_void, 1, Some(&mut bytes_written)) }
            .map_err(|e| format!("WriteProcessMemory failed: {e}"))?;
        if bytes_written != 1 {
            return Err("WriteProcessMemory: short write".to_string());
        }

        let mut dummy = PAGE_PROTECTION_FLAGS::default();
        unsafe { VirtualProtectEx(self.process, addr as usize as *const c_void, 1, old_protect, &mut dummy) }
            .map_err(|e| format!("VirtualProtectEx restore failed: {e}"))?;

        unsafe {
            _ = FlushInstructionCache(self.process, Some(addr as usize as *const c_void), 1);
        }

        return Ok(());
    }

    fn reinsert_breakpoint(&mut self, addr: Address) -> Result<(), String> {
        if !self.breakpoints.contains_key(&addr) {
            return Ok(());
        }
        // restore persistent breakpoint: original byte is already in the map
        let mut old_protect = PAGE_PROTECTION_FLAGS::default();
        unsafe { VirtualProtectEx(self.process, addr as usize as *const c_void, 1, PAGE_EXECUTE_READWRITE, &mut old_protect) }
            .map_err(|e| format!("VirtualProtectEx failed: {e}"))?;
        
        let mut bytes_written = 0usize;
        unsafe { WriteProcessMemory(self.process, addr as usize as *mut c_void, &INT3 as *const u8 as *const c_void, 1, Some(&mut bytes_written)) }
            .map_err(|e| format!("WriteProcessMemory failed: {e}"))?;

        let mut dummy = PAGE_PROTECTION_FLAGS::default();
        unsafe { VirtualProtectEx(self.process, addr as usize as *const c_void, 1, old_protect, &mut dummy) }
            .map_err(|e| format!("VirtualProtectEx restore failed: {e}"))?;

        unsafe {
            _ = FlushInstructionCache(self.process, Some(addr as usize as *const c_void), 1);
        }

        return Ok(());
    }

    fn get_context(&self, thread: HANDLE) -> Result<WOW64_CONTEXT, String> {
        let mut context = WOW64_CONTEXT::default();
        context.ContextFlags = WOW64_CONTEXT_FLAGS(WOW64_CONTEXT_CONTROL);

        unsafe { Wow64GetThreadContext(thread, &mut context) }
            .map_err(|e| format!("Wow64GetThreadContext failed: {e}"))?;

        return Ok(context);
    }

    fn set_context(&self, thread: HANDLE, ctx: &WOW64_CONTEXT) -> Result<(), String> {
        unsafe { Wow64SetThreadContext(thread, ctx) }
            .map_err(|e| format!("Wow64SetThreadContext failed: {e}"))?;
        
        return Ok(());
    }

    fn enable_trap_flag(&self, thread: HANDLE) -> Result<(), String> {
        let mut context = self.get_context(thread)?;
        context.EFlags |= 0x100;
        self.set_context(thread, &context)
    }

    fn clear_trap_flag(&self, thread: HANDLE) -> Result<(), String> {
        let mut context = self.get_context(thread)?;
        context.EFlags &= !0x100;
        self.set_context(thread, &context)
    }

    fn adjust_ip_back_after_int3(&self, thread: HANDLE) -> Result<(), String> {
        let mut context = self.get_context(thread)?;
        let ip = get_ip(&context);
        if ip > 0 {
            set_ip(&mut context, ip - 1);
        }
        self.set_context(thread, &context)
    }

    fn read_u8(&self, addr: Address) -> Result<u8, String> {
        let mut value = 0u8;
        let mut bytes_read = 0usize;

        unsafe { ReadProcessMemory(self.process, addr as usize as *const c_void, &mut value as *mut u8 as *mut c_void, 1, Some(&mut bytes_read)) }
            .map_err(|e| format!("ReadProcessMemory failed: {e}"))?;
        if bytes_read != 1 {
            return Err("ReadProcessMemory: short read".to_string());
        }

        return Ok(value);
    }

    fn read_u32(&self, addr: Address) -> Result<u32, String> {
        let mut value: u32 = 0;
        let mut bytes_read = 0usize;
        let size = std::mem::size_of::<u32>();

        unsafe { ReadProcessMemory(self.process, addr as usize as *const c_void, &mut value as *mut u32 as *mut c_void, size, Some(&mut bytes_read)) }
            .map_err(|e| format!("ReadProcessMemory failed: {e}"))?;

        if bytes_read != size {
            return Err("ReadProcessMemory: short read".to_string());
        }
        
        return Ok(value);
    }

    fn step_in(&self, thread: HANDLE) -> Result<(), String> {
        self.enable_trap_flag(thread)
    }

    fn step_over(&mut self, thread: HANDLE) -> Result<(), String> {
        let ctx = self.get_context(thread)?;
        let ip = get_ip(&ctx);

        // NOTE: compiler is hardcoded to produce only E8 calls, length 5
        let opcode = self.read_u8(ip)?;
        if opcode == 0xE8 {
            let next_ip = ip.wrapping_add(5);
            self.set_breakpoint(next_ip, true)?;
            return Ok(());
        }

        self.step_in(thread)
    }

    fn step_out(&mut self, thread: HANDLE) -> Result<(), String> {
        let ctx = self.get_context(thread)?;
        let sp = get_sp(&ctx);
        let return_addr = self.read_u32(sp)?;
        self.set_breakpoint(return_addr, true)
    }
}

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

        controller().set_session_active(true);

        let mut engine = DebugEngine::new();
        engine.process = process_info.hProcess;
        engine.threads.insert(process_info.dwThreadId, process_info.hThread);

        let mut debug_event = DEBUG_EVENT::default();

        loop {
            if WaitForDebugEvent(&mut debug_event, u32::MAX).is_err() {
                break;
            }

            let pid = debug_event.dwProcessId;
            let tid = debug_event.dwThreadId;

            match debug_event.dwDebugEventCode {
                CREATE_PROCESS_DEBUG_EVENT => {
                    let file = debug_event.u.CreateProcessInfo.hFile;
                    if !file.is_invalid() {
                        _ = CloseHandle(file);
                    }
                }
                CREATE_THREAD_DEBUG_EVENT => {
                    let h_thread = debug_event.u.CreateThread.hThread;
                    if !h_thread.is_invalid() {
                        engine.threads.insert(tid, h_thread);
                    } else {
                        if let Ok(opened) = OpenThread(THREAD_ALL_ACCESS, false, tid) {
                            if !opened.is_invalid() {
                                engine.threads.insert(tid, opened);
                            }
                        }
                    }
                }
                EXIT_THREAD_DEBUG_EVENT => {
                    if let Some(h) = engine.threads.remove(&tid) {
                        if !h.is_invalid() {
                            _ = CloseHandle(h);
                        }
                    }
                }
                EXCEPTION_DEBUG_EVENT => {
                    let code = debug_event.u.Exception.ExceptionRecord.ExceptionCode;
                    if code == EXCEPTION_BREAKPOINT_CODE {
                        // the thread just hit a registered debugger
                        if let Some(thread) = engine.thread_handle(tid) {
                            let context = engine.get_context(thread)?;
                            let breakpoint_address = get_ip(&context).wrapping_sub(1);

                            if let Some(bp) = engine.breakpoints.get(&breakpoint_address).copied() {
                                // rewind the instruction pointer
                                engine.clear_breakpoint(breakpoint_address)?;
                                engine.adjust_ip_back_after_int3(thread)?;

                                if !bp.temporary {
                                    engine.pending_reinsert = PendingReinsert::At(breakpoint_address);
                                    engine.enable_trap_flag(thread)?;
                                    _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
                                    continue;
                                }

                                controller().notify_stop(StopReason::Breakpoint, tid);
                                let command = controller().wait_for_command();
                                apply_command(&mut engine, tid, command)?;
                                _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
                                continue;
                            }

                            // unknown, have not encountered this in testing yet...
                            // probably just an application breakpoint
                            controller().notify_stop(StopReason::Breakpoint, tid);
                            let command = controller().wait_for_command();
                            apply_command(&mut engine, tid, command)?;
                            _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
                            continue;
                        }
                    } else if code == EXCEPTION_SINGLE_STEP_CODE {
                        if let Some(thread) = engine.thread_handle(tid) {
                            // reinsert persistent breakpoint and continue
                            if let PendingReinsert::At(addr) = engine.pending_reinsert {
                                engine.pending_reinsert = PendingReinsert::None;
                                _ = engine.clear_trap_flag(thread);
                                _ = engine.reinsert_breakpoint(addr);

                                if let Some(cmd) = controller().try_take_command() {
                                    apply_command(&mut engine, tid, cmd)?;
                                }

                                _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
                                continue;
                            }

                            // stopping execution after the step and re-issue another debug command
                            _ = engine.clear_trap_flag(thread);
                            controller().notify_stop(StopReason::SingleStep, tid);
                            let command = controller().wait_for_command();
                            apply_command(&mut engine, tid, command)?;
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

            _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
        }

        controller().set_session_active(false);

        for (_, handle) in engine.threads.drain() {
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
    }

    return Ok(());
}

unsafe fn apply_command(engine: &mut DebugEngine, thread_id: u32, cmd: DebugCommand) -> Result<(), String> {
    let Some(thread) = engine.thread_handle(thread_id) else {
        return Err(format!("apply_command: missing thread handle for thread {}", thread_id));
    };

    match cmd {
        DebugCommand::Continue => Ok(()),
        DebugCommand::StepIn => engine.step_in(thread),
        DebugCommand::Next => engine.step_over(thread),
        DebugCommand::StepOut => engine.step_out(thread),
    }
}

