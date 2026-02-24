// crates/omnish-client/src/proctitle.rs
//
// Overwrite the process argv area so that tmux (which reads /proc/<pid>/cmdline)
// shows the currently-running command instead of "omnish-client".
//
// On Linux the kernel exposes the argv memory range via fields 48-49
// (arg_start, arg_end) of /proc/self/stat.  We parse those once at init
// and then write directly into that memory region when updating the title.

use std::sync::OnceLock;

/// Thin wrapper around a raw pointer + length so we can store it in a static.
struct ArgArea(*mut u8, usize);

// SAFETY: the arg area is process-global memory (the initial argv/environ region
// on the stack).  We only mutate it from the single-threaded main loop.
unsafe impl Send for ArgArea {}
unsafe impl Sync for ArgArea {}

static ARG_AREA: OnceLock<ArgArea> = OnceLock::new();

/// Parse arg_start / arg_end from /proc/self/stat.
fn parse_arg_area() -> Option<(*mut u8, usize)> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // Field 2 (comm) is enclosed in parens and may contain spaces/parens.
    let close_paren = stat.rfind(')')?;
    // After ") " the fields are numbered from 3 (state).
    // arg_start = field 48, arg_end = field 49.
    // Index from the split after ")": 48 - 3 = 45, 49 - 3 = 46.
    let rest = &stat[close_paren + 2..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let arg_start: usize = fields.get(45)?.parse().ok()?;
    let arg_end: usize = fields.get(46)?.parse().ok()?;
    if arg_end <= arg_start {
        return None;
    }
    Some((arg_start as *mut u8, arg_end - arg_start))
}

/// Call once at startup to snapshot the argv memory region.
pub fn init() {
    ARG_AREA.get_or_init(|| {
        let (ptr, cap) = parse_arg_area().unwrap_or((std::ptr::null_mut(), 0));
        ArgArea(ptr, cap)
    });
}

/// Overwrite /proc/self/cmdline with `title`.
/// The title is silently truncated to the available argv capacity.
pub fn set(title: &str) {
    let Some(ArgArea(ptr, capacity)) = ARG_AREA.get() else {
        return;
    };
    let ptr = *ptr;
    let capacity = *capacity;
    if ptr.is_null() || capacity == 0 {
        return;
    }
    let bytes = title.as_bytes();
    let len = bytes.len().min(capacity - 1); // leave room for NUL
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, len);
        // NUL-terminate and zero-fill the rest so cmdline looks clean.
        std::ptr::write_bytes(ptr.add(len), 0, capacity - len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_arg_area() {
        let result = parse_arg_area();
        if cfg!(target_os = "linux") {
            assert!(result.is_some(), "should parse arg area on Linux");
            let (ptr, cap) = result.unwrap();
            assert!(!ptr.is_null());
            assert!(cap > 0);
        }
    }

    #[test]
    fn test_set_and_read_cmdline() {
        init();
        let original = std::fs::read_to_string("/proc/self/cmdline").unwrap_or_default();
        if original.is_empty() {
            return; // skip if not on Linux
        }

        set("test-title");
        let cmdline = std::fs::read_to_string("/proc/self/cmdline").unwrap_or_default();
        let first_arg = cmdline.split('\0').next().unwrap_or("");
        assert_eq!(first_arg, "test-title");

        // Restore original
        let orig_first = original.split('\0').next().unwrap_or("");
        set(orig_first);
    }
}
