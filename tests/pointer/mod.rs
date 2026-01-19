mod parse;

use std::str::FromStr;

use git2_lfs::Pointer;
use rstest::rstest;

use assertables::assert_ok;
use sha2::Digest;

#[rstest]
fn write_and_parse() -> Result<(), anyhow::Error> {
	let pointer = Pointer::from_blob_bytes(b"blob")?;

	let mut data = Vec::new();
	pointer.write_pointer(&mut data)?;

	let hash = sha2::Sha256::digest(b"blob");
	let hex = hex::encode(hash);
	let size = b"blob".len();

	let expected = format!(
		r#"version https://git-lfs.github.com/spec/v1
oid sha256:{hex}
size {size}
"#,
		hex = hex,
		size = size
	);

	let str = assert_ok!(str::from_utf8(&data));

	assert_eq!(str, expected);

	let pointer = Pointer::from_str(str);
	let pointer = assert_ok!(pointer);

	assert_eq!(pointer.hex(), hex);
	assert_eq!(pointer.size(), size);

	Ok(())
}

#[rstest]
fn is_pointer() -> Result<(), anyhow::Error> {
	let not_pointer = b"not a pointer";
	let pointer = Pointer::from_blob_bytes(b"blob")?;
	let pointer_bytes = pointer.as_bytes()?;

	assert!(!Pointer::is_pointer(not_pointer));
	assert!(Pointer::is_pointer(&pointer_bytes));

	let truncated_pointer_bytes = &pointer_bytes[..99];
	assert!(!Pointer::is_pointer(truncated_pointer_bytes));

	Ok(())
}
