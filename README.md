
Originally this crate was created as a separated core part of custom LFS spec implementation for [Gramax](https://github.com/Gram-ax/gramax).

You can find complete example of usage [here](https://github.com/Gram-ax/gramax/blob/master/crates/git/src/ext/lfs.rs).

### Features:
- Parse lfs pointer files
- Write pointer files to index and the original content to `.git/lfs/objects`. And the other way around during checkout
- Push and pull lfs objects from LFS remote over HTTP

### Usage

This crate depends on patched `git2` crate where bindings for [filters](https://libgit2.org/docs/reference/main/filter/index.html) are implemented.

And because of this, you're required to use the same feature-flags set for both `git2` and `git2-lfs` crates, see the example below:

```toml
git2 = { git = "https://github.com/pashokitsme/git2-rs.git", branch = "filter", default-features = false, features = [
  "https",
  "ssh",
  "vendored-libgit2",
  # "use-openssl"
] }

git2-lfs = { git = "https://github.com/gram-ax/git2-lfs.git", features = [
	# "git2-vendored-openssl",
	# "git2-use-openssl",
] }
```

Next, you have to initialize filters:

```rust
fn main() {
	// since libgit2 stores registered filters statically, you should call this method only once
	git2_lfs::LfsBuilder::default().install("filter=lfs").unwrap();
}
```

Now you can use `git2` as usual: checkout or add files to the index. Files marked with `filter=lfs` in `.gitattributes` file will be handled as a lfs object.

Example of `.gitattributes`:

```gitattributes
**/*.png filter=lfs
```
