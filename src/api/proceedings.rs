//! Open-access proceedings hosts (ADR-010): one rule per host maps a PDF or
//! landing-page URL to the paper's canonical PDF URL, landing URL or DOI, and
//! a landing page's `citation_*` meta tags give its metadata.
//!
//! Everything here is pure: URL rules and landing-page parsing.

use crate::html::{self, Node};

/// The URLs one form yields, as templates over the names its inputs bind.
/// Every output is absolute; `{name}` is replaced by the bound value.
pub struct Form {
    /// Path templates, with `?query` when the host keys papers by a query
    /// parameter. `{name}` matches one or more characters other than `/`,
    /// `?`, `&` and `#`; `{name*}` matches one or more of any character. A
    /// name bound twice must bind the same text.
    pub inputs: &'static [&'static str],
    pub pdf: Option<&'static str>,
    pub landing: Option<&'static str>,
    pub doi: Option<&'static str>,
}

/// A proceedings host family and its URL forms, tried in order.
pub struct Rule {
    /// Stable identifier, recorded as `host` in `source.yaml`.
    pub id: &'static str,
    pub hosts: &'static [&'static str],
    pub forms: &'static [Form],
}

const fn pdf_and_landing(inputs: &'static [&'static str], pdf: &'static str, landing: &'static str) -> Form {
    Form { inputs, pdf: Some(pdf), landing: Some(landing), doi: None }
}

const fn pdf_only(inputs: &'static [&'static str], pdf: &'static str) -> Form {
    Form { inputs, pdf: Some(pdf), landing: None, doi: None }
}

const fn landing_only(inputs: &'static [&'static str], landing: &'static str) -> Form {
    Form { inputs, pdf: None, landing: Some(landing), doi: None }
}

