use super::*;

#[cfg(debug_assertions)]
#[test]
fn debug_default_is_a_data_folder_beside_the_executable() {
    let expected = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join("data");
    assert_eq!(
        resolve_data_directory(|| None, default_data_directory).unwrap(),
        expected
    );
}

#[cfg(debug_assertions)]
#[test]
fn debug_override_does_not_require_a_standard_directory() {
    let path = PathBuf::from("development-data");
    let actual = resolve_data_directory(
        || Some(path.clone()),
        || panic!("the standard directory should not be requested"),
    )
    .unwrap();
    assert_eq!(actual, path);
}

#[cfg(not(debug_assertions))]
#[test]
fn release_uses_standard_directory_without_reading_override() {
    let path = PathBuf::from("application-data");
    let actual = resolve_data_directory(
        || panic!("release must not read MPLAYER_DATA_DIR"),
        || Some(path.clone()),
    )
    .unwrap();
    assert_eq!(actual, path);
}

#[test]
fn absent_or_empty_override_uses_default_directory() {
    for value in [None, Some(PathBuf::new())] {
        let expected = PathBuf::from("application-data");
        assert_eq!(
            resolve_data_directory(|| value, || Some(expected.clone())).unwrap(),
            expected
        );
    }
    assert!(resolve_data_directory(|| None, || None).is_err());
}
