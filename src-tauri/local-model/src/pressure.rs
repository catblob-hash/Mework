//! System memory-pressure notifications.
//!
//! On a Mac the model shares memory with everything else. Its weights and the
//! cached prompt states are the cheapest memory to give back, since they can
//! be read from disk again; the buffers of a running request are not. So a
//! pressure warning unloads the model once nothing is running.

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;

    use block2::RcBlock;

    #[repr(C)]
    struct SourceType {
        _opaque: [u8; 0],
    }

    const MEMORYPRESSURE_WARN: usize = 0x02;
    const MEMORYPRESSURE_CRITICAL: usize = 0x04;
    const QOS_CLASS_UTILITY: isize = 0x11;

    extern "C" {
        static _dispatch_source_type_memorypressure: SourceType;
        fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut c_void;
        fn dispatch_source_create(kind: *const SourceType, handle: usize, mask: usize, queue: *mut c_void) -> *mut c_void;
        fn dispatch_source_set_event_handler(source: *mut c_void, handler: &block2::DynBlock<dyn Fn()>);
        fn dispatch_source_get_data(source: *mut c_void) -> usize;
        fn dispatch_resume(object: *mut c_void);
        fn dispatch_source_cancel(source: *mut c_void);
        fn dispatch_release(object: *mut c_void);
    }

    pub struct Watch(*mut c_void);

    // SAFETY: a dispatch source is thread-safe; the handle is only cancelled and
    // released once, in Drop.
    unsafe impl Send for Watch {}
    unsafe impl Sync for Watch {}

    impl Drop for Watch {
        fn drop(&mut self) {
            unsafe {
                dispatch_source_cancel(self.0);
                dispatch_release(self.0);
            }
        }
    }

    pub fn watch(on_pressure: impl Fn(bool) + Send + Sync + 'static) -> Option<Watch> {
        unsafe {
            let queue = dispatch_get_global_queue(QOS_CLASS_UTILITY, 0);
            let source = dispatch_source_create(
                &_dispatch_source_type_memorypressure,
                0,
                MEMORYPRESSURE_WARN | MEMORYPRESSURE_CRITICAL,
                queue,
            );
            if source.is_null() {
                return None;
            }
            let raw = source as usize;
            let handler = RcBlock::new(move || {
                let level = dispatch_source_get_data(raw as *mut c_void);
                if level & (MEMORYPRESSURE_WARN | MEMORYPRESSURE_CRITICAL) != 0 {
                    on_pressure(level & MEMORYPRESSURE_CRITICAL != 0);
                }
            });
            dispatch_source_set_event_handler(source, &handler);
            dispatch_resume(source);
            Some(Watch(source))
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub struct Watch;

    pub fn watch(_on_pressure: impl Fn(bool) + Send + Sync + 'static) -> Option<Watch> {
        None
    }
}

pub use imp::{watch, Watch};