/// The host table. A form whose PDF URL cannot be computed from its input
/// yields only the landing page, whose `citation_pdf_url` names the PDF.
pub static RULES: &[Rule] = &[
    Rule {
        id: "neurips",
        hosts: &["proceedings.neurips.cc", "papers.nips.cc", "papers.neurips.cc"],
        forms: &[
            pdf_and_landing(
                &[
                    "/paper_files/paper/{y}/file/{h}-Paper.pdf",
                    "/paper_files/paper/{y}/hash/{h}-Abstract.html",
                    "/paper/{y}/file/{h}-Paper.pdf",
                    "/paper/{y}/hash/{h}-Abstract.html",
                ],
                "https://proceedings.neurips.cc/paper_files/paper/{y}/file/{h}-Paper.pdf",
                "https://proceedings.neurips.cc/paper_files/paper/{y}/hash/{h}-Abstract.html",
            ),
            pdf_and_landing(
                &[
                    "/paper_files/paper/{y}/file/{h}-Paper-{t}.pdf",
                    "/paper_files/paper/{y}/hash/{h}-Abstract-{t}.html",
                    "/paper/{y}/file/{h}-Paper-{t}.pdf",
                    "/paper/{y}/hash/{h}-Abstract-{t}.html",
                ],
                "https://proceedings.neurips.cc/paper_files/paper/{y}/file/{h}-Paper-{t}.pdf",
                "https://proceedings.neurips.cc/paper_files/paper/{y}/hash/{h}-Abstract-{t}.html",
            ),
            // Pre-2020 URLs named by number and slug; they redirect to the
            // hash pages, whose citation_pdf_url gives the canonical PDF.
            pdf_and_landing(
                &["/paper/{slug}.pdf", "/paper/{slug}"],
                "https://papers.nips.cc/paper/{slug}.pdf",
                "https://papers.nips.cc/paper/{slug}",
            ),
        ],
    },
    Rule {
        id: "pmlr",
        hosts: &["proceedings.mlr.press"],
        forms: &[
            pdf_and_landing(
                &["/v{v}/{id}/{id}.pdf"],
                "https://proceedings.mlr.press/v{v}/{id}/{id}.pdf",
                "https://proceedings.mlr.press/v{v}/{id}.html",
            ),
            pdf_and_landing(
                &["/v{v}/{id}.pdf"],
                "https://proceedings.mlr.press/v{v}/{id}.pdf",
                "https://proceedings.mlr.press/v{v}/{id}.html",
            ),
            // Volumes differ in which of the two PDF layouts they use.
            landing_only(&["/v{v}/{id}.html"], "https://proceedings.mlr.press/v{v}/{id}.html"),
        ],
    },
    Rule {
        id: "jmlr",
        hosts: &["jmlr.org", "www.jmlr.org"],
        forms: &[pdf_and_landing(
            &["/papers/volume{v}/{id}/{id}.pdf", "/papers/v{v}/{id}.html"],
            "https://jmlr.org/papers/volume{v}/{id}/{id}.pdf",
            "https://jmlr.org/papers/v{v}/{id}.html",
        )],
    },
    Rule {
        id: "openreview",
        hosts: &["openreview.net", "www.openreview.net"],
        forms: &[pdf_and_landing(
            &["/pdf?id={id}", "/forum?id={id}"],
            "https://openreview.net/pdf?id={id}",
            "https://openreview.net/forum?id={id}",
        )],
    },
    Rule {
        id: "acl",
        hosts: &["aclanthology.org", "www.aclanthology.org"],
        forms: &[pdf_and_landing(
            &["/{id}.pdf", "/{id}/", "/{id}"],
            "https://aclanthology.org/{id}.pdf",
            "https://aclanthology.org/{id}/",
        )],
    },
    Rule {
        id: "aaai",
        hosts: &["ojs.aaai.org"],
        forms: &[
            pdf_and_landing(
                &["/index.php/{j}/article/download/{n}/{m}", "/index.php/{j}/article/view/{n}/{m}"],
                "https://ojs.aaai.org/index.php/{j}/article/download/{n}/{m}",
                "https://ojs.aaai.org/index.php/{j}/article/view/{n}",
            ),
            landing_only(&["/index.php/{j}/article/view/{n}"], "https://ojs.aaai.org/index.php/{j}/article/view/{n}"),
        ],
    },
    Rule {
        id: "cvf",
        hosts: &["openaccess.thecvf.com"],
        forms: &[pdf_and_landing(
            &["/{c*}/papers/{name}.pdf", "/{c*}/html/{name}.html"],
            "https://openaccess.thecvf.com/{c}/papers/{name}.pdf",
            "https://openaccess.thecvf.com/{c}/html/{name}.html",
        )],
    },
    Rule {
        id: "ecva",
        hosts: &["www.ecva.net", "ecva.net"],
        forms: &[
            // The PDF is numbered differently from the landing page.
            pdf_only(&["/papers/{e}/papers_ECCV/papers/{id}.pdf"], "https://www.ecva.net/papers/{e}/papers_ECCV/papers/{id}.pdf"),
            landing_only(&["/papers/{e}/papers_ECCV/html/{n}.php"], "https://www.ecva.net/papers/{e}/papers_ECCV/html/{n}.php"),
        ],
    },
    Rule {
        id: "ijcai",
        hosts: &["www.ijcai.org", "ijcai.org"],
        forms: &[
            // The PDF number is zero-padded and the landing number is not.
            pdf_only(&["/proceedings/{y}/{n}.pdf"], "https://ijcai.org/proceedings/{y}/{n}.pdf"),
            landing_only(&["/proceedings/{y}/{n}"], "https://ijcai.org/proceedings/{y}/{n}"),
        ],
    },
    Rule {
        id: "acm",
        hosts: &["dl.acm.org"],
        forms: &[Form {
            inputs: &[
                "/doi/pdf/10.{d*}",
                "/doi/epdf/10.{d*}",
                "/doi/abs/10.{d*}",
                "/doi/full/10.{d*}",
                "/doi/10.{d*}",
            ],
            pdf: None,
            landing: None,
            doi: Some("10.{d}"),
        }],
    },
];

