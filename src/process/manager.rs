//! Compositor side of the forker socket.
//!
//! One send is in flight at a time. io_uring completions can arrive out of
//! submission order, and a `SetEnv` has to reach the forker before a later
//! `Spawn`. The forker pidfd is polled with a multishot poll, which stays
//! readable after the process dies, so the first completion removes it.

use std::{
    collections::{BTreeMap, VecDeque},
    os::fd::{AsRawFd, OwnedFd},
    rc::Rc,
};

use crate::{
    io_uring::{
        model::IoUringAction,
        ring::{PollCompleted, RecvmsgCompleted, RingId, SendmsgCompleted},
    },
    runner::{Dispatcher, Model, Redispatcher},
    state::State,
};

use super::{
    activation::{job_request, Activation},
    decode, encode, pidfd_send_signal, send_now, ChildExit, ForkerHandle, FromForker, ToForker,
};

/// What happened to one spawn.
#[derive(Debug)]
pub(crate) enum ProcessEvent {
    Spawned {
        pid: i32,
        pidfd: Rc<OwnedFd>,
    },
    Failed {
        error: String,
    },
    Exited {
        exit: ChildExit,
    },
    /// The forker died while this child was running, so its exit is unknown.
    Lost,
}

/// One program to run. `args` does not include `argv[0]`.
#[derive(Debug)]
pub(crate) struct SpawnRequest {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) env: Vec<(String, Option<String>)>,
    /// `(child slot, fd)`. The fd is kept alive until the send completes.
    pub(crate) fds: Vec<(i32, Rc<OwnedFd>)>,
    pub(crate) notify: Option<Redispatcher<ProcessEvent>>,
}

pub(crate) enum ProcessManagerAction {
    Spawn(SpawnRequest),
    SetEnv {
        key: String,
        value: Option<String>,
    },
    Start,
    Received(std::io::Result<RecvmsgCompleted>),
    Sent(std::io::Result<SendmsgCompleted>),
    ForkerDied,
    Stop,
    /// A spawn notification was logged. Nothing follows it.
    Logged,
    /// Record variables and queue one activation `Set`.
    ExportEnv {
        vars: Vec<(String, String)>,
    },
    /// Queue one value-matched activation `Unset` for keys this process exported.
    UnexportEnv {
        keys: Vec<String>,
    },
    /// The activation helper reported this event.
    ActivationJob {
        event: ProcessEvent,
    },
}

struct Outgoing {
    bytes: Box<[u8]>,
    fds: Vec<Rc<OwnedFd>>,
}

struct Child {
    notify: Option<Redispatcher<ProcessEvent>>,
    running: bool,
}

pub(crate) struct ProcessManager {
    handle: Option<ForkerHandle>,
    poll_op_id: Option<RingId>,
    send_in_flight: bool,
    outgoing: VecDeque<Outgoing>,
    /// Tokens start at 1.
    next_token: u64,
    children: BTreeMap<u64, Child>,
    activation: Activation,
    /// Pidfd of the activation helper that is running, if it has spawned.
    activation_pidfd: Option<Rc<OwnedFd>>,
}

impl ProcessManager {
    pub(crate) fn new(forker: Option<ForkerHandle>, export_environment: bool) -> Self {
        Self {
            handle: forker,
            poll_op_id: None,
            send_in_flight: false,
            outgoing: VecDeque::new(),
            next_token: 1,
            children: BTreeMap::new(),
            activation: Activation::new(export_environment),
            activation_pidfd: None,
        }
    }

    fn start(&mut self, dispatcher: &mut Dispatcher) {
        let Some(handle) = &self.handle else { return };
        let pidfd = handle.pidfd.as_raw_fd();
        let poll_op_id = RingId::fresh();
        self.poll_op_id = Some(poll_op_id);
        self.arm_recv(dispatcher);
        dispatcher.dispatch(IoUringAction::Poll {
            id: Some(poll_op_id),
            fd: pidfd,
            dispatch_to: Redispatcher::to::<Self>(|_result: std::io::Result<PollCompleted>| {
                ProcessManagerAction::ForkerDied
            }),
        });
    }

    fn arm_recv(&self, dispatcher: &mut Dispatcher) {
        let Some(socket) = self.handle.as_ref().map(|handle| handle.socket.as_raw_fd()) else {
            return;
        };
        dispatcher.dispatch(IoUringAction::Recvmsg {
            id: None,
            fd: socket,
            dispatch_to: Redispatcher::to::<Self>(ProcessManagerAction::Received),
        });
    }

