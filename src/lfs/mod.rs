use git2::Filter;
use git2::Repository;
use git2::*;

pub struct Lfs<'a>(&'a Repository);

impl<'a> Lfs<'a> {
  pub fn new(repo: &'a Repository) -> Self {
    Self(repo)
  }

  pub fn install(&self) -> Result<(), Error> {
    let mut filter = Filter::new()?;

    filter
      .on_init(|filter| {
        println!("on_init");
        Ok(())
      })
      .on_check(|filter, src, attr_values| {
        println!("on_check {:?} {:?}", src.mode(), src.path());
        Ok(true)
      })
      .on_apply(|filter, mut to, from, src| {
        match src.mode() {
          FilterMode::Smudge => {
            println!("on_apply: smudge {} ({:?})", src.path().unwrap().display(), src.id());
          }
          FilterMode::Clean => {
            println!("on_apply: clean {} ({:?})", src.path().unwrap().display(), src.id());
            println!("from: {:?}", from.as_bytes().len());

            to.as_allocated_vec().extend_from_slice(b"hello!");
            println!("to: {:?}", to.as_bytes().len());

            println!("_____");
            println!("from: {:?}", std::str::from_utf8(from.as_bytes()).unwrap());
            println!("to: {:?}", std::str::from_utf8(to.as_bytes()).unwrap());
          }
        }
        Ok(())
      });

    filter.register("lfs", 1)?;
    Ok(())
  }
}
