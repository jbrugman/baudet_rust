use crate::process_runner::{collect_drain, drain_thread, ProcessOutcome};
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    DuplicateHandle, LocalFree, DUPLICATE_SAME_ACCESS, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, GetAppContainerFolderPath,
};
use windows_sys::Win32::Security::{
    FreeSid, GetTokenInformation, TokenIsAppContainer, PSID, SECURITY_CAPABILITIES, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    DefineDosDeviceW, GetLogicalDrives, DDD_EXACT_MATCH_ON_REMOVE, DDD_NO_BROADCAST_SYSTEM, DDD_REMOVE_DEFINITION,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess, InitializeProcThreadAttributeList,
    OpenProcessToken, ResumeThread, UpdateProcThreadAttribute, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED,
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTUPINFOEXW,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_USESTDHANDLES,
};

struct Profile {
    name: Vec<u16>,
    sid: PSID,
}

// Win32 path normalization walks path components. An AppContainer can open
// the copy, but cannot inspect its host ancestors. The alias exposes the copy
// as a drive root without granting the container any additional access.
struct WorkspaceDrive {
    letter: u8,
    device: [u16; 3],
    target: Vec<u16>,
    mapped: bool,
    _lock: MutexGuard<'static, ()>,
}

impl WorkspaceDrive {
    fn map(path: &Path) -> io::Result<Self> {
        static DRIVE_LOCK: Mutex<()> = Mutex::new(());
        let lock = DRIVE_LOCK.lock().map_err(|_| io::Error::other("workspace drive lock was poisoned"))?;
        let drives = unsafe { GetLogicalDrives() };
        if drives == 0 {
            return Err(io::Error::last_os_error());
        }
        let letter = (b'D'..=b'Z')
            .rev()
            .find(|letter| drives & (1 << (letter - b'A')) == 0)
            .ok_or_else(|| io::Error::other("no free drive letter for the Windows workspace"))?;
        let device = [u16::from(letter), u16::from(b':'), 0];
        let target: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
        check(unsafe { DefineDosDeviceW(DDD_NO_BROADCAST_SYSTEM, device.as_ptr(), target.as_ptr()) })?;
        Ok(Self { letter, device, target, mapped: true, _lock: lock })
    }

    fn root(&self) -> String {
        format!("{}:\\", char::from(self.letter))
    }

    fn unmap(&mut self) -> io::Result<()> {
        if !self.mapped {
            return Ok(());
        }
        check(unsafe {
            DefineDosDeviceW(
                DDD_NO_BROADCAST_SYSTEM | DDD_REMOVE_DEFINITION | DDD_EXACT_MATCH_ON_REMOVE,
                self.device.as_ptr(),
                self.target.as_ptr(),
            )
        })?;
        self.mapped = false;
        Ok(())
    }
}

impl Drop for WorkspaceDrive {
    fn drop(&mut self) {
        let _ = self.unmap();
    }
}

