use std::collections::{HashMap, HashSet};
use std::os::raw::{c_char, c_void};
use std::ptr::null_mut;
use std::sync::{Mutex, OnceLock};

use windows::{
    Win32::{
        Foundation::*,
        System::{Diagnostics::Debug::*, Memory::*, Threading::*},
    },
    core::PCSTR,
};

use crate::dap::{DebuggerError, DebuggerResult};
use crate::debug_controller::{DebugCommand, StopReason, controller};
use crate::utility::*;

// Mist debug_engine.rs
// (c) Connor J. Link. All Rights Reserved.

// IMPORTANT NOTE: compiler and this debugger must be built x64 and debug x86 targets, thus the following are safe assumptions:
type Address = u32;

const INT3: u8 = 0xCC;

const EXCEPTION_BREAKPOINT_CODE: NTSTATUS = NTSTATUS(0x8000_0003u32 as i32);
const EXCEPTION_SINGLE_STEP_CODE: NTSTATUS = NTSTATUS(0x8000_0004u32 as i32);

#[derive(Debug, Clone, Copy)]
struct SoftwareBreakpoint
{
    address: Address,
    original: u8,
    temporary: bool,
}

const DR0: usize = 0;
const DR1: usize = 1;
const DR2: usize = 2;
const DR3: usize = 3;

#[derive(Debug, Clone, Copy)]
struct HardwareBreakpoint
{
    address: Address,
    slot: usize,
}

#[derive(Debug, Clone, Copy)]
enum PendingReinsert
{
    None,
    At(Address),
}

#[derive(Debug)]
pub struct DebugEngine
{
    process: HANDLE,
    threads: HashMap<u32, HANDLE>,
    software_breakpoints: HashMap<Address, SoftwareBreakpoint>,
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
pub fn get_engine() -> &'static Mutex<DebugEngine>
{
    ENGINE.get_or_init(|| Mutex::new(DebugEngine::new()))
}

impl DebugEngine
{
    fn new() -> Self
    {
        return Self {
            process: HANDLE(null_mut()),
            threads: HashMap::new(),
            software_breakpoints: HashMap::new(),
            pending_reinsert: PendingReinsert::None,
            hardware_breakpoints: [None, None, None, None],
            requested_function_breakpoints: Vec::new(),
            image_base: None,
            function_symbols_rva: HashMap::new(),
            function_symbols_va: HashMap::new(),
            hardware_breakpoints_dirty: true,
        };
    }

    fn mark_dirty(&mut self)
    {
        self.hardware_breakpoints_dirty = true;
    }

    fn resolve_function_address(&self, name: &str) -> Option<Address>
    {
        if let Some(&va) = self.function_symbols_va.get(name)
        {
            return Some(va);
        }
        if let Some(&rva) = self.function_symbols_rva.get(name)
        {
            let base = self.image_base?;
            return Some(base.wrapping_add(rva));
        }
        return parse_address_literal(name);
    }

    fn thread_handle(&self, thread_id: u32) -> Option<HANDLE>
    {
        return self.threads.get(&thread_id).copied();
    }

    fn set_breakpoint(&mut self, address: Address, temporary: bool) -> DebuggerResult<()>
    {
        if self.process.is_invalid()
        {
            return Err(DebuggerError("set_breakpoint: no process handle".to_string()));
        }
        if self.software_breakpoints.contains_key(&address)
        {
            return Ok(());
        }

        let mut old_protect = PAGE_PROTECTION_FLAGS::default();
        unsafe {
            VirtualProtectEx(
                self.process,
                address as usize as *const c_void,
                1,
                PAGE_EXECUTE_READWRITE,
                &mut old_protect,
            )
        }
        .map_err(|error| DebuggerError(format!("VirtualProtectEx failed: {error}")))?;

        let mut original = 0u8;
        let mut bytes_read = 0usize;
        unsafe {
            ReadProcessMemory(
                self.process,
                address as usize as *const c_void,
                &mut original as *mut u8 as *mut c_void,
                1,
                Some(&mut bytes_read),
            )
        }
        .map_err(|error| DebuggerError(format!("ReadProcessMemory failed: {error}")))?;
        if bytes_read != 1
        {
            return Err(DebuggerError("ReadProcessMemory: short read".to_string()));
        }

        let mut bytes_written = 0usize;
        unsafe {
            WriteProcessMemory(
                self.process,
                address as usize as *mut c_void,
                &INT3 as *const u8 as *const c_void,
                1,
                Some(&mut bytes_written),
            )
        }
        .map_err(|error| DebuggerError(format!("WriteProcessMemory failed: {error}")))?;
        if bytes_written != 1
        {
            return Err(DebuggerError("WriteProcessMemory: short write".to_string()));
        }

        let mut dummy = PAGE_PROTECTION_FLAGS::default();
        unsafe {
            VirtualProtectEx(
                self.process,
                address as usize as *const c_void,
                1,
                old_protect,
                &mut dummy,
            )
        }
        .map_err(|error| DebuggerError(format!("VirtualProtectEx restore failed: {error}")))?;

        unsafe {
            _ = FlushInstructionCache(self.process, Some(address as usize as *const c_void), 1);
        }

        self.software_breakpoints
            .insert(address, SoftwareBreakpoint { address: address, original, temporary });

        return Ok(());
    }

