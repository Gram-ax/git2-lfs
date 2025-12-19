use std::collections::HashSet;
use std::io::BufReader;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use git2::Filter;
use git2::FilterBuf;
use git2::FilterMode;
use git2::FilterRepository;

use crate::Error;

use tracing::*;

use crate::Pointer;

#[derive(Default, Clone, Debug)]
pub struct LfsBuilder {
  exts: Option<HashSet<String>>,
  max_file_size: Option<u64>,
}

pub struct Lfs<'a> {
  config: &'a LfsBuilder,
  repo: FilterRepository,
}

impl<'a> Lfs<'a> {
  pub fn new(repo: FilterRepository, config: &'a LfsBuilder) -> Self {
    Self { config, repo }
  }

  pub fn check(self, path: &Path) -> Result<bool, Error> {
    if let Some(exts) = &self.config.exts
      && let Some(ext) = path.extension().and_then(|value| value.to_str())
    {
      return Ok(exts.contains(ext));
    }

    if let Some(max_file_size) = self.config.max_file_size {
      let size = path.metadata()?.len();
      return Ok(size <= max_file_size);
    }

    Ok(false)
  }

  pub fn clean(self, input: &[u8], out: &mut FilterBuf) -> Result<bool, Error> {
    if input.len() == 0 {
      info!("clean: passing through zero-sized object");
      return Ok(false);
    }

    let pointer = Pointer::from_blob_bytes(input)?;
    self.store_object_if_not_exists(&pointer, input)?;
    pointer.write_pointer(&mut out.as_allocated_vec())?;

    Ok(true)
  }

  pub fn smudge(self, input: &[u8], out: &mut FilterBuf) -> Result<bool, Error> {
    if input.len() == 0 {
      info!("smudge: passing through zero-sized object");
      return Ok(false);
    }

    let Some(pointer) = Pointer::from_str_short(input) else {
      debug!("not a lfs pointer, passing through");
      return Ok(false);
    };

    self.load_object(&pointer, out)
  }

  fn store_object_if_not_exists(self, pointer: &Pointer, bytes: &[u8]) -> Result<(), Error> {
    let path = self.object_dir().join(pointer.path());

    if path.exists() {
      debug!(path = %path.display(), "object already exists, skipping");
      return Ok(());
    }

    pointer.write_blob_bytes(&self.object_dir(), bytes)?;
    Ok(())
  }

  fn load_object(self, pointer: &Pointer, out: &mut FilterBuf) -> Result<bool, Error> {
    let object_dir = self.object_dir();
    let path = self.object_dir().join(pointer.path());

    if !path.exists() {
      warn!(path = %path.strip_prefix(&object_dir).unwrap_or(&path).display(), "object not found, skipping");
      return Ok(false);
    }

    debug!(path = %path.strip_prefix(&object_dir).unwrap_or(&path).display(), "reading lfs object");

    let file = std::fs::File::open(&path)?;
    let mut reader = BufReader::new(file);
    std::io::copy(&mut reader, &mut out.as_allocated_vec())?;
    Ok(true)
  }

  fn object_dir(&self) -> PathBuf {
    self.repo.path().join("lfs/objects")
  }
}

impl LfsBuilder {
  pub fn with_file_extensions(mut self, exts: &[&str]) -> Self {
    self.exts = Some(exts.iter().map(|ext| ext.to_string()).collect());
    self
  }

  pub fn with_max_file_size(mut self, max_file_size: u64) -> Self {
    self.max_file_size = Some(max_file_size);
    self
  }

  pub fn install(self, attributes: &str) -> Result<(), Error> {
    let mut filter = Filter::<()>::new()?;

    let config = Arc::new(self);

    let on_check_config = Arc::clone(&config);
    let on_apply_config = Arc::clone(&config);

    filter
      .on_init(|_| Ok(()))
      .attributes(attributes)?
      .on_check(move |_, _, src, attrs_set| {
        if attrs_set {
          return Ok(true);
        }

        let lfs = Lfs::new(src.repo(), &on_check_config);

        let Some(path) = src.path() else {
          warn!("filter didn't provide a path, skipping");
          return Ok(false);
        };

        match lfs.check(path) {
          Ok(check) => Ok(check),
          Err(e) => {
            error!(path = %path.display(), "error checking lfs: {}", crate::report_error(&e));
            Err(git2::Error::from_str(&crate::report_error(&e)))
          }
        }
      })
      .on_apply(move |_, _, mut to, from, src| {
        let lfs = Lfs::new(src.repo(), &on_apply_config);

        match src.mode() {
          FilterMode::Clean => match lfs.clean(from.as_bytes(), &mut to) {
            Ok(applied) => Ok(applied),
            Err(e) => {
              error!(path = %src.path().unwrap().display(), "error cleaning lfs: {}", crate::report_error(&e));
              Err(git2::Error::from_str(&crate::report_error(&e)))
            }
          },
          FilterMode::Smudge => match lfs.smudge(from.as_bytes(), &mut to) {
            Ok(applied) => Ok(applied),
            Err(e) => {
              error!(path = %src.path().unwrap().display(), "error smudging lfs: {}", crate::report_error(&e));
              Err(git2::Error::from_str(&crate::report_error(&e)))
            }
          },
        }
      });

    filter.register("lfs", 1)?;
    Ok(())
  }
}
