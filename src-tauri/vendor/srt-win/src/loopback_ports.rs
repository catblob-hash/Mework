//! Mework: which loopback ports the WFP fence lets the sandbox user reach
//! (`srt-win wfp ports`), found by trying them as that user.
//!
//! The range is chosen at install (`--proxy-port-range`) and recorded only in
//! the filters' tags, which a non-elevated caller cannot enumerate. The
//! install is machine-wide and shared, and whichever program made it — or
//! replaced it with `--force` — may have chosen a range of its own, so a
//! program that runs its proxy for the sandbox asks here rather than
//! assuming the default.
//!
//! Each port is tried by listening on it and connecting to it: the fence
//! refuses the connect with WSAEACCES at `ALE_AUTH_CONNECT`, before any
//! packet, and otherwise it completes at once. The search starts in the
//! default range, where an install normally has it, and walks outward from
//! the first port that connects; only when none there does is every other
//! port tried.

use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use crate::wfp::DEFAULT_PROXY_PORT_RANGE;

/// What the fence did with one connect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Refused by the fence.
    Blocked,
    /// Let through: connected, or refused or still pending past the timeout
    /// — past the fence either way.
    Reached,
    /// Some other failure, which says nothing about the fence.
    Unknown,
}

/// What the search found.
#[derive(Debug, PartialEq, Eq)]
pub enum Found {
    /// `lo..=hi` connect, and the fence refuses other ports.
    Permitted(u16, u16),
    /// No port was refused, or more connect than any fence permits: the
    /// fence is not in effect.
    Unfenced,
    /// No port connects.
    Nothing,
}

/// A run of ports that connect longer than this is no fence. srt-win caps
/// a range at 65 ports; the rest is room for an install by other tooling.
pub const MAX_RUN: u32 = 1024;

/// Threads trying ports at once when the whole port space is searched.
const THREADS: u16 = 16;

/// Searches with `reach`, which tries one port.
pub fn search(reach: impl Fn(u16) -> Reach + Sync) -> Found {
    let (low, high) = DEFAULT_PROXY_PORT_RANGE;
    let mut blocked = false;
    let mut seed = None;
    for port in low..=high {
        match reach(port) {
            Reach::Reached => {
                seed = Some(port);
                break;
            }
            Reach::Blocked => blocked = true,
            Reach::Unknown => {}
        }
    }
    let seed = match seed {
        Some(port) => port,
        None => {
            let (found, refused) = search_all(&reach, low, high);
            blocked |= refused;
            match found {
                Some(port) => port,
                None if blocked => return Found::Nothing,
                None => return Found::Unfenced,
            }
        }
    };
    let mut run = (seed, seed);
    for down in [true, false] {
        loop {
            let next = if down {
                run.0.checked_sub(1).filter(|port| *port > 0)
            } else {
                run.1.checked_add(1)
            };
            let Some(next) = next else { break };
            match reach(next) {
                Reach::Reached => {
                    if down {
                        run.0 = next;
                    } else {
                        run.1 = next;
                    }
                    if u32::from(run.1 - run.0) + 1 > MAX_RUN {
                        return Found::Unfenced;
                    }
                }
                Reach::Blocked => {
                    blocked = true;
                    break;
                }
                Reach::Unknown => break,
            }
        }
    }
    if !blocked {
        // The run ended on ports that failed otherwise; one a little
        // further out shows the fence.
        blocked = (2..=17u16)
            .flat_map(|step| [run.0.checked_sub(step), run.1.checked_add(step)])
            .flatten()
            .filter(|port| *port > 0)
            .any(|port| reach(port) == Reach::Blocked);
    }
    if blocked {
        Found::Permitted(run.0, run.1)
    } else {
        Found::Unfenced
    }
}

