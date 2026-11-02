//! Public run characterization in isolated processes, with fixture trust
//! confined to the child's current-thread runtime.
use super::*;
use crate::http::publisher_transport_tests::{fixture, Script, override_client};
use std::process::Command;
use std::sync::Arc;
const DOI: &str = "10.3389/fncom.2020.00006";
const LANDING: &str = "https://www.frontiersin.org/article/full";
const IMAGE: &[u8] = b"\x89PNG\r\n\x1a\nfixture-image";

fn html(links: &str) -> String {
    format!("<meta name='citation_doi' content='{DOI}'><section id='supplementary-material'>{links}</section>")
}
fn response(status: &str, mime: &str, bytes: &[u8]) -> Script {
    let mut output = format!("HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).into_bytes();
    output.extend_from_slice(bytes);
    Script::Reply(output)
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "invoked only by the isolated public-run characterization driver"]
async fn child() {
    let case = std::env::var("SUPPLEMENTS_RUN_CASE").unwrap();
    let root = std::path::PathBuf::from(std::env::var_os("SUPPLEMENTS_RUN_ROOT").unwrap());
    if matches!(case.as_str(), "pmc_redirect_other_article" | "pmc_redirect_frontiers") {
        let requested = "https://pmc.ncbi.nlm.nih.gov/articles/PMC7025464/";
        let (final_url, body) = if case == "pmc_redirect_other_article" {
            ("https://pmc.ncbi.nlm.nih.gov/articles/PMC9999999/", format!("<meta name='citation_doi' content='{DOI}'><section><h2>Supplementary Material</h2><a href='/articles/instance/9999999/bin/Image_1.jpg'>Other article asset</a></section>"))
        } else {
            (LANDING, html("<a href='/good.png'>Publisher asset</a>"))
        };
        let redirect = crate::http::publisher_transport_tests::reply("302 Found", &format!("Location: {final_url}\r\n"), b"");
        let (client, server) = fixture(vec![redirect, response("200 OK", "text/html", body.as_bytes())], std::time::Duration::from_secs(2));
        let _override = override_client(client);
        let db = Arc::new(crate::db::Db::open(&root.join("lit.db")).unwrap());
        let ctx = super::super::Context { verbose: false, bib_file: None, bib_stdout: false, json: true, no_cache: false, db };
        let result = super::run(&ctx, DOI, true, Some(&root.join("capture")), Some(requested), None, None).await;
        assert_eq!(server.join().unwrap(), 2);
        if let Err(error) = result { eprintln!("{error}"); std::process::exit(1); }
        return;
    }
    if matches!(case.as_str(), "pmc_capture" | "pmc_cloud_capture") {
        let landing = "https://pmc.ncbi.nlm.nih.gov/articles/PMC7025464/";
        let body = format!("<meta name='citation_doi' content='{DOI}'><section><h2>Supplementary Material</h2><section class='sm'><div class='caption'><span>Supplementary Figure 1</span></div><a href='/articles/instance/7025464/bin/Image_1.jpg'>Data</a></section></section>");
        let source = root.join("input-capture");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("landing.html"), body.as_bytes()).unwrap();
        let original = serde_json::json!({"requested_url":landing,"final_url":landing,"http_status":200,"mime_type":"text/html","byte_length":body.len(),"sha256":hash(body.as_bytes()),"retrieved_at":123});
        std::fs::write(source.join("response.json"), serde_json::to_vec(&original).unwrap()).unwrap();
        // The script permits exactly the asset request. A landing or metadata
        // request would consume it and make the acquisition fail.
        let cloud = case == "pmc_cloud_capture";
        let cloud_dir = root.join("cloud-capture");
        let image = if cloud { b"\xff\xd8\xfffixture-image".as_slice() } else { IMAGE };
        if cloud {
            std::fs::create_dir(&cloud_dir).unwrap();
            let mut data: serde_json::Value = serde_json::from_str(include_str!("../../../test/fixtures/pmc-cloud/metadata.json")).unwrap();
            data["media_urls"][0] = format!("s3://pmc-oa-opendata/PMC7025464.1/Image_1.jpg?md5={}", super::cloud::md5(image)).into();
            let bytes = serde_json::to_vec(&data).unwrap();
            let mut receipt: serde_json::Value = serde_json::from_str(include_str!("../../../test/fixtures/pmc-cloud/response.json")).unwrap();
            receipt["sha256"] = hash(&bytes).into(); receipt["byte_length"] = bytes.len().into();
            std::fs::write(cloud_dir.join("metadata.json"), bytes).unwrap();
            std::fs::write(cloud_dir.join("response.json"), serde_json::to_vec(&receipt).unwrap()).unwrap();
        }
        let (client, server) = fixture(vec![response("200 OK", if cloud { "binary/octet-stream" } else { "image/png" }, image)], std::time::Duration::from_secs(2));
        let _override = override_client(client);
        let db = Arc::new(crate::db::Db::open(&root.join("lit.db")).unwrap());
        let ctx = super::super::Context { verbose: false, bib_file: None, bib_stdout: false, json: true, no_cache: false, db };
        let result = super::run(&ctx, DOI, false, None, None, Some(&source), if cloud { Some(&cloud_dir) } else { None }).await;
        assert_eq!(server.join().unwrap(), 1);
        if let Err(error) = result { eprintln!("{error}"); std::process::exit(1); }
        let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("etc/pdf/owner-with-unrelated-name/supplements/manifest.json")).unwrap()).unwrap();
        if cloud {
            assert_eq!(manifest["discovery"]["sha256"], original["sha256"]);
            assert_eq!(manifest["discovery"]["cloud"]["binding"]["pmcid"], "PMC7025464");
            assert_eq!(manifest["resources"][0]["md5"], super::cloud::md5(image));
        } else { assert_eq!(manifest["discovery"], original); }
        assert_eq!(manifest["resources"][0]["label"], "Supplementary Figure 1");
        assert_eq!(manifest["resources"][0]["status"], "acquired");
        return;
    }
    let body = if case == "denied" { "<html>Access denied</html>".into() }
        else if case == "no_links" { html("") }
        else if case == "recognized_epub" { html("") + "<a href='https://www.frontiersin.org/book/epub'>EPUB</a>" }
        else if case == "unsupported_epub" { format!("<meta name='citation_doi' content='{DOI}'><a href='/relative/epub'>relative</a><a href='https://example.invalid/book/epub'>external</a><a href='https://www.frontiersin.org/first/epub'>first</a><a href='https://www.frontiersin.org/second/epub'>second</a>") }
        else if case == "unsupported_relative_epub" { format!("<meta name='citation_doi' content='{DOI}'><a href='/relative/epub'>EPUB</a>") }
        else if case == "unsupported_xml_abort" { format!("<meta name='citation_doi' content='{DOI}'><meta name='citation_xml_url' content='https://www.frontiersin.org/article/xml'><a href='https://www.frontiersin.org/book/epub'>EPUB</a>") }
        else { html("<a href='/good.png'>Supplementary Figure 1</a><a href='/bad.png'>Supplementary Figure 2</a>") };
    let mut script = vec![response(if case == "denied" { "403 Forbidden" } else { "200 OK" }, "text/html", body.as_bytes())];
    if case == "partial" {
        script.push(response("200 OK", "image/png", IMAGE));
        script.push(response("200 OK", "text/html", b"<html>Login required</html>"));
    }
    if case == "unsupported_epub" { script.push(response("200 OK", "application/epub+zip", b"diagnostic package bytes")); }
    if case == "unsupported_xml_abort" { script.push(response("200 OK", "text/xml", b"\xff")); }
    let expected_requests = script.len();
    let (client, server) = fixture(script, std::time::Duration::from_secs(2));
    let _override = override_client(client);
    let db = Arc::new(crate::db::Db::open(&root.join("lit.db")).unwrap());
    db.cache_set(&format!("supplements_crossref:{DOI}"), "crossref fixture", &serde_json::json!({"message":{"resource":{"primary":{"URL":LANDING}}}}).to_string());
    let ctx = super::super::Context { verbose: false, bib_file: None, bib_stdout: false, json: true, no_cache: false, db };
    let capture_dir = root.join("capture");
    let result = super::run(&ctx, DOI, !matches!(case.as_str(), "partial" | "missing_parent"), if case == "default_discovery" { None } else { Some(&capture_dir) }, None, None, None).await;
    assert_eq!(server.join().unwrap(), expected_requests);
    if let Err(error) = result {
        crate::format::error(&error.to_string());
        std::process::exit(1);
    }
}

