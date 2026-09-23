//! Child processes: started without a console window, and never outliving us.
//!
//! Every Python child -- the station, a pull, a separation -- is the venv's
//! launcher, which re-execs into a second interpreter, so the process we
//! spawn is not the one doing the work. Killing it leaves the real one
//! holding the port, or the GPU, or a half-written file.
//!
//! Two kinds of job object close over that. The console puts itself in one
//! at startup, killed when its last handle closes -- which is when this
//! process ends, however it ends -- so every descendant is contained without
//! anyone having to remember to clean up. And each child gets a job of its
//! own, created before the child has run a single instruction, so stopping
//! one stops its whole tree and nothing else.
use std::ffi::OsStr;
use std::io;
use std::process::{Child, Command};

pub fn background(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Open a page in the browser. The browser is not ours: it leaves the
/// console's job, or closing the console would close it too.
pub fn open_url(url: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW};
        let spawn = |flags: u32| {
            Command::new("cmd").args(["/C", "start", "", url]).creation_flags(flags).spawn()
        };
        // Breaking away is refused inside a job that does not allow it (a
        // console started from some other tool's job); then it is only
        // started, which is still better than not opening at all.
        if spawn(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB).is_err() {
            let _ = spawn(CREATE_NO_WINDOW);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("xdg-open").arg(url).spawn();
    }
}

/// Put this process, and so everything it will ever start, in a job that
/// dies with it. Called once, first thing in `main`.
///
/// The handle is kept open for the life of the process on purpose: the
/// moment it closes -- at exit, by the kernel, even after a crash -- is the
/// moment every child still running is killed.
pub fn contain_self() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, JOB_OBJECT_LIMIT_BREAKAWAY_OK};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let Some(job) = Job::create(JOB_OBJECT_LIMIT_BREAKAWAY_OK) else { return };
        if AssignProcessToJobObject(job.0, GetCurrentProcess()) == 0 {
            crate::logfile::log!("process: could not contain the console in a job; children may outlive a crash");
            return;
        }
        std::mem::forget(job);
    }
}

/// One child's job. Dropping it kills the child and everything it started.
pub struct Job(#[cfg(windows)] windows_sys::Win32::Foundation::HANDLE);

// A job handle is a kernel handle: any thread may use or close it.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    #[cfg(windows)]
    fn create(extra: u32) -> Option<Job> {
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return None;
            }
            let job = Job(handle);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | extra;
            let set = SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            (set != 0).then_some(job)
        }
    }

    /// Kill the whole tree now. Asynchronous: wait on the child to know it
    /// has gone.
    pub fn terminate(&self) {
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0, 1);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
#[link(name = "ntdll", kind = "raw-dylib")]
extern "system" {
    fn NtResumeProcess(process: windows_sys::Win32::Foundation::HANDLE) -> i32;
}

/// Spawn `command` inside a job of its own.
///
/// The child is created suspended and put in its job before it runs, so
/// nothing it starts -- however quickly -- can be born outside the job. If
/// the job cannot be made the child still runs, contained only by the
/// console's own job; `None` then says stop() can only kill the child
/// itself.
pub fn spawn_contained(command: &mut Command) -> io::Result<(Child, Option<Job>)> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        let mut child = command.spawn()?;
        let handle = child.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
        let job = Job::create(0).filter(|job| unsafe { AssignProcessToJobObject(job.0, handle) } != 0);
        if unsafe { NtResumeProcess(handle) } < 0 {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("the child could not be started"));
        }
        Ok((child, job))
    }
    #[cfg(not(windows))]
    {
        Ok((command.spawn()?, None))
    }
}

/// Kill a contained child's tree, or the child alone without a job.
pub fn kill(child: &mut Child, job: Option<&Job>) {
    match job {
        Some(job) => job.terminate(),
        None => { let _ = child.kill(); }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn a_contained_child_runs_and_its_job_kills_it() {
        let mut command = background("cmd");
        command.args(["/C", "ping", "-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null());
        let (mut child, job) = spawn_contained(&mut command).expect("spawn");
        assert!(child.try_wait().unwrap().is_none(), "it never started, or died suspended");
        let job = job.expect("no job");
        job.terminate();
        let status = child.wait().unwrap();
        assert!(!status.success());
    }

    #[test]
    fn dropping_the_job_takes_the_tree_down() {
        let mut command = background("cmd");
        command.args(["/C", "ping", "-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null());
        let (mut child, job) = spawn_contained(&mut command).expect("spawn");
        drop(job);
        let began = std::time::Instant::now();
        let _ = child.wait();
        assert!(began.elapsed().as_secs() < 10);
    }
}
