//! The user docs bundled into the binary, split at `##`, for Search's Help group.

use std::sync::OnceLock;

pub const SITE: &str = "https://nerdterm.dev/";

/// Each doc and its page on the site, or none when the site has no page for it. menus.md and releasing.md are
/// contributor docs the site leaves out, and so does Search.
const DOCS: [(&str, Option<&str>); 8] = [
    (include_str!("../docs/usage.md"), Some("docs/usage/")),
    (include_str!("../docs/isolation.md"), Some("docs/isolation/")),
    (include_str!("../docs/messages-between-agents.md"), Some("docs/messages-between-agents/")),
    (include_str!("../docs/plugins.md"), Some("docs/plugins/")),
    (include_str!("../docs/configuration.md"), Some("docs/configuration/")),
    (include_str!("../docs/privacy-and-security.md"), Some("security/")),
    (include_str!("../docs/troubleshooting.md"), Some("docs/troubleshooting/")),
    (include_str!("../docs/uninstall.md"), None),
];

const README: &str = include_str!("../README.md");

/// The README's `##` sections by slug, each where the site puts it; Documentation is only the list of the docs above.
const README_PAGES: [(&str, Option<&str>); 8] = [
    ("installation", Some("install/")),
    ("quick-start", Some("docs/quick-start/")),
    ("requirements", Some("install/#requirements")),
    ("terminals", Some("docs/terminals/")),
    ("limitations", Some("install/#limitations")),
    ("documentation", None),
    ("contributing", Some("docs/contributing/")),
    ("license", Some("docs/license/")),
];

pub struct Section {
    /// "Usage", "Usage › Keys", or a README section's own heading.
    pub name: String,
    pub body: String,
    pub url: Option<String>,
    /// A page of its own on the site: what Help lists before anything is typed.
    pub page: bool,
}

/// An anchor as the site makes it from a heading: lowercase, every run of other characters one hyphen.
pub fn slug(heading: &str) -> String {
    let s: String = heading.to_lowercase().chars().filter(|c| *c != '`' && *c != '*').map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

pub fn sections() -> &'static [Section] {
    static ALL: OnceLock<Vec<Section>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut out = Vec::new();
        for (head, body) in split(README).into_iter().skip(1) {
            let head = head.unwrap_or_default();
            let url = README_PAGES.iter().find(|(s, _)| *s == slug(&head)).and_then(|(_, p)| *p).map(|p| format!("{SITE}{p}"));
            out.push(Section { name: head, body, url, page: true });
        }
        for (text, page) in DOCS {
            let title = text.lines().next().and_then(|l| l.strip_prefix("# ")).unwrap_or_default().to_string();
            for (head, body) in split(text) {
                let (name, url) = match &head {
                    None => (title.clone(), page.map(|p| format!("{SITE}{p}"))),
                    Some(h) => (format!("{title} › {h}"), page.map(|p| format!("{SITE}{p}#{}", slug(h)))),
                };
                out.push(Section { page: head.is_none(), name, body, url });
            }
        }
        out
    })
}

/// The text before the first `##`, then each `##` section; its `#` title and the site's summary comments left out.
fn split(text: &str) -> Vec<(Option<String>, String)> {
    let mut out = vec![(None, String::new())];
    for raw in text.lines() {
        let line = uncommented(raw);
        // a line that was only a comment goes, and a code fence; a blank line stays
        if line.starts_with("# ") || line.starts_with("```") || (line.trim().is_empty() && !raw.trim().is_empty()) {
            continue;
        }
        match line.strip_prefix("## ") {
            Some(h) => out.push((Some(h.trim().to_string()), String::new())),
            None => {
                // read as text, not as markdown
                let body = &mut out.last_mut().expect("starts with one").1;
                body.push_str(&line.replace("**", "").replace('`', ""));
                body.push('\n');
            }
        }
    }
    for s in &mut out {
        s.1 = s.1.trim().to_string();
    }
    out
}

/// The line without its `<!-- … -->` markers, which the site uses and a reader does not.
fn uncommented(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(at) = rest.find("<!--") {
        out.push_str(&rest[..at]);
        rest = rest[at..].find("-->").map_or("", |end| &rest[at + end + 3..]);
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slug_is_the_sites_anchor() {
        assert_eq!(slug("What leaves nerd"), "what-leaves-nerd");
        assert_eq!(slug("Protocol 2"), "protocol-2");
        assert_eq!(slug("What a program inside a pane can do"), "what-a-program-inside-a-pane-can-do");
        assert_eq!(slug("`nerd --preset`, and *more*"), "nerd-preset-and-more");
        assert_eq!(uncommented("| <!-- egress:link --> A link |"), "|  A link |");
    }

    #[test]
    fn every_bundled_section_has_a_site_page_or_is_marked_as_having_none() {
        let readme: Vec<String> = split(README).into_iter().skip(1).map(|(h, _)| slug(&h.unwrap_or_default())).collect();
        for s in &readme {
            assert!(README_PAGES.iter().any(|(p, _)| p == s), "README section {s:?} is not in README_PAGES");
        }
        let all = sections();
        assert!(all.len() > readme.len() + DOCS.len(), "the docs' sections are in");
        for s in all {
            if let Some(u) = &s.url {
                assert!(u.starts_with("https://nerdterm.dev/"), "{}: {u}", s.name);
            }
            assert!(!s.body.contains("<!--") && !s.name.is_empty(), "{}", s.name);
        }
        let named = |n: &str| all.iter().find(|s| s.name == n).unwrap_or_else(|| panic!("no {n}"));
        assert_eq!(named("Privacy and security › What leaves nerd").url.as_deref(), Some("https://nerdterm.dev/security/#what-leaves-nerd"));
        assert_eq!(named("Usage › Keys").url.as_deref(), Some("https://nerdterm.dev/docs/usage/#keys"));
        assert_eq!(named("Quick start").url.as_deref(), Some("https://nerdterm.dev/docs/quick-start/"));
        assert_eq!(named("Uninstall").url, None);
        assert_eq!(named("Documentation").url, None);
        assert!(named("Configuration").body.contains("preview_wrap"));
        assert!(!all.iter().any(|s| s.name.starts_with("Menus, settings and windows") || s.name.starts_with("Releasing")), "contributor docs stay out");
    }
}
