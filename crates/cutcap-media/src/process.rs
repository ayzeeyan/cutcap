use crate::{Cancellation, MediaError};
use std::{
    io::Read,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub(crate) struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub elapsed_us: u64,
    pub pid: u32,
}

/// Every return path terminates/reaps the child. FFmpeg is invoked directly,
/// never via a shell, and does not spawn helper processes with this adapter.
pub(crate) struct OwnedChild(pub Child);

/// Kernel-enforced per-child commit cap and parent-death cleanup. This is not
/// a filesystem/security sandbox. Only the child is assigned, never the host.
pub(crate) struct ProcessBudget {
    #[cfg(windows)]
    _job: win_custody::Job,
}
impl ProcessBudget {
    pub(crate) fn attach(child: &Child) -> Result<Self, MediaError> {
        #[cfg(windows)]
        {
            let limits = win_custody::JobLimits::new()
                .kill_on_close(true)
                .active_processes(1)
                .process_memory(1024 * 1024 * 1024);
            let job = win_custody::Job::create(&limits).map_err(|source| MediaError::Io {
                stage: "Create media resource job",
                source,
            })?;
            job.assign(child).map_err(|source| MediaError::Io {
                stage: "Limit media process",
                source,
            })?;
            Ok(Self { _job: job })
        }
        #[cfg(not(windows))]
        {
            let _ = child;
            Ok(Self {})
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _kill_result = self.0.kill();
        }
        let _reap_result = self.0.wait();
    }
}

fn read_limited(
    mut pipe: impl Read,
    cap: usize,
    overflow: Arc<AtomicBool>,
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(cap)
        .map_err(std::io::Error::other)?;
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        let count = pipe.read(&mut chunk)?;
        if count == 0 {
            return Ok(output);
        }
        if count > cap.saturating_sub(output.len()) {
            overflow.store(true, Ordering::Release);
            return Ok(output);
        }
        output.extend_from_slice(&chunk[..count]);
    }
}

