//! The Xwayland display and the process that serves it.
//!
//! The display is allocated at startup and kept across restarts. Xwayland
//! itself starts only when something connects to the listening socket.

mod launch;
mod lines;
mod restart;
mod socket;

use std::{
    io::ErrorKind,
    os::fd::{AsRawFd, OwnedFd},
    rc::Rc,
    time::{Duration, Instant},
};

use crate::{
    clients::{ClientId, ClientManagerAction, ClientOrigin},
    io_uring::{
        model::IoUringAction,
        ring::{PollCompleted, ReadWriteCompleted, RingId},
    },
    process::{pidfd_send_signal, ChildExit, ProcessEvent, ProcessManagerAction},
    runner::{Dispatcher, Model, Redispatcher},
    State,
};

use {
    launch::{Launch, OurEnds},
    lines::LineSplitter,
    restart::{Decision, RestartPolicy},
    socket::XDisplay,
};

const DISPLAYFD_BYTES: usize = 64;
const STDERR_BYTES: usize = 4096;

pub(crate) struct Xwayland {
    display: Option<XDisplay>,
    phase: Phase,
    policy: RestartPolicy,
    /// Generation of the launch in progress or last started. Older completions are ignored.
    run: u64,
    /// `Stop` has run. A pid that arrives afterwards is killed, and nothing is relaunched.
    stopped: bool,
}

enum Phase {
    /// Not allocated, either because startup has not run or Xwayland is turned off.
    Disabled,
    Waiting {
        poll: RingId,
    },
    Launching {
        ours: OurEnds,
        started: Instant,
    },
    Running(Run),
    GivenUp,
}

struct Run {
    pid: i32,
    pidfd: Rc<OwnedFd>,
    started: Instant,
    ready: bool,
    client: Option<ClientId>,
    wm: OwnedFd,
    stderr: Option<OwnedFd>,
    displayfd: Option<OwnedFd>,
    lines: LineSplitter,
    reads: Vec<RingId>,
}

#[derive(Copy, Clone)]
enum Pipe {
    Display,
    Stderr,
}

pub(crate) enum XwaylandAction {
    Start,
    Stop,
    ListenReadable { result: std::io::Result<PollCompleted> },
    Process { run: u64, event: ProcessEvent },
    ClientAdopted { run: u64, id: ClientId },
    ClientClosed { run: u64, id: ClientId },
    DisplayReady { run: u64, read: RingId, result: std::io::Result<ReadWriteCompleted> },
    Stderr { run: u64, read: RingId, result: std::io::Result<ReadWriteCompleted> },
}

impl Xwayland {
    pub(crate) fn new() -> Self {
        Self {
            display: None,
            phase: Phase::Disabled,
            policy: RestartPolicy::default(),
            run: 0,
            stopped: false,
        }
    }

    fn start(&mut self, state: &State, dispatcher: &mut Dispatcher) {
        if self.stopped || !matches!(self.phase, Phase::Disabled) || self.display.is_some() {
            return;
        }
        if !state.config.xwayland.enabled {
            log::info!("Xwayland disabled");
            return;
        }
        let display = match socket::allocate() {
            Ok(display) => display,
            Err(error) => {
                log::error!("could not allocate an X display: {error}");
                return;
            }
        };
        let number = display.number();
        log::info!("Allocated display :{number}");
        dispatcher.dispatch(ProcessManagerAction::SetEnv {
            key: "DISPLAY".to_string(),
            value: Some(format!(":{number}")),
        });
        self.display = Some(display);
        self.arm_poll(dispatcher);
    }

    fn arm_poll(&mut self, dispatcher: &mut Dispatcher) {
        let Some(display) = &self.display else { return };
        let poll = RingId::fresh();
        let fd = display.listener().as_raw_fd();
        self.phase = Phase::Waiting { poll };
        dispatcher.dispatch(IoUringAction::Poll {
            id: Some(poll),
            fd,
            dispatch_to: Redispatcher::to::<Self>(|result| XwaylandAction::ListenReadable {
                result,
            }),
        });
    }

