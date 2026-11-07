//! Remote URL normalization: a credential-free URL and a repository slug.
//!
//! A slug (`host/owner/name`) names where a repository lives on a forge. It is
//! an *observation* derived from a configured remote URL, never the
//! repository's identity (ADR-015): remotes get added, renamed, and mirrored,
//! and two clones of one project share a slug but are distinct repositories.
//! Mindspace makes no claim that a repository is or is not a fork.
//!
//! Remote URLs can embed secrets (`https://user:token@host/...`). `parse` is
//! the only place a raw URL is read, and its result never contains userinfo,
//! so nothing downstream can store or show a credential.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoSlug {
    /// Host as observed, lowercased, without port. May be an SSH alias such
    /// as `ssh.example.dev`; no web host is guessed.
    pub host: String,
    /// Owner as written. Nested groups keep their `/` (`group/subgroup`).
    pub owner: String,
    /// Repository name as written, without a trailing `.git`.
    pub name: String,
}

impl RepoSlug {
    /// Lowercased `host/owner/name`: the comparison key.
    pub fn key(&self) -> String {
        format!("{}/{}/{}", self.host, self.owner, self.name).to_lowercase()
    }

    /// `Owner/Name` as written, for display.
    pub fn short(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParsedRemote {
    /// The URL with any userinfo removed; safe to store and show.
    pub url: String,
    /// `None` for local paths and URLs without `owner/name`.
    pub slug: Option<RepoSlug>,
}

pub fn parse(raw: &str) -> ParsedRemote {
    let raw = raw.trim();
    if let Some((scheme, rest)) = raw.split_once("://") {
        let scheme = scheme.to_ascii_lowercase();
        if scheme == "file" {
            return ParsedRemote {
                url: format!("file://{}", rest.split(['?', '#']).next().unwrap_or("")),
                slug: None,
            };
        }
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let path = path.split(['?', '#']).next().unwrap_or("");
        // Everything up to the last '@' is userinfo (user, user:password, token).
        let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = host_only(host_port);
        let url = if path.is_empty() {
            format!("{scheme}://{host_port}")
        } else {
            format!("{scheme}://{host_port}/{path}")
        };
        return ParsedRemote {
            slug: slug_from(&host, path),
            url,
        };
    }
    if let Some((left, path)) = scp_like(raw) {
        let host_port = left.rsplit_once('@').map_or(left, |(_, h)| h);
        let host = host_only(host_port);
        return ParsedRemote {
            url: format!("{host_port}:{path}"),
            slug: slug_from(&host, path),
        };
    }
    ParsedRemote {
        url: raw.to_string(),
        slug: None,
    }
}

/// `[user@]host:path` (git's scp-like syntax), as opposed to a local path or
/// a Windows drive path.
fn scp_like(raw: &str) -> Option<(&str, &str)> {
    let (left, path) = raw.split_once(':')?;
    if left.is_empty() || left.contains('/') || path.is_empty() {
        return None;
    }
    let drive = left.len() == 1 && left.chars().all(|c| c.is_ascii_alphabetic());
    if drive && (path.starts_with('/') || path.starts_with('\\')) {
        return None;
    }
    Some((left, path))
}

/// Host without port, lowercased; understands `[ipv6]:port`.
fn host_only(host_port: &str) -> String {
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    };
    host.to_lowercase()
}

fn slug_from(host: &str, path: &str) -> Option<RepoSlug> {
    if host.is_empty() || host.contains(char::is_whitespace) {
        return None;
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() < 2
        || segments
            .iter()
            .any(|s| s.is_empty() || *s == "." || *s == "..")
    {
        return None;
    }
    let (name, owner) = segments.split_last()?;
    Some(RepoSlug {
        host: host.to_string(),
        owner: owner.join("/"),
        name: (*name).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slug(raw: &str) -> Option<(String, String)> {
        parse(raw).slug.map(|s| (s.key(), s.short()))
    }

    #[test]
    fn real_world_forms_normalize_to_one_key() {
        let expect = Some((
            "github.com/vikingowl91/mindspace".to_string(),
            "VikingOwl91/mindspace".to_string(),
        ));
        for raw in [
            "git@github.com:VikingOwl91/mindspace.git",
            "git@github.com:VikingOwl91/mindspace",
            "https://github.com/VikingOwl91/mindspace.git",
            "https://github.com/VikingOwl91/mindspace/",
            "ssh://git@github.com/VikingOwl91/mindspace.git",
            "ssh://git@github.com:22/VikingOwl91/mindspace.git",
            "git://github.com/VikingOwl91/mindspace.git",
            "  https://GitHub.com/VikingOwl91/mindspace.git  ",
        ] {
            assert_eq!(slug(raw), expect, "{raw}");
        }
    }

    #[test]
    fn host_aliases_are_kept_as_observed() {
        let (key, short) = slug("git@ssh.somegit.dev:Owlibou/owlen.git").unwrap();
        assert_eq!(key, "ssh.somegit.dev/owlibou/owlen");
        assert_eq!(short, "Owlibou/owlen");
    }

    #[test]
    fn nested_groups_keep_their_path() {
        let s = parse("https://gitlab.com/group/sub/team/proj.git")
            .slug
            .unwrap();
        assert_eq!(
            (s.owner.as_str(), s.name.as_str()),
            ("group/sub/team", "proj")
        );
        assert_eq!(s.key(), "gitlab.com/group/sub/team/proj");
    }

    #[test]
    fn credentials_never_survive_parsing() {
        for raw in [
            "https://user:SECRETTOKEN@github.com/o/r.git",
            "https://SECRETTOKEN@github.com/o/r.git",
            "https://x-access-token:SECRETTOKEN@github.com:443/o/r",
            "ssh://deploy:SECRETTOKEN@host.example/o/r.git",
        ] {
            let p = parse(raw);
            assert!(!p.url.contains("SECRETTOKEN"), "{raw} -> {}", p.url);
            assert!(!p.url.contains('@'), "{raw} -> {}", p.url);
            assert!(p.slug.is_some(), "{raw}");
        }
        // The scp-like `git@` is dropped too: it carries no information.
        assert_eq!(parse("git@github.com:o/r.git").url, "github.com:o/r.git");
    }

    #[test]
    fn local_and_odd_remotes_have_no_slug() {
        for raw in [
            "/srv/git/repo.git",
            "../sibling",
            "file:///srv/git/repo.git",
            "C:\\work\\repo",
            "C:/work/repo",
            "https://github.com/onlyone",
            "https://github.com/a/../b",
            "https://github.com//b",
            "",
        ] {
            assert_eq!(parse(raw).slug, None, "{raw:?}");
        }
        assert_eq!(parse("/srv/git/repo.git").url, "/srv/git/repo.git");
    }

    #[test]
    fn ports_and_ipv6_hosts_do_not_leak_into_the_slug() {
        assert_eq!(
            slug("https://git.example.com:8443/o/r.git").unwrap().0,
            "git.example.com/o/r"
        );
        assert_eq!(slug("ssh://git@[::1]:2222/o/r.git").unwrap().0, "::1/o/r");
        assert_eq!(
            parse("https://git.example.com:8443/o/r.git").url,
            "https://git.example.com:8443/o/r.git"
        );
    }
}
