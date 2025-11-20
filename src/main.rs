// pub fn main() {}

// pub mod lfs;

use std::path::Path;

use anyhow::Result;
use git2::*;

use lfs::Lfs;

fn add(repo: Repository, path: &Path) -> Result<()> {
  let mut index = repo.index()?;
  index.add_path(path)?;

  index.write()?;
  let tree = index.write_tree()?;

  let tree = repo.find_tree(tree)?;

  let entry = tree.get_path(path)?;
  let object = repo.find_object(entry.id(), Some(ObjectType::Blob))?;

  println!("on-disk: {:?}", std::fs::read_to_string(Path::new("repo").join(path))?);
  println!("odb: {:?}", std::str::from_utf8(object.into_blob().unwrap().content()).unwrap());

  println!("close repo");

  Ok(())
}

fn checkout(repo: Repository) -> Result<()> {
  let head = repo.head()?.peel_to_commit()?;
  repo.reset(head.as_object(), ResetType::Hard, None)?;
  Ok(())
}

fn status(repo: Repository) -> Result<()> {
  println!("status:");
  for status in repo.statuses(None)?.iter() {
    println!("{}: {:?}", status.path().unwrap(), status.status());
  }
  Ok(())
}

fn main() -> Result<()> {
  
  
  let lfs = Lfs();
  let repo = Repository::open("repo")?;
  lfs.install()?;

  let cmd = std::env::args().nth(1).unwrap_or_default();
  match cmd.as_str() {
    "add" => {
      let path = std::env::args().nth(2).expect("path is required");
      add(repo, Path::new(&path))?;
    }
    "reset" => {
      checkout(repo)?;
    }
    "status" => {
      status(repo)?;
    }
    _ => {
      println!("Usage: lfs add / reset");
    }
  }

  Ok(())
}
