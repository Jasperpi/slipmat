//! Who goes first when the computer is busy: the sound card always.
//! Background work (stem separation, analysis) asks for low priority, so a
//! song splitting into stems never makes the music stutter.

/// Lower the calling thread's priority. Threads it starts later (like ONNX
/// Runtime's worker pool) inherit it.
pub fn background() {
    #[cfg(target_os = "linux")]
    // SAFETY: plain system calls on the current thread.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, 10);
    }
    #[cfg(target_os = "macos")]
    // SAFETY: sets the quality-of-service class of the current thread.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
}
