pub mod ext;
pub mod remote;

mod lfs;
mod pointer;

pub use pointer::Pointer;

pub use sha2;

pub use lfs::Lfs;
pub use lfs::LfsBuilder;

#[derive(thiserror::Error, Debug)]
pub enum Error {
	#[error("resulting hash should be exactly 32 bytes, got {0}")]
	InvalidHashLength(usize),

	#[error("the pointer contains invalid spec, expected '{expected}', got '{actual}'")]
	InvalidSpec { expected: String, actual: String },

	#[error("the pointer contains invalid size: '{0}'")]
	InvalidSize(String),

	#[error("not a pointer")]
	NotAPointer,

	#[error(transparent)]
	Utf8(#[from] std::str::Utf8Error),

	#[error("hex: {0}")]
	Hex(#[from] hex::FromHexError),

	#[error("remote: {0}")]
	Remote(#[from] crate::remote::RemoteError),

	#[error(transparent)]
	Git2(#[from] git2::Error),

	#[error("io: {0}")]
	Io(#[from] std::io::Error),
}

pub fn report_error(mut err: &dyn std::error::Error) -> String {
	use std::fmt::Write;

	let mut s = format!("{err}");
	while let Some(src) = err.source() {
		let _ = write!(s, "\n\ncaused by: {src}");
		err = src;
	}

	s
}
