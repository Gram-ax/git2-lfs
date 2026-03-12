This crate was created as a separated core part of custom LFS spec implementation for [Gramax](https://github.com/Gram-ax/gramax).

You can find complete example of usage [here](https://github.com/Gram-ax/gramax/blob/master/crates/git/src/ext/lfs.rs).

### Features:
- Parsing LFS pointer files
- Writing blobs content to `.git/lfs/objects` and the corresponding LFS pointer to git object database, and reading them back
- Pulling and Pushing LFS objects from/to LFS remote over HTTP

### Usage
This crate depends on patched `git2` crate where bindings for [filters](https://libgit2.org/docs/reference/main/filter/index.html) API are implemented.

So you have to use the re-exported `git2` crate from this crate instead of the original one.

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
