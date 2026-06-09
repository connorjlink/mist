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
        _ => false
    };
}

#[cfg(test)]
mod tests
{
    use windows::core::w;

    use super::*;

    #[test]
    fn test_cstr_to_string_valid()
    {
        let original = "Hello, world!";
        let c_string = std::ffi::CString::new(original).unwrap();
        let pointer = c_string.as_ptr();
        let result = cstr_to_string(pointer);
        assert_eq!(result, Some(original.to_string()));
    }

    #[test]
    fn test_compare_pcwstr_case_insensitive_match()
    {
        let a = w!("HelloWorld");
        let b = w!("helloworld");
        assert!(compare_pcwstr_case_insensitive(a, b));
    }

    #[test]
    fn test_compare_pcwstr_case_insensitive_mismatch()
    {
        let a = w!("HelloWorld");
        let b = w!("HellooWorld");
        assert!(!compare_pcwstr_case_insensitive(a, b));
    }
}
