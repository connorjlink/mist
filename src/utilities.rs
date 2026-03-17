use windows::core::*;

// Mist utilities.rs
// (c) Connor J. Link. All Rights Reserved.

pub fn to_pcstr(s: &str) -> PCSTR {
    let bytes = std::ffi::CString::new(s).expect("CString::new failed");
    return PCSTR(bytes.as_ptr() as *const u8);
}

pub fn compare_pcwstr_case_insensitive(a: PCWSTR, b: PCWSTR) -> bool {
    unsafe {
        match (a.to_string(), b.to_string()) {
            (Ok(sa), Ok(sb)) => sa.to_lowercase() == sb.to_lowercase(),
            _ => false,
        }
    }
}
