//! A persistent, disposable process owns the blocking HTTP clients and OneOCR.
//! Cancellation terminates only that process; neither a native DLL call nor a
//! socket timeout can keep the UI or a subsequent request waiting. A Windows
//! Job also reaps the process if the UI crashes or is forcibly closed.

use super::{Engine, Output, Progress, Request, Task};
use crate::config::Config;
use anyhow::{bail, ensure, Context, Result};
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use std::{
    io::{BufReader, Read, Write},
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread::{self, JoinHandle},
};
use windows_sys::Win32::System::{
    JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
    Threading::CREATE_NO_WINDOW,
};

pub const SUBPROCESS_ARG: &str = "--sightocr-private-worker";
const MAGIC: &[u8; 16] = b"SightOCR-IPC-v1\0";
const MAX_MESSAGE: usize = 16 * 1024 * 1024;
const MAX_IMAGE: usize = 400_000_000;
const MAX_NOISE: usize = 64 * 1024;

#[derive(Default)]
struct State {
    active: u64,
    pending: Option<Request>,
    child: Option<Child>,
    invalid_process: bool,
    stopped: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    ready: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn active(&self, id: u64) -> bool {
        let state = self.lock();
        !state.stopped && state.active == id
    }

    // No pipe I/O, process wait or engine work is performed under this lock.
    fn cancel(&self, stop: bool) {
        let pending = {
            let mut state = self.lock();
            state.stopped |= stop;
            if state.active != 0 || stop {
                state.active = 0;
                state.invalid_process = true;
                if let Some(child) = state.child.as_mut() {
                    let _ = child.kill();
                }
            }
            state.pending.take()
        };
        self.ready.notify_one();
        drop(pending);
    }
}

/// One supervisor and a single replaceable pending request bound memory and
/// process use even when users repeatedly cancel and immediately start again.
pub struct Worker {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn start(resources: PathBuf, deliver: impl Fn(Output) + Send + 'static) -> Result<Self> {
        Self::start_with_progress(resources, deliver, |_| {})
    }

    pub fn start_with_progress(
        resources: PathBuf,
        deliver: impl Fn(Output) + Send + 'static,
        progress: impl Fn(Progress) + Send + 'static,
    ) -> Result<Self> {
        let executable = std::env::current_exe().context("无法定位识别进程")?;
        let job = ProcessJob::new()?;
        Self::start_with_launcher(
            move || {
                let mut command = Command::new(&executable);
                command.arg(SUBPROCESS_ARG).arg(&resources);
                Spawned::start(command, &job)
            },
            deliver,
            progress,
        )
    }

    fn start_with_launcher(
        launch: impl Fn() -> Result<Spawned> + Send + 'static,
        deliver: impl Fn(Output) + Send + 'static,
        progress: impl Fn(Progress) + Send + 'static,
    ) -> Result<Self> {
        let shared = Arc::new(Shared::default());
        let worker_shared = shared.clone();
        let thread = thread::Builder::new()
            .name("sightocr-supervisor".into())
            .spawn(move || supervise(worker_shared, launch, deliver, progress))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    pub fn submit(&self, request: Request) -> Result<()> {
        ensure!(request.id != 0 && request.id != u64::MAX, "无效任务编号");
        ensure!(
            self.thread
                .as_ref()
                .is_some_and(|thread| !thread.is_finished()),
            "识别进程已停止，请重新启动程序"
        );
        let mut state = self.shared.lock();
        ensure!(!state.stopped, "识别进程已停止，请重新启动程序");
        ensure!(state.active == 0, "已有任务正在处理，请稍后重试");
        state.active = request.id;
        state.pending = Some(request);
        self.shared.ready.notify_one();
        Ok(())
    }

    pub fn cancel(&self) {
        self.shared.cancel(false);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shared.cancel(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct ProcessJob(OwnedHandle);

impl ProcessJob {
    fn new() -> Result<Self> {
        // SAFETY: unnamed job with default security; the returned handle is owned.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        ensure!(!handle.is_null(), "无法创建识别进程管理器");
        // SAFETY: CreateJobObjectW returned a valid uniquely owned handle.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        // SAFETY: this C information structure consists only of integer/pointer fields.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the pointer and size describe the initialized information struct.
        let success = unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        ensure!(success != 0, "无法设置识别进程退出回收");
        Ok(job)
    }

    fn assign(&self, child: &Child) -> Result<()> {
        // SAFETY: both live handles belong to this process; only our child is assigned.
        let success =
            unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), child.as_raw_handle()) };
        ensure!(success != 0, "无法关联识别进程退出回收");
        Ok(())
    }
}

struct Session {
    input: ChildStdin,
    events: BufReader<Box<dyn Read + Send>>,
}

struct Spawned {
    child: Child,
    session: Session,
}

impl Spawned {
    fn start(mut command: Command, job: &ProcessJob) -> Result<Self> {
        let mut child = command
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::piped())
            // Native DLL diagnostics on stdout never enter the protocol.
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("无法启动识别进程")?;
        if let Err(error) = job.assign(&child) {
            terminate(child);
            return Err(error);
        }
        let input = child.stdin.take().context("无法建立识别请求管道")?;
        let events = child.stderr.take().context("无法建立识别结果管道")?;
        Ok(Self {
            child,
            session: Session {
                input,
                events: BufReader::new(Box::new(events)),
            },
        })
    }
}