    fn clear_breakpoint(&mut self, address: Address) -> DebuggerResult<()>
    {
        let Some(breakpoint) = self.software_breakpoints.remove(&address)
        else
        {
            return Ok(());
        };

        let mut old_protect = PAGE_PROTECTION_FLAGS::default();
        unsafe {
            VirtualProtectEx(
                self.process,
                address as usize as *const c_void,
                1,
                PAGE_EXECUTE_READWRITE,
                &mut old_protect,
            )
        }
        .map_err(|error| DebuggerError(format!("VirtualProtectEx failed: {error}")))?;

        let mut bytes_written = 0usize;
        unsafe {
            WriteProcessMemory(
                self.process,
                address as usize as *mut c_void,
                &breakpoint.original as *const u8 as *const c_void,
                1,
                Some(&mut bytes_written),
            )
        }
        .map_err(|error| DebuggerError(format!("WriteProcessMemory failed: {error}")))?;
        if bytes_written != 1
        {
            return Err(DebuggerError("WriteProcessMemory: short write".to_string()));
        }

        let mut dummy = PAGE_PROTECTION_FLAGS::default();
        unsafe {
            VirtualProtectEx(
                self.process,
                address as usize as *const c_void,
                1,
                old_protect,
                &mut dummy,
            )
        }
        .map_err(|error| DebuggerError(format!("VirtualProtectEx restore failed: {error}")))?;

        unsafe {
            _ = FlushInstructionCache(self.process, Some(address as usize as *const c_void), 1);
        }

        return Ok(());
    }

    fn reinsert_breakpoint(&mut self, address: Address) -> DebuggerResult<()>
    {
        if !self.software_breakpoints.contains_key(&address)
        {
            return Ok(());
        }
        // restore persistent breakpoint: original byte is already in the map
        let mut old_protect = PAGE_PROTECTION_FLAGS::default();
        unsafe {
            VirtualProtectEx(
                self.process,
                address as usize as *const c_void,
                1,
                PAGE_EXECUTE_READWRITE,
                &mut old_protect,
            )
        }
        .map_err(|error| DebuggerError(format!("VirtualProtectEx failed: {error}")))?;

        let mut bytes_written = 0usize;
        unsafe {
            WriteProcessMemory(
                self.process,
                address as usize as *mut c_void,
                &INT3 as *const u8 as *const c_void,
                1,
                Some(&mut bytes_written),
            )
        }
        .map_err(|error| DebuggerError(format!("WriteProcessMemory failed: {error}")))?;

        let mut dummy = PAGE_PROTECTION_FLAGS::default();
        unsafe {
            VirtualProtectEx(
                self.process,
                address as usize as *const c_void,
                1,
                old_protect,
                &mut dummy,
            )
        }
        .map_err(|error| DebuggerError(format!("VirtualProtectEx restore failed: {error}")))?;

        unsafe {
            _ = FlushInstructionCache(self.process, Some(address as usize as *const c_void), 1);
        }

        return Ok(());
    }

    fn get_context_flags(
        &self,
        thread: HANDLE,
        flags: WOW64_CONTEXT_FLAGS,
    ) -> DebuggerResult<WOW64_CONTEXT>
    {
        let mut context = WOW64_CONTEXT::default();
        context.ContextFlags = flags;

        unsafe { Wow64GetThreadContext(thread, &mut context) }
            .map_err(|error| DebuggerError(format!("Wow64GetThreadContext failed: {error}")))?;

        return Ok(context);
    }

