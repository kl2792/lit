use super::*;

const DOI: &str = "10.3389/fncom.2020.00006";
const PAGE: &str = "https://pmc.ncbi.nlm.nih.gov/articles/PMC7025464/";
fn resources() -> Vec<Resource> {
    discover(include_str!("../../../test/fixtures/pmc/landing.html"), PAGE, DOI).unwrap()
}
fn metadata() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../test/fixtures/pmc-cloud/metadata.json")).unwrap()
}
fn bind(value: serde_json::Value) -> Result<Vec<Resource>, Error> {
    let dir = tempfile::tempdir().unwrap();
    let bytes = serde_json::to_vec(&value).unwrap();
    std::fs::write(dir.path().join("metadata.json"), &bytes).unwrap();
    let mut receipt: serde_json::Value = serde_json::from_str(include_str!("../../../test/fixtures/pmc-cloud/response.json")).unwrap();
    receipt["sha256"] = hash(&bytes).into();
    receipt["byte_length"] = bytes.len().into();
    std::fs::write(dir.path().join("response.json"), serde_json::to_vec(&receipt).unwrap()).unwrap();
    cloud::bind(dir.path(), DOI, PAGE, &mut serde_json::json!({}), resources())
}

#[test]
fn cloud_metadata_maps_only_seven_declared_supplements_and_retains_binding() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("test/fixtures/pmc-cloud");
    let mut discovery = serde_json::json!({"original":"unchanged"});
    let mapped = cloud::bind(&fixture, DOI, PAGE, &mut discovery, resources()).unwrap();
    assert_eq!(mapped.len(), 7);
    assert_eq!(mapped[3].url, "https://pmc-oa-opendata.s3.amazonaws.com/PMC7025464.1/Image_4.png?md5=d6a9dd1d0bc0ae992ba6849d44da4ffe");
    assert_eq!(mapped[3].label, "Supplementary Figure 4");
    assert!(mapped[3].cloud.as_ref().unwrap().declared_url.starts_with("s3://pmc-oa-opendata/"));
    assert_eq!(discovery["original"], "unchanged");
    assert_eq!(discovery["cloud"]["binding"]["license_code"], "CC BY");
    assert_eq!(discovery["cloud"]["binding"]["version"], 1);
    assert_eq!(discovery["cloud"]["response"]["sha256"], "d8d2604b61b959fe58556b36c9f31225ef247010af1250f5549ac622b9d2b417");
}

#[test]
fn cloud_metadata_rejects_identity_license_and_publication_mismatch() {
    for (key, value) in [("doi", serde_json::json!("10.1/wrong")), ("pmcid", serde_json::json!("PMC9999999")),
        ("version", serde_json::json!(2)), ("version", serde_json::json!(0)), ("license_code", serde_json::json!("")),
        ("is_pmc_openaccess", serde_json::json!(false)), ("is_manuscript", serde_json::json!(true)),
        ("is_retracted", serde_json::json!(true))] {
        let mut data = metadata(); data[key] = value;
        assert!(bind(data).is_err(), "{key}");
    }
}

#[test]
fn cloud_metadata_rejects_missing_ambiguous_foreign_and_unsafe_media() {
    for bad in ["s3://evil/PMC7025464.1/Image_1.jpg?md5=cfcf3840f5c3b45747ab4b294bc4c002",
        "s3://pmc-oa-opendata/PMC9999999.1/Image_1.jpg?md5=cfcf3840f5c3b45747ab4b294bc4c002",
        "s3://pmc-oa-opendata/PMC7025464.2/Image_1.jpg?md5=cfcf3840f5c3b45747ab4b294bc4c002",
        "s3://pmc-oa-opendata/PMC7025464.1/Image_1.jpg",
        "s3://pmc-oa-opendata/PMC7025464.1/Image_1.jpg?md5=invalid",
        "s3://pmc-oa-opendata/PMC7025464.1/sub/Image_1.jpg?md5=cfcf3840f5c3b45747ab4b294bc4c002",
        "s3://pmc-oa-opendata/PMC7025464.1/Image_1.jpg?md5=cfcf3840f5c3b45747ab4b294bc4c002&other=x"] {
        let mut data = metadata(); data["media_urls"][0] = bad.into(); assert!(bind(data).is_err(), "{bad}");
    }
    let mut data = metadata(); data["media_urls"].as_array_mut().unwrap().remove(0); assert!(bind(data).is_err());
    let mut data = metadata(); let duplicate = data["media_urls"][0].clone(); data["media_urls"].as_array_mut().unwrap().push(duplicate); assert!(bind(data).is_err());
}

#[test]
fn cloud_metadata_rejects_tampered_capture_and_foreign_receipt() {
    for altered_body in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("test/fixtures/pmc-cloud/metadata.json"), dir.path().join("metadata.json")).unwrap();
        let mut receipt: serde_json::Value = serde_json::from_str(include_str!("../../../test/fixtures/pmc-cloud/response.json")).unwrap();
        if altered_body { std::fs::write(dir.path().join("metadata.json"), b"{}").unwrap(); }
        else { receipt["final_url"] = "https://evil.test/metadata/PMC7025464.1.json".into(); }
        std::fs::write(dir.path().join("response.json"), serde_json::to_vec(&receipt).unwrap()).unwrap();
        assert!(cloud::bind(dir.path(), DOI, PAGE, &mut serde_json::json!({}), resources()).is_err());
    }
}
