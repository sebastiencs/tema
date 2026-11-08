//! Which activation-environment updates to run, and in what order.
//!
//! The planner only records jobs. The process manager runs them one at a time.
//! A disabled planner drops every request.

use std::collections::{BTreeMap, VecDeque};

use crate::runner::Redispatcher;

use super::{ProcessEvent, SpawnRequest};

/// `sh -c` body. `$0` is `tema-activation` and `$@` is the `K=V` arguments.
pub(crate) const SET_SCRIPT: &str = "if command -v dbus-update-activation-environment >/dev/null 2>&1; then exec dbus-update-activation-environment --systemd \"$@\"; else exec systemctl --user set-environment \"$@\"; fi";

const SET_ARGV0: &str = "tema-activation";

/// One helper invocation. Pairs are `K=V` arguments, each pair one argument.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Job {
    Set(Vec<(String, String)>),
    Unset(Vec<(String, String)>),
}

/// The forker spawn for one [`Job`]. Values travel as arguments, so a space in
/// a value stays inside that argument.
pub(crate) fn job_request(job: &Job, notify: Redispatcher<ProcessEvent>) -> SpawnRequest {
    let (program, mut args) = match job {
        Job::Set(_) => (
            "sh".to_string(),
            vec!["-c".to_string(), SET_SCRIPT.to_string(), SET_ARGV0.to_string()],
        ),
        Job::Unset(_) => {
            ("systemctl".to_string(), vec!["--user".to_string(), "unset-environment".to_string()])
        }
    };
    args.extend(
        pairs(job)
            .iter()
            .map(|(key, value)| format!("{key}={value}")),
    );
    SpawnRequest {
        program,
        args,
        env: Vec::new(),
        fds: Vec::new(),
        notify: Some(notify),
    }
}

fn pairs(job: &Job) -> &[(String, String)] {
    match job {
        Job::Set(pairs) | Job::Unset(pairs) => pairs,
    }
}

/// Exported variables and the helper jobs still waiting to run.
pub(crate) struct Activation {
    enabled: bool,
    exported: BTreeMap<String, String>,
    queue: VecDeque<Job>,
    running: bool,
}