    fn get_context(&self, thread: HANDLE) -> DebuggerResult<WOW64_CONTEXT>
    {
        return self.get_context_flags(thread, WOW64_CONTEXT_CONTROL);
    }

    fn get_context_with_debug(&self, thread: HANDLE) -> DebuggerResult<WOW64_CONTEXT>
    {
        return self
            .get_context_flags(thread, WOW64_CONTEXT_CONTROL | WOW64_CONTEXT_DEBUG_REGISTERS);
    }

    fn set_context(&self, thread: HANDLE, context: &WOW64_CONTEXT) -> DebuggerResult<()>
    {
        unsafe { Wow64SetThreadContext(thread, context) }
            .map_err(|error| DebuggerError(format!("Wow64SetThreadContext failed: {error}")))?;

        return Ok(());
    }

    fn enable_trap_flag(&self, thread: HANDLE) -> DebuggerResult<()>
    {
        let mut context = self.get_context(thread)?;
        context.EFlags |= 0x100;
        return self.set_context(thread, &context);
    }

    fn clear_trap_flag(&self, thread: HANDLE) -> DebuggerResult<()>
    {
        let mut context = self.get_context(thread)?;
        context.EFlags &= !0x100;
        return self.set_context(thread, &context);
    }

    fn adjust_ip_back_after_int3(&self, thread: HANDLE) -> DebuggerResult<()>
    {
        let mut context = self.get_context(thread)?;
        let ip = context.Eip;
        if ip > 0
        {
            context.Eip = ip - 1;
        }
        return self.set_context(thread, &context);
    }

    fn read_u8(&self, address: Address) -> DebuggerResult<u8>
    {
        let mut value = 0u8;
        let mut bytes_read = 0usize;

        unsafe {
            ReadProcessMemory(
                self.process,
                address as usize as *const c_void,
                &mut value as *mut u8 as *mut c_void,
                1,
                Some(&mut bytes_read),
            )
        }
        .map_err(|error| DebuggerError(format!("ReadProcessMemory failed: {error}")))?;
        if bytes_read != 1
        {
            return Err(DebuggerError("ReadProcessMemory: short read".to_string()));
        }

        return Ok(value);
    }

    fn read_u32(&self, address: Address) -> DebuggerResult<u32>
    {
        let mut value: u32 = 0;
        let mut bytes_read = 0usize;
        let size = std::mem::size_of::<u32>();

        unsafe {
            ReadProcessMemory(
                self.process,
                address as usize as *const c_void,
                &mut value as *mut u32 as *mut c_void,
                size,
                Some(&mut bytes_read),
            )
        }
        .map_err(|error| DebuggerError(format!("ReadProcessMemory failed: {error}")))?;

        if bytes_read != size
        {
            return Err(DebuggerError("ReadProcessMemory: short read".to_string()));
        }

        return Ok(value);
    }

    fn step_in(&self, thread: HANDLE) -> DebuggerResult<()>
    {
        self.enable_trap_flag(thread)
    }

    fn step_over(&mut self, thread: HANDLE) -> DebuggerResult<()>
    {
        let context = self.get_context(thread)?;
        let ip = context.Eip;

        // NOTE: the Haze compiler is hardcoded to produce only E8 calls, length 5
        let opcode = self.read_u8(ip)?;
        if opcode == 0xE8
        {
            let next_ip = ip.wrapping_add(5);
            self.set_breakpoint(next_ip, true)?;
            return Ok(());
        }

        return self.step_in(thread);
    }

    fn step_out(&mut self, thread: HANDLE) -> DebuggerResult<()>
    {
        let context = self.get_context(thread)?;
        let esp = context.Esp;
        let return_addr = self.read_u32(esp)?;
        return self.set_breakpoint(return_addr, true);
    }

    fn set_hardware_breakpoint_slot(
        &self,
        thread: HANDLE,
        slot: usize,
        address: Option<Address>,
    ) -> DebuggerResult<()>
    {
        if slot >= 4
        {
            return Err(DebuggerError(
                "set_hardware_breakpoint_slot: slot out of range".to_string(),
            ));
        }

        let mut context = self.get_context_flags(thread, WOW64_CONTEXT_DEBUG_REGISTERS)?;

        let address_or = address.unwrap_or(0);
        match slot
        {
            DR0 => context.Dr0 = address_or,
            DR1 => context.Dr1 = address_or,
            DR2 => context.Dr2 = address_or,
            DR3 => context.Dr3 = address_or,
            _ =>
            {}
        }

        // set RW/LEN and local enables
        let enable_bit = 1u32 << (slot * 2);
        if address.is_some()
        {
            context.Dr7 |= enable_bit;
        }
        else
        {
            context.Dr7 &= !enable_bit;
        }

        // clear RW/LEN for this slot (force execute, len=1)
        let rwlen_shift = 16 + (slot * 4);
        context.Dr7 &= !(0xFu32 << rwlen_shift);
        context.Dr6 = 0;

        unsafe { Wow64SetThreadContext(thread, &context) }
            .map_err(|error| DebuggerError(format!("Wow64SetThreadContext failed: {error}")))?;

        return Ok(());
    }

