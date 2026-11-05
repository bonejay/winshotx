//! Single instance. The first process owns a named mutex; later ones signal it through named
//! events and exit: no arguments shows the settings, `--capture` starts a capture, `--cancel`
//! closes it.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Show,
    Capture,
    Cancel,
}

impl Request {
    pub fn from_args(args: &[String]) -> Self {
        if args.iter().any(|a| a == "--capture") {
            Request::Capture
        } else if args.iter().any(|a| a == "--cancel") {
            Request::Cancel
        } else {
            Request::Show
        }
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    fn event_name(self) -> &'static str {
        match self {
            Request::Show => "Local\\Snapframe.Show",
            Request::Capture => "Local\\Snapframe.Capture",
            Request::Cancel => "Local\\Snapframe.Cancel",
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::Request;
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForMultipleObjects,
        EVENT_MODIFY_STATE, INFINITE,
    };

    /// `true` if this is the first instance. Otherwise signals the running one.
    pub fn acquire(request: Request) -> bool {
        unsafe {
            let mutex = CreateMutexW(None, false, &HSTRING::from("Local\\Snapframe.Instance"));
            let first = mutex.is_ok() && GetLastError() != ERROR_ALREADY_EXISTS;
            if first {
                // The mutex must live as long as the process.
                std::mem::forget(mutex);
                return true;
            }
            if let Ok(event) = OpenEventW(
                EVENT_MODIFY_STATE,
                false,
                &HSTRING::from(request.event_name()),
            ) {
                let _ = SetEvent(event);
            }
            false
        }
    }

    /// Wait for requests from later instances on a background thread.
    pub fn listen(on_request: impl Fn(Request) + Send + 'static) {
        let requests = [Request::Show, Request::Capture, Request::Cancel];
        let raw: Vec<isize> = requests
            .iter()
            .filter_map(|r| unsafe {
                CreateEventW(None, false, false, &HSTRING::from(r.event_name())).ok()
            })
            .map(|h| h.0 as isize)
            .collect();
        if raw.len() != requests.len() {
            return;
        }
        std::thread::spawn(move || {
            let handles: Vec<HANDLE> = raw
                .iter()
                .map(|&h| HANDLE(h as *mut core::ffi::c_void))
                .collect();
            loop {
                let result = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
                let index = result.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
                match requests.get(index) {
                    Some(&r) => on_request(r),
                    None => break,
                }
            }
        });
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Request;
    pub fn acquire(_: Request) -> bool {
        true
    }
    pub fn listen(_: impl Fn(Request) + Send + 'static) {}
}

pub use imp::*;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arguments_choose_the_request() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(Request::from_args(&args(&["snapframe.exe"])), Request::Show);
        assert_eq!(
            Request::from_args(&args(&["x", "--capture"])),
            Request::Capture
        );
        assert_eq!(
            Request::from_args(&args(&["x", "--cancel"])),
            Request::Cancel
        );
    }
}
