//! Background thread running Python plugin queries (debounced, newest wins).

use super::*;

pub(super) struct Job {
    pub(super) seq: u64,
    pub(super) plugin: PluginRef,
    /// The plugin as configured when the job was made (indices shift when one is removed).
    pub(super) config: crate::config::PluginConfig,
    pub(super) settings: Value,
    pub(super) text: String,
}

/// A finished query: its sequence number, plugin, query text and outcome.
pub(super) type PyReply = (u64, PluginRef, String, std::result::Result<Vec<PyResult>, String>);

pub(super) fn spawn_py_worker(
    cfg: Config,
    hwnd: HWND,
    seq: Arc<AtomicU64>,
    child: Arc<Mutex<Option<Child>>>,
    reply: Arc<Mutex<Option<PyReply>>>,
    vd_warm: crate::vd::Warm,
) -> Sender<Job> {
    let (tx, rx) = mpsc::channel::<Job>();
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        while let Ok(mut job) = rx.recv() {
            // Debounce: wait until typing pauses, keep only the newest job.
            while let Ok(j) = rx.recv_timeout(Duration::from_millis(250)) {
                job = j;
            }
            if job.seq != seq.load(Ordering::SeqCst) {
                continue;
            }
            let warm = if job.plugin == PluginRef::Vd { crate::vd::take_warm(&vd_warm) } else { None };
            let r = pyplugin::query(&cfg.python, &job.config, &job.text, &job.settings, &child, warm);
            if matches!(&r, Err(e) if e == "cancelled") {
                continue;
            }
            *reply.lock().unwrap() = Some((job.seq, job.plugin, job.text, r));
            unsafe {
                let _ = PostMessageW(Some(HWND(h as _)), WM_APP_PY, WPARAM(job.seq as usize), LPARAM(0));
            }
        }
    });
    tx
}
