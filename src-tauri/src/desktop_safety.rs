//! Read-only desktop availability query. No switching/unlocking or input.
pub(crate) fn current_desktop_accepts_input() -> bool {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::StationsAndDesktops::{
        GetThreadDesktop, GetUserObjectInformationW, UOI_IO,
    };
    use windows::Win32::System::Threading::GetCurrentThreadId;
    let desktop = match unsafe { GetThreadDesktop(GetCurrentThreadId()) } {
        Ok(desktop) => desktop,
        Err(_) => return false,
    };
    // GetThreadDesktop returns a borrowed handle, never CloseDesktop it.
    let mut receiving = 0i32;
    let mut needed = 0u32;
    let success = unsafe {
        GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_IO,
            Some((&mut receiving as *mut i32).cast()),
            std::mem::size_of::<i32>() as u32,
            Some(&mut needed),
        )
    }
    .is_ok();
    valid_desktop_response(success, receiving, needed)
}
fn valid_desktop_response(success: bool, receiving: i32, needed: u32) -> bool {
    success && receiving != 0 && needed == std::mem::size_of::<i32>() as u32
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inactive_desktop_failed_query_and_invalid_buffer_fail_closed() {
        assert!(valid_desktop_response(true, 1, 4));
        assert!(!valid_desktop_response(true, 0, 4));
        assert!(!valid_desktop_response(false, 1, 4));
        assert!(!valid_desktop_response(true, 1, 0));
    }
}
