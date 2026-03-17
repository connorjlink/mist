use std::collections::{HashMap, HashSet};
use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::{Mutex, OnceLock};

// Mist breakpoints.rs
// (c) Connor J. Link. All Rights Reserved.

pub type Address = u32;

#[derive(Default)]
struct BreakpointState {
    image_base: Option<Address>,
    function_symbols_rva: HashMap<String, Address>,
    function_symbols_va: HashMap<String, Address>,
    requested_function_breakpoints: Vec<String>,
    generation: u64,
}

static STATE: OnceLock<Mutex<BreakpointState>> = OnceLock::new();

fn state() -> &'static Mutex<BreakpointState> {
    STATE.get_or_init(|| Mutex::new(BreakpointState::default()))
}

fn bump_generation(state: &mut BreakpointState) {
    state.generation = state.generation.wrapping_add(1);
    if state.generation == 0 {
        state.generation = 1;
    }
}

pub fn set_image_base(image_base: Address) {
    let mut state = state().lock().unwrap();
    if state.image_base == Some(image_base) {
        return;
    }
    state.image_base = Some(image_base);
    bump_generation(&mut state);
}

pub fn register_function_symbol_rva(name: &str, rva: Address) {
    let mut state = state().lock().unwrap();
    state.function_symbols_rva.insert(name.to_string(), rva);
    bump_generation(&mut state);
}

pub fn register_function_symbol_va(name: &str, va: Address) {
    let mut state = state().lock().unwrap();
    state.function_symbols_va.insert(name.to_string(), va);
    bump_generation(&mut state);
}

pub fn clear_function_symbols() {
    let mut state = state().lock().unwrap();
    state.function_symbols_rva.clear();
    state.function_symbols_va.clear();
    bump_generation(&mut state);
}

fn parse_address_literal(name: &str) -> Option<Address> {
    // accept 0x hex or decimal breakpoint addresses

    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(hex) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        return Address::from_str_radix(hex, 16).ok();
    }
    if trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Address::from_str_radix(trimmed, 16).ok();
    }

    return trimmed.parse::<Address>().ok();
}

fn resolve_function_address_locked(state: &BreakpointState, name: &str) -> Option<Address> {
    if let Some(&va) = state.function_symbols_va.get(name) {
        return Some(va);
    }
    if let Some(&rva) = state.function_symbols_rva.get(name) {
        let base = state.image_base?;
        return Some(base.wrapping_add(rva));
    }
    return parse_address_literal(name);
}

pub fn set_requested_function_breakpoints(names: Vec<String>) -> Vec<bool> {
    let mut state = state().lock().unwrap();
    state.requested_function_breakpoints = names;
    bump_generation(&mut state);

    return state.requested_function_breakpoints
        .iter()
        .map(|name| {
            state.function_symbols_va.contains_key(name)
                || state.function_symbols_rva.contains_key(name)
                || parse_address_literal(name).is_some()
        })
        .collect();
}

pub fn current_generation() -> u64 {
    let state = state().lock().unwrap();
    return state.generation;
}

pub fn take_desired_function_breakpoint_addresses(last_seen_generation: &mut u64) -> Option<Vec<Address>> {
    let state = state().lock().unwrap();
    if state.generation == *last_seen_generation {
        return None;
    }
    *last_seen_generation = state.generation;

    let mut out = Vec::new();
    let mut seen = HashSet::<Address>::new();
    for name in &state.requested_function_breakpoints {
        if let Some(addr) = resolve_function_address_locked(&state, name) {
            if seen.insert(addr) {
                out.push(addr);
                if out.len() == 4 {
                    break;
                }
            }
        }
    }
    return Some(out);
}

fn cstr_to_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let cstr = unsafe { CStr::from_ptr(ptr) };
    return Some(cstr.to_string_lossy().into_owned());
}

#[unsafe(no_mangle)]
pub extern "C" fn mist_clear_function_symbols() {
    clear_function_symbols();
}

/// Register a function symbol by RVA (relative to the debuggee image base).
#[unsafe(no_mangle)]
pub extern "C" fn mist_register_function_symbol_rva(name: *const c_char, rva: Address) {
    let Some(name) = cstr_to_string(name) else {
        return;
    };
    register_function_symbol_rva(&name, rva);
}

#[unsafe(no_mangle)]
pub extern "C" fn mist_register_function_symbol_va(name: *const c_char, va: Address) {
    let Some(name) = cstr_to_string(name) else {
        return;
    };
    register_function_symbol_va(&name, va);
}
