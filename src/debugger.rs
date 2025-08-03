pub struct Debugger {
    toolhelp_snapshot: HANDLE,
}

impl Drop for Debugger {
    fn drop(&mut self) {
        unsafe {
            _ = CloseHandle(self.toolhelp_snapshot);
        }
    }
}