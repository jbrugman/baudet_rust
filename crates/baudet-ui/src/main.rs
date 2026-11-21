//! `baudet-ui`: the transcript window, running against a scripted turn.
//!
//! Deliberately the whole binary. Phase one's deliverable is a UI that behaves with no backend
//! process at all, so shipping a `main` that proves it beats shipping a library someone has to
//! wire up first.

fn main() -> eframe::Result {
    match Startup::from_env() {
        Ok(startup) => baudet_ui::run_from(startup),
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    }
}

use baudet_ui::Startup;