impl Profile {
    fn unique() -> io::Result<Self> {
        let unique = tempfile::Builder::new().prefix("baudet-container-").tempdir()?;
        let name = unique
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("invalid AppContainer profile name"))?;
        Self::create(name)
    }

    fn create(name: &str) -> io::Result<Self> {
        let name: Vec<_> = name.encode_utf16().chain([0]).collect();
        let mut sid = null_mut();
        let result =
            unsafe { CreateAppContainerProfile(name.as_ptr(), name.as_ptr(), name.as_ptr(), null(), 0, &mut sid) };
        if result < 0 {
            return Err(io::Error::other(format!("CreateAppContainerProfile failed with {result:#010x}")));
        }
        Ok(Self { name, sid })
    }

    fn sid_string(&self) -> io::Result<Vec<u16>> {
        let mut raw_sid = null_mut();
        check(unsafe { ConvertSidToStringSidW(self.sid, &mut raw_sid) })?;
        let len = (0..).take_while(|index| unsafe { *raw_sid.add(*index) != 0 }).count();
        let sid = unsafe { std::slice::from_raw_parts(raw_sid, len + 1) }.to_vec();
        unsafe { LocalFree(raw_sid.cast()) };
        Ok(sid)
    }

    fn folder(&self) -> io::Result<std::path::PathBuf> {
        let sid = self.sid_string()?;
        let mut raw = null_mut();
        let result = unsafe { GetAppContainerFolderPath(sid.as_ptr(), &mut raw) };
        if result < 0 {
            return Err(io::Error::other(format!("GetAppContainerFolderPath failed with {result:#010x}")));
        }
        let len = (0..).take_while(|index| unsafe { *raw.add(*index) != 0 }).count();
        let folder = std::ffi::OsString::from_wide(unsafe { std::slice::from_raw_parts(raw, len) });
        unsafe { CoTaskMemFree(raw.cast()) };
        Ok(folder.into())
    }

    fn grant_workspace(&self, path: &Path, writable: bool) -> io::Result<()> {
        let sid = self.sid_string()?;
        let sid = String::from_utf16_lossy(&sid[..sid.len() - 1]);
        let system_root =
            std::env::var_os("SystemRoot").ok_or_else(|| io::Error::other("SystemRoot is unavailable"))?;
        let icacls = Path::new(&system_root).join("System32").join("icacls.exe");
        let apply = |args: &[&str]| -> io::Result<()> {
            let output = std::process::Command::new(&icacls).arg(path).args(args).output()?;
            if !output.status.success() {
                return Err(io::Error::other(format!(
                    "could not grant access to the temporary workspace: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            Ok(())
        };
        let rights = if writable { "M" } else { "RX" };
        apply(&["/grant:r", &format!("*{sid}:(OI)(CI){rights}")])?;
        if writable {
            apply(&["/grant", &format!("*{sid}:(CI)(DC)")])?;
            apply(&["/setintegritylevel", "(OI)(CI)L", "/T"])?;
        } else {
            apply(&["/deny", &format!("*{sid}:(OI)(CI)(WD,AD,WEA,WA,DE,DC,WDAC,WO)")])?;
        }
        Ok(())
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        unsafe {
            FreeSid(self.sid);
            DeleteAppContainerProfile(self.name.as_ptr());
        }
    }
}

struct Attributes {
    words: Vec<usize>,
}

impl Attributes {
    fn new() -> io::Result<Self> {
        let mut bytes = 0;
        // The first call reports the buffer size. Use pointer-aligned storage.
        unsafe { InitializeProcThreadAttributeList(null_mut(), 3, 0, &mut bytes) };
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut words = vec![0; bytes.div_ceil(size_of::<usize>())];
        check(unsafe { InitializeProcThreadAttributeList(words.as_mut_ptr().cast(), 3, 0, &mut bytes) })?;
        Ok(Self { words })
    }

    fn as_mut_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.words.as_mut_ptr().cast()
    }

    // Attribute values must stay alive through CreateProcessW.
    fn set<T>(&mut self, key: u32, value: &T) -> io::Result<()> {
        check(unsafe {
            UpdateProcThreadAttribute(
                self.as_mut_ptr(),
                0,
                key as usize,
                std::ptr::from_ref(value).cast(),
                size_of::<T>(),
                null_mut(),
                null(),
            )
        })
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.as_mut_ptr()) };
    }
}

#[track_caller]
fn check(result: i32) -> io::Result<()> {
    if result == 0 {
        let error = io::Error::last_os_error();
        Err(io::Error::other(format!("line {}: {error}", std::panic::Location::caller().line())))
    } else {
        Ok(())
    }
}

/// Launch with no capabilities or project grants and inherit only the stdio handles.
pub fn run(executable: &Path, cwd: &Path, command_line: &str, timeout_ms: u32) -> io::Result<ProcessOutcome> {
    launch(&Profile::unique()?, executable, cwd, command_line, timeout_ms)
}

/// Grant this profile access to the staged copy only.
pub fn run_in_workspace(
    workspace: &crate::windows_workspace::Workspace,
    writable: bool,
    executable: &Path,
    command_line: &str,
    timeout_ms: u32,
) -> io::Result<ProcessOutcome> {
    let profile = Profile::unique()?;
    profile.grant_workspace(workspace.path(), writable)?;
    launch(&profile, executable, workspace.path(), command_line, timeout_ms)
}

pub(crate) fn run_project(
    root: &Path,
    writable: bool,
    executable: &Path,
    command_line: &str,
    timeout_ms: u32,
) -> io::Result<ProcessOutcome> {
    let profile = Profile::unique()?;
    let folder = profile.folder()?;
    let workspace = crate::windows_workspace::Workspace::stage_in(root, &folder)?;
    profile.grant_workspace(workspace.path(), writable)?;
    if command_line.contains('\0') {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "a Windows command must not contain NUL"));
    }
    let mut drive = WorkspaceDrive::map(workspace.path())?;
    let outcome = launch(&profile, executable, Path::new(&drive.root()), command_line, timeout_ms)?;
    drive.unmap()?;
    drop(drive);
    if writable && outcome.exit_code == Some(0) {
        workspace.commit()?;
    }
    Ok(outcome)
}

pub(crate) fn system_cmd() -> io::Result<std::path::PathBuf> {
    let root = std::env::var_os("SystemRoot").ok_or_else(|| io::Error::other("SystemRoot is unavailable"))?;
    Ok(Path::new(&root).join("System32").join("cmd.exe"))
}

