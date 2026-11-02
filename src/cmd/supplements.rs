//! Publisher-declared supplementary resources, separate from main artifacts.
use crate::html;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::path::Path;
use crate::http::{PublisherResponse, is_frontiers_url, is_pmc_repository_url};
type Error = Box<dyn std::error::Error>;

#[cfg(test)]
mod run_tests;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Resource {
    pub label: String,
    pub declared_url: String,
    pub url: String,
}

fn discover(body: &str, landing: &str, doi: &str) -> Result<Vec<Resource>, String> {
    let base = Url::parse(landing).map_err(|e| e.to_string())?;
    if pmc_article(&base).is_some() { return discover_pmc(body, &base, doi); }
    if !is_frontiers_url(&base) { return Err("unsupported publisher".into()); }
    let nodes = html::parse(body);
    if !html::meta_all(&nodes, "citation_doi").iter().any(|d| d.eq_ignore_ascii_case(doi)) {
        return Err("publisher page does not confirm requested DOI".into());
    }
    let sections = html::find_all(&nodes, &|e| matches!(e.attr("id"), Some("supplementary-material" | "supplementary_material")));
    if sections.is_empty() { return Err("unsupported supplementary layout".into()); }
    let mut out: Vec<Resource> = Vec::new();
    for section in sections {
        for anchor in html::find_all(&section.children, &|e| e.name == "a" && e.attr("href").is_some()) {
            let declared = anchor.attr("href").unwrap();
            if declared.starts_with('#') { continue; }
            let mut url = base.join(declared).map_err(|e| e.to_string())?;
            if !is_frontiers_url(&url) { return Err(format!("unsupported supplementary URL: {declared}")); }
            url.set_fragment(None);
            if !out.iter().any(|r| r.url == url.as_str()) {
                out.push(Resource { label: anchor.text(), declared_url: declared.into(), url: url.into() });
            }
        }
    }
    Ok(out)
}

fn pmc_article(url: &Url) -> Option<&str> {
    if !is_pmc_repository_url(url) || url.host_str() != Some("pmc.ncbi.nlm.nih.gov") { return None; }
    url.path().strip_prefix("/articles/PMC")?.strip_suffix('/')
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
}

fn discover_pmc(body: &str, base: &Url, doi: &str) -> Result<Vec<Resource>, String> {
    let article = pmc_article(base).ok_or("unsupported PMC article URL")?;
    let nodes = html::parse(body);
    if !html::meta_all(&nodes, "citation_doi").iter().any(|d| d.eq_ignore_ascii_case(doi)) {
        return Err("repository page does not confirm requested DOI".into());
    }
    let sections = html::find_all(&nodes, &|e| matches!(e.name.as_str(), "section" | "div") && e.children.iter().any(|node| {
        matches!(node, html::Node::Element(h) if matches!(h.name.as_str(), "h2" | "h3") && matches!(h.text().as_str(), "Supplementary Material" | "Supplementary Materials"))
    }));
    if sections.is_empty() { return Err("unsupported supplementary layout".into()); }
    let mut out: Vec<Resource> = Vec::new();
    for section in sections {
        let groups = html::find_all(&section.children, &|e| e.name == "section" && e.has_class("sm"));
        let groups = if groups.is_empty() { vec![section] } else { groups };
        for group in groups {
            let label = html::find(&group.children, &|e| e.name == "div" && e.has_class("caption"))
                .and_then(|caption| html::find(&caption.children, &|e| e.name == "span")).map(|span| span.text());
            for anchor in html::find_all(&group.children, &|e| e.name == "a" && e.attr("href").is_some()) {
                let declared = anchor.attr("href").unwrap();
                if declared.starts_with('#') { continue; }
                let mut url = base.join(declared).map_err(|e| e.to_string())?;
                if is_frontiers_url(&url) { continue; } // The section also links the publisher's landing page.
                let same_article = [format!("/articles/instance/{article}/bin/"), format!("/articles/PMC{article}/bin/")]
                    .iter().any(|prefix| url.path().starts_with(prefix));
                if !is_pmc_repository_url(&url) || url.host_str() != base.host_str() || !same_article {
                    return Err(format!("unsupported supplementary URL for PMC{article}: {declared}"));
                }
                url.set_fragment(None);
                if !out.iter().any(|r| r.url == url.as_str()) {
                    out.push(Resource { label: label.clone().unwrap_or_else(|| anchor.text()), declared_url: declared.into(), url: url.into() });
                }
            }
        }
    }
    Ok(out)
}

