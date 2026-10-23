//! The user docs bundled into the binary, split at `##`, for Search's Help group.

use std::sync::OnceLock;

pub const SITE: &str = "https://nerdterm.dev/";

/// Each doc and its page on the site, or none when the site has no page for it; a doc that is a part of a page has
/// that part's anchor. menus.md and releasing.md are contributor docs the site leaves out, and so does Search.
const DOCS: [(&str, Option<&str>); 8] = [
    (include_str!("../docs/usage.md"), Some("docs/usage/")),
    (include_str!("../docs/isolation.md"), Some("docs/isolation/")),
    (include_str!("../docs/messages-between-agents.md"), Some("docs/messages-between-agents/")),
    (include_str!("../docs/plugins.md"), Some("docs/plugins/")),
    (include_str!("../docs/configuration.md"), Some("docs/configuration/")),
    (include_str!("../docs/privacy-and-security.md"), Some("security/")),
    (include_str!("../docs/troubleshooting.md"), Some("docs/troubleshooting/")),
    (include_str!("../docs/uninstall.md"), Some("install/#uninstall")),
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

/// An anchor as the site makes it from a heading's HTML: `&<>"'` as entities, marks dropped, lowercase, every run of
/// other characters (a letter outside a-z too) one hyphen.
pub fn slug(heading: &str) -> String {
    let mut html = String::new();
    for c in heading.chars() {
        match c {
            '&' => html.push_str("&amp;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            '"' => html.push_str("&quot;"),
            '\'' => html.push_str("&#39;"),
            '`' | '*' => {}
            c => html.push(c),
        }
    }
    let s: String = html.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
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
                    Some(h) => (format!("{title} › {h}"), page.map(|p| format!("{SITE}{}#{}", p.split('#').next().unwrap_or(p), slug(h)))),
                };
                out.push(Section { page: head.is_none(), name, body, url });
            }
        }
        out
    })
}

/// The text before the first `##`, then each `##` section; its `#` title and the site's summary comments left out.
/// Code between fences stays as it is.
fn split(text: &str) -> Vec<(Option<String>, String)> {
    let mut out = vec![(None, String::new())];
    let mut code = false;
    for raw in text.lines() {
        if raw.starts_with("```") {
            code = !code;
            continue;
        }
        if code {
            let body = &mut out.last_mut().expect("starts with one").1;
            body.push_str(raw);
            body.push('\n');
            continue;
        }
        let mut line = uncommented(raw);
        if let Some(t) = line.trim().strip_prefix("<summary>").and_then(|t| t.strip_suffix("</summary>")) {
            line = t.to_string();
        }
        // a line that was only a comment or an HTML tag goes, and a code fence; a blank line stays
        let tag = line.trim().starts_with('<') && line.trim().ends_with('>');
        if tag || line.starts_with("# ") || (line.trim().is_empty() && !raw.trim().is_empty()) {
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
        // as nerd-plugins site/build.mjs makes them, checked by running its slug on its own markdown
        assert_eq!(slug("Fish & chips"), "fish-amp-chips");
        assert_eq!(slug("It's \"quoted\""), "it-39-s-quot-quoted-quot");
        assert_eq!(slug("a < b > c"), "a-lt-b-gt-c");
        assert_eq!(slug("**Bold** --- dashes"), "bold-dashes");
        assert_eq!(slug("Café — naïve"), "caf-na-ve");
        assert_eq!(slug("Ünïcode only"), "n-code-only");
        assert_eq!(slug("日本語"), "");
        assert_eq!(slug("x_y"), "x-y");
        assert_eq!(uncommented("| <!-- egress:link --> A link |"), "|  A link |");
    }

    #[test]
    fn code_in_fences_stays_whole_and_makes_no_section() {
        let parts = split("# Title\n\nIntro\n\n## Run\n\n```sh\n# a comment\n## not a heading\n\n**kept**\n```\nAfter\n");
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert_eq!(parts[1], (Some("Run".into()), "# a comment\n## not a heading\n\n**kept**\nAfter".into()));
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
            // the site makes a link's label its anchor and a repeated heading's anchor longer; slug does neither
            assert!(!s.name.contains("]("), "{}", s.name);
        }
        // A page as the site makes it: a doc without its title, or a README section; its h2 to h5 are `#` to `####`.
        let mut pages: Vec<&str> = DOCS.iter().map(|(t, _)| t.split_once('\n').map_or("", |(_, rest)| rest)).collect();
        pages.extend(README.split("\n## ").skip(1));
        for page in pages {
            let mut code = false;
            let mut ids = Vec::new();
            for l in page.lines() {
                if l.starts_with("```") {
                    code = !code;
                }
                let n = l.chars().take_while(|c| *c == '#').count();
                if !code && (1..=4).contains(&n) && l[n..].starts_with(' ') {
                    ids.push(slug(l[n..].trim()));
                }
            }
            assert!(ids.iter().enumerate().all(|(i, h)| !ids[..i].contains(h)), "an id repeats on a page: {ids:?}");
        }
        let named = |n: &str| all.iter().find(|s| s.name == n).unwrap_or_else(|| panic!("no {n}"));
        assert_eq!(named("Privacy and security › What leaves nerd").url.as_deref(), Some("https://nerdterm.dev/security/#what-leaves-nerd"));
        assert_eq!(named("Usage › Keys").url.as_deref(), Some("https://nerdterm.dev/docs/usage/#keys"));
        assert_eq!(named("Quick start").url.as_deref(), Some("https://nerdterm.dev/docs/quick-start/"));
        assert!(all.iter().any(|s| s.body.contains("Build from source")), "the summary's text stays");
        assert!(!all.iter().any(|s| s.body.contains("<details>") || s.body.contains("</details>") || s.body.contains("<summary>")), "HTML tags go");
        assert_eq!(named("Uninstall").url.as_deref(), Some("https://nerdterm.dev/install/#uninstall"), "a part of the Install page");
        assert_eq!(named("Documentation").url, None);
        assert!(named("Configuration").body.contains("preview_wrap"));
        assert!(!all.iter().any(|s| s.name.starts_with("Menus, settings and windows") || s.name.starts_with("Releasing")), "contributor docs stay out");
    }
}
