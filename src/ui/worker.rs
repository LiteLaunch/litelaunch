//! Background thread running slow plugin queries (debounced, newest wins): Flow Python plugins
//! and native plugins' query_slow.

use super::*;

pub(super) struct Job {
    pub(super) seq: u64,
    pub(super) kind: JobKind,
}

pub(super) enum JobKind {
    Py {
        /// Index into the config's Flow plugins.
        plugin: usize,
        /// The plugin as configured when the job was made (indices shift when one is removed).
        config: crate::config::PluginConfig,
        settings: Value,
        text: String,
    },
    /// `index` into Engine::natives, for the rows' actions.
    Native { index: usize, plugin: Arc<crate::plugins::Native>, settings: Value, data: Value },
}

/// A finished query: its sequence number, the plugin's name and the outcome.
pub(super) type PyReply = (u64, String, std::result::Result<Vec<Item>, String>);

pub(super) fn spawn_py_worker(hwnd: HWND, seq: Arc<AtomicU64>, child: Arc<Mutex<Option<Child>>>, reply: Arc<Mutex<Option<PyReply>>>) -> Sender<Job> {
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
            let (name, r) = match job.kind {
                JobKind::Py { plugin, config, settings, text } => {
                    let r = pyplugin::query(&crate::python::current().0, &config, &text, &settings, &child);
                    (config.name, r.map(|results| crate::search::py_items(plugin, results)))
                }
                JobKind::Native { index, plugin, settings, data } => {
                    let r = plugin.query_slow(&settings, data);
                    (plugin.info.name.clone(), r.map(|items| items.into_iter().map(|it| crate::search::native_item(index, it)).collect()))
                }
            };
            if matches!(&r, Err(e) if e == "cancelled") || job.seq != seq.load(Ordering::SeqCst) {
                continue;
            }
            *reply.lock().unwrap() = Some((job.seq, name, r));
            unsafe {
                let _ = PostMessageW(Some(HWND(h as _)), WM_APP_PY, WPARAM(job.seq as usize), LPARAM(0));
            }
        }
    });
    tx
}