fn hash(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}
fn xml_resources(body: &str, document_url: &str) -> Result<Vec<Resource>, String> {
    fn walk(nodes: &[html::Node], base: &Url, output: &mut Vec<Resource>) -> Result<(), String> {
        for node in nodes {
            if let html::Node::Element(element) = node {
                let base = match element.attr("xml:base") {
                    Some(value) => base.join(value).map_err(|e| e.to_string())?,
                    None => base.clone(),
                };
                if element.name == "supplementary-material" {
                    let declared = element.attr("xlink:href").ok_or("supplementary XML resource lacks xlink:href")?;
                    let url = base.join(declared).map_err(|e| e.to_string())?;
                    if !is_frontiers_url(&url) { return Err(format!("unsupported supplementary XML URL: {url}")); }
                    let label = html::find(&element.children, &|e| e.name == "label").map(|e| e.text()).unwrap_or_default();
                    output.push(Resource { label, declared_url: declared.into(), url: url.into() });
                }
                walk(&element.children, &base, output)?;
            }
        }
        Ok(())
    }
    let mut output = Vec::new();
    walk(&html::parse(body), &Url::parse(document_url).map_err(|e| e.to_string())?, &mut output)?;
    Ok(output)
}
fn now() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() }
fn provenance(url: &str, response: &PublisherResponse) -> serde_json::Value {
    serde_json::json!({"requested_url":url,"final_url":response.final_url,"http_status":response.status,"mime_type":response.mime,"byte_length":response.bytes.len(),"sha256":hash(&response.bytes),"retrieved_at":now()})
}
fn capture(dir: &Path, response: &PublisherResponse, provenance: &serde_json::Value) -> Result<(), Error> {
    std::fs::create_dir_all(dir)?;
    atomic(&dir.join("landing.html"), &response.bytes)?;
    atomic(&dir.join("response.json"), &serde_json::to_vec_pretty(provenance)?)?;
    Ok(())
}
fn load_discovery(dir: &Path) -> Result<(PublisherResponse, serde_json::Value), Error> {
    let bytes = std::fs::read(dir.join("landing.html"))?;
    let metadata: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("response.json"))?)?;
    if metadata["sha256"].as_str() != Some(hash(&bytes).as_str()) || metadata["byte_length"].as_u64() != Some(bytes.len() as u64) {
        return Err("discovery capture hash or length mismatch".into());
    }
    let requested = Url::parse(metadata["requested_url"].as_str().ok_or("discovery capture lacks requested URL")?)?;
    let final_url = Url::parse(metadata["final_url"].as_str().ok_or("discovery capture lacks final URL")?)?;
    if let Some(article) = pmc_article(&requested) {
        if pmc_article(&final_url) != Some(article) { return Err("discovery capture PMC article mismatch".into()); }
    } else if !is_frontiers_url(&requested) || !is_frontiers_url(&final_url) {
        return Err("unsupported discovery capture URL".into());
    }
    let status = metadata["http_status"].as_u64().filter(|n| *n <= u16::MAX as u64).ok_or("discovery capture lacks HTTP status")? as u16;
    let mime = metadata["mime_type"].as_str().ok_or("discovery capture lacks MIME type")?.to_string();
    if metadata["retrieved_at"].as_u64().is_none() { return Err("discovery capture lacks retrieval time".into()); }
    Ok((PublisherResponse { final_url: final_url.into(), status, mime, bytes }, metadata))
}
fn atomic(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    use std::io::Write;
    let temporary = path.with_extension(format!("{}.{}.tmp", std::process::id(), now()));
    let result = (|| -> Result<(), Error> {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() { let _ = std::fs::remove_file(temporary); }
    result
}
fn validated_extension(mime: &str, bytes: &[u8]) -> Result<&'static str, String> {
    let mime = mime.split(';').next().unwrap_or("").trim();
    let valid = match mime {
        "application/pdf" if bytes.starts_with(b"%PDF-") => "pdf",
        "image/png" if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => "png",
        "image/jpeg" if bytes.starts_with(b"\xff\xd8\xff") => "jpg",
        "image/tiff" if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") => "tiff",
        "image/gif" if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") => "gif",
        _ => return Err(format!("unsupported or mismatched supplementary MIME/magic: {mime}")),
    };
    Ok(valid)
}

pub async fn run(ctx: &super::Context, input: &str, url_only: bool, capture_dir: Option<&Path>, repository_page: Option<&str>, from_discovery: Option<&Path>) -> Result<(), Error> {
    use super::artifact::{classify, Target, owner_dir};
    let Target::Doi(doi) = classify(input) else { return Err("supplements require a DOI".into()); };
    let client = ctx.client();
    let (response, discovery) = if let Some(dir) = from_discovery {
        load_discovery(dir)?
    } else {
        let landing = if let Some(page) = repository_page {
            if pmc_article(&Url::parse(page)?).is_none() { return Err(format!("unsupported PMC article URL: {page}").into()); }
            page.to_string()
        } else {
            let metadata_url = crate::api::crossref::doi_url(&doi);
            let body = client.get_cached(&format!("supplements_crossref:{doi}"), &metadata_url, crate::db::TTL_DOI).await?;
            let metadata: serde_json::Value = serde_json::from_str(&body)?;
            let page = metadata.pointer("/message/resource/primary/URL").and_then(|v| v.as_str())
                .ok_or("unsupported publisher: metadata has no primary landing URL")?;
            if !is_frontiers_url(&Url::parse(page)?) { return Err(format!("unsupported publisher landing URL: {page}").into()); }
            page.to_string()
        };
        let landing = landing.as_str();
        let response = client.get_publisher(landing).await?;
        let discovery = provenance(landing, &response);
        if let Some(dir) = capture_dir { capture(dir, &response, &discovery)?; }
        (response, discovery)
    };
    if !(200..300).contains(&response.status) { return Err(format!("publisher discovery HTTP {} for {}", response.status, response.final_url).into()); }
    if !response.mime.starts_with("text/html") { return Err(format!("unsupported publisher page MIME: {}", response.mime).into()); }
    if let Some(requested) = repository_page {
        let requested_url = Url::parse(requested)?;
        let final_url = Url::parse(&response.final_url)?;
        if pmc_article(&final_url) != pmc_article(&requested_url) {
            return Err(format!("PMC article mismatch: requested {requested}, received {}", response.final_url).into());
        }
    }
    let landing_body = std::str::from_utf8(&response.bytes)?;
    let resources = match discover(landing_body, &response.final_url, &doi) {
        Ok(resources) => resources,
        Err(error) => {
            // Preserve the publisher-declared XML as supplementary discovery
            // evidence when explicitly requested. Bare package filenames do
            // not establish public download URLs.
            if let Some(dir) = capture_dir {
                let nodes = html::parse(landing_body);
                if let Some(xml_url) = html::meta_all(&nodes, "citation_xml_url").first() {
                    let xml = client.get_publisher(xml_url).await?;
                    atomic(&dir.join("article.xml"), &xml.bytes)?;
                    atomic(&dir.join("xml-response.json"), &serde_json::to_vec_pretty(&provenance(xml_url, &xml))?)?;
                    if (200..300).contains(&xml.status) && xml.mime.starts_with("text/xml") {
                        let resources = xml_resources(std::str::from_utf8(&xml.bytes)?, &xml.final_url)?;
                        atomic(&dir.join("xml-declared-resources.json"), &serde_json::to_vec_pretty(&resources)?)?;
                        for (index, resource) in resources.iter().enumerate() {
                            match client.get_publisher(&resource.url).await {
                                Ok(response) => {
                                    atomic(&dir.join(format!("xml-resource-{}.body", index + 1)), &response.bytes)?;
                                    atomic(&dir.join(format!("xml-resource-{}.json", index + 1)), &serde_json::to_vec_pretty(&provenance(&resource.url, &response))?)?;
                                }
                                Err(error) => atomic(&dir.join(format!("xml-resource-{}.json", index + 1)), &serde_json::to_vec_pretty(&serde_json::json!({"requested_url":resource.url,"error":error.to_string(),"retrieved_at":now()}))?)?,
                            }
                        }
                    }
                }
                if let Some(epub) = html::find_all(&nodes, &|e| e.name == "a").into_iter()
                    .filter_map(|e| e.attr("href")).find(|url| Url::parse(url).is_ok_and(|u| is_frontiers_url(&u) && u.path().ends_with("/epub"))) {
                    let package = client.get_publisher(epub).await?;
                    atomic(&dir.join("article.epub"), &package.bytes)?;
                    atomic(&dir.join("epub-response.json"), &serde_json::to_vec_pretty(&provenance(epub, &package))?)?;
                }
            }
            return Err(error.into());
        }
    };
    if url_only {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({"parent_id":doi,"role":"supplement","status":if resources.is_empty() {"no_links"} else {"discovered"},"discovery":discovery,"resources":resources}))?);
        return Ok(());
    }
    let parent = owner_dir(&super::read::find_pdf_base()?, &Target::Doi(doi.clone()))?
        .ok_or("parent article is not cached; download the main article first")?;
    let manifest = acquire(&parent, &doi, discovery, resources, |url| {
        let client = &client;
        async move { client.get_publisher(&url).await }
    }).await?;
    let manifest_path = parent.join("supplements/manifest.json");
    if ctx.json { println!("{}", serde_json::to_string_pretty(&manifest)?); }
    if manifest["status"] == "partial_failure" {
        return Err(format!("supplement acquisition incomplete; inspect {}", manifest_path.display()).into());
    }
    if !ctx.json { println!("Acquired supplementary resources: {}", manifest_path.display()); }
    Ok(())
}