fn terminate(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn discard_process(shared: &Shared, session: &mut Option<Session>) {
    let child = shared.lock().child.take();
    if let Some(child) = child {
        terminate(child);
    }
    *session = None;
}

fn prepare_session(
    shared: &Shared,
    session: &mut Option<Session>,
    launch: &impl Fn() -> Result<Spawned>,
    id: u64,
) -> Result<()> {
    ensure!(shared.active(id), "任务已取消");
    if session.is_some() && !shared.lock().invalid_process {
        return Ok(());
    }
    discard_process(shared, session);
    let Spawned {
        child,
        session: new,
    } = launch()?;
    let mut state = shared.lock();
    if state.stopped || state.active != id {
        drop(state);
        terminate(child);
        bail!("任务已取消");
    }
    state.child = Some(child);
    state.invalid_process = false;
    *session = Some(new);
    Ok(())
}

fn supervise(
    shared: Arc<Shared>,
    launch: impl Fn() -> Result<Spawned>,
    deliver: impl Fn(Output),
    progress: impl Fn(Progress),
) {
    let mut session = None;
    loop {
        let request = {
            let mut state = shared.lock();
            while state.pending.is_none() && !state.stopped {
                state = shared
                    .ready
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
            }
            if state.stopped {
                break;
            }
            state.pending.take().expect("pending request")
        };
        let id = request.id;
        let result = prepare_session(&shared, &mut session, &launch, id).and_then(|()| {
            transact(
                session.as_mut().expect("prepared session"),
                request,
                &shared,
                &progress,
            )
        });
        let output = match result {
            Ok(output) => output,
            Err(error) => {
                discard_process(&shared, &mut session);
                Output {
                    id,
                    error: Some(format!("识别进程未完成：{error:#}")),
                    ..Output::default()
                }
            }
        };
        let completed = {
            let mut state = shared.lock();
            // A cancelled request must never clear the ID of its replacement.
            if !state.stopped && state.active == id {
                state.active = 0;
                true
            } else {
                false
            }
        };
        if completed {
            deliver(output);
        }
    }
    discard_process(&shared, &mut session);
}

fn transact(
    session: &mut Session,
    request: Request,
    shared: &Shared,
    progress: &impl Fn(Progress),
) -> Result<Output> {
    let id = request.id;
    ensure!(shared.active(id), "任务已取消");
    write_request(&mut session.input, request)?;
    loop {
        let bytes = read_frame(&mut session.events, MAX_MESSAGE)?.context("识别进程已退出")?;
        ensure!(shared.active(id), "任务已取消");
        // Serde errors can quote payload strings. Never echo protocol content,
        // credentials or native diagnostics into a user-visible error chain.
        match serde_json::from_slice::<Event>(&bytes)
            .map_err(|_| anyhow::anyhow!("识别结果格式无效"))?
        {
            Event::Progress(event) => {
                ensure!(event.id == id, "识别进度任务编号不匹配");
                progress(event);
            }
            Event::Output(output) => {
                ensure!(output.id == id, "识别结果任务编号不匹配");
                return Ok(output);
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
enum WireTask {
    Recognize {
        width: u32,
        height: u32,
        translate: bool,
    },
    Translate(String),
}

#[derive(Serialize, Deserialize)]
struct WireRequest {
    id: u64,
    config: Config,
    task: WireTask,
}

#[derive(Serialize, Deserialize)]
enum Event {
    Progress(Progress),
    Output(Output),
}

fn write_request(writer: &mut impl Write, request: Request) -> Result<()> {
    let (task, image) = match request.task {
        Task::Translate(text) => (WireTask::Translate(text), None),
        Task::Recognize { image, translate } => {
            ensure!(
                image.as_raw().len() <= MAX_IMAGE,
                "图片过大（最多 1 亿像素）"
            );
            (
                WireTask::Recognize {
                    width: image.width(),
                    height: image.height(),
                    translate,
                },
                Some(image),
            )
        }
    };
    let metadata = serde_json::to_vec(&WireRequest {
        id: request.id,
        config: request.config,
        task,
    })?;
    ensure!(metadata.len() <= MAX_MESSAGE, "任务文字或配置过长");
    write_frame(writer, &metadata)?;
    if let Some(image) = image {
        write_frame(writer, image.as_raw())?;
    }
    writer.flush()?;
    Ok(())
}

fn read_request(reader: &mut impl Read) -> Result<Option<Request>> {
    let Some(bytes) = read_frame(reader, MAX_MESSAGE)? else {
        return Ok(None);
    };
    let wire: WireRequest = serde_json::from_slice(&bytes).context("识别请求格式无效")?;
    let task = match wire.task {
        WireTask::Translate(text) => Task::Translate(text),
        WireTask::Recognize {
            width,
            height,
            translate,
        } => {
            let pixels = u64::from(width) * u64::from(height);
            ensure!(
                pixels > 0 && pixels <= (MAX_IMAGE / 4) as u64,
                "图片尺寸无效"
            );
            let expected = pixels * 4;
            let pixels = read_frame(reader, expected as usize)?.context("缺少图片数据")?;
            ensure!(pixels.len() as u64 == expected, "图片数据长度无效");
            Task::Recognize {
                image: RgbaImage::from_raw(width, height, pixels).context("图片数据无效")?,
                translate,
            }
        }
    };
    Ok(Some(Request {
        id: wire.id,
        config: wire.config,
        task,
    }))
}

fn write_frame(writer: &mut impl Write, bytes: &[u8]) -> Result<()> {
    writer.write_all(MAGIC)?;
    writer.write_all(&(bytes.len() as u64).to_le_bytes())?;
    writer.write_all(bytes)?;
    writer.flush()?;
    Ok(())
}

fn read_frame(reader: &mut impl Read, limit: usize) -> Result<Option<Vec<u8>>> {
    // Length framing preserves Unicode and newlines. Skip bounded native DLL
    // diagnostics between frames without treating them as JSON or user text.
    let mut matched = 0;
    let mut scanned = 0;
    loop {
        let mut byte = [0];
        if reader.read(&mut byte)? == 0 {
            ensure!(matched == 0, "识别消息意外中断");
            return Ok(None);
        }
        scanned += 1;
        ensure!(scanned <= MAX_NOISE + MAGIC.len(), "识别消息格式无效");
        if byte[0] == MAGIC[matched] {
            matched += 1;
            if matched == MAGIC.len() {
                break;
            }
        } else {
            matched = usize::from(byte[0] == MAGIC[0]);
        }
    }
    let mut length = [0; 8];
    reader.read_exact(&mut length)?;
    let length = u64::from_le_bytes(length);
    ensure!(length <= limit as u64, "识别消息超过大小限制");
    let mut bytes = vec![0; length as usize];
    reader.read_exact(&mut bytes)?;
    Ok(Some(bytes))
}

/// Private entry point, reached before GUI, tray, configuration files or console
/// attachment. Credentials and image data exist only in anonymous pipes/memory.
pub fn run_subprocess(resources: PathBuf) -> Result<()> {
    run_engine(
        resources,
        &mut std::io::stdin().lock(),
        &mut std::io::stderr().lock(),
    )
}

fn run_engine(resources: PathBuf, input: &mut impl Read, output: &mut impl Write) -> Result<()> {
    let mut engine = Engine::new(resources);
    while let Some(request) = read_request(input)? {
        let mut result = Output {
            id: request.id,
            ..Output::default()
        };
        let writer = std::cell::RefCell::new(&mut *output);
        let failed = std::cell::Cell::new(false);
        if let Err(error) = engine.execute_with_progress(
            request,
            &mut result,
            || failed.get(),
            |progress| {
                let sent = serde_json::to_vec(&Event::Progress(progress))
                    .ok()
                    .is_some_and(|bytes| write_frame(&mut *writer.borrow_mut(), &bytes).is_ok());
                if !sent {
                    failed.set(true);
                }
            },
        ) {
            result.error = Some(format!("{error:#}"));
        }
        ensure!(!failed.get(), "识别结果管道已关闭");
        write_frame(output, &serde_json::to_vec(&Event::Output(result))?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Cursor,
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        time::{Duration, Instant},
    };

    const HELPER: &str = "worker::process::tests::subprocess_test_entry";

    fn helper_command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", HELPER, "--nocapture"])
            .env("SIGHTOCR_TEST_PROCESS_MODE", mode);
        command
    }

    fn production_executable() -> Result<PathBuf> {
        if let Some(executable) = std::env::var_os("SIGHTOCR_TEST_EXECUTABLE") {
            let executable = PathBuf::from(executable);
            ensure!(
                executable.is_absolute(),
                "SIGHTOCR_TEST_EXECUTABLE 必须是绝对路径"
            );
            ensure!(executable.is_file(), "指定的集成测试程序不存在");
            return Ok(executable);
        }
        let test_exe = std::env::current_exe()?;
        let directory = test_exe
            .parent()
            .and_then(|path| path.parent())
            .context("无法定位编译目录")?;
        Ok(directory.join("SightOCR.exe"))
    }

    fn production_command() -> Result<Command> {
        let executable = production_executable()?;
        let adjacent = executable
            .parent()
            .context("程序目录无效")?
            .join("resources/oneocr");
        let resources = if adjacent.is_dir() {
            adjacent
        } else {
            super::super::resources_dir()
        };
        let mut command = Command::new(executable);
        command.arg(SUBPROCESS_ARG).arg(resources);
        Ok(command)
    }

    fn test_worker(
        mode: &'static str,
    ) -> (
        Worker,
        mpsc::Receiver<Output>,
        mpsc::Receiver<Progress>,
        Arc<AtomicUsize>,
    ) {
        let job = ProcessJob::new().unwrap();
        let starts = Arc::new(AtomicUsize::new(0));
        let launch_count = starts.clone();
        let (output_tx, output_rx) = mpsc::channel();
        let (progress_tx, progress_rx) = mpsc::channel();
        let worker = Worker::start_with_launcher(
            move || {
                launch_count.fetch_add(1, Ordering::SeqCst);
                let command = if mode == "production" {
                    production_command()?
                } else {
                    helper_command(mode)
                };
                Spawned::start(command, &job)
            },
            move |event| {
                let _ = output_tx.send(event);
            },
            move |event| {
                let _ = progress_tx.send(event);
            },
        )
        .unwrap();
        (worker, output_rx, progress_rx, starts)
    }

    fn request(id: u64, text: &str) -> Request {
        Request {
            id,
            config: Config::default(),
            task: Task::Translate(text.into()),
        }
    }

    fn recv<T>(receiver: &mpsc::Receiver<T>) -> T {
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("worker response")
    }

    #[test]
    fn protocol_round_trip_preserves_all_scripts_and_raw_pixels() {
        let text = "한국어 العربية हिन्दी ไทย ខ្មែរ 繁體中文 Українська 🧑🏽‍💻\n\t";
        let mut input = Vec::new();
        let mut original = request(42, text);
        original.config.openai_api_key = "test-secret-only-in-memory".into();
        write_request(&mut input, original).unwrap();
        let restored = read_request(&mut Cursor::new(&input)).unwrap().unwrap();
        assert_eq!(restored.id, 42);
        assert_eq!(restored.config.openai_api_key, "test-secret-only-in-memory");
        assert!(matches!(restored.task, Task::Translate(value) if value == text));

        input.clear();
        let pixels = vec![0, 12, 255, 44, 10, 255, 9, 128];
        write_request(
            &mut input,
            Request {
                id: 43,
                config: Config::default(),
                task: Task::Recognize {
                    image: RgbaImage::from_raw(2, 1, pixels.clone()).unwrap(),
                    translate: true,
                },
            },
        )
        .unwrap();
        let restored = read_request(&mut Cursor::new(&input)).unwrap().unwrap();
        match restored.task {
            Task::Recognize { image, translate } => {
                assert_eq!(image.dimensions(), (2, 1));
                assert_eq!(*image.as_raw(), pixels);
                assert!(translate);
            }
            _ => panic!("image request expected"),
        }
    }

    #[test]
    fn protocol_skips_native_diagnostics_and_rejects_truncated_or_oversized_frames() {
        let mut stream = b"native DLL diagnostic\r\n".to_vec();
        write_frame(&mut stream, b"first\nline").unwrap();
        stream.extend_from_slice(b"another diagnostic\n");
        write_frame(&mut stream, "한글".as_bytes()).unwrap();
        let mut reader = Cursor::new(stream);
        assert_eq!(
            read_frame(&mut reader, 100).unwrap().unwrap(),
            b"first\nline"
        );
        assert_eq!(
            read_frame(&mut reader, 100).unwrap().unwrap(),
            "한글".as_bytes()
        );
        assert!(read_frame(&mut reader, 100).unwrap().is_none());

        let mut oversized = MAGIC.to_vec();
        oversized.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(read_frame(&mut Cursor::new(oversized), MAX_MESSAGE).is_err());
        let mut truncated = Vec::new();
        write_frame(&mut truncated, b"message").unwrap();
        truncated.pop();
        assert!(read_frame(&mut Cursor::new(truncated), 100).is_err());

        let mut malformed = Vec::new();
        write_frame(
            &mut malformed,
            &serde_json::to_vec(&WireRequest {
                id: 1,
                config: Config::default(),
                task: WireTask::Recognize {
                    width: u32::MAX,
                    height: u32::MAX,
                    translate: false,
                },
            })
            .unwrap(),
        )
        .unwrap();
        assert!(read_request(&mut Cursor::new(malformed)).is_err());
    }

    #[test]
    fn cancellation_interrupts_blocking_work_and_accepts_a_new_task_immediately() {
        let (worker, outputs, progress, starts) = test_worker("normal");
        worker.submit(request(1, "block")).unwrap();
        assert_eq!(recv(&progress).id, 1);
        let before = Instant::now();
        worker.cancel();
        worker.submit(request(2, "한국어 العربية हिन्दी")).unwrap();
        let next = recv(&outputs);
        assert_eq!(next.id, 2, "cancelled results must not be delivered");
        assert_eq!(next.translated.as_deref(), Some("한국어 العربية हिन्दी"));
        assert!(
            before.elapsed() < Duration::from_secs(2),
            "cancellation must not await the 90-second call"
        );
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert!(outputs.try_recv().is_err());
    }

    #[test]
    fn successful_tasks_reuse_the_process_and_repeated_cancel_never_accumulates_work() {
        let (worker, outputs, progress, starts) = test_worker("normal");
        for id in 1..=2 {
            worker.submit(request(id, "echo")).unwrap();
            assert_eq!(recv(&outputs).id, id);
            assert_eq!(recv(&progress).id, id);
        }
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "retain engine and token caches"
        );
        worker.submit(request(3, "block")).unwrap();
        assert_eq!(recv(&progress).id, 3);
        for id in 4..=100 {
            worker.cancel();
            worker.submit(request(id, "block")).unwrap();
        }
        worker.cancel();
        worker.submit(request(101, "last")).unwrap();
        assert_eq!(recv(&outputs).id, 101);
        assert!(outputs.try_recv().is_err());
        let state = worker.shared.lock();
        assert_eq!(state.active, 0);
        assert!(
            state.pending.is_none(),
            "cancelled queued requests must be removed"
        );
    }

    #[test]
    fn dropping_a_busy_worker_reaps_the_process_without_waiting_for_its_call() {
        let (worker, _, progress, _) = test_worker("normal");
        worker.submit(request(1, "block")).unwrap();
        recv(&progress);
        let before = Instant::now();
        drop(worker);
        assert!(before.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn cancel_and_shutdown_also_interrupt_a_blocked_image_pipe_write() {
        let (worker, _, _, _) = test_worker("do-not-read");
        worker
            .submit(Request {
                id: 1,
                config: Config::default(),
                task: Task::Recognize {
                    image: RgbaImage::new(2048, 2048),
                    translate: false,
                },
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while worker.shared.lock().child.is_none() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        thread::sleep(Duration::from_millis(30));
        let before = Instant::now();
        worker.cancel();
        drop(worker);
        assert!(before.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_crashed_child_is_replaced_and_its_error_does_not_poison_future_requests() {
        let (worker, outputs, _, starts) = test_worker("normal");
        worker.submit(request(1, "crash")).unwrap();
        let failed = recv(&outputs);
        assert_eq!(failed.id, 1);
        assert!(failed.error.is_some());
        worker.submit(request(2, "recovered")).unwrap();
        assert_eq!(recv(&outputs).translated.as_deref(), Some("recovered"));
        assert_eq!(starts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn closing_the_job_kills_children_even_without_cooperative_cancellation() {
        let job = ProcessJob::new().unwrap();
        let mut spawned = Spawned::start(helper_command("do-not-read"), &job).unwrap();
        drop(job);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if spawned.child.try_wait().unwrap().is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "job must reap child after parent handle closes"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn cancellation_during_process_launch_cannot_start_the_cancelled_request() {
        let job = ProcessJob::new().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let starts = AtomicUsize::new(0);
        let (output_tx, output_rx) = mpsc::channel();
        let (progress_tx, progress_rx) = mpsc::channel();
        let worker = Worker::start_with_launcher(
            move || {
                let spawned = Spawned::start(helper_command("normal"), &job)?;
                if starts.fetch_add(1, Ordering::SeqCst) == 0 {
                    started_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                Ok(spawned)
            },
            move |output| {
                let _ = output_tx.send(output);
            },
            move |progress| {
                let _ = progress_tx.send(progress);
            },
        )
        .unwrap();
        worker.submit(request(1, "block")).unwrap();
        recv(&started_rx);
        worker.cancel();
        worker.submit(request(2, "new task")).unwrap();
        release_tx.send(()).unwrap();
        assert_eq!(recv(&output_rx).id, 2);
        assert_eq!(recv(&progress_rx).id, 2);
        assert!(output_rx.try_recv().is_err() && progress_rx.try_recv().is_err());
    }

    #[test]
    fn cancellation_closes_real_openai_ocr_and_translation_sockets_immediately() {
        exercise_network_cancellation("real-engine");
    }

    #[test]
    #[ignore = "requires cargo build --bin SightOCR; exercises the actual application entry point"]
    fn production_executable_cancels_network_and_restarts_without_opening_a_window() {
        exercise_network_cancellation("production");
    }

    #[test]
    #[ignore = "requires built SightOCR and OneOCR resources; SIGHTOCR_TEST_EXECUTABLE selects a packaged executable"]
    fn production_executable_recognizes_with_oneocr_reuses_process_and_exits() {
        use std::os::windows::io::AsHandle;
        use windows_sys::Win32::{
            Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject,
        };

        let executable = production_executable().unwrap();
        if std::env::var_os("SIGHTOCR_TEST_EXECUTABLE").is_some() {
            assert!(
                executable
                    .parent()
                    .unwrap()
                    .join("resources/oneocr")
                    .is_dir(),
                "packaged OneOCR test requires resources adjacent to the selected executable"
            );
        }
        let (worker, outputs, progress, starts) = test_worker("production");
        let mut child_pid = None;
        for (id, selection) in [(1, "默认"), (2, "默认_table")] {
            let image = image::load_from_memory(include_bytes!("../../tests/fixtures/basic.png"))
                .unwrap()
                .into_rgba8();
            let before = Instant::now();
            worker
                .submit(Request {
                    id,
                    config: Config {
                        last_ocr_selection: selection.into(),
                        proxy: crate::config::ProxyConfig {
                            mode: crate::config::ProxyMode::Direct,
                            ..crate::config::ProxyConfig::default()
                        },
                        ..Config::default()
                    },
                    task: Task::Recognize {
                        image,
                        translate: false,
                    },
                })
                .unwrap();
            let output = outputs
                .recv_timeout(Duration::from_secs(30))
                .expect("bundled OneOCR result");
            assert_eq!(output.id, id);
            assert!(
                output.error.is_none(),
                "private worker OCR failed: {:?}",
                output.error
            );
            assert!(output.warning.is_none() && output.translated.is_none());
            let text = output
                .recognized
                .expect("recognized text crosses the private IPC pipe");
            if selection.ends_with("_table") {
                assert_eq!(text.trim(), "Name\tAmount\nAlpha\t100\nBeta\t200");
            } else {
                assert!(text.contains("Alpha") && text.contains("100") && text.contains("Beta"));
            }
            let event = recv(&progress);
            assert_eq!(event.id, id);
            assert_eq!(event.stage, super::super::ProgressStage::Recognizing);
            let pid = worker
                .shared
                .lock()
                .child
                .as_ref()
                .expect("persistent child")
                .id();
            if let Some(previous) = child_pid {
                assert_eq!(pid, previous, "OneOCR child must be reused");
            }
            child_pid = Some(pid);
            eprintln!(
                "production OneOCR {selection}: task={id}, pid={pid}, elapsed={:?}, executable={}",
                before.elapsed(),
                executable.display()
            );
        }
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        let process_handle = worker
            .shared
            .lock()
            .child
            .as_ref()
            .unwrap()
            .as_handle()
            .try_clone_to_owned()
            .unwrap();
        let before = Instant::now();
        drop(worker);
        assert!(
            before.elapsed() < Duration::from_secs(2),
            "OneOCR shutdown must not wait on its IPC reader"
        );
        assert_eq!(
            // SAFETY: this duplicated handle still refers to our own worker child.
            unsafe { WaitForSingleObject(process_handle.as_raw_handle(), 0) },
            WAIT_OBJECT_0,
            "the real OneOCR process must exit, not merely lose its UI reference"
        );
        assert!(outputs.try_recv().is_err() && progress.try_recv().is_err());
    }

    fn exercise_network_cancellation(mode: &'static str) {
        use crate::config::{ProxyConfig, ProxyMode};
        use std::net::TcpListener;

        for recognizing in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let (blocked_tx, blocked_rx) = mpsc::channel();
            let (closed_tx, closed_rx) = mpsc::channel();
            let proxy = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                Instant::now() < deadline,
                                "real engine must reach local proxy"
                            );
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("local proxy accept failed: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut connect = Vec::new();
                while !connect.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    connect.push(byte[0]);
                    assert!(connect.len() < 16_384);
                }
                assert!(connect.starts_with(b"CONNECT api.openai.com:443 "));
                assert!(!String::from_utf8_lossy(&connect).contains("synthetic-api-key"));
                stream
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .unwrap();
                // Read a real ClientHello, then deliberately withhold the TLS
                // handshake. No provider is contacted and no text/image leaves
                // localhost; reqwest is now blocked in its actual network path.
                let mut bytes = [0; 8192];
                assert!(stream.read(&mut bytes).unwrap() > 0);
                blocked_tx.send(()).unwrap();
                loop {
                    match stream.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::ConnectionReset
                                    | std::io::ErrorKind::ConnectionAborted
                                    | std::io::ErrorKind::UnexpectedEof
                            ) =>
                        {
                            break
                        }
                        Err(error) => panic!("cancel must close socket before timeout: {error}"),
                    }
                }
                closed_tx.send(()).unwrap();
            });
            let (worker, outputs, _, _) = test_worker(mode);
            let config = Config {
                openai_api_key: "synthetic-api-key".into(),
                last_ocr_selection: "OpenAI_table".into(),
                last_translate_selection: "OpenAI".into(),
                source_lang: "en".into(),
                target_lang: "zh-Hans".into(),
                proxy: ProxyConfig {
                    mode: ProxyMode::Manual,
                    url: format!("http://{address}"),
                    ..ProxyConfig::default()
                },
                ..Config::default()
            };
            worker
                .submit(Request {
                    id: 1,
                    config,
                    task: if recognizing {
                        Task::Recognize {
                            image: RgbaImage::new(2, 2),
                            translate: false,
                        }
                    } else {
                        Task::Translate("synthetic text".into())
                    },
                })
                .unwrap();
            recv(&blocked_rx);
            let before = Instant::now();
            worker.cancel();
            let cancel_elapsed = before.elapsed();
            let mut next = request(2, "다시 시작 / restart");
            next.config.source_lang = "en".into();
            next.config.target_lang = "en".into();
            next.config.proxy.mode = ProxyMode::Direct;
            worker.submit(next).unwrap();
            recv(&closed_rx);
            let disconnected_elapsed = before.elapsed();
            let output = recv(&outputs);
            assert_eq!(output.id, 2);
            assert_eq!(output.translated.as_deref(), Some("다시 시작 / restart"));
            assert!(output.error.is_none());
            assert!(before.elapsed() < Duration::from_secs(2));
            assert!(cancel_elapsed < Duration::from_millis(250));
            eprintln!("real OpenAI {} cancellation: API={cancel_elapsed:?}, socket closed={disconnected_elapsed:?}, replacement={:?}",
                if recognizing { "OCR" } else { "translation" }, before.elapsed());
            proxy.join().unwrap();
        }
    }

    #[test]
    #[ignore = "private subprocess fixture; invoked by process lifecycle tests"]
    fn subprocess_test_entry() {
        let Ok(mode) = std::env::var("SIGHTOCR_TEST_PROCESS_MODE") else {
            return;
        };
        if mode == "real-engine" {
            let code = i32::from(run_subprocess(super::super::resources_dir()).is_err());
            std::process::exit(code);
        }
        if mode == "do-not-read" {
            thread::sleep(Duration::from_secs(90));
            std::process::exit(0);
        }
        let mut input = std::io::stdin().lock();
        let mut output = std::io::stderr().lock();
        while let Some(request) = read_request(&mut input).unwrap() {
            let Task::Translate(text) = request.task else {
                panic!("translation fixture")
            };
            write_frame(
                &mut output,
                &serde_json::to_vec(&Event::Progress(Progress {
                    id: request.id,
                    stage: super::super::ProgressStage::Translating,
                    recognized: None,
                    warning: None,
                }))
                .unwrap(),
            )
            .unwrap();
            if text == "block" {
                thread::sleep(Duration::from_secs(90));
            }
            if text == "crash" {
                std::process::exit(2);
            }
            write_frame(
                &mut output,
                &serde_json::to_vec(&Event::Output(Output {
                    id: request.id,
                    translated: Some(text),
                    ..Output::default()
                }))
                .unwrap(),
            )
            .unwrap();
        }
        std::process::exit(0);
    }
}
