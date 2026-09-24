//! Process groups that end when Mework does (macOS and Linux).
//!
//! On Windows the shell tool's commands, dev and language servers, and the
//! managed MCP sidecar each live in a kill-on-close job object, so they die
//! with Mework however it exits. On macOS and Linux each is instead started as
//! the leader of its own process group — so stopping it can reach its whole
//! tree without reaching Mework — and nothing ends such a group when Mework
//! quits: a `run_in_background` dev server started by the agent kept running,
//! and kept its port, after the application was gone. Each group is registered
//! here for as long as its owner holds it, and the application's exit path ends
//! whatever is still registered.
//!
//! Unix only; Windows has the job objects.

use std::{collections::BTreeSet, sync::Mutex};

static GROUPS: Mutex<BTreeSet<i32>> = Mutex::new(BTreeSet::new());

fn groups() -> std::sync::MutexGuard<'static, BTreeSet<i32>> {
    GROUPS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A registered group; dropping it unregisters the group. Owners drop it only
/// after reaping the leader, so an id here never names a reused pid for long.
#[derive(Debug)]
pub(crate) struct GroupRegistration(i32);

impl Drop for GroupRegistration {
    fn drop(&mut self) {
        groups().remove(&self.0);
    }
}

/// Registers the group led by `leader`, a child that made itself a group
/// leader (`setsid` or `setpgid(0, 0)`) before exec. `None` for an id that
/// cannot name a group.
pub(crate) fn register(leader: u32) -> Option<GroupRegistration> {
    let group = i32::try_from(leader).ok().filter(|group| *group > 1)?;
    groups().insert(group);
    Some(GroupRegistration(group))
}

/// Ends every registered group: SIGTERM so servers can release what they hold,
/// then SIGKILL for any group still alive after a short grace. For the
/// application's exit only; the registrations stay with their owners.
pub(crate) fn terminate_all() {
    let registered: Vec<i32> = groups().iter().copied().collect();
    terminate(&registered);
}

fn terminate(registered: &[i32]) {
    const GRACE: std::time::Duration = std::time::Duration::from_millis(300);
    if registered.is_empty() {
        return;
    }
    // SAFETY: `kill` with a negative id only signals the processes of that group.
    let alive = |group: &i32| unsafe { libc::kill(-group, 0) } == 0;
    for group in registered {
        unsafe {
            libc::kill(-group, libc::SIGTERM);
        }
    }
    let deadline = std::time::Instant::now() + GRACE;
    while registered.iter().any(alive) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    for group in registered.iter().filter(|group| alive(group)) {
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    fn leader(script: &str) -> std::process::Child {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]).stdin(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.spawn().unwrap()
    }

    #[test]
    fn exit_ends_registered_groups_and_their_children() {
        let mut polite = leader("sleep 30 & wait");
        let mut stubborn = leader("trap '' TERM; sleep 30 & wait");
        let _polite = register(polite.id()).unwrap();
        let _stubborn = register(stubborn.id()).unwrap();
        // Let the stubborn shell install its trap before the signal arrives.
        std::thread::sleep(std::time::Duration::from_millis(100));
        let groups = [polite.id() as i32, stubborn.id() as i32];

        // Only this test's groups: other tests' commands are registered too.
        terminate(&groups);

        polite.wait().unwrap();
        stubborn.wait().unwrap();
        // The `sleep` children went with their leaders. They were orphaned, so
        // init reaps them on its own time: allow it a moment.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        for group in groups {
            while unsafe { libc::kill(-group, 0) } == 0 && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert_ne!(unsafe { libc::kill(-group, 0) }, 0, "group {group} survived");
        }
    }

    #[test]
    fn a_dropped_registration_is_forgotten() {
        let registration = register(4_000_000).unwrap();
        assert!(groups().contains(&4_000_000));
        drop(registration);
        assert!(!groups().contains(&4_000_000));
        assert!(register(0).is_none());
        assert!(register(1).is_none());
    }
}