    fn apply_hardware_breakpoints_to_thread(&self, thread: HANDLE) -> DebuggerResult<()>
    {
        for (slot, address) in self.hardware_breakpoints.iter().copied().enumerate()
        {
            self.set_hardware_breakpoint_slot(thread, slot, address)?;
        }

        return Ok(());
    }

    fn sync_breakpoints_from_registry(&mut self) -> DebuggerResult<()>
    {
        if !self.hardware_breakpoints_dirty
        {
            return Ok(());
        }
        self.hardware_breakpoints_dirty = false;

        let mut out = Vec::new();
        let mut seen = HashSet::<Address>::new();
        for name in &self.requested_function_breakpoints
        {
            if let Some(address) = self.resolve_function_address(name)
            {
                if seen.insert(address)
                {
                    out.push(address);
                }
            }
        }

        self.hardware_breakpoints = [None, None, None, None];
        let mut hardware_count = 0;
        let mut software_addresses = Vec::new();

        for address in out
        {
            if hardware_count < 4
            {
                self.hardware_breakpoints[hardware_count] = Some(address);
                hardware_count += 1;
            }
            else
            {
                software_addresses.push(address);
            }
        }

        // commit breakpoint changes to all threads in case code is re-entrant
        for (_, thread) in self.threads.iter()
        {
            if !thread.is_invalid()
            {
                let _ = self.apply_hardware_breakpoints_to_thread(*thread);
            }
        }

        // remove software breakpoints that are no longer requested
        let to_remove: Vec<Address> = self
            .software_breakpoints
            .iter()
            .filter(|(address, breakpoint)| {
                !breakpoint.temporary && !software_addresses.contains(address)
            })
            .map(|(address, _)| *address)
            .collect();

        for address in to_remove
        {
            self.clear_breakpoint(address)?;
        }

        // apply software breakpoints if all hardware slots were exhausted
        for address in software_addresses
        {
            self.set_breakpoint(address, false)?;
        }

        return Ok(());
    }

    fn is_hardware_breakpoint_exception(&self, thread: HANDLE) -> DebuggerResult<bool>
    {
        let context = self.get_context_flags(thread, WOW64_CONTEXT_DEBUG_REGISTERS)?;
        let dr6 = context.Dr6;
        return Ok((dr6 & 0xF) != 0);
    }