fn launch(
    profile: &Profile,
    executable: &Path,
    cwd: &Path,
    command_line: &str,
    timeout_ms: u32,
) -> io::Result<ProcessOutcome> {
    let started_at = Instant::now();
    let job = unsafe { CreateJobObjectW(null(), null()) };
    if job.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = unsafe { OwnedHandle::from_raw_handle(job) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    check(unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    })?;

    let capabilities = SECURITY_CAPABILITIES { AppContainerSid: profile.sid, ..Default::default() };
    let jobs: [HANDLE; 1] = [job.as_raw_handle()];
    let (stdin_reader, stdin_writer) = io::pipe()?;
    drop(stdin_writer);
    let (stdout_reader, stdout_writer) = io::pipe()?;
    let (stderr_reader, stderr_writer) = io::pipe()?;
    let stdio = [
        inheritable(stdin_reader.as_raw_handle())?,
        inheritable(stdout_writer.as_raw_handle())?,
        inheritable(stderr_writer.as_raw_handle())?,
    ];
    drop((stdin_reader, stdout_writer, stderr_writer));
    let handles = stdio.each_ref().map(AsRawHandle::as_raw_handle);
    let mut attributes = Attributes::new()?;
    attributes.set(PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, &capabilities)?;
    attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
    attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    [startup.StartupInfo.hStdInput, startup.StartupInfo.hStdOutput, startup.StartupInfo.hStdError] = handles;
    startup.lpAttributeList = attributes.as_mut_ptr();

    let executable: Vec<_> = executable.as_os_str().encode_wide().chain([0]).collect();
    let cwd: Vec<_> = cwd.as_os_str().encode_wide().chain([0]).collect();
    let mut command_line: Vec<_> = command_line.encode_utf16().chain([0]).collect();
    let mut environment = Vec::new();
    for setting in [
        "GIT_ALLOW_PROTOCOL=",
        "GIT_CONFIG_GLOBAL=NUL",
        "GIT_CONFIG_NOSYSTEM=1",
        "GIT_OPTIONAL_LOCKS=0",
        "GIT_TERMINAL_PROMPT=0",
    ] {
        environment.extend(setting.encode_utf16().chain([0]));
    }
    for name in ["LOCALAPPDATA", "SystemRoot", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(name) {
            environment.extend(name.encode_utf16().chain([b'=' as u16]).chain(value.encode_wide()).chain([0]));
        }
    }
    environment.push(0);
    let mut child = PROCESS_INFORMATION::default();
    check(unsafe {
        CreateProcessW(
            executable.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            1,
            CREATE_NO_WINDOW | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
            environment.as_ptr().cast(),
            cwd.as_ptr(),
            &startup.StartupInfo,
            &mut child,
        )
    })?;
    drop(attributes);
    drop(stdio);
    let process = unsafe { OwnedHandle::from_raw_handle(child.hProcess) };
    let thread = unsafe { OwnedHandle::from_raw_handle(child.hThread) };

    let mut token = null_mut();
    check(unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &mut token) })?;
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut is_appcontainer = 0u32;
    let mut written = 0;
    check(unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenIsAppContainer,
            std::ptr::from_mut(&mut is_appcontainer).cast(),
            size_of::<u32>() as u32,
            &mut written,
        )
    })?;
    if is_appcontainer == 0 {
        return Err(io::Error::other("the suspended process has no AppContainer token"));
    }
    let stdout = drain_thread(Some(stdout_reader), "stdout");
    let stderr = drain_thread(Some(stderr_reader), "stderr");
    if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    let exit_code = match unsafe { WaitForSingleObject(process.as_raw_handle(), timeout_ms) } {
        WAIT_OBJECT_0 => {
            let mut status = 0;
            check(unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut status) })?;
            Some(status as i32)
        }
        WAIT_TIMEOUT => None,
        _ => return Err(io::Error::last_os_error()),
    };
    stop_job(job.as_raw_handle())?;
    Ok(ProcessOutcome {
        exit_code,
        timed_out: exit_code.is_none(),
        stdout: collect_drain(stdout),
        stderr: collect_drain(stderr),
        duration_ms: started_at.elapsed().as_millis() as u64,
    })
}

// The caller may inspect the project copy as soon as we return. Make sure no
// child can still modify it, including on a timeout or successful early exit.
fn stop_job(job: HANDLE) -> io::Result<()> {
    check(unsafe { TerminateJobObject(job, 1) })?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        check(unsafe {
            QueryInformationJobObject(
                job,
                JobObjectBasicAccountingInformation,
                std::ptr::from_mut(&mut accounting).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            )
        })?;
        if accounting.ActiveProcesses == 0 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "the AppContainer job is still active"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn inheritable(handle: HANDLE) -> io::Result<OwnedHandle> {
    let current = unsafe { GetCurrentProcess() };
    let mut duplicate = null_mut();
    check(unsafe { DuplicateHandle(current, handle, current, &mut duplicate, 0, 1, DUPLICATE_SAME_ACCESS) })?;
    Ok(unsafe { OwnedHandle::from_raw_handle(duplicate) })
}
