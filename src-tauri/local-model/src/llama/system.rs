//! The two facts about the machine llama.cpp does not report usefully: how
//! many cores should run the CPU backend, and how much memory is free (its
//! CPU device claims all of it is, except on Windows).

/// Physical cores worth running on: the performance cores where the CPU has
/// several kinds (Apple's P cores, Intel's P cores), or every physical core
/// when there are too few of those. `None` when the platform won't say.
pub fn performance_cores() -> Option<usize> {
    let (performance, physical) = imp::cores()?;
    let physical = physical.max(1);
    let performance = performance.clamp(1, physical);
    // A laptop with two P cores and eight E cores decodes faster on more of them.
    Some(if performance >= 4 { performance } else { physical.min(4).max(performance) })
}

/// Memory the system could hand out now without paging, if it says.
pub fn available_memory() -> Option<u64> {
    imp::available_memory()
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::HashSet;
    use std::fs;

    pub fn cores() -> Option<(usize, usize)> {
        let all = physical_cores(|_| true)?;
        // Intel hybrid parts list their P cores here (and E cores under cpu_atom).
        let performance = fs::read_to_string("/sys/devices/cpu_core/cpus")
            .ok()
            .and_then(|list| {
                let cpus = parse_cpu_list(list.trim());
                physical_cores(|cpu| cpus.contains(&cpu))
            })
            .unwrap_or(all);
        Some((performance, all))
    }

    /// Distinct (package, core) pairs among the online CPUs `keep` accepts.
    fn physical_cores(keep: impl Fn(usize) -> bool) -> Option<usize> {
        let online = fs::read_to_string("/sys/devices/system/cpu/online").ok()?;
        let mut cores = HashSet::new();
        for cpu in parse_cpu_list(online.trim()) {
            if !keep(cpu) {
                continue;
            }
            let base = format!("/sys/devices/system/cpu/cpu{cpu}/topology");
            let read = |name: &str| fs::read_to_string(format!("{base}/{name}")).ok().map(|s| s.trim().to_owned());
            let (Some(package), Some(core)) = (read("physical_package_id"), read("core_id")) else { continue };
            cores.insert((package, core));
        }
        (!cores.is_empty()).then_some(cores.len())
    }

    /// "0-3,8,10-11" → [0, 1, 2, 3, 8, 10, 11].
    fn parse_cpu_list(list: &str) -> Vec<usize> {
        let mut cpus = Vec::new();
        for part in list.split(',').filter(|part| !part.is_empty()) {
            match part.split_once('-') {
                Some((a, b)) => {
                    if let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>()) {
                        cpus.extend(a..=b);
                    }
                }
                None => cpus.extend(part.parse::<usize>().ok()),
            }
        }
        cpus
    }

    pub fn available_memory() -> Option<u64> {
        let info = fs::read_to_string("/proc/meminfo").ok()?;
        let line = info.lines().find(|line| line.starts_with("MemAvailable:"))?;
        let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kib * 1024)
    }

    #[cfg(test)]
    #[test]
    fn parses_cpu_lists() {
        assert_eq!(parse_cpu_list("0-3,8,10-11"), vec![0, 1, 2, 3, 8, 10, 11]);
        assert_eq!(parse_cpu_list("0"), vec![0]);
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{c_char, c_int, c_void, CStr};

    extern "C" {
        fn sysctlbyname(
            name: *const c_char,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *mut c_void,
            newlen: usize,
        ) -> c_int;
    }

    fn sysctl<T: Copy + Default>(name: &CStr) -> Option<T> {
        let mut value = T::default();
        let mut len = std::mem::size_of::<T>();
        // SAFETY: `value` is a plain integer of `len` bytes; the name is NUL-terminated.
        let status =
            unsafe { sysctlbyname(name.as_ptr(), (&mut value as *mut T).cast(), &mut len, std::ptr::null_mut(), 0) };
        (status == 0 && len == std::mem::size_of::<T>()).then_some(value)
    }

    pub fn cores() -> Option<(usize, usize)> {
        let physical = sysctl::<i32>(c"hw.physicalcpu")? as usize;
        // Levels run from fastest to slowest; every level but the last is a performance level.
        let levels = sysctl::<i32>(c"hw.nperflevels").unwrap_or(1).max(1) as usize;
        let performance = if levels > 1 {
            (0..levels - 1)
                .filter_map(|level| {
                    let name = std::ffi::CString::new(format!("hw.perflevel{level}.physicalcpu")).ok()?;
                    sysctl::<i32>(&name)
                })
                .sum::<i32>() as usize
        } else {
            physical
        };
        Some((performance, physical))
    }

    pub fn available_memory() -> Option<u64> {
        let total = sysctl::<u64>(c"hw.memsize")?;
        // The kernel's own "how much is free before pressure" figure, in percent.
        let level = sysctl::<i32>(c"kern.memorystatus_level")?;
        Some(total / 100 * level.clamp(0, 100) as u64)
    }
}

#[cfg(windows)]
mod imp {
    /// `RelationProcessorCore`.
    const RELATION_PROCESSOR_CORE: i32 = 0;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetLogicalProcessorInformationEx(relationship: i32, buffer: *mut u8, returned_length: *mut u32) -> i32;
    }

    pub fn cores() -> Option<(usize, usize)> {
        let mut len = 0u32;
        // SAFETY: a null buffer with length 0 asks for the size needed.
        unsafe { GetLogicalProcessorInformationEx(RELATION_PROCESSOR_CORE, std::ptr::null_mut(), &mut len) };
        if len == 0 {
            return None;
        }
        let mut buffer = vec![0u8; len as usize];
        // SAFETY: `buffer` holds `len` bytes, as the first call asked for.
        if unsafe { GetLogicalProcessorInformationEx(RELATION_PROCESSOR_CORE, buffer.as_mut_ptr(), &mut len) } == 0 {
            return None;
        }
        // SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX: Relationship (u32), Size (u32),
        // then PROCESSOR_RELATIONSHIP: Flags (u8), EfficiencyClass (u8), ...
        let mut classes = Vec::new();
        let mut offset = 0usize;
        let len = (len as usize).min(buffer.len());
        while offset + 10 <= len {
            let relationship = u32::from_ne_bytes(buffer[offset..offset + 4].try_into().ok()?);
            let size = u32::from_ne_bytes(buffer[offset + 4..offset + 8].try_into().ok()?) as usize;
            if size == 0 {
                break;
            }
            if relationship == RELATION_PROCESSOR_CORE as u32 {
                classes.push(buffer[offset + 9]);
            }
            offset += size;
        }
        if classes.is_empty() {
            return None;
        }
        // Higher EfficiencyClass is faster; on hybrid parts the E cores have the lowest.
        let lowest = *classes.iter().min()?;
        let performance = classes.iter().filter(|class| **class > lowest).count();
        let performance = if performance == 0 { classes.len() } else { performance };
        Some((performance, classes.len()))
    }

    /// ggml's CPU device reports GlobalMemoryStatusEx's available memory on
    /// Windows; the caller uses that.
    pub fn available_memory() -> Option<u64> {
        None
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    pub fn cores() -> Option<(usize, usize)> {
        None
    }

    pub fn available_memory() -> Option<u64> {
        None
    }
}