fn json_from_stdout(bytes: &[u8]) -> serde_json::Value {
    let start = bytes.iter().position(|&b| b == b'{').expect("public run must print a JSON object");
    serde_json::Deserializer::from_slice(&bytes[start..]).into_iter::<serde_json::Value>().next().unwrap().unwrap()
}

fn reject_changed_pmc_article(case: &str, expected_final_url: &str) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let parent = root.join("etc/pdf/parent");
    std::fs::create_dir_all(&parent).unwrap();
    std::fs::write(parent.join("paper.pdf"), b"main article").unwrap();
    let source = format!("doi: '{DOI}'\n");
    std::fs::write(parent.join("source.yaml"), &source).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "cmd::supplements::run_tests::child", "--ignored", "--nocapture"])
        .env("SUPPLEMENTS_RUN_CASE", case).env("SUPPLEMENTS_RUN_ROOT", root)
        .env("LIT_PROJECT_ROOT", root).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{case}: {stderr}");
    assert!(stderr.contains("PMC article mismatch"), "{case}: {stderr}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("\"resources\""));
    let body = std::fs::read(root.join("capture/landing.html")).unwrap();
    let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("capture/response.json")).unwrap()).unwrap();
    assert_eq!(receipt["requested_url"], "https://pmc.ncbi.nlm.nih.gov/articles/PMC7025464/");
    assert_eq!(receipt["final_url"], expected_final_url);
    assert_eq!(receipt["sha256"], hash(&body));
    assert_eq!(receipt["byte_length"], body.len());
    assert_eq!(receipt["http_status"], 200);
    assert!(receipt["retrieved_at"].as_u64().unwrap() > 0);
    assert!(!parent.join("supplements").exists());
    assert_eq!(std::fs::read(parent.join("paper.pdf")).unwrap(), b"main article");
    assert_eq!(std::fs::read_to_string(parent.join("source.yaml")).unwrap(), source);
}