/// What a proceedings URL names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// A paper on `rule`'s host, by its canonical PDF URL, landing URL or both.
    Paper { rule: &'static str, pdf: Option<String>, landing: Option<String> },
    /// A paper the URL names by DOI.
    Doi(String),
}

/// The hosts of every rule, for the unsupported-host error.
pub fn hosts() -> Vec<&'static str> {
    RULES.iter().flat_map(|r| r.hosts.iter().copied()).collect()
}

/// Map `url` through the rule of its host, or `None` when no rule's host or
/// form accepts it.
pub fn resolve(url: &str) -> Option<Resolved> {
    let (host, path, query) = split_url(url)?;
    let rule = RULES.iter().find(|r| r.hosts.contains(&host.as_str()))?;
    for form in rule.forms {
        for input in form.inputs {
            let mut binds = Vec::new();
            let matched = match input.split_once('?') {
                Some(_) => query.is_some_and(|q| {
                    let target = format!("{}?{}", path, q);
                    match_prefix(input, &target, &mut binds).is_some_and(|rest| rest.is_empty() || rest.starts_with('&'))
                }),
                None => match_prefix(input, path, &mut binds).is_some_and(str::is_empty),
            };
            if !matched {
                continue;
            }
            let fill = |t: Option<&str>| t.map(|t| substitute(t, &binds));
            return Some(match fill(form.doi) {
                Some(doi) => Resolved::Doi(doi),
                None => Resolved::Paper { rule: rule.id, pdf: fill(form.pdf), landing: fill(form.landing) },
            });
        }
    }
    None
}

/// The lowercased host, the path, and the query (fragment removed) of an
/// http(s) URL.
fn split_url(url: &str) -> Option<(String, &str, Option<&str>)> {
    let trimmed = url.trim();
    let scheme = ["https://", "http://"].into_iter().find(|s| trimmed.get(..s.len()).is_some_and(|p| p.eq_ignore_ascii_case(s)))?;
    let rest = &trimmed[scheme.len()..];
    let rest = &rest[..rest.find('#').unwrap_or(rest.len())];
    let (rest, query) = match rest.split_once('?') {
        Some((r, q)) => (r, Some(q)),
        None => (rest, None),
    };
    let slash = rest.find('/').unwrap_or(rest.len());
    let path = if slash == rest.len() { "/" } else { &rest[slash..] };
    Some((rest[..slash].to_ascii_lowercase(), path, query))
}

/// Match `template` against the start of `input`, binding placeholders into
/// `binds`; returns the unmatched rest of `input`. Placeholders are greedy
/// and backtrack, so `{id}.pdf` binds `2020.acl-main.1` in
/// `2020.acl-main.1.pdf`.
fn match_prefix<'a>(template: &str, input: &'a str, binds: &mut Vec<(String, String)>) -> Option<&'a str> {
    let Some(open) = template.find('{') else {
        return input.strip_prefix(template);
    };
    let input = input.strip_prefix(&template[..open])?;
    let close = open + template[open..].find('}').expect("rule template placeholders are closed");
    let (name, any) = match &template[open + 1..close] {
        n if n.ends_with('*') => (&n[..n.len() - 1], true),
        n => (n, false),
    };
    let after = &template[close + 1..];
    let max = if any { input.len() } else { input.find(['/', '?', '&', '#']).unwrap_or(input.len()) };
    for len in (1..=max).rev().filter(|&len| input.is_char_boundary(len)) {
        let value = &input[..len];
        let mark = binds.len();
        match binds.iter().find(|(n, _)| n == name) {
            Some((_, bound)) if bound != value => continue,
            Some(_) => {}
            None => binds.push((name.to_string(), value.to_string())),
        }
        if let Some(rest) = match_prefix(after, &input[len..], binds) {
            return Some(rest);
        }
        binds.truncate(mark);
    }
    None
}