async fn acquire<F, Fut>(parent: &Path, doi: &str, discovery: serde_json::Value, resources: Vec<Resource>, fetch: F) -> Result<serde_json::Value, Error>
where F: Fn(String) -> Fut, Fut: std::future::Future<Output = Result<PublisherResponse, Error>> {
    let dir = parent.join("supplements");
    std::fs::create_dir_all(&dir)?;
    let _lock = match super::lock::acquire(&dir, &doi)? {
        super::lock::Acquired::Locked(lock) => lock,
        super::lock::Acquired::Held(_) => return Err("supplement acquisition already running".into()),
    };
    let manifest_path = dir.join("manifest.json");
    let previous: serde_json::Value = match std::fs::read(&manifest_path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::Value::Null,
        Err(e) => return Err(e.into()),
    };
    let mut records = Vec::new();
    let mut failed = false;
    for resource in resources {
        if let Some(old) = previous.get("resources").and_then(|v| v.as_array()).and_then(|r| r.iter().find(|r| r["url"] == resource.url && r["status"] == "acquired")) {
            if let (Some(path), Some(expected)) = (old["local_relative_path"].as_str(), old["sha256"].as_str()) {
                // Only reuse our content-addressed single-component filenames.
                if Path::new(path).components().count() == 1 && path.starts_with("resource-") {
                    if std::fs::read(dir.join(path)).is_ok_and(|b| hash(&b) == expected) {
                        records.push(old.clone()); continue;
                    }
                }
            }
        }
        let mut record = serde_json::to_value(&resource)?;
        record["role"] = "supplement".into();
        record["requested_url"] = resource.url.clone().into();
        record["attempted_at"] = now().into();
        let acquired = async {
            let response = fetch(resource.url.clone()).await?;
            // Receipt, validation and storage are separate facts. A complete
            // rejected response still has provenance, even without a local file.
            for (key, value) in provenance(&resource.url, &response).as_object().unwrap() {
                record[key] = value.clone();
            }
            if !(200..300).contains(&response.status) { return Err(format!("HTTP {} for {}", response.status, response.final_url).into()); }
            let extension = validated_extension(&response.mime, &response.bytes)?;
            let digest = hash(&response.bytes);
            let filename = format!("resource-{digest}.{extension}");
            atomic(&dir.join(&filename), &response.bytes)?;
            Ok::<_, Error>(filename)
        }.await;
        match acquired {
            Ok(filename) => {
                record["local_relative_path"] = filename.into();
                record["status"] = "acquired".into();
            }
            Err(err) => {
                if let Some((url, status, mime)) = crate::http::publisher_failure_headers(err.as_ref()) {
                    record["final_url"] = url.into();
                    record["http_status"] = status.into();
                    record["mime_type"] = mime.into();
                }
                failed = true;
                record["status"] = "failed".into();
                record["failed_at"] = now().into();
                record["error"] = err.to_string().into();
            }
        }
        records.push(record);
        let manifest = serde_json::json!({"schema_version":1,"parent_id":doi,"role":"supplement","discovery":discovery,"status":"in_progress","resources":records});
        atomic(&manifest_path, &serde_json::to_vec_pretty(&manifest)?)?;
    }
    let manifest = serde_json::json!({"schema_version":1,"parent_id":doi,"role":"supplement","discovery":discovery,"status":if failed {"partial_failure"} else if records.is_empty() {"no_links"} else {"complete"},"resources":records});
    atomic(&manifest_path, &serde_json::to_vec_pretty(&manifest)?)?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    const LANDING: &str = "https://www.frontiersin.org/articles/10.3389/fncom.2020.00006/full";
    const DOI: &str = "10.3389/fncom.2020.00006";
    #[test]
    fn pmc_supplements_use_matching_doi_and_same_article_declared_links() {
        // Synthetic layout: URLs are test inputs, not claims about live resources.
        let landing = "https://pmc.ncbi.nlm.nih.gov/articles/PMC7025464/";
        let body = format!("<meta name='citation_doi' content='{DOI}'><section id='supp'><h2>Supplementary Material</h2><p>Supplementary Figure 1</p><a href='/articles/instance/7025464/bin/Image_1.jpg'>Additional data</a><a href='/articles/instance/7025464/bin/Image_1.jpg'>Duplicate</a><a href='https://www.frontiersin.org/articles/{DOI}/full'>Publisher</a></section>");
        let resources = discover(&body, landing, DOI).unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].declared_url, "/articles/instance/7025464/bin/Image_1.jpg");
        assert_eq!(resources[0].url, "https://pmc.ncbi.nlm.nih.gov/articles/instance/7025464/bin/Image_1.jpg");
        assert!(discover(&body, landing, "10.1/wrong").is_err());
        for wrong in ["https://pmc.ncbi.nlm.nih.gov.evil.test/articles/PMC7025464/", "http://pmc.ncbi.nlm.nih.gov/articles/PMC7025464/"] {
            assert!(discover(&body, wrong, DOI).is_err());
        }
        assert!(discover(&body.replace("instance/7025464", "instance/9999999"), landing, DOI).is_err());
        assert!(discover(&body.replace("/articles/instance/7025464/bin/Image_1.jpg", "https://evil.test/Image_1.jpg"), landing, DOI).is_err());
        assert!(discover(&format!("<meta name='citation_doi' content='{DOI}'>"), landing, DOI).is_err());
    }
    #[test]
    fn captured_pmc_page_preserves_all_seven_publisher_labels_and_literal_links() {
        let body = include_str!("../../test/fixtures/pmc/landing.html");
        let receipt: serde_json::Value = serde_json::from_str(include_str!("../../test/fixtures/pmc/response.json")).unwrap();
        assert_eq!(hash(body.as_bytes()), receipt["sha256"]);
        assert_eq!(body.len(), receipt["byte_length"]);
        let links = discover(body, receipt["final_url"].as_str().unwrap(), DOI).unwrap();
        assert_eq!(links.len(), 7);
        for (index, resource) in links.iter().enumerate() {
            assert_eq!(resource.label, format!("Supplementary Figure {}", index + 1));
            let ext = if index == 3 { "png" } else { "jpg" };
            assert_eq!(resource.declared_url, format!("/articles/instance/7025464/bin/Image_{}.{ext}", index + 1));
        }
    }
    #[test]
    fn captured_pmc_discovery_rejects_changed_article_url_and_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let body = include_bytes!("../../test/fixtures/pmc/landing.html");
        let mut receipt: serde_json::Value = serde_json::from_str(include_str!("../../test/fixtures/pmc/response.json")).unwrap();
        std::fs::write(temp.path().join("landing.html"), body).unwrap();
        std::fs::write(temp.path().join("response.json"), serde_json::to_vec(&receipt).unwrap()).unwrap();
        let (response, original) = load_discovery(temp.path()).unwrap();
        assert_eq!(response.bytes, body);
        assert_eq!(original, receipt);
        receipt["requested_url"] = "https://pmc.ncbi.nlm.nih.gov/articles/PMC9999999/".into();
        std::fs::write(temp.path().join("response.json"), serde_json::to_vec(&receipt).unwrap()).unwrap();
        assert!(load_discovery(temp.path()).unwrap_err().to_string().contains("article mismatch"));
        receipt["requested_url"] = receipt["final_url"].clone();
        std::fs::write(temp.path().join("response.json"), serde_json::to_vec(&receipt).unwrap()).unwrap();
        std::fs::write(temp.path().join("landing.html"), b"tampered").unwrap();
        assert!(load_discovery(temp.path()).unwrap_err().to_string().contains("hash or length mismatch"));
    }
    #[test]
    fn xml_declared_relative_uris_resolve_against_document_url() {
        let xml = "<article><supplementary-material xlink:href='Image_1.jpg'><label>Supplementary Figure 1</label></supplementary-material></article>";
        let links = xml_resources(xml, "https://www.frontiersin.org/articles/10.1/test/xml").unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].declared_url, "Image_1.jpg");
        assert_eq!(links[0].url, "https://www.frontiersin.org/articles/10.1/test/Image_1.jpg");
        assert_eq!(links[0].label, "Supplementary Figure 1");
    }
    #[test]
    fn xml_base_is_inherited_and_unsafe_resource_hosts_are_rejected() {
        let xml = "<article xml:base='https://www.frontiersin.org/files/'><sec xml:base='supp/'><supplementary-material xlink:href='one.jpg'><label>One</label></supplementary-material></sec></article>";
        assert_eq!(xml_resources(xml, LANDING).unwrap()[0].url, "https://www.frontiersin.org/files/supp/one.jpg");
        let unsafe_xml = xml.replace("https://www.frontiersin.org/files/", "https://evil.test/");
        assert!(xml_resources(&unsafe_xml, LANDING).is_err());
    }
    #[test]
    fn captured_official_page_requires_further_discovery_and_is_not_no_links() {
        let body = include_str!("../../test/fixtures/frontiers/landing.html");
        let provenance: serde_json::Value = serde_json::from_str(include_str!("../../test/fixtures/frontiers/response.json")).unwrap();
        assert_eq!(hash(body.as_bytes()), provenance["sha256"]);
        assert_eq!(discover(body, LANDING, DOI).unwrap_err(), "unsupported supplementary layout");
        let doc = html::parse(body);
        let captions = html::find_all(&doc, &|e| e.has_class("SupplementaryMaterial"));
        assert_eq!(captions.len(), 7);
        for (index, element) in captions.iter().enumerate() {
            assert_eq!(element.attr("id").unwrap(), format!("SM{}", index + 1));
        }
        let xml = include_str!("../../test/fixtures/frontiers/article.xml");
        let metadata: serde_json::Value = serde_json::from_str(include_str!("../../test/fixtures/frontiers/xml-response.json")).unwrap();
        assert_eq!(hash(xml.as_bytes()), metadata["sha256"]);
        let resources = xml_resources(xml, metadata["final_url"].as_str().unwrap()).unwrap();
        assert_eq!(resources.len(), 7);
        for (index, resource) in resources.iter().enumerate() {
            let ext = if index == 3 { "png" } else { "jpg" };
            assert_eq!(resource.declared_url, format!("Image_{}.{ext}", index + 1));
            assert_eq!(resource.label, format!("Supplementary Figure {}", index + 1));
        }
    }
    fn resource(name: &str) -> Resource {
        Resource { label: name.into(), declared_url: name.into(), url: format!("https://www.frontiersin.org/{name}") }
    }
    fn response(url: String, bytes: &[u8]) -> PublisherResponse {
        PublisherResponse { final_url: url, status: 200, mime: "image/png".into(), bytes: bytes.into() }
    }
    async fn rejected_response(status: u16, mime: &str, body: &[u8], block_storage: bool) -> serde_json::Value {
        let temp = tempfile::tempdir().unwrap();
        if block_storage {
            std::fs::create_dir_all(temp.path().join("supplements").join(format!("resource-{}.png", hash(body)))).unwrap();
        }
        let result = acquire(temp.path(), DOI, serde_json::json!({}), vec![resource("declared.png")], |_| {
            std::future::ready(Ok(PublisherResponse { final_url: "https://www.frontiersin.org/redirected.png".into(), status, mime: mime.into(), bytes: body.into() }))
        }).await.unwrap();
        let record = result["resources"][0].clone();
        assert_eq!(record["status"], "failed");
        assert_eq!(record["requested_url"], "https://www.frontiersin.org/declared.png");
        assert_eq!(record["final_url"], "https://www.frontiersin.org/redirected.png");
        assert_eq!(record["http_status"], status);
        assert_eq!(record["mime_type"], mime);
        assert_eq!(record["byte_length"], body.len());
        assert_eq!(record["sha256"], hash(body));
        assert!(record["retrieved_at"].as_u64().is_some());
        assert!(record["attempted_at"].as_u64().is_some());
        assert!(record["failed_at"].as_u64().is_some());
        assert!(record["local_relative_path"].is_null());
        record
    }
    #[tokio::test]
    async fn acquisition_http_denial_retains_complete_response_provenance() {
        assert!(rejected_response(403, "text/html", b"<html>Forbidden</html>", false).await["error"].as_str().unwrap().contains("403"));
    }
    #[tokio::test]
    async fn acquisition_mime_rejection_retains_complete_response_provenance() {
        assert!(rejected_response(200, "text/html", b"<html>Login required</html>", false).await["error"].as_str().unwrap().contains("MIME"));
    }
    #[tokio::test]
    async fn acquisition_storage_failure_retains_complete_response_provenance() {
        assert!(!rejected_response(200, "image/png", b"\x89PNG\r\n\x1a\nimage", true).await["error"].as_str().unwrap().is_empty());
    }
    #[tokio::test]
    async fn acquisition_no_response_failure_records_attempt_without_inventing_response() {
        let temp = tempfile::tempdir().unwrap();
        let result = acquire(temp.path(), DOI, serde_json::json!({}), vec![resource("absent.png")], |_| {
            std::future::ready(Err("connection failed before response".into()))
        }).await.unwrap();
        let record = &result["resources"][0];
        assert_eq!(record["requested_url"], "https://www.frontiersin.org/absent.png");
        assert!(record["attempted_at"].as_u64().is_some());
        assert!(record["failed_at"].as_u64().is_some());
        for field in ["final_url", "http_status", "mime_type", "byte_length", "sha256", "retrieved_at", "local_relative_path"] {
            assert!(record.get(field).is_none(), "unavailable {field} must not be fabricated");
        }
    }
    #[tokio::test]
    async fn acquisition_incomplete_redirect_and_timeout_preserve_only_known_headers() {
        use crate::http::publisher_transport_tests::{fixture, reply, Script, fetch};
        for timeout in [false, true] {
            let script = if timeout { vec![Script::DelayBody] } else {
                vec![reply("302 Found", "Location: https://www.frontiersin.org/final.png\r\n", b""),
                    Script::Reply(b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 100\r\nConnection: close\r\n\r\npartial".to_vec())]
            };
            let count = script.len();
            let (client, server) = fixture(script, std::time::Duration::from_millis(if timeout { 75 } else { 2000 }));
            let temp = tempfile::tempdir().unwrap();
            let result = acquire(temp.path(), DOI, serde_json::json!({}), vec![resource("original.png")], |url| {
                let client = &client;
                async move { fetch(client, &url).await }
            }).await.unwrap();
            assert_eq!(server.join().unwrap(), count);
            let record = &result["resources"][0];
            assert_eq!(record["status"], "failed");
            assert_eq!(record["final_url"], if timeout { "https://www.frontiersin.org/original.png" } else { "https://www.frontiersin.org/final.png" });
            assert_eq!(record["http_status"], 200);
            assert_eq!(record["mime_type"], if timeout { "" } else { "image/png" });
            for field in ["sha256", "byte_length", "retrieved_at", "local_relative_path"] { assert!(record.get(field).is_none(), "{field} requires a complete stored/received body"); }
            assert!(record["attempted_at"].as_u64().is_some());
            assert!(record["failed_at"].as_u64().is_some());
        }
    }
    #[tokio::test]
    async fn acquisition_exhausted_http_retry_retains_last_response_headers() {
        use crate::http::publisher_transport_tests::{fixture, reply, fetch};
        let (client, server) = fixture(vec![reply("429 Too Many Requests", "Retry-After: 0\r\n", b"rate limited")], std::time::Duration::from_secs(2));
        let temp = tempfile::tempdir().unwrap();
        let result = acquire(temp.path(), DOI, serde_json::json!({}), vec![resource("limited.png")], |url| {
            let client = &client;
            async move { fetch(client, &url).await }
        }).await.unwrap();
        assert_eq!(server.join().unwrap(), 1);
        let record = &result["resources"][0];
        assert_eq!(record["http_status"], 429);
        assert_eq!(record["final_url"], "https://www.frontiersin.org/limited.png");
        assert_eq!(record["mime_type"], "image/png");
        // The retry path did not consume this body, so complete bytes are unknown.
        assert!(record.get("byte_length").is_none());
        assert!(record.get("sha256").is_none());
    }
    #[tokio::test]
    async fn acquisition_over_real_transport_preserves_success_and_rejects_partial_response() {
        use crate::http::publisher_transport_tests::{fixture, reply, Script, fetch};
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("paper.pdf"), b"main").unwrap();
        std::fs::write(temp.path().join("source.yaml"), b"main provenance").unwrap();
        let broken = b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 100\r\nConnection: close\r\n\r\npartial";
        let (client, server) = fixture(vec![reply("200 OK", "", b"\x89PNG\r\n\x1a\nimage"), Script::Reply(broken.to_vec())], std::time::Duration::from_secs(2));
        let result = acquire(temp.path(), DOI, serde_json::json!({}), vec![resource("good.png"), resource("bad.png")], |url| {
            let client = &client;
            async move { fetch(client, &url).await }
        }).await.unwrap();
        assert_eq!(server.join().unwrap(), 2);
        assert_eq!(result["status"], "partial_failure");
        assert_eq!(result["resources"][0]["status"], "acquired");
        assert_eq!(result["resources"][1]["status"], "failed");
        assert!(result["resources"][1]["error"].as_str().unwrap().contains("https://www.frontiersin.org/bad.png"));
        assert!(result["resources"][1]["local_relative_path"].is_null());
        let failed = &result["resources"][1];
        assert_eq!(failed["final_url"], "https://www.frontiersin.org/bad.png");
        assert_eq!(failed["http_status"], 200);
        assert_eq!(failed["mime_type"], "image/png");
        assert!(failed.get("byte_length").is_none());
        assert!(failed.get("sha256").is_none());
        assert_eq!(std::fs::read_dir(temp.path().join("supplements")).unwrap().count(), 2);
        assert_eq!(std::fs::read(temp.path().join("paper.pdf")).unwrap(), b"main");
        assert_eq!(std::fs::read(temp.path().join("source.yaml")).unwrap(), b"main provenance");
    }
    #[tokio::test]
    async fn acquisition_keeps_partial_success_and_retries_only_failed_or_corrupt_files() {
        use std::cell::Cell;
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("paper.pdf"), b"main").unwrap();
        std::fs::write(temp.path().join("source.yaml"), b"provenance").unwrap();
        let resources = vec![resource("a/data.png"), resource("b/data.png")];
        let calls = Cell::new(0);
        let first = acquire(temp.path(), DOI, serde_json::json!({}), resources.clone(), |url| {
            calls.set(calls.get() + 1);
            std::future::ready(if url.contains("/b/") { Err("timeout".into()) } else { Ok(response(url, b"\x89PNG\r\n\x1a\none")) })
        }).await.unwrap();
        assert_eq!(first["status"], "partial_failure");
        assert_eq!(first["resources"][0]["status"], "acquired");
        assert_eq!(first["resources"][1]["error"], "timeout");
        assert_eq!(calls.get(), 2);
        let second = acquire(temp.path(), DOI, serde_json::json!({}), resources.clone(), |url| {
            calls.set(calls.get() + 1);
            std::future::ready(Ok(response(url, b"\x89PNG\r\n\x1a\ntwo")))
        }).await.unwrap();
        assert_eq!(calls.get(), 3);
        assert_eq!(second["status"], "complete");
        let left = second["resources"][0]["local_relative_path"].as_str().unwrap();
        let right = second["resources"][1]["local_relative_path"].as_str().unwrap();
        assert_ne!(left, right, "same basename must not collide");
        let third = acquire(temp.path(), DOI, serde_json::json!({}), resources.clone(), |_| {
            panic!("idempotent rerun must not fetch an intact artifact");
            #[allow(unreachable_code)] std::future::ready(Err("unexpected".into()))
        }).await.unwrap();
        assert_eq!(third["resources"], second["resources"]);
        std::fs::write(temp.path().join("supplements").join(left), "corrupt").unwrap();
        acquire(temp.path(), DOI, serde_json::json!({}), resources, |url| {
            calls.set(calls.get() + 1);
            std::future::ready(Ok(response(url, b"\x89PNG\r\n\x1a\none")))
        }).await.unwrap();
        assert_eq!(calls.get(), 4, "hash mismatch must refetch only that artifact");
        assert_eq!(std::fs::read(temp.path().join("paper.pdf")).unwrap(), b"main");
        assert_eq!(std::fs::read(temp.path().join("source.yaml")).unwrap(), b"provenance");
    }
    #[tokio::test]
    async fn acquisition_records_html_disguised_as_image_as_failure() {
        let temp = tempfile::tempdir().unwrap();
        let result = acquire(temp.path(), DOI, serde_json::json!({}), vec![resource("a.png")], |url| {
            std::future::ready(Ok(response(url, b"<html>login</html>")))
        }).await.unwrap();
        assert_eq!(result["status"], "partial_failure");
        assert_eq!(result["resources"][0]["status"], "failed");
        assert!(result["resources"][0]["local_relative_path"].is_null());
        assert_eq!(std::fs::read_dir(temp.path().join("supplements")).unwrap().count(), 1);
    }
    #[test]
    fn mime_magic_validation_rejects_html_and_disagreement() {
        for (mime, body, extension) in [
            ("application/pdf", b"%PDF-1.4".as_slice(), "pdf"),
            ("image/png", b"\x89PNG\r\n\x1a\n".as_slice(), "png"),
            ("image/jpeg", b"\xff\xd8\xff".as_slice(), "jpg"),
        ] {
            assert_eq!(validated_extension(mime, body).unwrap(), extension);
            assert!(validated_extension(mime, b"<html>login</html>").is_err());
        }
        assert!(validated_extension("image/jpeg", b"%PDF-1.4").is_err());
        assert!(validated_extension("text/html", b"%PDF-1.4").is_err());
    }
    #[test]
    fn content_hash_is_sha256_and_atomic_write_leaves_main_intact() {
        assert_eq!(hash(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("paper.pdf"), b"main").unwrap();
        std::fs::write(temp.path().join("source.yaml"), b"main provenance").unwrap();
        let dir = temp.path().join("supplements");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join(format!("resource-{}.pdf", hash(b"supplement")));
        atomic(&path, b"supplement").unwrap();
        atomic(&path, b"supplement").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"supplement");
        assert_eq!(std::fs::read(temp.path().join("paper.pdf")).unwrap(), b"main");
        assert_eq!(std::fs::read(temp.path().join("source.yaml")).unwrap(), b"main provenance");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }
    // Synthetic markup, not evidence about any actual publisher resource.
    fn page(links: &str) -> String {
        format!("<meta name='citation_doi' content='{DOI}'><section id='supplementary-material'>{links}</section>")
    }
    #[test]
    fn synthetic_declared_links_preserve_labels_and_deduplicate() {
        let links = "<a href='/files/a.pdf'>Supplementary Data 1</a><a href='/files/a.pdf'>duplicate</a><a href='https://www.frontiersin.org/files/b.png'>Figure 2</a>";
        let got = discover(&page(links), LANDING, DOI).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].label, "Supplementary Data 1");
        assert_eq!(got[0].url, "https://www.frontiersin.org/files/a.pdf");
        assert_eq!(got[0].declared_url, "/files/a.pdf");
    }
    #[test]
    fn synthetic_no_links_is_distinct_from_unrecognized_page() {
        assert!(discover(&page(""), LANDING, DOI).unwrap().is_empty());
        assert!(discover("<html>Access denied</html>", LANDING, DOI).is_err());
        assert!(discover("<html>article without supported section</html>", LANDING, DOI).is_err());
        assert!(discover(&page(""), LANDING, "10.3389/wrong").is_err());
    }
    #[test]
    fn synthetic_unsafe_links_fail_instead_of_silently_disappearing() {
        for link in ["javascript:x", "https://evil.test/a.pdf", "http://www.frontiersin.org/a.pdf", "https://user@www.frontiersin.org/a.pdf"] {
            assert!(discover(&page(&format!("<a href='{link}'>Data</a>")), LANDING, DOI).is_err());
        }
    }
}