#[test]
fn live_pmc_discovery_rejects_redirect_to_another_article() {
    reject_changed_pmc_article("pmc_redirect_other_article", "https://pmc.ncbi.nlm.nih.gov/articles/PMC9999999/");
}

#[test]
fn live_pmc_discovery_rejects_redirect_to_frontiers() {
    reject_changed_pmc_article("pmc_redirect_frontiers", LANDING);
}

#[test]
fn public_run_discovery_capture_partial_failure_and_parent_lookup() {
    for case in ["discovery", "no_links", "denied", "partial", "missing_parent", "default_discovery", "recognized_epub", "unsupported_epub", "unsupported_relative_epub", "unsupported_xml_abort", "pmc_capture", "pmc_cloud_capture"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let owner = root.join("etc/pdf/owner-with-unrelated-name");
        let other = root.join("etc/pdf/misleading-name");
        std::fs::create_dir_all(&owner).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(owner.join("paper.pdf"), b"main article").unwrap();
        let source = format!("doi: \"{}\"\nsource_url: \"main provenance\"\n", if case == "missing_parent" { "10.1/different" } else { DOI });
        std::fs::write(owner.join("source.yaml"), &source).unwrap();
        std::fs::write(other.join("source.yaml"), "doi: \"10.1/other\"\n").unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cmd::supplements::run_tests::child", "--ignored", "--nocapture"])
            .env("SUPPLEMENTS_RUN_CASE", case).env("SUPPLEMENTS_RUN_ROOT", root)
            .env("LIT_PROJECT_ROOT", root).output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(if matches!(case, "denied" | "partial" | "missing_parent" | "unsupported_epub" | "unsupported_relative_epub" | "unsupported_xml_abort") { 1 } else { 0 }), "{case}: {stderr}");
        if matches!(case, "pmc_capture" | "pmc_cloud_capture") {
            assert_eq!(std::fs::read(owner.join("paper.pdf")).unwrap(), b"main article");
            assert_eq!(std::fs::read_to_string(owner.join("source.yaml")).unwrap(), source);
            assert!(!other.join("supplements").exists());
            continue;
        }
        if case == "default_discovery" {
            assert_eq!(json_from_stdout(&output.stdout)["status"], "discovered");
            assert!(!root.join("capture").exists(), "default discovery does not persist landing bytes");
            assert!(!owner.join("supplements").exists());
            continue;
        }
        let captured = std::fs::read(root.join("capture/landing.html")).unwrap();
        let provenance: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("capture/response.json")).unwrap()).unwrap();
        assert_eq!(provenance["requested_url"], LANDING);
        assert_eq!(provenance["final_url"], LANDING);
        assert_eq!(provenance["sha256"], hash(&captured));
        assert_eq!(provenance["byte_length"], captured.len());
        assert!(provenance["retrieved_at"].as_u64().unwrap() > 0);
        assert_eq!(provenance["mime_type"], "text/html");
        assert_eq!(std::fs::read(owner.join("paper.pdf")).unwrap(), b"main article");
        assert_eq!(std::fs::read_to_string(owner.join("source.yaml")).unwrap(), source);
        assert!(!other.join("supplements").exists());
        match case {
            "discovery" | "no_links" => {
                assert_eq!(captured, html(if case == "no_links" { "" } else { "<a href='/good.png'>Supplementary Figure 1</a><a href='/bad.png'>Supplementary Figure 2</a>" }).as_bytes());
                let json = json_from_stdout(&output.stdout);
                assert_eq!(json["parent_id"], DOI);
                assert_eq!(json["role"], "supplement");
                assert_eq!(json["discovery"], provenance);
                assert_eq!(json["status"], if case == "no_links" { "no_links" } else { "discovered" });
                assert_eq!(json["resources"].as_array().unwrap().len(), if case == "no_links" { 0 } else { 2 });
                if case == "discovery" {
                    assert_eq!(json["resources"][0], serde_json::json!({"label":"Supplementary Figure 1","declared_url":"/good.png","url":"https://www.frontiersin.org/good.png"}));
                }
                assert!(!owner.join("supplements").exists());
            }
            "denied" => {
                assert_eq!(captured, b"<html>Access denied</html>");
                assert_eq!(provenance["http_status"], 403);
                assert!(stderr.contains("publisher discovery HTTP 403"));
                assert!(!owner.join("supplements").exists());
            }
            "missing_parent" => {
                assert!(stderr.contains("parent article is not cached"));
                assert!(!owner.join("supplements").exists());
            }
            "partial" => {
                let json = json_from_stdout(&output.stdout);
                let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(owner.join("supplements/manifest.json")).unwrap()).unwrap();
                assert_eq!(json, manifest);
                assert_eq!(json["status"], "partial_failure");
                assert_eq!(json["resources"][0]["status"], "acquired");
                assert_eq!(json["resources"][1]["status"], "failed");
                assert!(json["resources"][1]["local_relative_path"].is_null());
                let path = json["resources"][0]["local_relative_path"].as_str().unwrap();
                assert_eq!(std::fs::read(owner.join("supplements").join(path)).unwrap(), IMAGE);
                assert!(stderr.contains("supplement acquisition incomplete"));
                assert!(stderr.contains(&owner.join("supplements/manifest.json").display().to_string()));
            }
            "recognized_epub" => {
                assert_eq!(json_from_stdout(&output.stdout)["status"], "no_links");
                assert!(!root.join("capture/article.epub").exists());
            }
            "unsupported_epub" => {
                assert!(stderr.contains("unsupported supplementary layout"));
                assert_eq!(std::fs::read(root.join("capture/article.epub")).unwrap(), b"diagnostic package bytes");
                let metadata: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("capture/epub-response.json")).unwrap()).unwrap();
                assert_eq!(metadata["requested_url"], "https://www.frontiersin.org/first/epub");
                assert!(!root.join("capture/article.xml").exists(), "an XML link is not required to attempt EPUB capture");
            }
            "unsupported_relative_epub" | "unsupported_xml_abort" => {
                assert!(!root.join("capture/article.epub").exists());
                if case == "unsupported_xml_abort" { assert_eq!(std::fs::read(root.join("capture/article.xml")).unwrap(), b"\xff"); }
            }
            _ => unreachable!(),
        }
    }
}