/// `template` with each `{name}` replaced by its bound value.
fn substitute(template: &str, binds: &[(String, String)]) -> String {
    binds.iter().fold(template.to_string(), |t, (name, value)| t.replace(&format!("{{{}}}", name), value))
}

/// A landing page's bibliographic metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landing {
    pub title: String,
    /// Display names in "First Last" order.
    pub authors: Vec<String>,
    /// Four-digit year, or empty when the page states none.
    pub year: String,
    pub doi: Option<String>,
    /// `citation_pdf_url`, as an absolute URL.
    pub pdf_url: Option<String>,
    /// The page's `<a href>` targets ending in `.pdf`, as absolute URLs in
    /// page order.
    pub pdf_links: Vec<String>,
}

/// Parse a landing page fetched from `page_url`.
///
/// Fails when the page has no `citation_title`: it is then a bot challenge,
/// an error page or a page without tags, and its `<title>` would be invented
/// metadata (ADR-002). The error carries the page's PDF URLs
/// (`citation_pdf_url`, then links), since a page without tags can still
/// link its PDF.
pub fn parse_landing(page_url: &str, body: &str) -> Result<Landing, (String, Vec<String>)> {
    let doc = html::parse(body);
    let meta = |name: &str| html::meta_all(&doc, name);
    let base = reqwest::Url::parse(page_url).ok();
    let absolute = |href: String| match &base {
        Some(base) => base.join(&href).ok().map(String::from),
        None => Some(href),
    };
    let pdf_url = meta("citation_pdf_url").into_iter().find_map(absolute);
    let pdf_links: Vec<String> = pdf_links(&doc).into_iter().filter_map(absolute).collect();
    let Some(title) = meta("citation_title").into_iter().next() else {
        let urls = pdf_url.into_iter().chain(pdf_links).collect();
        return Err(("the page has no citation_title meta tag".to_string(), urls));
    };
    let authors = meta("citation_author").iter().map(|a| super::web::first_last(a)).collect();
    let year = ["citation_publication_date", "citation_date", "citation_year"]
        .iter()
        .flat_map(|name| meta(name))
        .find_map(|d| year_in(&d))
        .unwrap_or_default();
    let doi = meta("citation_doi").into_iter().next();
    Ok(Landing { title, authors, year, doi, pdf_url, pdf_links })
}

/// The `<a href>` targets ending in `.pdf`, as written.
fn pdf_links(doc: &[Node]) -> Vec<String> {
    html::find_all(doc, &|e| e.name == "a" && e.attr("href").is_some_and(|h| h.to_ascii_lowercase().ends_with(".pdf")))
        .into_iter()
        .filter_map(|e| e.attr("href").map(str::to_string))
        .collect()
}

