//! Executor lifetime ownership. The parent revokes input BEFORE disconnecting
//! or killing a worker; process termination is never treated as input cleanup.
use std::io;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct WorkerExit {
    pub forced: bool,
    pub status: Option<ExitStatus>,
    pub error: Option<String>,
}

pub struct SupervisedWorker {
    child: Child,
    #[cfg(windows)]
    job: WindowsJob,
}

impl SupervisedWorker {
    /// Spawn only workers that wait for private-pipe bootstrap before doing
    /// work. Job assignment completes before the caller can send bootstrap.
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        #[cfg(windows)]
        let job = WindowsJob::new()?;
        let mut child = command.spawn()?;
        #[cfg(windows)]
        if let Err(error) = job.assign(&child) {
            let _ = child.kill();
            let _ = wait_until(&mut child, Duration::from_secs(1));
            return Err(error);
        }
        Ok(Self {
            child,
            #[cfg(windows)]
            job,
        })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }
    pub fn stdin(&mut self) -> Option<&mut ChildStdin> {
        self.child.stdin.as_mut()
    }
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub fn stop(&mut self, revoke: impl FnOnce(), grace: Duration) -> WorkerExit {
        revoke();
        drop(self.child.stdin.take());
        match wait_until(&mut self.child, grace.min(Duration::from_secs(2))) {
            Ok(Some(status)) => {
                return WorkerExit {
                    forced: false,
                    status: Some(status),
                    error: None,
                }
            }
            Ok(None) => {}
            Err(error) => {
                return WorkerExit {
                    forced: false,
                    status: None,
                    error: Some(error.to_string()),
                }
            }
        }
        #[cfg(windows)]
        let kill = self.job.terminate();
        #[cfg(not(windows))]
        let kill = self.child.kill();
        if let Err(error) = kill {
            // A worker may have exited between try_wait and termination.
            if let Ok(Some(status)) = self.child.try_wait() {
                return WorkerExit {
                    forced: true,
                    status: Some(status),
                    error: None,
                };
            }
            return WorkerExit {
                forced: true,
                status: None,
                error: Some(error.to_string()),
            };
        }
        match wait_until(&mut self.child, Duration::from_secs(1)) {
            Ok(status) => WorkerExit {
                forced: true,
                status,
                error: if status.is_none() {
                    Some("worker termination not confirmed before deadline".into())
                } else {
                    None
                },
            },
            Err(error) => WorkerExit {
                forced: true,
                status: None,
                error: Some(error.to_string()),
            },
        }
    }
}

impl Drop for SupervisedWorker {
    fn drop(&mut self) {
        // Last-resort containment, not a replacement for explicit revoke+clean.
        drop(self.child.stdin.take());
        #[cfg(windows)]
        let _ = self.job.terminate();
        let _ = self.child.kill();
        let _ = wait_until(&mut self.child, Duration::from_millis(200));
    }
}

fn wait_until(child: &mut Child, timeout: Duration) -> io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(windows)]
struct WindowsJob(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl WindowsJob {
    fn new() -> io::Result<Self> {
        use windows::Win32::System::JobObjects::*;
        let job = Self(
            unsafe { CreateJobObjectW(None, windows::core::PCWSTR::null()) }
                .map_err(io::Error::other)?,
        );
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            )
        }
        .map_err(io::Error::other)?;
        Ok(job)
    }
    fn assign(&self, child: &Child) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        unsafe {
            windows::Win32::System::JobObjects::AssignProcessToJobObject(
                self.0,
                windows::Win32::Foundation::HANDLE(child.as_raw_handle()),
            )
        }
        .map_err(io::Error::other)
    }
    fn terminate(&self) -> io::Result<()> {
        unsafe { windows::Win32::System::JobObjects::TerminateJobObject(self.0, 1) }
            .map_err(io::Error::other)
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.0) };
    }
}
