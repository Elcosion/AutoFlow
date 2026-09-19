//! Independent controller lease and per-interactive-session authority lock.
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, OpenProcess, SetEvent, WaitForSingleObject,
    PROCESS_SYNCHRONIZE, SYNCHRONIZATION_SYNCHRONIZE,
};

pub(crate) struct ControllerLease {
    handle: Arc<OwnedHandle>,
    stop: Arc<OwnedHandle>,
    pub(crate) name: String,
    pub(crate) stop_name: String,
    closed: Arc<AtomicBool>,
}
impl ControllerLease {
    pub(crate) fn new(secret: &str, closed: Arc<AtomicBool>) -> Result<Self, String> {
        let name = format!("Local\\AutoFlow.ControlLease.{secret}");
        let handle =
            unsafe { CreateEventW(None, false, false, &windows::core::HSTRING::from(&name)) }
                .map_err(|error| error.to_string())?;
        let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let handle = Arc::new(unsafe { OwnedHandle::from_raw_handle(handle.0) });
        if exists {
            return Err("控制租约标识已存在".into());
        }
        unsafe { SetEvent(HANDLE(handle.as_raw_handle())) }.map_err(|error| error.to_string())?;
        let stop_name = format!("Local\\AutoFlow.StopSignal.{secret}");
        let stop = unsafe {
            CreateEventW(
                None,
                false,
                false,
                &windows::core::HSTRING::from(&stop_name),
            )
        }
        .map_err(|error| error.to_string())?;
        let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let stop = Arc::new(unsafe { OwnedHandle::from_raw_handle(stop.0) });
        if exists {
            return Err("急停信号标识已存在".into());
        }
        Ok(Self {
            handle,
            stop,
            name,
            stop_name,
            closed,
        })
    }
    pub(crate) fn pulse(&self) -> Result<(), String> {
        unsafe { SetEvent(HANDLE(self.handle.as_raw_handle())) }.map_err(|error| error.to_string())
    }
    pub(crate) fn request_stop(&self) -> Result<(), String> {
        unsafe { SetEvent(HANDLE(self.stop.as_raw_handle())) }.map_err(|error| error.to_string())
    }
    pub(crate) fn start_pulsing(self: &Arc<Self>) -> Result<(), String> {
        let lease = self.clone();
        std::thread::Builder::new()
            .name("autoflow-controller-lease".into())
            .spawn(move || {
                while !lease.closed.load(Ordering::Acquire) {
                    if lease.pulse().is_err() {
                        lease.closed.store(true, Ordering::Release);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

pub(crate) struct ParentWatch {
    event: OwnedHandle,
    stop: OwnedHandle,
    parent: OwnedHandle,
    last_pulse: Instant,
}
impl ParentWatch {
    pub(crate) fn new(name: &str, stop_name: &str, parent_pid: u32) -> Result<Self, String> {
        if !name.starts_with("Local\\AutoFlow.ControlLease.") || name.len() > 128 {
            return Err("控制租约名称无效".into());
        }
        let event = unsafe {
            OpenEventW(
                SYNCHRONIZATION_SYNCHRONIZE,
                false,
                &windows::core::HSTRING::from(name),
            )
        }
        .map_err(|error| error.to_string())?;
        let event = unsafe { OwnedHandle::from_raw_handle(event.0) };
        if !stop_name.starts_with("Local\\AutoFlow.StopSignal.") || stop_name.len() > 128 {
            return Err("急停信号名称无效".into());
        }
        let stop = unsafe {
            OpenEventW(
                SYNCHRONIZATION_SYNCHRONIZE,
                false,
                &windows::core::HSTRING::from(stop_name),
            )
        }
        .map_err(|error| error.to_string())?;
        let stop = unsafe { OwnedHandle::from_raw_handle(stop.0) };
        let parent = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, parent_pid) }
            .map_err(|error| error.to_string())?;
        Ok(Self {
            event,
            stop,
            parent: unsafe { OwnedHandle::from_raw_handle(parent.0) },
            last_pulse: Instant::now(),
        })
    }
    pub(crate) fn healthy(&mut self) -> bool {
        if unsafe { WaitForSingleObject(HANDLE(self.parent.as_raw_handle()), 0) } != WAIT_TIMEOUT {
            return false;
        }
        match unsafe { WaitForSingleObject(HANDLE(self.event.as_raw_handle()), 10) } {
            WAIT_OBJECT_0 => {
                self.last_pulse = Instant::now();
                true
            }
            WAIT_TIMEOUT => self.last_pulse.elapsed() <= Duration::from_millis(500),
            _ => false,
        }
    }
    pub(crate) fn stop_requested(&self) -> bool {
        (unsafe { WaitForSingleObject(HANDLE(self.stop.as_raw_handle()), 0) }) == WAIT_OBJECT_0
    }
}

pub(crate) fn claim_authority(mock: bool, session: &str) -> Result<OwnedHandle, String> {
    let name = if mock {
        format!("Local\\AutoFlow.MockInputAuthority.{session}")
    } else {
        "Local\\AutoFlow.InputAuthority".into()
    };
    let handle = unsafe { CreateMutexW(None, false, &windows::core::HSTRING::from(name)) }
        .map_err(|error| error.to_string())?;
    let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
    if exists {
        return Err("已有 AutoFlow 输入控制者正在运行；请先关闭另一实例".into());
    }
    Ok(handle)
}
