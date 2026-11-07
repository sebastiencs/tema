//! When an Xwayland exit should start another one, and when to give up.
//!
//! A run fails early when it never becomes ready, or when it exits with a
//! non-zero code or a signal in under ten seconds. Three early failures in a
//! row give up. Any other run clears the count and launches again.

use std::time::Duration;

use crate::process::ChildExit;

const MAX_EARLY_FAILURES: u32 = 3;
const EARLY: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
pub(crate) struct RestartPolicy {
    failures: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Relaunch,
    GiveUp,
}

impl RestartPolicy {
    pub(crate) fn on_exit(
        &mut self,
        ready: bool,
        exit: Option<ChildExit>,
        ran_for: Duration,
    ) -> Decision {
        if early_failure(ready, exit, ran_for) {
            self.failures += 1;
            if self.failures >= MAX_EARLY_FAILURES {
                Decision::GiveUp
            } else {
                Decision::Relaunch
            }
        } else {
            self.failures = 0;
            Decision::Relaunch
        }
    }
}

fn early_failure(ready: bool, exit: Option<ChildExit>, ran_for: Duration) -> bool {
    if !ready {
        return true;
    }
    if ran_for >= EARLY {
        return false;
    }
    match exit {
        Some(ChildExit::Code(0)) => false,
        Some(ChildExit::Code(_)) | Some(ChildExit::Signal(_)) => true,
        // A spawn that never started is reported with `ready == false`.
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decide(
        start: u32,
        ready: bool,
        exit: Option<ChildExit>,
        ran_for: Duration,
    ) -> (Decision, u32) {
        let mut policy = RestartPolicy { failures: start };
        let decision = policy.on_exit(ready, exit, ran_for);
        (decision, policy.failures)
    }

    #[test]
    fn early_failures_give_up_on_the_third_and_a_good_run_resets() {
        let sec = Duration::from_secs;
        let cases = [
            // Spawn failures and a process that never became ready.
            (0, false, None, sec(0), Decision::Relaunch, 1),
            (1, false, Some(ChildExit::Code(0)), sec(30), Decision::Relaunch, 2),
            (2, false, Some(ChildExit::Signal(9)), sec(0), Decision::GiveUp, 3),
            // Ready, but a bad exit inside the window.
            (2, true, Some(ChildExit::Code(1)), sec(9), Decision::GiveUp, 3),
            (2, true, Some(ChildExit::Signal(15)), sec(9), Decision::GiveUp, 3),
            // The same exits at ten seconds, and a clean exit, clear the count.
            (2, true, Some(ChildExit::Code(1)), sec(10), Decision::Relaunch, 0),
            (2, true, Some(ChildExit::Signal(9)), sec(10), Decision::Relaunch, 0),
            (2, true, Some(ChildExit::Code(0)), sec(1), Decision::Relaunch, 0),
            (2, true, Some(ChildExit::Code(0)), sec(30), Decision::Relaunch, 0),
        ];

        for (start, ready, exit, ran_for, decision, failures) in cases {
            assert_eq!(
                decide(start, ready, exit, ran_for),
                (decision, failures),
                "start {start}, ready {ready}, exit {exit:?}, ran {ran_for:?}"
            );
        }

        let mut policy = RestartPolicy::default();
        assert_eq!(policy.on_exit(false, None, sec(0)), Decision::Relaunch);
        assert_eq!(policy.on_exit(false, None, sec(0)), Decision::Relaunch);
        assert_eq!(policy.on_exit(true, Some(ChildExit::Code(0)), sec(1)), Decision::Relaunch);
        assert_eq!(policy.on_exit(false, None, sec(0)), Decision::Relaunch);
        assert_eq!(policy.on_exit(false, None, sec(0)), Decision::Relaunch);
        assert_eq!(policy.failures, 2);
    }
}