/// The first four-digit run starting with 1 or 2 in `s`.
fn year_in(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    (0..bytes.len().saturating_sub(3))
        .find(|&i| {
            matches!(bytes[i], b'1' | b'2')
                && bytes[i..i + 4].iter().all(u8::is_ascii_digit)
                && !bytes.get(i + 4).is_some_and(u8::is_ascii_digit)
                && (i == 0 || !bytes[i - 1].is_ascii_digit())
        })
        .map(|i| s[i..i + 4].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEURIPS: &str = include_str!("../../test/proceedings/neurips-2013-adaptive-dropout.html");
    const ACL: &str = include_str!("../../test/proceedings/acl-N19-1423.html");
    const ECVA: &str = include_str!("../../test/proceedings/ecva-2020-267.html");

    fn paper(rule: &'static str, pdf: Option<&str>, landing: Option<&str>) -> Option<Resolved> {
        Some(Resolved::Paper { rule, pdf: pdf.map(String::from), landing: landing.map(String::from) })
    }

    /// Assert that each of `urls` resolves to `expected`.
    fn assert_all(urls: &[&str], expected: Option<Resolved>) {
        for url in urls {
            assert_eq!(resolve(url), expected, "{}", url);
        }
    }

    #[test]
    fn neurips_pdf_and_abstract_urls_on_every_host_share_one_canonical_form() {
        let h = "7b5b23f4aadf9513306bcd59afb6e4c9";
        assert_all(
            &[
                &format!("https://proceedings.neurips.cc/paper/2013/file/{}-Paper.pdf", h),
                &format!("https://proceedings.neurips.cc/paper_files/paper/2013/file/{}-Paper.pdf", h),
                &format!("https://papers.nips.cc/paper/2013/hash/{}-Abstract.html", h),
                &format!("http://papers.neurips.cc/paper_files/paper/2013/hash/{}-Abstract.html#x", h),
            ],
            paper(
                "neurips",
                Some(&format!("https://proceedings.neurips.cc/paper_files/paper/2013/file/{}-Paper.pdf", h)),
                Some(&format!("https://proceedings.neurips.cc/paper_files/paper/2013/hash/{}-Abstract.html", h)),
            ),
        );
    }

    #[test]
    fn neurips_track_suffixed_urls_keep_their_track() {
        let h = "0001";
        assert_all(
            &[
                &format!("https://proceedings.neurips.cc/paper_files/paper/2023/file/{}-Paper-Conference.pdf", h),
                &format!("https://proceedings.neurips.cc/paper_files/paper/2023/hash/{}-Abstract-Conference.html", h),
            ],
            paper(
                "neurips",
                Some(&format!("https://proceedings.neurips.cc/paper_files/paper/2023/file/{}-Paper-Conference.pdf", h)),
                Some(&format!("https://proceedings.neurips.cc/paper_files/paper/2023/hash/{}-Abstract-Conference.html", h)),
            ),
        );
    }

    #[test]
    fn neurips_numbered_slug_urls_map_to_their_redirecting_pages() {
        let slug = "5032-adaptive-dropout-for-training-deep-neural-networks";
        assert_all(
            &[&format!("https://papers.nips.cc/paper/{}.pdf", slug), &format!("https://papers.nips.cc/paper/{}", slug)],
            paper(
                "neurips",
                Some(&format!("https://papers.nips.cc/paper/{}.pdf", slug)),
                Some(&format!("https://papers.nips.cc/paper/{}", slug)),
            ),
        );
    }

    #[test]
    fn pmlr_maps_both_pdf_layouts_and_the_abstract_page() {
        assert_eq!(
            resolve("https://proceedings.mlr.press/v139/radford21a/radford21a.pdf"),
            paper(
                "pmlr",
                Some("https://proceedings.mlr.press/v139/radford21a/radford21a.pdf"),
                Some("https://proceedings.mlr.press/v139/radford21a.html")
            )
        );
        assert_eq!(
            resolve("http://proceedings.mlr.press/v37/ioffe15.pdf"),
            paper("pmlr", Some("https://proceedings.mlr.press/v37/ioffe15.pdf"), Some("https://proceedings.mlr.press/v37/ioffe15.html"))
        );
        assert_eq!(
            resolve("https://proceedings.mlr.press/v37/ioffe15.html"),
            paper("pmlr", None, Some("https://proceedings.mlr.press/v37/ioffe15.html"))
        );
        // The two segments of the nested layout must name one paper.
        assert_eq!(resolve("https://proceedings.mlr.press/v139/a21a/b21b.pdf"), None);
    }

    #[test]
    fn jmlr_maps_pdf_and_abstract_both_ways() {
        assert_all(
            &["https://www.jmlr.org/papers/volume15/srivastava14a/srivastava14a.pdf", "https://jmlr.org/papers/v15/srivastava14a.html"],
            paper(
                "jmlr",
                Some("https://jmlr.org/papers/volume15/srivastava14a/srivastava14a.pdf"),
                Some("https://jmlr.org/papers/v15/srivastava14a.html"),
            ),
        );
    }

    #[test]
    fn openreview_maps_forum_and_pdf_by_the_id_parameter() {
        assert_all(
            &[
                "https://openreview.net/forum?id=YicbFdNTTy",
                "https://openreview.net/pdf?id=YicbFdNTTy",
                "https://openreview.net/forum?id=YicbFdNTTy&noteId=abc",
            ],
            paper("openreview", Some("https://openreview.net/pdf?id=YicbFdNTTy"), Some("https://openreview.net/forum?id=YicbFdNTTy")),
        );
        assert_eq!(resolve("https://openreview.net/forum?noteId=abc"), None);
    }

    #[test]
    fn acl_maps_old_and_new_ids_with_dots() {
        assert_all(
            &["https://aclanthology.org/N19-1423.pdf", "https://aclanthology.org/N19-1423/", "https://aclanthology.org/N19-1423"],
            paper("acl", Some("https://aclanthology.org/N19-1423.pdf"), Some("https://aclanthology.org/N19-1423/")),
        );
        assert_eq!(
            resolve("https://aclanthology.org/2020.acl-main.1.pdf"),
            paper("acl", Some("https://aclanthology.org/2020.acl-main.1.pdf"), Some("https://aclanthology.org/2020.acl-main.1/"))
        );
    }

    #[test]
    fn aaai_maps_download_and_viewer_urls_and_the_article_page() {
        assert_all(
            &[
                "https://ojs.aaai.org/index.php/AAAI/article/download/16826/16633",
                "https://ojs.aaai.org/index.php/AAAI/article/view/16826/16633",
            ],
            paper(
                "aaai",
                Some("https://ojs.aaai.org/index.php/AAAI/article/download/16826/16633"),
                Some("https://ojs.aaai.org/index.php/AAAI/article/view/16826"),
            ),
        );
        assert_eq!(
            resolve("https://ojs.aaai.org/index.php/AAAI/article/view/16826"),
            paper("aaai", None, Some("https://ojs.aaai.org/index.php/AAAI/article/view/16826"))
        );
    }

    #[test]
    fn cvf_maps_html_and_papers_under_any_conference_prefix() {
        assert_all(
            &[
                "https://openaccess.thecvf.com/content_cvpr_2016/html/He_Deep_Residual_Learning_CVPR_2016_paper.html",
                "https://openaccess.thecvf.com/content_cvpr_2016/papers/He_Deep_Residual_Learning_CVPR_2016_paper.pdf",
            ],
            paper(
                "cvf",
                Some("https://openaccess.thecvf.com/content_cvpr_2016/papers/He_Deep_Residual_Learning_CVPR_2016_paper.pdf"),
                Some("https://openaccess.thecvf.com/content_cvpr_2016/html/He_Deep_Residual_Learning_CVPR_2016_paper.html"),
            ),
        );
        assert_eq!(
            resolve("https://openaccess.thecvf.com/content/CVPR2023W/ABAW/html/X_paper.html"),
            paper(
                "cvf",
                Some("https://openaccess.thecvf.com/content/CVPR2023W/ABAW/papers/X_paper.pdf"),
                Some("https://openaccess.thecvf.com/content/CVPR2023W/ABAW/html/X_paper.html")
            )
        );
    }

    #[test]
    fn ecva_maps_pdf_and_landing_separately() {
        assert_eq!(
            resolve("https://ecva.net/papers/eccv_2020/papers_ECCV/papers/123460001.pdf"),
            paper("ecva", Some("https://www.ecva.net/papers/eccv_2020/papers_ECCV/papers/123460001.pdf"), None)
        );
        assert_eq!(
            resolve("https://www.ecva.net/papers/eccv_2020/papers_ECCV/html/267_ECCV_2020_paper.php"),
            paper("ecva", None, Some("https://www.ecva.net/papers/eccv_2020/papers_ECCV/html/267_ECCV_2020_paper.php"))
        );
    }

    #[test]
    fn ijcai_maps_pdf_and_landing_separately() {
        assert_eq!(
            resolve("https://www.ijcai.org/proceedings/2019/0123.pdf"),
            paper("ijcai", Some("https://ijcai.org/proceedings/2019/0123.pdf"), None)
        );
        assert_eq!(resolve("https://www.ijcai.org/proceedings/2019/123"), paper("ijcai", None, Some("https://ijcai.org/proceedings/2019/123")));
    }

    #[test]
    fn acm_urls_name_their_doi() {
        assert_all(
            &[
                "https://dl.acm.org/doi/10.1145/3292500.3330701",
                "https://dl.acm.org/doi/pdf/10.1145/3292500.3330701",
                "https://dl.acm.org/doi/abs/10.1145/3292500.3330701?download=true",
            ],
            Some(Resolved::Doi("10.1145/3292500.3330701".to_string())),
        );
    }

    #[test]
    fn other_hosts_and_unmatched_paths_resolve_to_nothing() {
        for url in ["https://example.com/paper.pdf", "https://distill.pub/2020/circuits/zoom-in/", "https://jmlr.org/", "not a url"] {
            assert_eq!(resolve(url), None, "{}", url);
        }
    }

    #[test]
    fn every_rule_host_is_listed_once() {
        let hosts = hosts();
        let mut unique = hosts.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), hosts.len());
        for required in ["proceedings.neurips.cc", "proceedings.mlr.press", "openreview.net", "aclanthology.org", "dl.acm.org"] {
            assert!(hosts.contains(&required), "{}", required);
        }
    }

    #[test]
    fn neurips_landing_gives_metadata_and_the_canonical_pdf() {
        let page = "https://proceedings.neurips.cc/paper_files/paper/2013/hash/7b5b23f4aadf9513306bcd59afb6e4c9-Abstract.html";
        let landing = parse_landing(page, NEURIPS).unwrap();
        assert_eq!(landing.title, "Adaptive dropout for training deep neural networks");
        assert_eq!(landing.authors, vec!["Jimmy Ba", "Brendan Frey"]);
        assert_eq!(landing.year, "2013");
        assert_eq!(landing.doi, None);
        assert_eq!(
            landing.pdf_url.as_deref(),
            Some("https://proceedings.neurips.cc/paper_files/paper/2013/file/7b5b23f4aadf9513306bcd59afb6e4c9-Paper.pdf")
        );
    }

    #[test]
    fn acl_landing_with_unquoted_attributes_gives_metadata() {
        let landing = parse_landing("https://aclanthology.org/N19-1423/", ACL).unwrap();
        assert!(landing.title.starts_with("BERT: Pre-training"), "{}", landing.title);
        assert_eq!(landing.authors.first().map(String::as_str), Some("Jacob Devlin"));
        assert_eq!(landing.year, "2019");
        assert_eq!(landing.doi.as_deref(), Some("10.18653/v1/N19-1423"));
        assert_eq!(landing.pdf_url.as_deref(), Some("https://aclanthology.org/N19-1423.pdf"));
    }

    #[test]
    fn a_page_without_citation_tags_gives_no_metadata_but_keeps_its_pdf_links() {
        let page = "https://www.ecva.net/papers/eccv_2020/papers_ECCV/html/267_ECCV_2020_paper.php";
        let (why, links) = parse_landing(page, ECVA).unwrap_err();
        assert!(why.contains("citation_title"), "{}", why);
        assert_eq!(links, vec!["https://www.ecva.net/papers/eccv_2020/papers_ECCV/papers/123460001.pdf"]);
        assert!(parse_landing(page, "<html><title>Making sure you're not a bot!</title></html>").is_err());
    }

    #[test]
    fn year_in_takes_the_first_standalone_four_digit_year() {
        assert_eq!(year_in("2013"), Some("2013".into()));
        assert_eq!(year_in("2019/06/02"), Some("2019".into()));
        assert_eq!(year_in("vol 123456, 1999"), Some("1999".into()));
        assert_eq!(year_in("n.d."), None);
    }
}
