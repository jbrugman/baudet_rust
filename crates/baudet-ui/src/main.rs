
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
