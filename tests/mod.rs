use std::path::Path;
use std::sync::Once;

use git2_lfs::LfsBuilder;
use rstest::fixture;
use tempfile::TempDir;

mod lfs;
mod pointer;

#[fixture]
pub fn repo(#[default(&sandbox())] sandbox: &TempDir) -> git2::Repository {
	static ONCE: Once = Once::new();
	ONCE.call_once(|| LfsBuilder::default().install("filter=lfs").unwrap());

	std::fs::write(sandbox.path().join(".gitattributes"), "*.bin filter=lfs diff=lfs").unwrap();

	git2::Repository::init(sandbox.path()).unwrap()
}

fn init_logger() {
	use tracing_subscriber::EnvFilter;
	use tracing_subscriber::layer::SubscriberExt;
	use tracing_subscriber::util::SubscriberInitExt;

	let show_output = std::env::args().any(|arg| arg == "--exact");
	if !show_output {
		return;
	}

	tracing_subscriber::registry()
		.with(EnvFilter::try_from_default_env().unwrap_or(EnvFilter::new("debug")))
		.with(tracing_subscriber::fmt::layer().with_ansi(true))
		.init();
}

#[fixture]
pub fn sandbox() -> TempDir {
	static INIT: Once = Once::new();
	INIT.call_once(init_logger);

	let path = Path::new(&std::env::temp_dir()).join("testing");
	std::fs::create_dir_all(&path).unwrap();
	tempfile::tempdir_in(path).unwrap()
}
