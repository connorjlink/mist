use std::ffi::{CStr, c_char};
use windows::core::PCWSTR;

pub fn cstr_to_string(pointer: *const c_char) -> Option<String>
{
    if pointer.is_null()
    {
        return None;
    }
    let cstr = unsafe { CStr::from_ptr(pointer) };
    return Some(cstr.to_string_lossy().into_owned());
}

pub fn compare_pcwstr_case_insensitive(a: PCWSTR, b: PCWSTR) -> bool
{
    let a_string = unsafe { a.to_string() };
    let b_string = unsafe { b.to_string() };

    return match (a_string, b_string)
    {
        (Ok(ok_a), Ok(ok_b)) => ok_a.to_lowercase() == ok_b.to_lowercase(),
        _ => false,
    };
}