pub(crate) fn run(
    mut command: Command,
    stage: &'static str,
    cancellation: &Cancellation,
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
) -> Result<Output, MediaError> {
    if cancellation.is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    command
        .env("AV_LOG_FORCE_NOCOLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let started = Instant::now();
    let child = command
        .spawn()
        .map_err(|source| MediaError::Io { stage, source })?;
    run_child(
        child,
        stage,
        cancellation,
        timeout,
        stdout_limit,
        stderr_limit,
        started,
    )
}

fn run_child(
    child: Child,
    stage: &'static str,
    cancellation: &Cancellation,
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
    started: Instant,
) -> Result<Output, MediaError> {
    let mut child = OwnedChild(child);
    let _budget = ProcessBudget::attach(&child.0)?;
    let pid = child.0.id();
    let stdout = child.0.stdout.take().ok_or(MediaError::Worker)?;
    let stderr = child.0.stderr.take().ok_or(MediaError::Worker)?;
    let overflow = Arc::new(AtomicBool::new(false));
    let limit = overflow.clone();
    let out = thread::Builder::new()
        .name("cutcap-media-stdout".into())
        .spawn(move || read_limited(stdout, stdout_limit, limit))
        .map_err(|source| MediaError::Io { stage, source })?;
    let limit = overflow.clone();
    let err = match thread::Builder::new()
        .name("cutcap-media-stderr".into())
        .spawn(move || read_limited(stderr, stderr_limit, limit))
    {
        Ok(reader) => reader,
        Err(source) => {
            drop(child);
            let _reader_result = out.join();
            return Err(MediaError::Io { stage, source });
        }
    };
    let status = loop {
        if cancellation.is_cancelled() {
            break Err(MediaError::Cancelled);
        }
        if overflow.load(Ordering::Acquire) {
            break Err(MediaError::Budget(
                "subprocess output exceeded its byte budget",
            ));
        }
        if started.elapsed() >= timeout {
            break Err(MediaError::Timeout(stage));
        }
        match child.0.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => cancellation.wait(Duration::from_millis(10)),
            Err(source) => break Err(MediaError::Io { stage, source }),
        }
    };
    // Close the process before joining readers, including on timeout/cancel.
    drop(child);
    let stdout_result = out.join();
    let stderr_result = err.join();
    let stdout = stdout_result
        .map_err(|_| MediaError::Worker)?
        .map_err(|source| MediaError::Io { stage, source })?;
    let stderr = stderr_result
        .map_err(|_| MediaError::Worker)?
        .map_err(|source| MediaError::Io { stage, source })?;
    let status = status?;
    if overflow.load(Ordering::Acquire) {
        return Err(MediaError::Budget(
            "subprocess output exceeded its byte budget",
        ));
    }
    if cancellation.is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    if !status.success() {
        return Err(MediaError::Process {
            stage,
            code: status.code(),
            detail: String::from_utf8_lossy(&stderr)
                .chars()
                .take(2000)
                .collect(),
        });
    }
    Ok(Output {
        stdout,
        stderr,
        elapsed_us: started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
        pid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[cfg(windows)]
    #[test]
    fn child_job_enforces_commit_limit_and_kills_on_close() {
        let mut child = helper("hang").spawn().unwrap();
        let budget = ProcessBudget::attach(&child).unwrap();
        let limits = budget._job.limits().unwrap();
        assert_eq!(limits.process_memory_limit(), Some(1024 * 1024 * 1024));
        assert!(limits.kills_on_close());
        assert!(child.try_wait().unwrap().is_none());
        let close = Instant::now();
        drop(budget);
        // Kill-on-close may use exit status zero on Windows. Bounded exit of
        // the 30-second hanging fixture, not its numeric code, proves cleanup.
        child.wait().unwrap();
        assert!(close.elapsed() < Duration::from_secs(1));
    }

    #[test]
    #[ignore = "child-process fixture, invoked by the process ownership tests"]
    fn child_fixture() {
        match std::env::var("CUT_CAP_CHILD_MODE").as_deref() {
            Ok("hang") => thread::sleep(Duration::from_secs(30)),
            Ok("flood") => {
                let bytes = [1_u8; 8192];
                loop {
                    if std::io::stdout().write_all(&bytes).is_err() {
                        break;
                    }
                }
            }
            Ok("fail") => std::process::exit(17),
            _ => {}
        }
    }

    fn helper(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "process::tests::child_fixture",
                "--nocapture",
            ])
            .env("CUT_CAP_CHILD_MODE", mode);
        command
    }

    #[test]
    fn failed_spawn_exit_and_output_limit_are_typed() {
        assert!(matches!(
            run(
                Command::new("cutcap-no-such-process.exe"),
                "test",
                &Cancellation::default(),
                Duration::from_secs(2),
                1024,
                1024
            ),
            Err(MediaError::Io { .. })
        ));
        let failed = run(
            helper("fail"),
            "test",
            &Cancellation::default(),
            Duration::from_secs(2),
            1024,
            1024,
        );
        assert!(
            matches!(failed, Err(MediaError::Process { code: Some(17), .. })),
            "unexpected child failure error: {:?}",
            failed.as_ref().err()
        );
        assert!(matches!(
            run(
                helper("flood"),
                "test",
                &Cancellation::default(),
                Duration::from_secs(2),
                1024,
                1024
            ),
            Err(MediaError::Budget(_))
        ));
    }

    #[test]
    fn timeout_and_cancellation_terminate_and_reap_children() {
        let token = Cancellation::default();
        let mut command = helper("hang");
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().unwrap();
        let pid = child.id();
        let cancel = token.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            cancel.cancel();
        });
        assert!(matches!(
            run_child(
                child,
                "test",
                &token,
                Duration::from_secs(2),
                1024,
                1024,
                Instant::now()
            ),
            Err(MediaError::Cancelled)
        ));
        canceller.join().unwrap();
        #[cfg(windows)]
        {
            let listing = Command::new("tasklist.exe")
                .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
                .output()
                .unwrap();
            assert!(!String::from_utf8_lossy(&listing.stdout).contains(&format!("\"{pid}\"")));
        }
        assert!(matches!(
            run(
                helper("hang"),
                "test",
                &Cancellation::default(),
                Duration::from_millis(50),
                1024,
                1024
            ),
            Err(MediaError::Timeout("test"))
        ));
    }
}
