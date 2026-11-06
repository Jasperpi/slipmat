//! Slipmat: a free DJ app. Spotify songs on two decks, mixed in a browser
//! page served from this machine, played through a Pioneer DDJ controller.

mod analysis;
mod app;
mod autodj;
mod doctor;
mod engine;
mod loader;
mod midi;
mod paths;
mod priority;
mod server;
mod session;
mod settings;
mod spotify;
mod stems;
mod store;
mod track;

use std::io::Write;
use std::sync::Mutex;

pub const PORT: u16 = 4141;

const HELP: &str = "\
slipmat — DJ with Spotify on your Pioneer controller

  slipmat              start, and open the decks in your browser
  slipmat --no-open    start without opening a browser
  slipmat doctor       check Spotify, the sound card and the controller
  slipmat doctor --load \"song name\"   decode and analyse one song (silent)
";

struct Log(Mutex<std::fs::File>);

impl log::Log for Log {
    fn enabled(&self, m: &log::Metadata) -> bool {
        let t = m.target();
        let verbose = std::env::var_os("SLIPMAT_DEBUG").is_some();
        m.level() <= log::Level::Info && (t.starts_with("slipmat") || m.level() <= log::Level::Warn) || (verbose && m.level() <= log::Level::Debug)
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
            let line = format!("{now:.3} {} {}: {}\n", r.level(), r.target(), r.args());
            if let Ok(mut f) = self.0.lock() {
                let _ = f.write_all(line.as_bytes());
            }
            if std::env::var_os("SLIPMAT_LOG").is_some() {
                eprint!("{line}");
            }
        }
    }
    fn flush(&self) {}
}

fn init_logging() {
    let dir = paths::cache();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("slipmat.log")) {
        let _ = log::set_boxed_logger(Box::new(Log(Mutex::new(f))));
        log::set_max_level(if std::env::var_os("SLIPMAT_DEBUG").is_some() { log::LevelFilter::Debug } else { log::LevelFilter::Info });
    }
}

/// ALSA prints a line for every device name it can't open while sound cards
/// are listed ("Unknown PCM pulse"...). None of it matters here.
#[cfg(target_os = "linux")]
fn quiet_alsa() {
    extern "C" fn ignore() {}
    // SAFETY: the handler takes no arguments and returns at once, so the
    // variadic signature ALSA calls it with is never read.
    unsafe {
        let handler: alsa_sys::snd_lib_error_handler_t = std::mem::transmute(ignore as extern "C" fn());
        alsa_sys::snd_lib_error_set_handler(handler);
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    quiet_alsa();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return;
    }
    init_logging();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio");
    let result = rt.block_on(async {
        match args.first().map(String::as_str) {
            Some("doctor") => doctor::run(&args[1..]).await,
            _ => server::run(!args.iter().any(|a| a == "--no-open")).await,
        }
    });
    if let Err(e) = result {
        eprintln!("slipmat: {e:#}");
        std::process::exit(1);
    }
}
