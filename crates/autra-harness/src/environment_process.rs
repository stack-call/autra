use autra_core::CancelFlag;
use autra_execution::{CommandOutput as ProcessOutput, ToolError};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
const CAPTURE_MAX_BYTES: usize = 256 * 1024;
/// Windows Job Object keeps the whole child tree owned by the agent. Closing
/// the job kills any process that outlived the direct child.
#[cfg(windows)]
struct ProcessJob(usize);

#[cfg(windows)]
impl ProcessJob {
    fn assign(child: &tokio::process::Child) -> Option<Self> {
        use std::ffi::c_void;
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            let Some(process) = child.raw_handle() else {
                CloseHandle(job);
                return None;
            };
            let assigned = AssignProcessToJobObject(job, process as HANDLE);
            if configured == 0 || assigned == 0 {
                CloseHandle(job);
                return None;
            }
            Some(Self(job as usize))
        }
    }

    fn terminate(&self) {
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        unsafe {
            TerminateJobObject(self.0 as HANDLE, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessJob {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        unsafe {
            CloseHandle(self.0 as HANDLE);
        }
    }
}

#[cfg(not(windows))]
struct ProcessJob;

/// Runs a program in the workspace root with a timeout and captured output.
pub(crate) async fn run_program(
    root: &Path,
    program: &str,
    args: &[String],
    timeout: Duration,
    cancel: Option<CancelFlag>,
) -> Result<ProcessOutput, ToolError> {
    let mut command = tokio::process::Command::new(program);
    command
        .current_dir(native_path(root))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    sanitize_environment(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| ToolError::new(format!("cannot start `{program}`: {error}")))?;
    #[cfg(windows)]
    let job = ProcessJob::assign(&child);
    #[cfg(not(windows))]
    let job: Option<ProcessJob> = None;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ToolError::new("cannot capture stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ToolError::new("cannot capture stderr"))?;

    // Read both pipes to EOF before reaping. On Windows, waiting first can
    // close the pipe handles while buffered output is still unread.
    let wait = async {
        let (stdout, stderr) = tokio::join!(
            read_capped(stdout, CAPTURE_MAX_BYTES),
            read_capped(stderr, CAPTURE_MAX_BYTES),
        );
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((status, stdout, stderr))
    };
    enum Completion {
        Done(std::io::Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>)>),
        TimedOut,
        Cancelled,
    }
    let completion = {
        tokio::pin!(wait);
        tokio::select! {
            result = &mut wait => Completion::Done(result),
            _ = tokio::time::sleep(timeout) => Completion::TimedOut,
            _ = wait_for_cancel(cancel.as_ref()) => Completion::Cancelled,
        }
    };
    match completion {
        Completion::Done(Ok((status, stdout, stderr))) => Ok(ProcessOutput {
            code: status.code(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        }),
        Completion::Done(Err(error)) => {
            kill_tree(&mut child, job.as_ref()).await;
            Err(ToolError::new(format!(
                "`{program}` failed to run: {error}"
            )))
        }
        Completion::TimedOut => {
            kill_tree(&mut child, job.as_ref()).await;
            Err(ToolError::outcome_unknown(format!(
                "`{program}` timed out after {}s; it may still have changed the workspace",
                timeout.as_secs()
            )))
        }
        Completion::Cancelled => {
            kill_tree(&mut child, job.as_ref()).await;
            Err(ToolError::outcome_unknown(format!(
                "`{program}` was cancelled; it may still have changed the workspace"
            )))
        }
    }
}

async fn wait_for_cancel(cancel: Option<&CancelFlag>) {
    match cancel {
        Some(cancel) => {
            while !cancel.is_cancelled() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        None => std::future::pending().await,
    }
}

/// Git for Windows and some native tools misbehave with verbatim (`\\?\`)
/// working directories. The canonical root stays for path confinement; the
/// child process receives the equivalent native path.
fn native_path(path: &Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return std::path::PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return std::path::PathBuf::from(rest);
        }
    }
    path.to_path_buf()
}

/// Reads at most `cap` bytes, draining the pipe so the child never blocks.
async fn read_capped<R>(mut reader: R, cap: usize) -> Vec<u8>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut collected = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                if collected.len() < cap {
                    let remaining = cap - collected.len();
                    collected.extend_from_slice(&buffer[..read.min(remaining)]);
                }
            }
            Err(_) => break,
        }
    }
    collected
}

/// Commands must not inherit credentials from the agent process.
fn sanitize_environment(command: &mut tokio::process::Command) {
    let inherited: Vec<(std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os()
        .filter(|(key, _)| !is_sensitive_key(key))
        .collect();
    command.env_clear();
    command.envs(inherited);
}

fn is_sensitive_key(key: &std::ffi::OsStr) -> bool {
    let key = key.to_string_lossy().to_ascii_uppercase();
    key.contains("KEY")
        || key.contains("TOKEN")
        || key.contains("SECRET")
        || key.contains("PASSWORD")
        || key.contains("CREDENTIAL")
        || key.ends_with("_AUTH")
}

async fn kill_tree(child: &mut tokio::process::Child, _job: Option<&ProcessJob>) {
    #[cfg(windows)]
    {
        if let Some(job) = _job {
            job.terminate();
        }
    }
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            let pid = pid as i32;
            // The child was started as its own process group; signal the whole
            // tree, then force-kill anything that ignored the first signal.
            unsafe {
                libc::kill(-pid, libc::SIGTERM);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
    #[cfg(windows)]
    {
        if let Some(pid) = child.id() {
            let _ = tokio::process::Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[cfg(test)]
mod tests {
    use super::{is_sensitive_key, read_capped};
    use std::ffi::OsStr;

    #[test]
    fn credential_environment_variables_are_filtered() {
        assert!(is_sensitive_key(OsStr::new("OPENAI_API_KEY")));
        assert!(is_sensitive_key(OsStr::new("GITHUB_TOKEN")));
        assert!(is_sensitive_key(OsStr::new("AWS_SECRET_ACCESS_KEY")));
        assert!(is_sensitive_key(OsStr::new("SERVICE_PASSWORD")));
        assert!(!is_sensitive_key(OsStr::new("PATH")));
        assert!(!is_sensitive_key(OsStr::new("GIT_AUTHOR_NAME")));
        assert!(!is_sensitive_key(OsStr::new("RUSTUP_HOME")));
    }

    #[tokio::test]
    async fn captured_output_is_bounded_while_the_pipe_is_drained() {
        let input = std::io::Cursor::new(vec![b'x'; 4096]);

        let collected = read_capped(input, 32).await;

        assert_eq!(collected.len(), 32);
    }
}
