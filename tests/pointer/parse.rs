use std::str::FromStr;

use assertables::assert_ok;
use git2_lfs::Error;
use git2_lfs::Pointer;
use rstest::rstest;

use assert_matches::assert_matches;
use assertables::assert_err;

#[rstest]
fn parse_pointer_naive() -> Result<(), anyhow::Error> {
	let data = r#"version https://git-lfs.github.com/spec/v1
oid sha256:e69de29bb2d7d028ab54a17b17c1b611b022acc8755b950963d6444464ef4c44
size 100
"#;

	let pointer = Pointer::from_str(data)?;
	assert_eq!(pointer.hex(), "e69de29bb2d7d028ab54a17b17c1b611b022acc8755b950963d6444464ef4c44");
	assert_eq!(pointer.size(), 100);

	Ok(())
}

#[rstest]
fn parse_pointer_invalid_hash_length() -> Result<(), anyhow::Error> {
	let data = r#"version https://git-lfs.github.com/spec/v1
oid sha256:1234
size 100
  "#;

	let result = Pointer::from_str(data);

	let err = assert_err!(result);

	assert_matches!(err, Error::InvalidHashLength(4));

	Ok(())
}

#[rstest]
fn parse_pointer_excess_space() -> Result<(), anyhow::Error> {
	let data = r#"
  version https://git-lfs.github.com/spec/v1
  oid sha256:e69de29bb2d7d028ab54a17b17c1b611b022acc8755b950963d6444464ef4c44
  size 100


  "#;

	let pointer = Pointer::from_str(data);

	let err = assert_err!(pointer);
	assert_matches!(err, Error::InvalidSpec { expected: _, actual: _ });

	Ok(())
}

#[rstest]
fn parse_pointer_missing_size() -> Result<(), anyhow::Error> {
	let data = r#"version https://git-lfs.github.com/spec/v1
oid sha256:e69de29bb2d7d028ab54a17b17c1b611b022acc8755b950963d6444464ef4c44
"#;

	let pointer = Pointer::from_str(data);

	let pointer = assert_ok!(pointer);
	assert_eq!(pointer.size(), 0);

	Ok(())
}
