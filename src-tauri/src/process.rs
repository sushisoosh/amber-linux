//! Helper processes: bounded diagnostic output and termination with Amber, including on
//! crashes (a Windows job object or Linux's parent-death signal).

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

/// The last lines a helper printed - the reason when it dies.
pub type Output = Arc<Mutex<VecDeque<String>>>;

const KEEP_LINES: usize = 200;
const MAX_LINE_BYTES: usize = 4096;

/// Unlike std's Child, dropping an owned cleanup helper also kills and reaps it.
pub struct SupervisedChild(Child);

impl SupervisedChild {
    pub fn spawn(cmd: Command, output: &Output) -> std::io::Result<Self> {
        spawn(cmd, output).map(Self)
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.0.try_wait()
    }

    pub fn stop(&mut self) {
        stop(&mut self.0);
    }
}

impl Drop for SupervisedChild {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(target_os = "linux")]
fn spawn_linux(mut cmd: Command) -> std::io::Result<Child> {
    use std::os::unix::process::CommandExt;
    use std::sync::{mpsc, OnceLock};

    type Job = (Command, mpsc::SyncSender<std::io::Result<Child>>);
    static SPAWNER: OnceLock<std::io::Result<Mutex<mpsc::SyncSender<Job>>>> = OnceLock::new();

    let parent = std::process::id() as libc::pid_t;
    // Only async-signal-safe calls between fork and exec. SIGKILL cannot be ignored by a
    // wedged helper. Checking the parent closes the race before prctl installs the signal.
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }

    // PDEATHSIG follows the spawning *thread*. A short-lived warm-up/request thread must
    // never spawn directly; this thread and its sender live until the app exits.
    let spawner = SPAWNER.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<Job>(1);
        std::thread::Builder::new().name("amber-helper-spawner".into()).spawn(move || {
            for (mut cmd, reply) in rx {
                if let Err(mpsc::SendError(Ok(mut child))) = reply.send(cmd.spawn()) {
                    stop(&mut child);
                }
            }
        })?;
        Ok(Mutex::new(tx))
    });
    let spawner = spawner.as_ref().map_err(|e| std::io::Error::new(e.kind(), "The helper supervisor could not start."))?;
    let (tx, rx) = mpsc::sync_channel(0);
    spawner.lock().unwrap_or_else(|e| e.into_inner()).send((cmd, tx))
        .map_err(|_| std::io::Error::other("The helper supervisor stopped."))?;
    rx.recv().map_err(|_| std::io::Error::other("The helper supervisor stopped."))?
}

/// Starts [cmd] hidden, with its stdout and stderr read into [output] (cleared first).
pub fn spawn(mut cmd: Command, output: &Output) -> std::io::Result<Child> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(target_os = "linux")]
    let mut child = spawn_linux(cmd)?;
    #[cfg(not(target_os = "linux"))]
    let mut child = cmd.spawn()?;
    #[cfg(windows)]
    job::attach(&child);

    output.lock().unwrap_or_else(|e| e.into_inner()).clear();
    let pipes = [
        child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    ];
    for pipe in pipes.into_iter().flatten() {
        let out = output.clone();
        if let Err(e) = std::thread::Builder::new().name("amber-helper-output".into()).spawn(move || capture_output(pipe, &out)) {
            stop(&mut child);
            return Err(e);
        }
    }
    Ok(child)
}

