//! Flush a dirty session store five seconds after the last change, and once on shutdown.

use std::{
    os::fd::{AsFd, AsRawFd},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use nix::sys::timerfd::{self, Expiration, TimerFd, TimerFlags, TimerSetTimeFlags};

use crate::{
    io_uring::{model::IoUringAction, ring::RingId},
    runner::{Dispatcher, Model, ModelFailure, Redispatcher},
    state::State,
};

pub(crate) struct SessionStoreModel {
    fd: TimerFd,
    read: Option<RingId>,
    armed: bool,
}

pub(crate) enum SessionStoreAction {
    Arm,
    Fired,
    Stop,
}

impl SessionStoreModel {
    pub(crate) fn new() -> std::io::Result<Self> {
        let fd = TimerFd::new(
            timerfd::ClockId::CLOCK_MONOTONIC,
            TimerFlags::TFD_CLOEXEC | TimerFlags::TFD_NONBLOCK,
        )?;
        Ok(Self { fd, read: None, armed: false })
    }
}

impl Model for SessionStoreModel {
    type Action = SessionStoreAction;

    const STOP: Option<Self::Action> = Some(SessionStoreAction::Stop);

    fn process(
        &mut self,
        state: &mut State,
        action: Self::Action,
        dispatcher: &mut Dispatcher,
    ) -> std::result::Result<(), ModelFailure> {
        match action {
            SessionStoreAction::Arm => {
                if !self.armed {
                    self.armed = true;
                    let expiration = Expiration::OneShot(Duration::from_secs(5).into());
                    if let Err(error) = self.fd.set(expiration, TimerSetTimeFlags::empty()) {
                        log::warn!("failed to arm session store timer: {error}");
                    }
                    if self.read.is_none() {
                        let id = RingId::fresh();
                        self.read = Some(id);
                        dispatcher.dispatch(IoUringAction::Read {
                            id: Some(id),
                            fd: self.fd.as_fd().as_raw_fd(),
                            offset: 0,
                            length: std::mem::size_of::<u64>(),
                            dispatch_to: Redispatcher::to::<SessionStoreModel>(|_| {
                                SessionStoreAction::Fired
                            }),
                        });
                    }
                }
            }
            SessionStoreAction::Fired => {
                self.read = None;
                if self.armed {
                    self.armed = false;
                    save_now(state);
                }
            }
            SessionStoreAction::Stop => {
                self.armed = false;
                let _ = self.fd.unset();
                if let Some(id) = self.read.take() {
                    dispatcher.dispatch(IoUringAction::Cancel { target: id });
                }
                save_now(state);
            }
        }
        Ok(())
    }
}

pub(crate) fn save_now(state: &mut State) {
    let Some(store) = state.session_store.as_mut() else {
        return;
    };
    if !store.is_dirty() {
        return;
    }
    if let Err(error) = store.save(unix_now()) {
        log::warn!("failed to save session store: {error}");
    }
}

pub(crate) fn note_dirty(state: &State, dispatcher: &mut Dispatcher) {
    if state
        .session_store
        .as_ref()
        .is_some_and(|store| store.is_dirty())
    {
        dispatcher.dispatch(SessionStoreAction::Arm);
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{io_uring::model::IoUringAction, test_support::state_for_tests},
    };

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new() -> Self {
            let output = std::process::Command::new("mktemp")
                .arg("-d")
                .output()
                .unwrap();
            assert!(output.status.success(), "mktemp failed");
            Self(std::path::PathBuf::from(String::from_utf8(output.stdout).unwrap().trim()))
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn arm_twice_reads_once_and_stop_saves_a_dirty_store() {
        let dir = TempDir::new();
        let path = dir.0.join("sessions.json");
        let mut store = crate::session_store::SessionStore::load(path.clone());
        let now = unix_now();
        store.touch_session("session", now);
        let mut state = state_for_tests();
        state.session_store = Some(store);
        let mut model = SessionStoreModel::new().unwrap();
        let mut dispatcher = Dispatcher::default();

        model
            .process(&mut state, SessionStoreAction::Arm, &mut dispatcher)
            .unwrap();
        model
            .process(&mut state, SessionStoreAction::Arm, &mut dispatcher)
            .unwrap();
        assert_eq!(
            dispatcher.test_count_queued_action(|action: &IoUringAction| {
                matches!(action, IoUringAction::Read { length: 8, .. })
            }),
            1
        );

        model
            .process(&mut state, SessionStoreAction::Stop, &mut dispatcher)
            .unwrap();
        assert!(dispatcher.test_has_queued_action(|action: &IoUringAction| {
            matches!(action, IoUringAction::Cancel { .. })
        }));
        let loaded = crate::session_store::SessionStore::load(path);
        assert!(loaded.has_session("session"));
        assert!(!state.session_store.as_ref().unwrap().is_dirty());
    }
}