    fn on_listen(&mut self, result: std::io::Result<PollCompleted>, dispatcher: &mut Dispatcher) {
        let Phase::Waiting { poll } = self.phase else { return };
        dispatcher.dispatch(IoUringAction::PollRemove { target: poll });
        match result {
            Err(error) => self.give_up(dispatcher, &format!("listen poll failed: {error}")),
            Ok(_) => self.launch(dispatcher),
        }
    }

    fn launch(&mut self, dispatcher: &mut Dispatcher) {
        self.run = self.run.wrapping_add(1);
        let run = self.run;
        let Some(display) = &self.display else { return };
        let notify = Redispatcher::to::<Self>(move |event| XwaylandAction::Process { run, event });
        match Launch::prepare(display.listener(), notify) {
            Ok(Launch { request, ours }) => {
                dispatcher.dispatch(ProcessManagerAction::Spawn(request));
                self.phase = Phase::Launching { ours, started: Instant::now() };
            }
            Err(error) => {
                log::error!("could not prepare Xwayland: {error}");
                self.early_failure(dispatcher);
            }
        }
    }

    /// `prepare` failed before a process existed. The restart policy counts that.
    fn early_failure(&mut self, dispatcher: &mut Dispatcher) {
        self.decide(false, None, Duration::ZERO, dispatcher, "repeated launch failures");
    }

    fn decide(
        &mut self,
        ready: bool,
        exit: Option<ChildExit>,
        ran_for: Duration,
        dispatcher: &mut Dispatcher,
        reason: &str,
    ) {
        match self.policy.on_exit(ready, exit, ran_for) {
            Decision::Relaunch => self.arm_poll(dispatcher),
            Decision::GiveUp => self.give_up(dispatcher, reason),
        }
    }

    fn on_spawned(&mut self, run: u64, pid: i32, pidfd: Rc<OwnedFd>, dispatcher: &mut Dispatcher) {
        if self.run != run {
            return;
        }
        if self.stopped {
            self.kill(&pidfd);
            return;
        }
        if !matches!(self.phase, Phase::Launching { .. }) {
            return;
        }
        let Phase::Launching { ours, started } =
            std::mem::replace(&mut self.phase, Phase::Disabled)
        else {
            unreachable!("phase was Launching");
        };
        let OurEnds { stderr, displayfd, wm, wayland } = ours;
        let stderr_fd = stderr.as_raw_fd();
        let display_fd = displayfd.as_raw_fd();
        self.phase = Phase::Running(Run {
            pid,
            pidfd,
            started,
            ready: false,
            client: None,
            wm,
            stderr: Some(stderr),
            displayfd: Some(displayfd),
            lines: LineSplitter::default(),
            reads: Vec::new(),
        });
        self.arm_read(dispatcher, display_fd, DISPLAYFD_BYTES, Pipe::Display);
        self.arm_read(dispatcher, stderr_fd, STDERR_BYTES, Pipe::Stderr);
        dispatcher.dispatch(ClientManagerAction::AdoptClient {
            fd: wayland,
            origin: ClientOrigin { xwayland: true, pid: Some(pid) },
            adopted: Redispatcher::to::<Self>(move |id| XwaylandAction::ClientAdopted { run, id }),
            closed: Redispatcher::to::<Self>(move |id| XwaylandAction::ClientClosed { run, id }),
        });
    }

    fn on_failed(&mut self, error: String, dispatcher: &mut Dispatcher) {
        if !matches!(self.phase, Phase::Launching { .. }) {
            return;
        }
        let Phase::Launching { started, .. } = std::mem::replace(&mut self.phase, Phase::Disabled)
        else {
            unreachable!("phase was Launching");
        };
        let ran_for = started.elapsed();
        log::error!("Xwayland spawn failed: {error}");
        self.decide(false, None, ran_for, dispatcher, "repeated early failures");
    }

    fn on_exited(&mut self, exit: ChildExit, dispatcher: &mut Dispatcher) {
        let Some(finished) = self.take_child() else { return };
        cancel_reads(&finished.reads, dispatcher);
        let ran_for = finished.started.elapsed();
        match exit {
            ChildExit::Code(code) => {
                log::info!("Xwayland exited with code {code} after {ran_for:?}")
            }
            ChildExit::Signal(signal) => {
                log::info!("Xwayland exited with signal {signal} after {ran_for:?}")
            }
        }
        self.decide(finished.ready, Some(exit), ran_for, dispatcher, "repeated early failures");
    }