    fn spawn(&mut self, request: SpawnRequest, dispatcher: &mut Dispatcher) {
        if self.handle.is_none() {
            log::warn!("spawn failed: forker is unavailable");
            self.notify(
                request.notify,
                ProcessEvent::Failed { error: "forker is unavailable".into() },
                dispatcher,
            );
            return;
        }
        let token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1);
        let (slots, fds): (Vec<i32>, Vec<Rc<OwnedFd>>) = request.fds.into_iter().unzip();
        let message = ToForker::Spawn {
            token,
            program: request.program.into_bytes(),
            args: request.args.into_iter().map(String::into_bytes).collect(),
            env: request
                .env
                .into_iter()
                .map(|(key, value)| (key.into_bytes(), value.map(String::into_bytes)))
                .collect(),
            slots,
        };
        if self.enqueue(&message, fds, dispatcher).is_err() {
            self.notify(
                request.notify,
                ProcessEvent::Failed { error: "spawn request is too large".into() },
                dispatcher,
            );
            return;
        }
        self.children
            .insert(token, Child { notify: request.notify, running: false });
    }

    fn set_env(&mut self, key: String, value: Option<String>, dispatcher: &mut Dispatcher) {
        if self.handle.is_none() {
            log::warn!("setenv failed: forker is unavailable");
            return;
        }
        let message =
            ToForker::SetEnv { key: key.into_bytes(), value: value.map(String::into_bytes) };
        if self.enqueue(&message, Vec::new(), dispatcher).is_err() {
            log::error!("setenv request is too large");
        }
    }

    fn enqueue(
        &mut self,
        message: &ToForker,
        fds: Vec<Rc<OwnedFd>>,
        dispatcher: &mut Dispatcher,
    ) -> std::io::Result<()> {
        let outgoing = Outgoing { bytes: encode(message)?.into_boxed_slice(), fds };
        if self.send_in_flight {
            self.outgoing.push_back(outgoing);
        } else {
            self.submit(outgoing, dispatcher);
        }
        Ok(())
    }

    fn submit(&mut self, outgoing: Outgoing, dispatcher: &mut Dispatcher) {
        let Some(socket) = self.handle.as_ref().map(|handle| handle.socket.as_raw_fd()) else {
            return;
        };
        self.send_in_flight = true;
        dispatcher.dispatch(IoUringAction::Sendmsg {
            fd: socket,
            buffer: outgoing.bytes,
            fds: outgoing.fds,
            dispatch_to: Redispatcher::to::<Self>(ProcessManagerAction::Sent),
        });
    }

    fn sent(&mut self, result: std::io::Result<SendmsgCompleted>, dispatcher: &mut Dispatcher) {
        self.send_in_flight = false;
        if self.handle.is_none() {
            return;
        }
        if let Err(error) = result {
            log::error!("forker send failed: {error}");
            self.died("send failed", dispatcher);
            return;
        }
        if let Some(outgoing) = self.outgoing.pop_front() {
            self.submit(outgoing, dispatcher);
        }
    }

    fn received(&mut self, result: std::io::Result<RecvmsgCompleted>, dispatcher: &mut Dispatcher) {
        if self.handle.is_none() {
            return;
        }
        let completed = match result {
            Ok(completed) => completed,
            Err(error) => {
                self.died(&format!("recv failed: {error}"), dispatcher);
                return;
            }
        };
        // A zero-length seqpacket read is the peer closing.
        if completed.buffer.is_empty() {
            self.died("socket closed", dispatcher);
            return;
        }
        self.handle_message(&completed.buffer, completed.fds, dispatcher);
        self.arm_recv(dispatcher);
    }

    fn handle_message(&mut self, bytes: &[u8], mut fds: Vec<OwnedFd>, dispatcher: &mut Dispatcher) {
        let message = match decode::<FromForker>(bytes) {
            Ok(message) => message,
            Err(error) => {
                log::error!("forker message: {error}");
                return;
            }
        };
        match message {
            FromForker::Log { level, message } => {
                log::log!(log_level(level), "forker: {message}");
            }
            FromForker::Spawned { token, pid } => self.spawned(token, pid, &mut fds, dispatcher),
            FromForker::SpawnFailed { token, error } => {
                self.finish(token, ProcessEvent::Failed { error }, dispatcher);
            }
            FromForker::Exited { token, exit } => {
                self.finish(token, ProcessEvent::Exited { exit }, dispatcher);
            }
        }
    }

    fn spawned(
        &mut self,
        token: u64,
        pid: i32,
        fds: &mut Vec<OwnedFd>,
        dispatcher: &mut Dispatcher,
    ) {
        if !self.children.contains_key(&token) {
            log::error!("forker reported unknown spawn {token}");
            return;
        }
        // The pidfd is the first attached descriptor. Any extras are dropped.
        let Some(pidfd) = fds.drain(..1).next() else {
            log::error!("forker sent no pidfd for spawn {token}");
            let notify = self.children.remove(&token).and_then(|child| child.notify);
            self.notify(
                notify,
                ProcessEvent::Failed { error: "forker sent no pidfd".into() },
                dispatcher,
            );
            return;
        };
        let child = self.children.get_mut(&token).expect("child checked above");
        child.running = true;
        let notify = child.notify.clone();
        self.notify(notify, ProcessEvent::Spawned { pid, pidfd: Rc::new(pidfd) }, dispatcher);
    }

    fn finish(&mut self, token: u64, event: ProcessEvent, dispatcher: &mut Dispatcher) {
        let Some(child) = self.children.remove(&token) else {
            log::error!("forker reported unknown child {token}");
            return;
        };
        self.notify(child.notify, event, dispatcher);
    }

    fn forker_died(&mut self, dispatcher: &mut Dispatcher) {
        self.died("pidfd poll", dispatcher);
    }

    fn stop(&mut self, dispatcher: &mut Dispatcher) {
        if self.handle.is_none() {
            return;
        }
        log::info!("stopping the forker");
        // Kill first, so a helper still applying an export cannot land after the unset.
        self.kill_activation_helper();
        self.send_final_unset();
        self.drop_forker("forker stopped", dispatcher);
    }

    fn kill_activation_helper(&mut self) {
        let Some(pidfd) = self.activation_pidfd.take() else { return };
        if let Err(error) = pidfd_send_signal(&pidfd, libc::SIGKILL) {
            log::warn!("could not kill the activation helper: {error}");
        }
    }

    /// Hand the forker one unset for everything still exported, then return.
    ///
    /// The send is synchronous and non-blocking so the datagram is queued
    /// before [`Self::drop_forker`] closes the socket. No child is tracked:
    /// the forker runs the spawn on its own after the compositor is gone.
    fn send_final_unset(&mut self) {
        let Some(job) = self.activation.final_unset() else { return };
        let Some(socket) = self.handle.as_ref().map(|handle| handle.socket.as_raw_fd()) else {
            return;
        };
        let request = job_request(&job, Redispatcher::None);
        let token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1);
        let message = ToForker::Spawn {
            token,
            program: request.program.into_bytes(),
            args: request.args.into_iter().map(String::into_bytes).collect(),
            env: Vec::new(),
            slots: Vec::new(),
        };
        let bytes = match encode(&message) {
            Ok(bytes) => bytes,
            Err(error) => {
                log::warn!("could not unset the activation environment: {error}");
                return;
            }
        };
        if let Err(error) = send_now(&socket, &bytes) {
            log::warn!("could not unset the activation environment: {error}");
        }
    }

    fn died(&mut self, detail: &str, dispatcher: &mut Dispatcher) {
        if self.handle.is_none() {
            return;
        }
        log::error!("forker died: {detail}");
        self.drop_forker("forker died", dispatcher);
    }

    /// Drop the socket so the forker sees EOF, and tell every waiter.
    fn drop_forker(&mut self, error: &str, dispatcher: &mut Dispatcher) {
        if let Some(poll_op_id) = self.poll_op_id.take() {
            dispatcher.dispatch(IoUringAction::PollRemove { target: poll_op_id });
        }
        self.handle = None;
        self.outgoing.clear();
        self.send_in_flight = false;
        let children = std::mem::take(&mut self.children);
        for (_, child) in children {
            let event = if child.running {
                ProcessEvent::Lost
            } else {
                ProcessEvent::Failed { error: error.to_string() }
            };
            self.notify(child.notify, event, dispatcher);
        }
    }

    /// Start the next activation job when the previous one has finished.
    ///
    /// The spawn joins `outgoing`, so it stays behind a `SetEnv` already queued.
    /// A missing forker fails the job through the spawn path. That queues
    /// `ActivationJob`, which finishes the job and pumps the next one.
    fn pump(&mut self, dispatcher: &mut Dispatcher) {
        let Some(job) = self.activation.next_job() else { return };
        let request = job_request(
            &job,
            Redispatcher::to::<Self>(|event| ProcessManagerAction::ActivationJob { event }),
        );
        self.spawn(request, dispatcher);
    }

    fn activation_job(&mut self, event: ProcessEvent, dispatcher: &mut Dispatcher) {
        match event {
            ProcessEvent::Spawned { pidfd, .. } => {
                self.activation_pidfd = Some(pidfd);
            }
            ProcessEvent::Exited { exit } => {
                if exit != ChildExit::Code(0) {
                    log::debug!("activation helper exited with {exit:?}");
                }
                self.finish_activation_job(dispatcher);
            }
            ProcessEvent::Failed { error } => {
                log::debug!("activation helper failed: {error}");
                self.finish_activation_job(dispatcher);
            }
            ProcessEvent::Lost => self.finish_activation_job(dispatcher),
        }
    }

    fn finish_activation_job(&mut self, dispatcher: &mut Dispatcher) {
        self.activation_pidfd = None;
        self.activation.job_finished();
        self.pump(dispatcher);
    }

    fn notify(
        &self,
        notify: Option<Redispatcher<ProcessEvent>>,
        event: ProcessEvent,
        dispatcher: &mut Dispatcher,
    ) {
        let Some(notify) = notify else { return };
        dispatcher.redispatch_to(notify, event);
    }
}

