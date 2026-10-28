use agmen_client::{InstallReceipt, read_install_receipt_at};

#[test]
fn desktop_and_doctor_read_same_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("install-receipt.json");
    assert_eq!(read_install_receipt_at(&path).unwrap(), None);
    let receipt = InstallReceipt {
        channel: "shell".into(),
        version: "0.1.0".into(),
        owned_paths: vec![dir.path().join("agmen")],
    };
    std::fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    assert_eq!(read_install_receipt_at(&path).unwrap(), Some(receipt));
}

#[test]
fn receipt_errors_block_channel_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("install-receipt.json");
    for contents in [
        "broken".to_owned(),
        r#"{"channel":"shell","version":"1","owned_paths":["relative"]}"#.to_owned(),
        " ".repeat(65537),
    ] {
        std::fs::write(&path, contents).unwrap();
        assert!(read_install_receipt_at(&path).is_err());
    }
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(read_install_receipt_at(&path).is_err());
}