    fn on_lost(&mut self, dispatcher: &mut Dispatcher) {
        match &self.phase {
            Phase::Running(run) => {
                let reads = run.reads.clone();
                let pidfd = Rc::clone(&run.pidfd);
                cancel_reads(&reads, dispatcher);
                self.kill(&pidfd);
            }
            Phase::Launching { .. } => {}
            _ => return,
        }
        self.give_up(dispatcher, "the forker is gone");
    }

    fn on_client_closed(&mut self, run: u64, id: ClientId) {
        if self.run != run {
            return;
        }
        let Phase::Running(running) = &self.phase else { return };
        if running.client.is_some_and(|client| client != id) {
            return;
        }
        let pidfd = Rc::clone(&running.pidfd);
        self.kill(&pidfd);
    }

    fn take_child(&mut self) -> Option<Finished> {
        if !matches!(self.phase, Phase::Running(_) | Phase::Launching { .. }) {
            return None;
        }
        match std::mem::replace(&mut self.phase, Phase::Disabled) {
            Phase::Running(run) => {
                Some(Finished { ready: run.ready, started: run.started, reads: run.reads })
            }
            Phase::Launching { started, .. } => {
                Some(Finished { ready: false, started, reads: Vec::new() })
            }
            other => {
                self.phase = other;
                None
            }
        }
    }

    fn stop(&mut self, dispatcher: &mut Dispatcher) {
        self.stopped = true;
        match std::mem::replace(&mut self.phase, Phase::Disabled) {
            Phase::Waiting { poll } => {
                dispatcher.dispatch(IoUringAction::PollRemove { target: poll });
            }
            Phase::Running(run) => {
                cancel_reads(&run.reads, dispatcher);
                self.kill(&run.pidfd);
            }
            Phase::Launching { .. } | Phase::Disabled | Phase::GivenUp => {}
        }
        self.display = None;
    }

    fn kill(&self, pidfd: &impl AsRawFd) {
        if let Err(error) = pidfd_send_signal(pidfd, libc::SIGKILL) {
            log::error!("could not kill Xwayland: {error}");
        }
    }

    fn on_adopted(&mut self, run: u64, id: ClientId) {
        if self.run != run {
            return;
        }
        if let Phase::Running(running) = &mut self.phase {
            running.client = Some(id);
        }
    }

    fn on_display_ready(
        &mut self,
        run: u64,
        read: RingId,
        result: std::io::Result<ReadWriteCompleted>,
    ) {
        if self.run != run {
            return;
        }
        self.finish_read(read);
        let Some(number) = self.display.as_ref().map(XDisplay::number) else { return };
        let Phase::Running(running) = &mut self.phase else { return };
        match result {
            Ok(completed) => {
                let text = String::from_utf8_lossy(&completed.buffer);
                let text = text.trim_end_matches(['\r', '\n']);
                running.ready = true;
                running.displayfd = None;
                log::info!("Xwayland ready on :{number}: {text}");
            }
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => running.displayfd = None,
            Err(error) => {
                log::error!("Xwayland displayfd read failed: {error}");
                running.displayfd = None;
            }
        }
    }

    fn on_stderr(
        &mut self,
        dispatcher: &mut Dispatcher,
        run: u64,
        read: RingId,
        result: std::io::Result<ReadWriteCompleted>,
    ) {
        if self.run != run {
            return;
        }
        self.finish_read(read);
        let rearm = match &mut self.phase {
            Phase::Running(running) => match result {
                Ok(completed) => {
                    let fd = running.stderr.as_ref().map(|fd| fd.as_raw_fd());
                    running.lines.push(&completed.buffer, log_stderr);
                    fd
                }
                Err(error) => {
                    if error.kind() != ErrorKind::UnexpectedEof {
                        log::error!("Xwayland stderr read failed: {error}");
                    }
                    running.lines.finish(log_stderr);
                    running.stderr = None;
                    None
                }
            },
            _ => return,
        };
        if let Some(fd) = rearm {
            self.arm_read(dispatcher, fd, STDERR_BYTES, Pipe::Stderr);
        }
    }

