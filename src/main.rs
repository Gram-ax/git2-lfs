pub fn main() {}

// pub mod lfs;

// use std::ffi::CStr;
// use std::ffi::c_void;
// use std::path::Path;

// use anyhow::Result;
// use git2::raw::git_filter;
// use git2::*;

// use git2::raw::*;

// fn main() -> Result<()> {
//   let repo = Repository::open("lfs-repo")?;
//   let lfs = lfs::Lfs::new(&repo);
//   lfs.install()?;

//   let mut index = repo.index()?;
//   index.add_path(Path::new("hello.txt"))?;

//   index.write()?;
//   let tree = index.write_tree()?;

//   let tree = repo.find_tree(tree)?;

//   let entry = tree.get_path(Path::new("hello.txt"))?;
//   let object = repo.find_object(entry.id(), Some(ObjectType::Blob))?;

//   println!();
//   println!("on-disk: {:?}", std::fs::read_to_string("lfs-repo/hello.txt").unwrap());
//   println!("odb: {:?}", std::str::from_utf8(object.into_blob().unwrap().content()).unwrap());

//   Ok(())
// }
