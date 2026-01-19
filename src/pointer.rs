use std::fmt::Display;

use std::io::BufWriter;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;

use sha2::Digest;

use crate::Error;

use tracing::*;

pub const POINTER_ROUGH_LEN: std::ops::Range<usize> = 120..220;

const HASH_LEN: usize = 32;
const HEX_LEN: usize = HASH_LEN * 2;

const VERSION: &str = "version https://git-lfs.github.com/spec/v1";
const OID_PREFIX: &str = "oid sha256:";
const SIZE_PREFIX: &str = "size ";

#[derive(PartialEq, Eq, Hash, Clone, Copy)]
pub struct Pointer {
	hash: [u8; HASH_LEN],
	size: usize,
}

impl Display for Pointer {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "sha256:{} size {}", self.hex(), self.size)
	}
}

impl std::fmt::Debug for Pointer {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Pointer").field("hash", &self.hex()).field("size", &self.size).finish()
	}
}

impl Pointer {
	pub fn from_parts(hash: &[u8], size: usize) -> Self {
		let mut copied_hash = [0; HASH_LEN];
		copied_hash.copy_from_slice(hash);

		Self { hash: copied_hash, size }
	}

	pub fn from_blob_bytes(bytes: &[u8]) -> Result<Self, Error> {
		let mut hasher = sha2::Sha256::default();
		hasher.update(bytes);
		let val = hasher.finalize();
		if val.len() != 32 {
			return Err(Error::InvalidHashLength(val.len()));
		}

		let mut hash = [0; 32];
		hash.copy_from_slice(val.as_slice());

		Ok(Self { hash, size: bytes.len() })
	}

	pub fn size(&self) -> usize {
		self.size
	}

	pub fn hex(&self) -> String {
		hex::encode(self.hash)
	}

	pub fn hash(&self) -> &[u8; 32] {
		&self.hash
	}

	pub fn path(&self) -> PathBuf {
		let hex = self.hex();
		Path::new(&hex[..2]).join(&hex[2..4]).join(&hex)
	}

	pub fn write_pointer(&self, writer: &mut impl Write) -> Result<(), Error> {
		writer.write_all(b"version https://git-lfs.github.com/spec/v1\n")?;
		writer.write_all(b"oid sha256:")?;

		let mut output = [0; HEX_LEN];
		hex::encode_to_slice(self.hash, &mut output)?;
		writer.write_all(&output)?;
		writer.write_all(b"\n")?;

		writer.write_all(b"size ")?;
		writer.write_all(self.size.to_string().as_bytes())?;
		writer.write_all(b"\n")?;

		writer.flush()?;

		Ok(())
	}

	pub fn as_bytes(&self) -> Result<Vec<u8>, Error> {
		let mut bytes = Vec::with_capacity(120);
		self.write_pointer(&mut bytes)?;
		Ok(bytes)
	}

	pub fn write_blob_bytes(&self, absolute_object_dir: &Path, bytes: &[u8]) -> Result<(), Error> {
		let file = absolute_object_dir.join(self.path());
		std::fs::create_dir_all(file.parent().unwrap())?;

		info!(path = %file.display(), "writing lfs object");

		let mut file = std::fs::File::options().create_new(true).write(true).open(&file)?;
		BufWriter::new(&mut file).write_all(bytes)?;
		Ok(())
	}

	pub fn from_str_short(bytes: &[u8]) -> Option<Self> {
		match bytes.get(..(bytes.len().min(POINTER_ROUGH_LEN.end))).map(str::from_utf8) {
			Some(Ok(text)) => {
				let pointer = Pointer::from_str(text);
				if let Err(ref err) = pointer {
					trace!("Pointer::from_str_short: {:?}", err);
				}
				pointer.ok()
			}
			_ => None,
		}
	}

	pub fn is_pointer(bytes: &[u8]) -> bool {
		Pointer::from_str_short(bytes).is_some()
	}
}

impl FromStr for Pointer {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let mut lines = s.lines();

		let version = lines.next().unwrap_or_default();
		let oid = lines.next().unwrap_or_default();

		if version != VERSION {
			let actual = if version.is_empty() || version == "\n" {
				"<empty line>"
			} else {
				version
			};
			return Err(Error::InvalidSpec {
				expected: VERSION.to_string(),
				actual: actual.to_string(),
			});
		}

		if !oid.starts_with(OID_PREFIX) {
			let actual = if oid.is_empty() || oid == "\n" { "<empty line>" } else { oid };
			return Err(Error::InvalidSpec {
				expected: OID_PREFIX.to_string(),
				actual: actual.to_string(),
			});
		}

		if oid.len() != HEX_LEN + OID_PREFIX.len() {
			return Err(Error::InvalidHashLength(oid.len() - OID_PREFIX.len()));
		}

		let hex = &oid[OID_PREFIX.len()..HEX_LEN + OID_PREFIX.len()];

		let size = lines.next();

		let size = match size {
			Some("" | "\n") | None => 0,
			Some(size) => {
				if !size.starts_with(SIZE_PREFIX) {
					let actual = if size.is_empty() || size == "\n" { "<empty line>" } else { size };
					return Err(Error::InvalidSpec {
						expected: SIZE_PREFIX.to_string(),
						actual: actual.to_string(),
					});
				}

				let size = &size[SIZE_PREFIX.len()..];
				size.parse::<usize>().map_err(|err| Error::InvalidSize(err.to_string()))?
			}
		};

		let mut hash = [0; HASH_LEN];
		hex::decode_to_slice(hex, &mut hash).map_err(Error::Hex)?;

		trace!("Pointer::from_str: {:?} -> {:?}", s, Pointer { hash, size });

		Ok(Pointer { hash, size })
	}
}