/// Every port but `skip_low..=skip_high`, from the top, where ranges are
/// chosen, down, and a thread per residue so a range is shared among them,
/// until one connects: that port, and whether any was refused.
fn search_all(reach: &(impl Fn(u16) -> Reach + Sync), skip_low: u16, skip_high: u16) -> (Option<u16>, bool) {
    let found = AtomicU32::new(0);
    let blocked = AtomicBool::new(false);
    std::thread::scope(|scope| {
        for offset in 0..THREADS {
            let (found, blocked) = (&found, &blocked);
            scope.spawn(move || {
                let mut port = u16::MAX - offset;
                loop {
                    if found.load(Ordering::Relaxed) != 0 {
                        return;
                    }
                    if !(skip_low..=skip_high).contains(&port) {
                        match reach(port) {
                            Reach::Reached => {
                                let _ = found.compare_exchange(0, u32::from(port), Ordering::Relaxed, Ordering::Relaxed);
                                return;
                            }
                            Reach::Blocked => blocked.store(true, Ordering::Relaxed),
                            Reach::Unknown => {}
                        }
                    }
                    match port.checked_sub(THREADS).filter(|next| *next > 0) {
                        Some(next) => port = next,
                        None => return,
                    }
                }
            });
        }
    });
    let found = found.into_inner();
    ((found != 0).then_some(found as u16), blocked.into_inner())
}

/// Tries `port` on `127.0.0.1` from this process.
pub fn reach(port: u16) -> Reach {
    // WSAEACCES, what WFP answers a connect its filter blocks.
    const WSAEACCES: i32 = 10013;
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    // Something to connect to, so a connect the fence lets through
    // completes at once and a blocking connect can be used: the fence's
    // refusal reaches a non-blocking one hundreds of times later (about
    // 14ms against 40µs), which over the whole port space is the
    // difference between two seconds and fifteen. Another
    // process may hold the port; the connect then goes to it, with a
    // timeout since it may not be listening, and refused or pending
    // means the same.
    let listener = TcpListener::bind(address).ok();
    let connected = if listener.is_some() {
        TcpStream::connect(address)
    } else {
        TcpStream::connect_timeout(&address, Duration::from_millis(500))
    };
    match connected {
        Ok(_) => Reach::Reached,
        Err(error) if error.raw_os_error() == Some(WSAEACCES) => Reach::Blocked,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::TimedOut
            ) =>
        {
            Reach::Reached
        }
        Err(_) => Reach::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fence(low: u16, high: u16) -> impl Fn(u16) -> Reach + Sync {
        move |port| {
            if (low..=high).contains(&port) {
                Reach::Reached
            } else {
                Reach::Blocked
            }
        }
    }

    #[test]
    fn the_default_range_is_found_without_searching_the_rest() {
        let tried = std::sync::Mutex::new(Vec::new());
        let found = search(|port| {
            tried.lock().unwrap().push(port);
            fence(60080, 60089)(port)
        });
        assert_eq!(found, Found::Permitted(60080, 60089));
        assert!(tried.lock().unwrap().len() <= 12);
    }

    #[test]
    fn a_range_chosen_at_install_is_found_wherever_it_is() {
        for (low, high) in [(1, 1), (40000, 40000), (61000, 61009), (65471, 65535), (2000, 2064)] {
            assert_eq!(search(fence(low, high)), Found::Permitted(low, high), "{low}-{high}");
        }
    }

    #[test]
    fn a_range_overlapping_the_default_is_walked_to_its_ends() {
        assert_eq!(search(fence(60085, 60149)), Found::Permitted(60085, 60149));
        assert_eq!(search(fence(60020, 60084)), Found::Permitted(60020, 60084));
    }

    #[test]
    fn nothing_refused_is_no_fence() {
        assert_eq!(search(|_| Reach::Reached), Found::Unfenced);
        assert_eq!(search(fence(59000, 61000)), Found::Unfenced);
    }

    #[test]
    fn a_fence_that_permits_nothing_is_told_apart() {
        assert_eq!(search(|_| Reach::Blocked), Found::Nothing);
    }

    #[test]
    fn a_port_that_fails_otherwise_ends_the_run_but_does_not_prove_the_fence() {
        let odd_ends = |port: u16| match port {
            60080..=60089 => Reach::Reached,
            60079 | 60090 => Reach::Unknown,
            _ => Reach::Blocked,
        };
        assert_eq!(search(odd_ends), Found::Permitted(60080, 60089));
        let nothing_refused = |port: u16| match port {
            60080..=60089 => Reach::Reached,
            _ => Reach::Unknown,
        };
        assert_eq!(search(nothing_refused), Found::Unfenced);
    }
}
