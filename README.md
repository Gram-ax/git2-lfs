This crate was created as a separated core part of custom LFS spec implementation for [Gramax](https://github.com/Gram-ax/gramax).

You can find complete example of usage [here](https://github.com/Gram-ax/gramax/blob/master/crates/git/src/ext/lfs.rs).

### Features:
- Parsing LFS pointer files
- Writing blobs content to `.git/lfs/objects` and the corresponding LFS pointer to git object database, and reading them back
- Pulling and Pushing LFS objects from/to LFS remote over HTTP

### Usage
This crate depends on patched `git2` crate where bindings for [filters](https://libgit2.org/docs/reference/main/filter/index.html) API are implemented.

And because of this, you've to use the same patched crate and with the same feature-flags set in your project to avoid compilation conflicts. 

Here's an example of dependencies in `Cargo.toml`:

```toml
git2 = { git = "https://github.com/gram-ax/git2-rs.git", branch = "filter", default-features = false, features = [
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

Next, you've to init lfs filter for libgit2.

```rust
fn main() {
	// since libgit2 stores registered filters statically, you should call this method only once
	git2_lfs::LfsBuilder::default().install("filter=lfs").unwrap();
}
```

Now you can use `git2` as usual: checkout or add files to the index. File path patterns marked as `filter=lfs` attribute in `.gitattributes` file will be handled as lfs object.

Example of `.gitattributes`:

```gitattributes
**/*.png filter=lfs
```
