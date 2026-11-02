//! Match official PMC Cloud media to supplements already declared by an article.
use super::*;
use crate::http::is_pmc_cloud_url;

pub(super) fn md5(bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    format!("{:x}", Md5::digest(bytes))
}

pub(super) fn bind(dir: &Path, doi: &str, landing: &str, discovery: &mut serde_json::Value, resources: Vec<Resource>) -> Result<Vec<Resource>, Error> {
    let article_url = Url::parse(landing)?;
    let article = pmc_article(&article_url).ok_or("Cloud metadata requires a PMC article discovery capture")?;
    let (response, receipt) = load_capture(dir, "metadata.json")?;
    if response.status != 200 || !matches!(response.mime.split(';').next().unwrap_or("").trim(), "application/json" | "binary/octet-stream" | "application/octet-stream") {
        return Err("unsupported PMC Cloud metadata response".into());
    }
    let data: serde_json::Value = serde_json::from_slice(&response.bytes)?;
    let version = data["version"].as_u64().filter(|n| *n > 0).ok_or("PMC Cloud metadata lacks a positive version")?;
    let pmcid = format!("PMC{article}");
    if data["pmcid"].as_str() != Some(pmcid.as_str()) || !data["doi"].as_str().is_some_and(|v| v.eq_ignore_ascii_case(doi)) {
        return Err("PMC Cloud DOI or article mismatch".into());
    }
    if data["is_pmc_openaccess"] != true || data["is_manuscript"] != false || data["is_retracted"] != false
        || !data["license_code"].as_str().is_some_and(|v| !v.trim().is_empty()) {
        return Err("PMC Cloud metadata does not confirm a licensed open access published article".into());
    }
    let metadata_url = format!("https://pmc-oa-opendata.s3.amazonaws.com/metadata/{pmcid}.{version}.json");
    if receipt["requested_url"] != metadata_url || response.final_url != metadata_url {
        return Err("PMC Cloud metadata URL does not match article and version".into());
    }
    let prefix = format!("/{pmcid}.{version}/");
    let media = data["media_urls"].as_array().ok_or("PMC Cloud metadata lacks media URLs")?;
    let mut mapped = Vec::new();
    for mut resource in resources {
        let original = Url::parse(&resource.url)?;
        let basename = original.path().rsplit('/').next().ok_or("supplement lacks filename")?;
        let mut matches = Vec::new();
        for value in media {
            let literal = value.as_str().ok_or("PMC Cloud media URL is not a string")?;
            let source = Url::parse(literal)?;
            let expected_path = format!("{prefix}{basename}");
            // Only literal media URLs may supply an alternative location.
            // Other article figures remain metadata, never new supplements.
            if source.path().rsplit('/').next() != Some(basename) { continue; }
            if source.scheme() != "s3" || source.host_str() != Some("pmc-oa-opendata")
                || !source.username().is_empty() || source.password().is_some() || source.port().is_some()
                || source.fragment().is_some() || source.path() != expected_path {
                return Err(format!("unsupported PMC Cloud media URL: {literal}").into());
            }
            let query: Vec<_> = source.query_pairs().collect();
            if query.len() != 1 || query[0].0 != "md5" || query[0].1.len() != 32 || !query[0].1.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("PMC Cloud media URL lacks one valid MD5: {literal}").into());
            }
            let url = format!("https://pmc-oa-opendata.s3.amazonaws.com{}?{}", source.path(), source.query().unwrap());
            if !is_pmc_cloud_url(&Url::parse(&url)?) { return Err(format!("unsafe PMC Cloud media object: {literal}").into()); }
            matches.push((url, CloudBinding { declared_url: literal.into(), original_url: resource.url.clone(), expected_md5: query[0].1.to_ascii_lowercase() }));
        }
        if matches.len() != 1 { return Err(format!("PMC Cloud metadata must declare exactly one media URL for {basename}").into()); }
        let (url, binding) = matches.pop().unwrap();
        resource.url = url;
        resource.cloud = Some(binding);
        mapped.push(resource);
    }
    discovery["cloud"] = serde_json::json!({"response":receipt,"binding":data});
    Ok(mapped)
}

pub(super) fn validate_response(url: &str, binding: &CloudBinding, response: &PublisherResponse) -> Result<&'static str, Error> {
    let parsed = Url::parse(url)?;
    if !is_pmc_cloud_url(&parsed) || response.final_url != url {
        return Err("PMC Cloud asset redirect changed the declared object".into());
    }
    if md5(&response.bytes) != binding.expected_md5 { return Err("PMC Cloud asset MD5 mismatch".into()); }
    let extension = parsed.path().rsplit('.').next().unwrap_or("");
    let expected_mime = match extension {
        "pdf" => "application/pdf", "png" => "image/png", "jpg" | "jpeg" => "image/jpeg",
        "tif" | "tiff" => "image/tiff", "gif" => "image/gif",
        _ => return Err("unsupported PMC Cloud media extension".into()),
    };
    let mime = response.mime.split(';').next().unwrap_or("").trim();
    if !matches!(mime, "binary/octet-stream" | "application/octet-stream") && mime != expected_mime {
        return Err("PMC Cloud media MIME does not match declared extension".into());
    }
    Ok(validated_extension(expected_mime, &response.bytes)?)
}
