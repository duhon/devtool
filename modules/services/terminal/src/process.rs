//! Kills the whole process group of a child when it is dropped.

pub(crate) struct ChildGroup(pub(crate) u32);

impl ChildGroup {
    pub(crate) fn signal(&self, signal: i32) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.0 as i32), signal);
        }
    }
}

impl Drop for ChildGroup {
    fn drop(&mut self) {
        self.signal(libc::SIGKILL);
    }
}