    fn arm_read(&mut self, dispatcher: &mut Dispatcher, fd: i32, length: usize, pipe: Pipe) {
        let run = self.run;
        let Phase::Running(running) = &mut self.phase else { return };
        let read = RingId::fresh();
        running.reads.push(read);
        dispatcher.dispatch(IoUringAction::ReadSome {
            id: Some(read),
            fd,
            offset: 0,
            length,
            dispatch_to: Redispatcher::to::<Self>(move |result| match pipe {
                Pipe::Display => XwaylandAction::DisplayReady { run, read, result },
                Pipe::Stderr => XwaylandAction::Stderr { run, read, result },
            }),
        });
    }

    fn finish_read(&mut self, read: RingId) {
        if let Phase::Running(running) = &mut self.phase {
            running.reads.retain(|queued| *queued != read);
        }
    }

    fn give_up(&mut self, dispatcher: &mut Dispatcher, reason: &str) {
        if matches!(self.phase, Phase::GivenUp) {
            return;
        }
        log::error!("Xwayland gave up: {reason}");
        self.phase = Phase::GivenUp;
        self.display = None;
        dispatcher
            .dispatch(ProcessManagerAction::SetEnv { key: "DISPLAY".to_string(), value: None });
    }
}

fn log_stderr(line: &str) {
    log::info!("Xwayland: {line}");
}

struct Finished {
    ready: bool,
    started: Instant,
    reads: Vec<RingId>,
}

fn cancel_reads(reads: &[RingId], dispatcher: &mut Dispatcher) {
    for read in reads {
        dispatcher.dispatch(IoUringAction::Cancel { target: *read });
    }
}

impl Model for Xwayland {
    type Action = XwaylandAction;
    const START: Option<Self::Action> = Some(XwaylandAction::Start);
    const STOP: Option<Self::Action> = Some(XwaylandAction::Stop);