impl Activation {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            exported: BTreeMap::new(),
            queue: VecDeque::new(),
            running: false,
        }
    }

    /// Record `vars` and queue one `Set` for the pairs that changed.
    ///
    /// A key already exported with the same value is left out. A key repeated
    /// in this call keeps its last value.
    pub(crate) fn export(&mut self, vars: Vec<(String, String)>) {
        if !self.enabled {
            return;
        }
        let mut changed = Vec::new();
        for (key, value) in vars {
            if self
                .exported
                .get(&key)
                .is_some_and(|current| current == &value)
            {
                continue;
            }
            if let Some(slot) = changed.iter_mut().find(|(existing, _)| existing == &key) {
                slot.1 = value.clone();
            } else {
                changed.push((key.clone(), value.clone()));
            }
            self.exported.insert(key, value);
        }
        if !changed.is_empty() {
            self.queue.push_back(Job::Set(changed));
        }
    }

    /// Queue one value-matched `Unset` for the keys tema exported, and forget them.
    pub(crate) fn unexport(&mut self, keys: &[&str]) {
        if !self.enabled {
            return;
        }
        let mut pairs = Vec::new();
        for key in keys {
            if let Some(value) = self.exported.remove(*key) {
                pairs.push(((*key).to_string(), value));
            }
        }
        if !pairs.is_empty() {
            self.queue.push_back(Job::Unset(pairs));
        }
    }

    /// The next job, once the previous one has finished.
    pub(crate) fn next_job(&mut self) -> Option<Job> {
        if !self.enabled || self.running {
            return None;
        }
        let job = self.queue.pop_front()?;
        self.running = true;
        Some(job)
    }

    pub(crate) fn job_finished(&mut self) {
        if !self.enabled {
            return;
        }
        self.running = false;
    }

    /// One `Unset` for everything still exported. Pending jobs are dropped.
    ///
    /// Keys come out in sorted order.
    pub(crate) fn final_unset(&mut self) -> Option<Job> {
        if !self.enabled || self.exported.is_empty() {
            self.queue.clear();
            self.running = false;
            return None;
        }
        let pairs = self
            .exported
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        self.exported.clear();
        self.queue.clear();
        self.running = false;
        Some(Job::Unset(pairs))
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{job_request, Activation, Job, SET_SCRIPT},
        crate::runner::Redispatcher,
    };

    fn notify() -> Redispatcher<crate::process::ProcessEvent> {
        Redispatcher::None
    }

    fn display() -> (String, String) {
        ("DISPLAY".to_string(), ":500".to_string())
    }

    #[test]
    fn a_set_passes_each_pair_as_one_argument() {
        let request = job_request(&Job::Set(vec![display()]), notify());
        assert_eq!(request.program, "sh");
        assert_eq!(
            request.args,
            vec![
                "-c".to_string(),
                SET_SCRIPT.to_string(),
                "tema-activation".to_string(),
                "DISPLAY=:500".to_string()
            ]
        );
        assert!(request.env.is_empty());
        assert!(request.fds.is_empty());
    }

    #[test]
    fn an_unset_is_systemctl_with_the_exported_value() {
        let request = job_request(&Job::Unset(vec![display()]), notify());
        assert_eq!(request.program, "systemctl");
        assert_eq!(
            request.args,
            vec!["--user".to_string(), "unset-environment".to_string(), "DISPLAY=:500".to_string()]
        );
    }

    #[test]
    fn a_value_with_spaces_stays_one_argument() {
        let request =
            job_request(&Job::Set(vec![("DISPLAY".to_string(), "foo bar".to_string())]), notify());
        assert_eq!(request.args.last().map(String::as_str), Some("DISPLAY=foo bar"));
    }

    #[test]
    fn the_same_value_queues_one_job_and_a_new_value_queues_another() {
        let mut activation = Activation::new(true);
        activation.export(vec![display()]);
        activation.export(vec![display(), ("WAYLAND_DISPLAY".to_string(), "tema-0".to_string())]);
        assert_eq!(activation.next_job(), Some(Job::Set(vec![display()])));
        activation.job_finished();
        assert_eq!(
            activation.next_job(),
            Some(Job::Set(vec![("WAYLAND_DISPLAY".to_string(), "tema-0".to_string())]))
        );
    }

    #[test]
    fn an_unknown_key_queues_no_unset() {
        let mut activation = Activation::new(true);
        activation.export(vec![display()]);
        let _ = activation.next_job();
        activation.unexport(&["WAYLAND_DISPLAY"]);
        activation.job_finished();
        assert_eq!(activation.next_job(), None);
    }

    #[test]
    fn jobs_come_out_one_at_a_time() {
        let mut activation = Activation::new(true);
        activation.export(vec![display()]);
        activation.export(vec![("WAYLAND_DISPLAY".to_string(), "tema-0".to_string())]);
        assert!(activation.next_job().is_some());
        assert_eq!(activation.next_job(), None);
        activation.job_finished();
        assert!(activation.next_job().is_some());
        assert_eq!(activation.next_job(), None);
    }

    #[test]
    fn final_unset_lists_every_exported_pair_and_empties_the_state() {
        let mut activation = Activation::new(true);
        activation.export(vec![("WAYLAND_DISPLAY".to_string(), "tema-0".to_string()), display()]);
        let _ = activation.next_job();
        assert_eq!(
            activation.final_unset(),
            Some(Job::Unset(vec![
                display(),
                ("WAYLAND_DISPLAY".to_string(), "tema-0".to_string()),
            ]))
        );
        assert_eq!(activation.next_job(), None);
        activation.unexport(&["DISPLAY", "WAYLAND_DISPLAY"]);
        assert_eq!(activation.next_job(), None);
        activation.export(vec![display()]);
        assert_eq!(activation.next_job(), Some(Job::Set(vec![display()])));
    }

    #[test]
    fn a_disabled_activation_does_nothing() {
        let mut activation = Activation::new(false);
        activation.export(vec![display()]);
        activation.unexport(&["DISPLAY"]);
        activation.job_finished();
        assert_eq!(activation.next_job(), None);
        assert_eq!(activation.final_unset(), None);
    }
}