fn log_level(level: u8) -> log::Level {
    match level {
        1 => log::Level::Error,
        2 => log::Level::Warn,
        3 => log::Level::Info,
        4 => log::Level::Debug,
        5 => log::Level::Trace,
        _ => log::Level::Error,
    }
}

impl Model for ProcessManager {
    type Action = ProcessManagerAction;
    const START: Option<Self::Action> = Some(ProcessManagerAction::Start);
    const STOP: Option<Self::Action> = Some(ProcessManagerAction::Stop);

    fn process(
        &mut self,
        _state: &mut State,
        action: Self::Action,
        dispatcher: &mut Dispatcher,
    ) -> Result<(), crate::runner::ModelFailure> {
        match action {
            ProcessManagerAction::Start => self.start(dispatcher),
            ProcessManagerAction::Spawn(request) => self.spawn(request, dispatcher),
            ProcessManagerAction::SetEnv { key, value } => self.set_env(key, value, dispatcher),
            ProcessManagerAction::Received(result) => self.received(result, dispatcher),
            ProcessManagerAction::Sent(result) => self.sent(result, dispatcher),
            ProcessManagerAction::ForkerDied => self.forker_died(dispatcher),
            ProcessManagerAction::Stop => self.stop(dispatcher),
            ProcessManagerAction::Logged => {}
            ProcessManagerAction::ExportEnv { vars } => {
                self.activation.export(vars);
                self.pump(dispatcher);
            }
            ProcessManagerAction::UnexportEnv { keys } => {
                let keys = keys.iter().map(String::as_str).collect::<Vec<_>>();
                self.activation.unexport(&keys);
                self.pump(dispatcher);
            }
            ProcessManagerAction::ActivationJob { event } => self.activation_job(event, dispatcher),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
        rc::Rc,
    };

    use nix::{
        poll::{poll, PollFd, PollFlags},
        sys::{
            signal::{kill, Signal},
            wait::{waitid, Id, WaitPidFlag},
        },
        unistd::{self, ForkResult, Pid},
    };

    use crate::{
        io_uring::{
            model::{IoUringAction, IoUringState},
            ring::{RecvmsgCompleted, SendmsgCompleted},
        },
        runner::{AnyAction, Dispatcher, Model, Redispatcher, Runner, ShutdownReason},
        state::State,
        test_support::state_for_tests,
    };

    use super::{
        super::{
            decode, encode, pidfd_open, pipe_cloexec, recv_with_fds, socketpair_seqpacket,
            spawn_forker, waitid_pidfd, ChildExit, FromForker, ToForker, MAX_MESSAGE,
        },
        ProcessEvent, ProcessManager, ProcessManagerAction, SpawnRequest,
    };

    fn request(program: &str, notify: Option<Redispatcher<ProcessEvent>>) -> SpawnRequest {
        SpawnRequest {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            fds: Vec::new(),
            notify,
        }
    }

    fn mock() -> ProcessManager {
        let (socket, _peer) = socketpair_seqpacket().unwrap();
        let (pidfd, _write) = pipe_cloexec().unwrap();
        ProcessManager::new(Some(super::ForkerHandle { socket, pidfd, pid: -1 }), true)
    }

    fn recorder() -> (Rc<RefCell<Vec<ProcessEvent>>>, Redispatcher<ProcessEvent>) {
        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded = events.clone();
        let notify = Redispatcher::to_any(move |event| {
            recorded.borrow_mut().push(event);
            AnyAction::empty()
        });
        (events, notify)
    }

    fn sendmsg_programs(dispatcher: &Dispatcher) -> Vec<Vec<u8>> {
        sendmsg_spawns(dispatcher)
            .into_iter()
            .map(|(program, _)| program)
            .collect()
    }

    fn sendmsg_spawns(dispatcher: &Dispatcher) -> Vec<(Vec<u8>, Vec<Vec<u8>>)> {
        let mut spawns = Vec::new();
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            let IoUringAction::Sendmsg { buffer, .. } = action else { return };
            let Ok(ToForker::Spawn { program, args, .. }) = decode::<ToForker>(buffer) else {
                return;
            };
            spawns.push((program, args));
        });
        spawns
    }

    fn sent(manager: &mut ProcessManager, dispatcher: &mut Dispatcher) {
        drive(manager, ProcessManagerAction::Sent(Ok(SendmsgCompleted { fd: 0 })), dispatcher);
    }

    fn drive(
        manager: &mut ProcessManager,
        action: ProcessManagerAction,
        dispatcher: &mut Dispatcher,
    ) {
        manager
            .process(&mut state_for_tests(), action, dispatcher)
            .unwrap();
    }

    #[test]
    fn a_second_spawn_waits_for_the_first_send() {
        let mut manager = mock();
        let mut dispatcher = Dispatcher::default();
        drive(&mut manager, ProcessManagerAction::Start, &mut dispatcher);
        drive(&mut manager, ProcessManagerAction::Spawn(request("one", None)), &mut dispatcher);
        drive(&mut manager, ProcessManagerAction::Spawn(request("two", None)), &mut dispatcher);
        assert_eq!(sendmsg_programs(&dispatcher), vec![b"one".to_vec()]);
        drive(
            &mut manager,
            ProcessManagerAction::Sent(Ok(SendmsgCompleted { fd: 0 })),
            &mut dispatcher,
        );
        assert_eq!(sendmsg_programs(&dispatcher), vec![b"one".to_vec(), b"two".to_vec()]);
    }

    #[test]
    fn a_spawn_without_a_forker_fails() {
        let mut manager = ProcessManager::new(None, true);
        let mut dispatcher = Dispatcher::default();
        let (events, notify) = recorder();
        drive(
            &mut manager,
            ProcessManagerAction::Spawn(request("sh", Some(notify))),
            &mut dispatcher,
        );
        match events.borrow().as_slice() {
            [ProcessEvent::Failed { error }] => assert_eq!(error, "forker is unavailable"),
            other => panic!("unexpected events: {other:?}"),
        };
    }

    #[test]
    fn a_closed_socket_fails_pending_and_loses_running() {
        let mut manager = mock();
        let mut dispatcher = Dispatcher::default();
        let (events, notify) = recorder();
        drive(&mut manager, ProcessManagerAction::Start, &mut dispatcher);
        drive(
            &mut manager,
            ProcessManagerAction::Spawn(request("sh", Some(notify.clone()))),
            &mut dispatcher,
        );
        drive(
            &mut manager,
            ProcessManagerAction::Spawn(request("queued", Some(notify.clone()))),
            &mut dispatcher,
        );

        let spawned = encode(&FromForker::Spawned { token: 1, pid: 42 }).unwrap();
        let (pidfd, _write) = pipe_cloexec().unwrap();
        drive(
            &mut manager,
            ProcessManagerAction::Received(Ok(RecvmsgCompleted {
                fd: 0,
                buffer: spawned.into_boxed_slice(),
                fds: vec![pidfd],
            })),
            &mut dispatcher,
        );
        drive(
            &mut manager,
            ProcessManagerAction::Received(Ok(RecvmsgCompleted {
                fd: 0,
                buffer: Box::new([]),
                fds: Vec::new(),
            })),
            &mut dispatcher,
        );
        drive(
            &mut manager,
            ProcessManagerAction::Spawn(request("later", Some(notify))),
            &mut dispatcher,
        );

        let events = events.borrow();
        assert!(matches!(events[0], ProcessEvent::Spawned { pid: 42, .. }));
        assert!(matches!(events[1], ProcessEvent::Lost));
        match &events[2] {
            ProcessEvent::Failed { error } => assert_eq!(error, "forker died"),
            other => panic!("unexpected event: {other:?}"),
        }
        match &events[3] {
            ProcessEvent::Failed { error } => assert_eq!(error, "forker is unavailable"),
            other => panic!("unexpected event: {other:?}"),
        }
        assert_eq!(
            dispatcher.test_count_queued_action::<IoUringAction>(|action| {
                matches!(action, IoUringAction::PollRemove { .. })
            }),
            1
        );
    }

    #[test]
    fn a_child_exit_is_reported() {
        let _lock = super::super::forker::lock_forks();
        let ring = IoUringState::try_new().expect("io_uring");
        let handle = spawn_forker(None).expect("forker");
        let pid = nix::unistd::Pid::from_raw(handle.pid);

        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded = events.clone();
        let mut runner = Runner::with_state(state_for_tests());
        runner.add_model(Box::new(ring));
        runner.add_model(Box::new(ProcessManager::new(Some(handle), true)));
        runner.add_model(Box::new(Probe { events: recorded }));

        let mut dispatcher = Dispatcher::default();
        dispatcher.start::<ProcessManager>();
        dispatcher.dispatch(ProcessManagerAction::Spawn(SpawnRequest {
            program: "sh".into(),
            args: vec!["-c".into(), "exit 7".into()],
            env: Vec::new(),
            fds: Vec::new(),
            notify: Some(Redispatcher::to::<Probe>(ProbeAction::Event)),
        }));
        let outcome = runner.run(dispatcher);
        drop(runner);
        nix::sys::wait::waitpid(pid, None).expect("reap forker");

        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.reason, ShutdownReason::Quit);
        let events = events.borrow();
        assert!(
            matches!(events[0], ProcessEvent::Spawned { pid, .. } if pid > 0),
            "{:?}",
            events[0]
        );
        assert!(
            matches!(events[1], ProcessEvent::Exited { exit: ChildExit::Code(7) }),
            "{:?}",
            events[1]
        );
    }

    #[test]
    fn an_export_spawns_sh_and_the_next_waits_until_it_exits() {
        let mut manager = mock();
        let mut dispatcher = Dispatcher::default();
        drive(
            &mut manager,
            ProcessManagerAction::ExportEnv { vars: vec![("DISPLAY".into(), ":500".into())] },
            &mut dispatcher,
        );
        let spawns = sendmsg_spawns(&dispatcher);
        assert_eq!(spawns.len(), 1);
        assert_eq!(spawns[0].0, b"sh");
        assert_eq!(spawns[0].1[0], b"-c");
        assert_eq!(spawns[0].1[3], b"DISPLAY=:500");

        sent(&mut manager, &mut dispatcher);
        drive(
            &mut manager,
            ProcessManagerAction::ExportEnv {
                vars: vec![("WAYLAND_DISPLAY".into(), "tema-0".into())],
            },
            &mut dispatcher,
        );
        assert_eq!(sendmsg_spawns(&dispatcher).len(), 1);

        drive(
            &mut manager,
            ProcessManagerAction::ActivationJob {
                event: ProcessEvent::Exited { exit: ChildExit::Code(0) },
            },
            &mut dispatcher,
        );
        let spawns = sendmsg_spawns(&dispatcher);
        assert_eq!(spawns.len(), 2);
        assert_eq!(spawns[1].0, b"sh");
        assert_eq!(spawns[1].1[3], b"WAYLAND_DISPLAY=tema-0");
    }

    #[test]
    fn an_unexport_spawns_systemctl_after_the_export_exits() {
        let mut manager = mock();
        let mut dispatcher = Dispatcher::default();
        drive(
            &mut manager,
            ProcessManagerAction::ExportEnv { vars: vec![("DISPLAY".into(), ":500".into())] },
            &mut dispatcher,
        );
        sent(&mut manager, &mut dispatcher);
        drive(
            &mut manager,
            ProcessManagerAction::UnexportEnv { keys: vec!["DISPLAY".into()] },
            &mut dispatcher,
        );
        assert_eq!(sendmsg_spawns(&dispatcher).len(), 1);

        drive(
            &mut manager,
            ProcessManagerAction::ActivationJob {
                event: ProcessEvent::Exited { exit: ChildExit::Code(0) },
            },
            &mut dispatcher,
        );
        let spawns = sendmsg_spawns(&dispatcher);
        assert_eq!(spawns.len(), 2);
        assert_eq!(spawns[1].0, b"systemctl");
        assert_eq!(
            spawns[1].1,
            vec![b"--user".to_vec(), b"unset-environment".to_vec(), b"DISPLAY=:500".to_vec(),]
        );
    }

    #[test]
    fn a_disabled_activation_queues_nothing() {
        let (socket, _peer) = socketpair_seqpacket().unwrap();
        let (pidfd, _write) = pipe_cloexec().unwrap();
        let mut manager =
            ProcessManager::new(Some(super::ForkerHandle { socket, pidfd, pid: -1 }), false);
        let mut dispatcher = Dispatcher::default();
        drive(
            &mut manager,
            ProcessManagerAction::ExportEnv { vars: vec![("DISPLAY".into(), ":500".into())] },
            &mut dispatcher,
        );
        drive(
            &mut manager,
            ProcessManagerAction::UnexportEnv { keys: vec!["DISPLAY".into()] },
            &mut dispatcher,
        );
        assert!(sendmsg_spawns(&dispatcher).is_empty());
    }

    #[test]
    fn a_failed_helper_does_not_block_the_next_job() {
        let mut manager = mock();
        let mut dispatcher = Dispatcher::default();
        drive(
            &mut manager,
            ProcessManagerAction::ExportEnv { vars: vec![("DISPLAY".into(), ":500".into())] },
            &mut dispatcher,
        );
        sent(&mut manager, &mut dispatcher);
        drive(
            &mut manager,
            ProcessManagerAction::ExportEnv {
                vars: vec![("WAYLAND_DISPLAY".into(), "tema-0".into())],
            },
            &mut dispatcher,
        );
        drive(
            &mut manager,
            ProcessManagerAction::ActivationJob {
                event: ProcessEvent::Failed { error: "spawn failed".into() },
            },
            &mut dispatcher,
        );
        let spawns = sendmsg_spawns(&dispatcher);
        assert_eq!(spawns.len(), 2);
        assert_eq!(spawns[1].0, b"sh");
        assert_eq!(spawns[1].1[3], b"WAYLAND_DISPLAY=tema-0");
    }

    fn paired(export_environment: bool) -> (ProcessManager, OwnedFd) {
        let (socket, peer) = socketpair_seqpacket().unwrap();
        let (pidfd, _write) = pipe_cloexec().unwrap();
        let manager = ProcessManager::new(
            Some(super::ForkerHandle { socket, pidfd, pid: -1 }),
            export_environment,
        );
        (manager, peer)
    }

    /// The next datagram, or `None` when the peer has closed.
    fn peer_message(peer: &OwnedFd) -> Option<ToForker> {
        let mut polls = [PollFd::new(peer.as_fd(), PollFlags::POLLIN)];
        assert!(poll(&mut polls, 1_000u16).expect("poll") > 0, "socket not ready");
        let mut buffer = vec![0u8; MAX_MESSAGE];
        let (len, fds) = recv_with_fds(peer, &mut buffer).unwrap();
        if len == 0 {
            assert!(fds.is_empty());
            return None;
        }
        Some(decode(&buffer[..len]).unwrap())
    }

    #[test]
    fn stop_writes_one_unset_for_everything_exported() {
        let (mut manager, peer) = paired(true);
        let mut dispatcher = Dispatcher::default();
        drive(
            &mut manager,
            ProcessManagerAction::ExportEnv {
                vars: vec![
                    ("WAYLAND_DISPLAY".into(), "tema-0".into()),
                    ("DISPLAY".into(), ":500".into()),
                ],
            },
            &mut dispatcher,
        );
        drive(&mut manager, ProcessManagerAction::Stop, &mut dispatcher);
        match peer_message(&peer) {
            Some(ToForker::Spawn { program, args, env, slots, .. }) => {
                assert_eq!(program, b"systemctl");
                assert_eq!(
                    args,
                    vec![
                        b"--user".to_vec(),
                        b"unset-environment".to_vec(),
                        b"DISPLAY=:500".to_vec(),
                        b"WAYLAND_DISPLAY=tema-0".to_vec(),
                    ]
                );
                assert!(env.is_empty());
                assert!(slots.is_empty());
            }
            other => panic!("unexpected message: {other:?}"),
        }
        assert!(peer_message(&peer).is_none());
    }

    #[test]
    fn stop_writes_nothing_when_nothing_was_exported() {
        let (mut manager, peer) = paired(true);
        let mut dispatcher = Dispatcher::default();
        drive(&mut manager, ProcessManagerAction::Stop, &mut dispatcher);
        assert!(peer_message(&peer).is_none());
    }

    #[test]
    fn stop_writes_nothing_when_export_is_disabled() {
        let (mut manager, peer) = paired(false);
        let mut dispatcher = Dispatcher::default();
        drive(
            &mut manager,
            ProcessManagerAction::ExportEnv { vars: vec![("DISPLAY".into(), ":500".into())] },
            &mut dispatcher,
        );
        drive(&mut manager, ProcessManagerAction::Stop, &mut dispatcher);
        assert!(peer_message(&peer).is_none());
    }

    #[test]
    fn stop_kills_a_running_activation_helper() {
        let _lock = super::super::forker::lock_forks();
        let mut manager = mock();
        let mut dispatcher = Dispatcher::default();
        let ForkResult::Parent { child } = unsafe { unistd::fork() }.expect("fork") else {
            unistd::sleep(30);
            unsafe { libc::_exit(0) };
        };
        let pidfd = pidfd_open(child.as_raw()).expect("pidfd");
        let reap = Reap { pid: child, pidfd: pidfd.try_clone().expect("dup pidfd") };
        drive(
            &mut manager,
            ProcessManagerAction::ActivationJob {
                event: ProcessEvent::Spawned { pid: child.as_raw(), pidfd: Rc::new(pidfd) },
            },
            &mut dispatcher,
        );
        drive(&mut manager, ProcessManagerAction::Stop, &mut dispatcher);
        let mut polls = [PollFd::new(reap.pidfd.as_fd(), PollFlags::POLLIN)];
        assert!(poll(&mut polls, 2_000u16).expect("poll") > 0, "helper still running");
        assert!(polls[0]
            .revents()
            .is_some_and(|events| events.contains(PollFlags::POLLIN)));
        assert_eq!(waitid_pidfd(&reap.pidfd).unwrap(), Some(ChildExit::Signal(libc::SIGKILL)));
    }

    /// Collects `pid` on drop so a failed assertion does not leave a zombie.
    struct Reap {
        pid: Pid,
        pidfd: OwnedFd,
    }

    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = kill(self.pid, Signal::SIGKILL);
            let borrowed = unsafe { BorrowedFd::borrow_raw(self.pidfd.as_raw_fd()) };
            let _ = waitid(Id::PIDFd(borrowed), WaitPidFlag::WEXITED);
        }
    }

    struct Probe {
        events: Rc<RefCell<Vec<ProcessEvent>>>,
    }

    enum ProbeAction {
        Event(ProcessEvent),
        Stop,
    }

    impl Model for Probe {
        type Action = ProbeAction;
        const STOP: Option<Self::Action> = Some(ProbeAction::Stop);

        fn process(
            &mut self,
            _state: &mut State,
            action: Self::Action,
            dispatcher: &mut Dispatcher,
        ) -> Result<(), crate::runner::ModelFailure> {
            match action {
                ProbeAction::Event(event) => {
                    let finished = matches!(
                        event,
                        ProcessEvent::Exited { .. }
                            | ProcessEvent::Failed { .. }
                            | ProcessEvent::Lost
                    );
                    self.events.borrow_mut().push(event);
                    if finished {
                        dispatcher.request_shutdown(ShutdownReason::Quit);
                    }
                }
                ProbeAction::Stop => {}
            }
            Ok(())
        }
    }
}