fn capture_output(pipe: impl Read, output: &Output) {
    let mut reader = BufReader::new(pipe);
    let mut line = Vec::with_capacity(MAX_LINE_BYTES);
    let mut truncated = false;
    loop {
        let bytes = match reader.fill_buf() {
            Ok([]) => break,
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        for &byte in bytes {
            if byte == b'\n' {
                keep_line(output, &line, truncated);
                line.clear();
                truncated = false;
            } else if line.len() < MAX_LINE_BYTES {
                line.push(byte);
            } else {
                truncated = true;
            }
        }
        let consumed = bytes.len();
        reader.consume(consumed);
    }
    if !line.is_empty() || truncated {
        keep_line(output, &line, truncated);
    }
}

fn keep_line(output: &Output, bytes: &[u8], truncated: bool) {
    let mut line = String::from_utf8_lossy(bytes.strip_suffix(b"\r").unwrap_or(bytes)).into_owned();
    let mut end = line.len().min(MAX_LINE_BYTES);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let truncated = truncated || end < line.len();
    line.truncate(end);
    if truncated {
        line.push_str(" [truncated]");
    }
    let mut output = output.lock().unwrap_or_else(|e| e.into_inner());
    if output.len() >= KEEP_LINES {
        output.pop_front();
    }
    output.push_back(line);
}

pub fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

pub fn last_lines(output: &Output, n: usize) -> String {
    let out = output.lock().unwrap_or_else(|e| e.into_inner());
    out.iter().skip(out.len().saturating_sub(n)).cloned().collect::<Vec<_>>().join(" | ")
}

pub fn free_port() -> std::io::Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// Physical cores are what ggml scales with; hyper-threads mostly get in the way.
/// Half the logical processors, capped at 8.
pub fn default_threads() -> u32 {
    amber_core::local::default_threads()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_bounded_even_without_newlines_or_valid_utf8() {
        let output = Output::default();
        let long = std::io::repeat(0xff).take((MAX_LINE_BYTES * 100) as u64);
        capture_output(long, &output);
        let line = last_lines(&output, 1);
        assert!(line.ends_with(" [truncated]"));
        assert!(line.len() <= MAX_LINE_BYTES + " [truncated]".len());

        let many = (0..KEEP_LINES + 50).map(|n| format!("line {n}\r\n")).collect::<String>();
        capture_output(many.as_bytes(), &output);
        let output = output.lock().unwrap();
        assert_eq!(output.len(), KEEP_LINES);
        assert_eq!(output.front().unwrap(), "line 50");
        assert_eq!(output.back().unwrap(), &format!("line {}", KEEP_LINES + 49));
    }

    #[test]
    fn output_keeps_unterminated_lines_and_empty_lines() {
        let output = Output::default();
        capture_output(&b"first\n\nlast"[..], &output);
        assert_eq!(last_lines(&output, 3), "first |  | last");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn helper_outlives_request_thread_and_is_reaped_on_drop() {
        let mut child = std::thread::spawn(|| {
            let mut cmd = Command::new("sleep");
            cmd.arg("30");
            SupervisedChild::spawn(cmd, &Output::default()).unwrap()
        }).join().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(child.try_wait().unwrap().is_none(), "died with the request thread");
        let pid = child.0.id() as libc::pid_t;
        drop(child);
        assert_eq!(unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) }, -1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ECHILD));
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "helper survived its owner");
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn spawn_failure_does_not_stop_the_supervisor() {
        assert!(SupervisedChild::spawn(Command::new("/amber-missing-test-helper"), &Output::default()).is_err());
        let mut child = SupervisedChild::spawn(Command::new("true"), &Output::default()).unwrap();
        assert!(child.0.wait().unwrap().success());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parent_exit_kills_the_helper() {
        const MODE: &str = "AMBER_TEST_PARENT_DEATH";
        let fixture = |mode| {
            let mut cmd = Command::new(std::env::current_exe().unwrap());
            cmd.args(["--exact", "process::tests::parent_exit_kills_the_helper", "--nocapture"])
                .env(MODE, mode);
            cmd
        };
        match std::env::var(MODE).ok().as_deref() {
            Some("exit" | "crash") => {
                let mut cmd = Command::new("sleep");
                cmd.arg("30");
                let child = SupervisedChild::spawn(cmd, &Output::default()).unwrap();
                println!("helper-pid: {}", child.0.id());
                if std::env::var(MODE).unwrap() == "crash" {
                    unsafe { libc::kill(libc::getpid(), libc::SIGKILL); }
                }
                // Bypass Drop, as Tauri's exit path does. The kernel must kill the helper.
                std::process::exit(0);
            }
            Some("watcher") => {
                // Isolate subreaper state in this fixture process, not the parallel test
                // harness, and reap the orphan so the test leaves no zombie behind.
                assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) }, 0);
                for mode in ["exit", "crash"] {
                    let output = fixture(mode).output().unwrap();
                    let stdout = String::from_utf8(output.stdout).unwrap();
                    let pid: libc::pid_t = stdout.split_once("helper-pid: ").unwrap().1.split_whitespace().next().unwrap().parse().unwrap();
                    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
                    let mut status = 0;
                    loop {
                        let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
                        assert!(reaped >= 0, "could not reap helper: {}", std::io::Error::last_os_error());
                        if reaped == pid {
                            break;
                        }
                        if std::time::Instant::now() >= until {
                            unsafe {
                                libc::kill(pid, libc::SIGKILL);
                                libc::waitpid(pid, &mut status, 0);
                            }
                            panic!("helper survived parent {mode}");
                        }
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    assert!(libc::WIFSIGNALED(status));
                    assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
                }
            }
            _ => {
                let output = fixture("watcher").output().unwrap();
                assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            }
        }
    }
}

#[cfg(windows)]
mod job {
    use std::process::Child;
    use std::sync::Mutex;

    use windows::Win32::Foundation::HANDLE;

    struct Job(HANDLE);
    // Only used behind the mutex below.
    unsafe impl Send for Job {}

    static JOB: Mutex<Option<Job>> = Mutex::new(None);

    fn create() -> Option<Job> {
        use windows::Win32::System::JobObjects::*;
        unsafe {
            let job = CreateJobObjectW(None, None).ok()?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .ok()?;
            Some(Job(job))
        }
    }

    /// The job is created once and never closed: when Amber's process ends, Windows closes
    /// the handle and kills every helper in it.
    pub fn attach(child: &Child) {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::System::JobObjects::AssignProcessToJobObject;
        let mut job = JOB.lock().unwrap();
        if job.is_none() {
            *job = create();
        }
        if let Some(job) = job.as_ref() {
            if let Err(e) = unsafe { AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle())) } {
                crate::diag::warn(format!("helper process not tied to the app: {e}"));
            }
        }
    }
}
