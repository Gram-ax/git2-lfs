use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use sha2::Digest;

const HASH_LEN: usize = 32;
const HEX_LEN: usize = HASH_LEN * 2;

#[derive(thiserror::Error, Debug)]
pub enum Error {
  #[error("resulting hash should be exactly 32 bytes, got {0}")]
  InvalidHashLength(usize),

  #[error("the pointer containts invalid spec, expected '{expected}', got '{actual}'")]
  InvalidSpec { expected: String, actual: String },

  #[error("the pointer containts invalid size: '{0}'")]
  InvalidSize(String),

  #[error("hex: {0}")]
  Hex(#[from] hex::FromHexError),

  #[error("io: {0}")]
  Io(#[from] std::io::Error),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Pointer {
  hash: [u8; HASH_LEN],
  size: usize,
}

impl Pointer {
  pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
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

  pub fn path(&self) -> PathBuf {
    let hex = self.hex();
    Path::new("lfs").join("objects").join(&hex[..=2]).join(&hex[2..=4]).join(&hex[5..])
  }

  pub fn write(&self, writer: &mut impl Write) -> Result<(), Error> {
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
}

impl std::str::FromStr for Pointer {
  type Err = Error;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    const VERSION: &str = "version https://git-lfs.github.com/spec/v1";
    const OID_PREFIX: &str = "oid sha256:";
    const SIZE_PREFIX: &str = "size ";
    let mut lines = s.lines();

    let version = lines.next().unwrap_or_default();
    let oid = lines.next().unwrap_or_default();
    let size = lines.next().unwrap_or_default();

    if version != VERSION {
      let actual = if version.is_empty() || version == "\n" { "<empty line>" } else { version };
      return Err(Error::InvalidSpec { expected: VERSION.to_string(), actual: actual.to_string() });
    }

    if !oid.starts_with(OID_PREFIX) {
      let actual = if oid.is_empty() || oid == "\n" { "<empty line>" } else { oid };
      return Err(Error::InvalidSpec { expected: OID_PREFIX.to_string(), actual: actual.to_string() });
    }

    if oid.len() != HEX_LEN + OID_PREFIX.len() {
      return Err(Error::InvalidHashLength(oid.len() - OID_PREFIX.len()));
    }

    let hex = &oid[OID_PREFIX.len()..HEX_LEN + OID_PREFIX.len()];

    if !size.starts_with(SIZE_PREFIX) {
      let actual = if size.is_empty() || size == "\n" { "<empty line>" } else { size };
      return Err(Error::InvalidSpec { expected: SIZE_PREFIX.to_string(), actual: actual.to_string() });
    }

    let size = &size[SIZE_PREFIX.len()..];
    let size = size.parse::<usize>().map_err(|err| Error::InvalidSize(err.to_string()))?;

    let mut hash = [0; HASH_LEN];
    hex::decode_to_slice(hex, &mut hash).map_err(Error::Hex)?;

    Ok(Pointer { hash, size })
  }
}
