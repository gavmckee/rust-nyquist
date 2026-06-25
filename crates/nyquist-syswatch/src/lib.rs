pub mod event;
pub mod snapshot;
pub mod sink;
pub mod inotify;
pub mod watcher;

#[cfg(target_os = "linux")]
pub mod bpf;

pub use watcher::SysWatcher;