    fn clear_hardware_breakpoint_status(&self, thread: HANDLE) -> DebuggerResult<()>
    {
        let mut context = self.get_context_flags(thread, WOW64_CONTEXT_DEBUG_REGISTERS)?;
        context.Dr6 = 0;
        unsafe { Wow64SetThreadContext(thread, &context) }
            .map_err(|error| DebuggerError(format!("Wow64SetThreadContext failed: {error}")))?;

        return Ok(());
    }
}

pub fn set_requested_function_breakpoints(names: Vec<String>) -> Vec<bool>
{
    let mut engine = get_engine().lock().unwrap();
    engine.requested_function_breakpoints = names;
    engine.mark_dirty();

    return engine
        .requested_function_breakpoints
        .iter()
        .map(|name| {
            engine.function_symbols_va.contains_key(name)
                || engine.function_symbols_rva.contains_key(name)
                || parse_address_literal(name).is_some()
        })
        .collect();
}

pub fn read_memory(address: Address, count: i64) -> DebuggerResult<Vec<u8>>
{
    let engine = get_engine().lock().unwrap();
    let mut buffer = vec![0u8; count as usize];
    let mut bytes_read = 0usize;

    unsafe {
        ReadProcessMemory(
            engine.process,
            address as usize as *const c_void,
            buffer.as_mut_ptr() as *mut c_void,
            count as usize,
            Some(&mut bytes_read),
        )
    }
    .map_err(|error| DebuggerError(format!("ReadProcessMemory failed: {error}")))?;

    buffer.truncate(bytes_read);
    return Ok(buffer);
}

#[unsafe(no_mangle)]
pub extern "C" fn mist_clear_function_symbols() -> bool
{
    let mut engine = get_engine().lock().unwrap();
    engine.function_symbols_rva.clear();
    engine.function_symbols_va.clear();
    engine.mark_dirty();
    return true;
}

#[unsafe(no_mangle)]
pub extern "C" fn mist_register_function_symbol_rva(name: *const c_char, rva: Address) -> bool
{
    let Some(name) = cstr_to_string(name)
    else
    {
        return false;
    };
    let mut engine = get_engine().lock().unwrap();
    engine.function_symbols_rva.insert(name, rva);
    engine.mark_dirty();
    return true;
}

#[unsafe(no_mangle)]
pub extern "C" fn mist_register_function_symbol_va(name: *const c_char, va: Address) -> bool
{
    let Some(name) = cstr_to_string(name)
    else
    {
        return false;
    };
    let mut engine = get_engine().lock().unwrap();
    engine.function_symbols_va.insert(name, va);
    engine.mark_dirty();
    return true;
}

#[unsafe(no_mangle)]
pub extern "C" fn mist_launch_target(target_path: *const c_char)
{
    if target_path.is_null()
    {
        eprintln!("launch_target: target_path was null");
        return;
    }

    std::thread::spawn(move || {
        if let Err(error) = launch_and_debug(target_path)
        {
            eprintln!("launch_target: failed because {error:?}");
        }
    });
}

fn launch_and_debug(target_path: *const c_char) -> DebuggerResult<()>
{
    let mut startup_info = STARTUPINFOA::default();
    startup_info.cb = std::mem::size_of::<STARTUPINFOA>() as u32;

    let mut process_info = PROCESS_INFORMATION::default();

    unsafe {
        CreateProcessA(
            PCSTR(target_path),
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
        .map_err(|error| DebuggerError(format!("Could not spawn debugee: {error}")))?;

        controller().set_session_active(true);

        {
            let mut engine = get_engine().lock().unwrap();
            engine.process = process_info.hProcess;
            engine.threads.insert(process_info.dwThreadId, process_info.hThread);
        }

        let mut debug_event = DEBUG_EVENT::default();

        loop
        {
            if WaitForDebugEvent(&mut debug_event, u32::MAX).is_err()
            {
                break;
            }

            let mut engine = get_engine().lock().unwrap();
            engine.sync_breakpoints_from_registry()?;

            let pid = debug_event.dwProcessId;
            let tid = debug_event.dwThreadId;

            match debug_event.dwDebugEventCode
            {
                CREATE_PROCESS_DEBUG_EVENT =>
                {
                    let base = debug_event.u.CreateProcessInfo.lpBaseOfImage as usize as u32;
                    engine.image_base = Some(base);
                    engine.mark_dirty();

                    let file = debug_event.u.CreateProcessInfo.hFile;
                    if !file.is_invalid()
                    {
                        _ = CloseHandle(file);
                    }

                    if let Some(thread) = engine.thread_handle(tid)
                    {
                        engine.apply_hardware_breakpoints_to_thread(thread)?;
                    }
                }
                CREATE_THREAD_DEBUG_EVENT =>
                {
                    let h_thread = debug_event.u.CreateThread.hThread;
                    if !h_thread.is_invalid()
                    {
                        engine.threads.insert(tid, h_thread);
                        let _ = engine.apply_hardware_breakpoints_to_thread(h_thread);
                    }
                    else
                    {
                        if let Ok(opened) = OpenThread(THREAD_ALL_ACCESS, false, tid)
                        {
                            if !opened.is_invalid()
                            {
                                engine.threads.insert(tid, opened);
                                engine.apply_hardware_breakpoints_to_thread(opened)?;
                            }
                        }
                    }
                }
                EXIT_THREAD_DEBUG_EVENT =>
                {
                    if let Some(h) = engine.threads.remove(&tid)
                    {
                        if !h.is_invalid()
                        {
                            _ = CloseHandle(h);
                        }
                    }
                }
                EXCEPTION_DEBUG_EVENT =>
                {
                    let code = debug_event.u.Exception.ExceptionRecord.ExceptionCode;
                    if code == EXCEPTION_BREAKPOINT_CODE
                    {
                        if let Some(thread) = engine.thread_handle(tid)
                        {
                            let context = engine.get_context(thread)?;
                            let breakpoint_address = context.Eip.wrapping_sub(1);

                            if let Some(breakpoint) =
                                engine.software_breakpoints.get(&breakpoint_address).copied()
                            {
                                engine.clear_breakpoint(breakpoint_address)?;
                                engine.adjust_ip_back_after_int3(thread)?;

                                if !breakpoint.temporary
                                {
                                    engine.pending_reinsert =
                                        PendingReinsert::At(breakpoint_address);
                                    engine.enable_trap_flag(thread)?;
                                    drop(engine);
                                    _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
                                    continue;
                                }

                                controller().notify_stop(StopReason::Breakpoint, tid);
                                drop(engine);
                                let command = controller().wait_for_command();
                                engine = get_engine().lock().unwrap();
                                apply_command(&mut engine, tid, command)?;
                                engine.sync_breakpoints_from_registry()?;
                                drop(engine);
                                _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
                                continue;
                            }
                        }
                        else if code == EXCEPTION_SINGLE_STEP_CODE
                        {
                            controller().notify_stop(StopReason::Breakpoint, tid);
                            drop(engine);
                            let command = controller().wait_for_command();
                            engine = get_engine().lock().unwrap();
                            apply_command(&mut engine, tid, command)?;
                            engine.sync_breakpoints_from_registry()?;
                            drop(engine);
                            _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
                            continue;
                        }
                    }
                    else if code == EXCEPTION_SINGLE_STEP_CODE
                    {
                        if let Some(thread) = engine.thread_handle(tid)
                        {
                            if let PendingReinsert::At(address) = engine.pending_reinsert
                            {
                                engine.pending_reinsert = PendingReinsert::None;
                                engine.clear_trap_flag(thread)?;
                                engine.reinsert_breakpoint(address)?;

                                if let Some(command) = controller().try_take_command()
                                {
                                    apply_command(&mut engine, tid, command)?;
                                }

                                drop(engine);
                                _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
                                continue;
                            }

                            if engine.is_hardware_breakpoint_exception(thread)?
                            {
                                engine.clear_hardware_breakpoint_status(thread)?;
                                controller().notify_stop(StopReason::Breakpoint, tid);
                                drop(engine);
                                let command = controller().wait_for_command();
                                engine = get_engine().lock().unwrap();
                                apply_command(&mut engine, tid, command)?;
                                engine.sync_breakpoints_from_registry()?;
                                drop(engine);
                                _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
                                continue;
                            }

                            engine.clear_trap_flag(thread)?;
                            controller().notify_stop(StopReason::SingleStep, tid);
                            drop(engine);
                            let command = controller().wait_for_command();
                            engine = get_engine().lock().unwrap();
                            apply_command(&mut engine, tid, command)?;
                            let _ = engine.sync_breakpoints_from_registry();
                            drop(engine);
                            _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
                            continue;
                        }
                    }
                }
                EXIT_PROCESS_DEBUG_EVENT =>
                {
                    controller().notify_stop(StopReason::ProcessExit, tid);
                    break;
                }
                _ =>
                {}
            }

            drop(engine);
            _ = ContinueDebugEvent(pid, tid, DBG_CONTINUE);
        }

        controller().set_session_active(false);

        let mut final_guard = get_engine().lock().unwrap();
        for (_, handle) in final_guard.threads.drain()
        {
            if !handle.is_invalid()
            {
                _ = CloseHandle(handle);
            }
        }
        if !process_info.hThread.is_invalid()
        {
            _ = CloseHandle(process_info.hThread);
        }
        if !process_info.hProcess.is_invalid()
        {
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
) -> DebuggerResult<()>
{
    let Some(thread) = engine.thread_handle(thread_id)
    else
    {
        return Err(DebuggerError(format!(
            "apply_command: missing thread handle for thread {}",
            thread_id
        )));
    };

    return match command
    {
        DebugCommand::Continue => Ok(()),
        DebugCommand::StepIn => engine.step_in(thread),
        DebugCommand::StepOver => engine.step_over(thread),
        DebugCommand::StepOut => engine.step_out(thread),
    };
}

pub fn parse_address_literal(name: &str) -> Option<Address>
{
    let trimmed = name.trim();
    if trimmed.is_empty()
    {
        return None;
    }
    if let Some(hex) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X"))
    {
        return Address::from_str_radix(hex, 16).ok();
    }
    if trimmed.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Address::from_str_radix(trimmed, 16).ok();
    }
    return trimmed.parse::<Address>().ok();
}
