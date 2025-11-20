use std::fs;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::str;

use git2::{Error, Filter, FilterMode, FilterSource, ObjectType};

pub(crate) mod pointer;

pub struct Payload {
  last_file_name: String,
}

impl Drop for Payload {
  fn drop(&mut self) {
    println!("CLEANUP: {:?}", self.last_file_name);
  }
}

pub struct Lfs();

impl Lfs {
  pub fn install(&self) -> Result<(), Error> {
    
    pointer::Pointer::from_bytes(b"test");
    
    let mut filter = Filter::<Payload>::new()?;

    filter
      .on_init(|_| Ok(()))
      .on_check(|_, mut payload, src, _| {
        let should_filter = is_bin(src.path());
        if !should_filter {
          return Ok(false);
        }

        let maybe_path = src.path().map(|value| value.to_string_lossy().into_owned());
        if let (true, Some(last_file_name)) = (payload.inner().is_none(), maybe_path) {
          payload.replace(Payload { last_file_name });
        }

        Ok(true)
      })
      .on_apply(|_, _, mut to, from, src| match src.mode() {
        FilterMode::Clean => {
          if !is_bin(src.path()) {
            let buf = to.as_allocated_vec();
            buf.clear();
            buf.extend_from_slice(from.as_bytes());
            return Ok(());
          }

          let hash = store_object(from.as_bytes(), &src)?;
          let pointer = format!("lfs {}", hash);
          let buf = to.as_allocated_vec();
          buf.clear();
          buf.extend_from_slice(pointer.as_bytes());
          Ok(())
        }
        FilterMode::Smudge => {
          if !is_bin(src.path()) {
            let buf = to.as_allocated_vec();
            buf.clear();
            buf.extend_from_slice(from.as_bytes());
            return Ok(());
          }

          let Some(hash) = parse_pointer(from.as_bytes()) else {
            let buf = to.as_allocated_vec();
            buf.clear();
            buf.extend_from_slice(from.as_bytes());
            return Ok(());
          };

          let data = load_object(&hash, &src)?;
          let buf = to.as_allocated_vec();
          buf.clear();
          buf.extend_from_slice(&data);
          Ok(())
        }
      });

    filter.register("lfs", 1)?;
    Ok(())
  }
}

fn store_object(bytes: &[u8], src: &FilterSource) -> Result<String, Error> {
  let hash = git2::Oid::hash_object(ObjectType::Blob, bytes)?.to_string();
  let repo = src.repo();
  let objects_dir = repo.path().join("lfs").join("objects");
  fs::create_dir_all(&objects_dir).map_err(|err| Error::from_str(&format!("create dir: {err}")))?;

  let object_path = objects_dir.join(&hash);
  if !object_path.exists() {
    fs::write(&object_path, bytes).map_err(|err| Error::from_str(&format!("write object: {err}")))?;
  }

  Ok(hash)
}

fn load_object(hash: &str, src: &FilterSource) -> Result<Vec<u8>, Error> {
  let repo = src.repo();
  let object_path = repo.path().join("lfs").join("objects").join(hash);
  fs::read(&object_path).map_err(|err| Error::from_str(&format!("read object: {err}")))
}

fn parse_pointer(bytes: &[u8]) -> Option<String> {
  let text = str::from_utf8(bytes).ok()?.trim();
  let hash = text.strip_prefix("lfs ")?;
  let _ = git2::Oid::from_str(hash).ok()?;
  Some(hash.to_string())
}

fn is_bin(path: Option<&Path>) -> bool {
  path.and_then(|value| value.extension()).is_some_and(|ext| ext.eq_ignore_ascii_case("bin"))
}