    fn process(
        &mut self,
        state: &mut State,
        action: Self::Action,
        dispatcher: &mut Dispatcher,
    ) -> std::result::Result<(), crate::runner::ModelFailure> {
        match action {
            XwaylandAction::Start => self.start(state, dispatcher),
            XwaylandAction::ListenReadable { result } => self.on_listen(result, dispatcher),
            XwaylandAction::Process { run, event } => {
                if run != self.run {
                    return Ok(());
                }
                match event {
                    ProcessEvent::Spawned { pid, pidfd } => {
                        self.on_spawned(run, pid, pidfd, dispatcher)
                    }
                    ProcessEvent::Failed { error } => self.on_failed(error, dispatcher),
                    ProcessEvent::Exited { exit } => self.on_exited(exit, dispatcher),
                    ProcessEvent::Lost => self.on_lost(dispatcher),
                }
            }
            XwaylandAction::ClientAdopted { run, id } => self.on_adopted(run, id),
            XwaylandAction::ClientClosed { run, id } => self.on_client_closed(run, id),
            XwaylandAction::DisplayReady { run, read, result } => {
                self.on_display_ready(run, read, result)
            }
            XwaylandAction::Stderr { run, read, result } => {
                self.on_stderr(dispatcher, run, read, result)
            }
            XwaylandAction::Stop => self.stop(dispatcher),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, rc::Rc};

    use {super::*, crate::test_support::state_for_tests};

    fn started() -> (Xwayland, State, Dispatcher) {
        let mut xwayland = Xwayland::new();
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        xwayland
            .process(&mut state, XwaylandAction::Start, &mut dispatcher)
            .unwrap();
        (xwayland, state, dispatcher)
    }

    fn allocated_display(dispatcher: &Dispatcher) -> u32 {
        let mut number = None;
        dispatcher.test_for_each_queued_action::<ProcessManagerAction>(|action| {
            if let ProcessManagerAction::SetEnv { key, value: Some(value) } = action {
                if key == "DISPLAY" {
                    number = value.strip_prefix(':').and_then(|text| text.parse().ok());
                }
            }
        });
        number.expect("DISPLAY was set")
    }

    fn count_spawn(dispatcher: &Dispatcher) -> usize {
        dispatcher.test_count_queued_action::<ProcessManagerAction>(|action| {
            matches!(action, ProcessManagerAction::Spawn(_))
        })
    }

    fn read_lengths(dispatcher: &Dispatcher) -> Vec<usize> {
        let mut lengths = Vec::new();
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            if let IoUringAction::ReadSome { offset, length, .. } = action {
                assert_eq!(*offset, 0);
                lengths.push(*length);
            }
        });
        lengths.sort_unstable();
        lengths
    }

    fn display_paths(number: u32) -> (String, String) {
        (format!("/tmp/.X11-unix/X{number}"), format!("/tmp/.X{number}-lock"))
    }

    #[test]
    fn disabled_config_queues_nothing() {
        let mut xwayland = Xwayland::new();
        let mut state = state_for_tests();
        state.config.xwayland.enabled = false;
        let mut dispatcher = Dispatcher::default();
        xwayland
            .process(&mut state, XwaylandAction::Start, &mut dispatcher)
            .unwrap();
        assert_eq!(dispatcher.test_count_queued_action::<ProcessManagerAction>(|_| true), 0);
        assert_eq!(dispatcher.test_count_queued_action::<IoUringAction>(|_| true), 0);
    }

    #[test]
    fn start_queues_display_and_a_poll() {
        let (xwayland, _state, dispatcher) = started();
        let number = allocated_display(&dispatcher);
        let (socket, lock) = display_paths(number);
        assert!(Path::new(&socket).exists());
        assert!(Path::new(&lock).exists());
        assert_eq!(
            dispatcher.test_count_queued_action::<ProcessManagerAction>(|action| {
                matches!(
                    action,
                    ProcessManagerAction::SetEnv { key, value: Some(_) } if key == "DISPLAY"
                )
            }),
            1
        );
        assert_eq!(
            dispatcher.test_count_queued_action::<IoUringAction>(|action| {
                matches!(action, IoUringAction::Poll { .. })
            }),
            1
        );
        assert_eq!(count_spawn(&dispatcher), 0);
        drop(xwayland);
        assert!(!Path::new(&socket).exists());
        assert!(!Path::new(&lock).exists());
    }

    #[test]
    fn a_connection_removes_the_poll_and_spawns_xwayland() {
        let (mut xwayland, mut state, mut dispatcher) = started();
        xwayland
            .process(
                &mut state,
                XwaylandAction::ListenReadable { result: Ok(PollCompleted {}) },
                &mut dispatcher,
            )
            .unwrap();
        assert_eq!(
            dispatcher.test_count_queued_action::<IoUringAction>(|action| {
                matches!(action, IoUringAction::PollRemove { .. })
            }),
            1
        );
        assert_eq!(count_spawn(&dispatcher), 1);
        dispatcher.test_for_each_queued_action::<ProcessManagerAction>(|action| {
            if let ProcessManagerAction::Spawn(request) = action {
                assert_eq!(request.program, "Xwayland");
                assert_eq!(
                    request.args,
                    [
                        "-terminate",
                        "-rootless",
                        "-verbose",
                        "10",
                        "-displayfd",
                        "3",
                        "-listenfd",
                        "4",
                        "-wm",
                        "5",
                    ]
                    .map(str::to_string)
                );
                assert_eq!(
                    request.env,
                    vec![
                        ("WAYLAND_SOCKET".to_string(), Some("6".to_string())),
                        ("LIBEI_SOCKET".to_string(), None),
                    ]
                );
                assert_eq!(
                    request
                        .fds
                        .iter()
                        .map(|(slot, _)| *slot)
                        .collect::<Vec<_>>(),
                    vec![2, 3, 4, 5, 6]
                );
            }
        });
    }

    #[test]
    fn a_listen_error_does_not_spawn_and_gives_up() {
        let (mut xwayland, mut state, mut dispatcher) = started();
        let number = allocated_display(&dispatcher);
        let (socket, lock) = display_paths(number);
        xwayland
            .process(
                &mut state,
                XwaylandAction::ListenReadable {
                    result: Err(std::io::Error::other("poll failed")),
                },
                &mut dispatcher,
            )
            .unwrap();
        assert_eq!(count_spawn(&dispatcher), 0);
        assert_eq!(
            dispatcher.test_count_queued_action::<ProcessManagerAction>(|action| {
                matches!(
                    action,
                    ProcessManagerAction::SetEnv { key, value: None } if key == "DISPLAY"
                )
            }),
            1
        );
        assert!(!Path::new(&socket).exists());
        assert!(!Path::new(&lock).exists());
    }

    #[test]
    fn spawned_adopts_the_wayland_socket_and_reads_both_pipes() {
        let (mut xwayland, mut state, mut dispatcher) = running();
        assert_eq!(
            dispatcher.test_count_queued_action::<ClientManagerAction>(|action| {
                matches!(
                    action,
                    ClientManagerAction::AdoptClient { origin, .. }
                        if origin.xwayland && origin.pid == Some(42)
                )
            }),
            1
        );
        assert_eq!(read_lengths(&dispatcher), vec![DISPLAYFD_BYTES, STDERR_BYTES]);
        let _ = (xwayland, state);
    }

    #[test]
    fn a_stale_run_is_ignored() {
        let (mut xwayland, mut state, mut dispatcher) = launched();
        let pidfd = Rc::new(std::fs::File::open("/dev/null").unwrap().into());
        xwayland
            .process(
                &mut state,
                XwaylandAction::Process { run: 0, event: ProcessEvent::Spawned { pid: 42, pidfd } },
                &mut dispatcher,
            )
            .unwrap();
        assert_eq!(dispatcher.test_count_queued_action::<ClientManagerAction>(|_| true), 0);
        assert!(read_lengths(&dispatcher).is_empty());
    }

    #[test]
    fn displayfd_marks_ready_without_another_read_and_stderr_is_rearmed() {
        let (mut xwayland, mut state, mut dispatcher) = running();
        let mut reads = Vec::new();
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            if let IoUringAction::ReadSome { id: Some(id), length, .. } = action {
                reads.push((*id, *length));
            }
        });
        let display = reads
            .iter()
            .find(|(_, length)| *length == DISPLAYFD_BYTES)
            .unwrap()
            .0;
        let stderr = reads
            .iter()
            .find(|(_, length)| *length == STDERR_BYTES)
            .unwrap()
            .0;
        xwayland
            .process(
                &mut state,
                XwaylandAction::DisplayReady {
                    run: 1,
                    read: display,
                    result: Ok(ReadWriteCompleted {
                        fd: -1,
                        offset: 0,
                        buffer: b"501\n".to_vec().into_boxed_slice(),
                    }),
                },
                &mut dispatcher,
            )
            .unwrap();
        assert_eq!(read_lengths(&dispatcher), vec![DISPLAYFD_BYTES, STDERR_BYTES]);
        xwayland
            .process(
                &mut state,
                XwaylandAction::Stderr {
                    run: 1,
                    read: stderr,
                    result: Ok(ReadWriteCompleted {
                        fd: -1,
                        offset: 0,
                        buffer: b"hello\n".to_vec().into_boxed_slice(),
                    }),
                },
                &mut dispatcher,
            )
            .unwrap();
        assert_eq!(read_lengths(&dispatcher), vec![DISPLAYFD_BYTES, STDERR_BYTES, STDERR_BYTES]);
        xwayland
            .process(
                &mut state,
                XwaylandAction::Stderr {
                    run: 0,
                    read: stderr,
                    result: Ok(ReadWriteCompleted {
                        fd: -1,
                        offset: 0,
                        buffer: b"stale\n".to_vec().into_boxed_slice(),
                    }),
                },
                &mut dispatcher,
            )
            .unwrap();
        assert_eq!(read_lengths(&dispatcher), vec![DISPLAYFD_BYTES, STDERR_BYTES, STDERR_BYTES]);
    }

    #[test]
    fn a_clean_exit_after_ready_relaunches_on_the_same_display() {
        let (mut xwayland, mut state, mut dispatcher) = running();
        let number = allocated_display(&dispatcher);
        let (socket, lock) = display_paths(number);
        let read = display_read(&dispatcher);
        xwayland
            .process(
                &mut state,
                XwaylandAction::DisplayReady {
                    run: 1,
                    read,
                    result: Ok(ReadWriteCompleted {
                        fd: -1,
                        offset: 0,
                        buffer: b"1\n".to_vec().into_boxed_slice(),
                    }),
                },
                &mut dispatcher,
            )
            .unwrap();
        xwayland
            .process(
                &mut state,
                XwaylandAction::Process {
                    run: 1,
                    event: ProcessEvent::Exited { exit: ChildExit::Code(0) },
                },
                &mut dispatcher,
            )
            .unwrap();
        assert_eq!(
            count_kind::<IoUringAction>(&dispatcher, |action| {
                matches!(action, IoUringAction::Poll { .. })
            }),
            2
        );
        assert_eq!(
            count_kind::<IoUringAction>(&dispatcher, |action| {
                matches!(action, IoUringAction::Cancel { .. })
            }),
            1
        );
        assert_eq!(count_unset_display(&dispatcher), 0);
        assert!(Path::new(&socket).exists());
        assert!(Path::new(&lock).exists());
    }

    #[test]
    fn three_spawn_failures_give_up() {
        let (mut xwayland, mut state, mut dispatcher) = started();
        let number = allocated_display(&dispatcher);
        let (socket, lock) = display_paths(number);
        for run in 1..=3 {
            xwayland
                .process(
                    &mut state,
                    XwaylandAction::ListenReadable { result: Ok(PollCompleted {}) },
                    &mut dispatcher,
                )
                .unwrap();
            xwayland
                .process(
                    &mut state,
                    XwaylandAction::Process {
                        run,
                        event: ProcessEvent::Failed { error: "spawn failed".to_string() },
                    },
                    &mut dispatcher,
                )
                .unwrap();
        }
        assert_eq!(count_unset_display(&dispatcher), 1);
        assert!(!Path::new(&socket).exists());
        assert!(!Path::new(&lock).exists());
    }

    #[test]
    fn a_lost_forker_gives_up() {
        let (mut xwayland, mut state, mut dispatcher) = running();
        let number = allocated_display(&dispatcher);
        let (socket, lock) = display_paths(number);
        xwayland
            .process(
                &mut state,
                XwaylandAction::Process { run: 1, event: ProcessEvent::Lost },
                &mut dispatcher,
            )
            .unwrap();
        assert_eq!(count_unset_display(&dispatcher), 1);
        assert!(!Path::new(&socket).exists());
        assert!(!Path::new(&lock).exists());
    }

    #[test]
    fn closing_the_client_kills_the_current_run() {
        let _lock = crate::process::lock_forks();
        let (pidfd, _reap) = sleeping_child();
        let (mut xwayland, mut state, mut dispatcher) = running_with(Rc::clone(&pidfd));
        xwayland
            .process(
                &mut state,
                XwaylandAction::ClientClosed { run: 0, id: ClientId::from_raw(1) },
                &mut dispatcher,
            )
            .unwrap();
        assert!(!pidfd_readable(pidfd.as_ref(), 0));
        xwayland
            .process(
                &mut state,
                XwaylandAction::ClientClosed { run: 1, id: ClientId::from_raw(1) },
                &mut dispatcher,
            )
            .unwrap();
        assert!(pidfd_readable(pidfd.as_ref(), 5_000));
    }

    #[test]
    fn stop_unlinks_the_files_without_clearing_display() {
        let (mut xwayland, mut state, mut dispatcher) = started();
        let number = allocated_display(&dispatcher);
        let (socket, lock) = display_paths(number);
        xwayland
            .process(&mut state, XwaylandAction::Stop, &mut dispatcher)
            .unwrap();
        assert!(!Path::new(&socket).exists());
        assert!(!Path::new(&lock).exists());
        assert_eq!(count_unset_display(&dispatcher), 0);
        assert_eq!(
            count_kind::<IoUringAction>(&dispatcher, |action| {
                matches!(action, IoUringAction::PollRemove { .. })
            }),
            1
        );
    }

    #[test]
    fn stop_kills_a_running_server() {
        let _lock = crate::process::lock_forks();
        let (pidfd, _reap) = sleeping_child();
        let (mut xwayland, mut state, mut dispatcher) = running_with(Rc::clone(&pidfd));
        let number = allocated_display(&dispatcher);
        let (socket, lock) = display_paths(number);
        xwayland
            .process(&mut state, XwaylandAction::Stop, &mut dispatcher)
            .unwrap();
        assert!(pidfd_readable(pidfd.as_ref(), 5_000));
        assert!(!Path::new(&socket).exists());
        assert!(!Path::new(&lock).exists());
        assert_eq!(count_unset_display(&dispatcher), 0);
        assert_eq!(
            count_kind::<IoUringAction>(&dispatcher, |action| {
                matches!(action, IoUringAction::Cancel { .. })
            }),
            2
        );
    }

    fn launched() -> (Xwayland, State, Dispatcher) {
        let (mut xwayland, mut state, mut dispatcher) = started();
        xwayland
            .process(
                &mut state,
                XwaylandAction::ListenReadable { result: Ok(PollCompleted {}) },
                &mut dispatcher,
            )
            .unwrap();
        (xwayland, state, dispatcher)
    }

    fn running() -> (Xwayland, State, Dispatcher) {
        let (mut xwayland, mut state, mut dispatcher) = launched();
        let pidfd = Rc::new(std::fs::File::open("/dev/null").unwrap().into());
        xwayland
            .process(
                &mut state,
                XwaylandAction::Process { run: 1, event: ProcessEvent::Spawned { pid: 42, pidfd } },
                &mut dispatcher,
            )
            .unwrap();
        (xwayland, state, dispatcher)
    }

    fn running_with(pidfd: Rc<OwnedFd>) -> (Xwayland, State, Dispatcher) {
        let (mut xwayland, mut state, mut dispatcher) = launched();
        xwayland
            .process(
                &mut state,
                XwaylandAction::Process { run: 1, event: ProcessEvent::Spawned { pid: 42, pidfd } },
                &mut dispatcher,
            )
            .unwrap();
        (xwayland, state, dispatcher)
    }

    fn display_read(dispatcher: &Dispatcher) -> RingId {
        let mut found = None;
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            if let IoUringAction::ReadSome { id: Some(id), length, .. } = action {
                if *length == DISPLAYFD_BYTES {
                    found = Some(*id);
                }
            }
        });
        found.expect("displayfd read")
    }

    fn count_unset_display(dispatcher: &Dispatcher) -> usize {
        dispatcher.test_count_queued_action::<ProcessManagerAction>(|action| {
            matches!(action, ProcessManagerAction::SetEnv { key, value: None } if key == "DISPLAY")
        })
    }

    fn count_kind<T: 'static>(dispatcher: &Dispatcher, predicate: impl Fn(&T) -> bool) -> usize {
        dispatcher.test_count_queued_action(predicate)
    }

    fn sleeping_child() -> (Rc<OwnedFd>, Reap) {
        let pid = match unsafe { nix::unistd::fork() }.expect("fork") {
            nix::unistd::ForkResult::Parent { child } => child,
            nix::unistd::ForkResult::Child => {
                nix::unistd::sleep(30);
                unsafe { libc::_exit(0) }
            }
        };
        let pidfd = crate::process::pidfd_open(pid.as_raw()).expect("pidfd_open");
        let reap = Reap(pidfd.try_clone().expect("dup pidfd"));
        (Rc::new(pidfd), reap)
    }

    fn pidfd_readable(pidfd: &OwnedFd, timeout_ms: u16) -> bool {
        use std::os::fd::AsFd;
        let mut polls = [nix::poll::PollFd::new(pidfd.as_fd(), nix::poll::PollFlags::POLLIN)];
        nix::poll::poll(&mut polls, timeout_ms).expect("poll") > 0
    }

    /// Kills and collects the child if the test fails before it exits.
    struct Reap(OwnedFd);

    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = pidfd_send_signal(&self.0, libc::SIGKILL);
            let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(self.0.as_raw_fd()) };
            let _ = nix::sys::wait::waitid(
                nix::sys::wait::Id::PIDFd(borrowed),
                nix::sys::wait::WaitPidFlag::WEXITED,
            );
        }
    }
}
