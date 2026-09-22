//! Permission nodes: dotted strings such as `garnet.command.kick`, with `*`
//! matching any suffix (`garnet.command.*`) and a leading `-` denying.
//!
//! Resolution order: explicit user nodes, then each of the user's groups
//! (first group wins), then the `default` group. Ops have everything unless
//! a node explicitly denies it.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PermissionGroup {
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Groups whose permissions this group also has.
    #[serde(default)]
    pub inherits: Vec<String>,
    /// Shown in front of the name in chat, `&`-codes allowed.
    #[serde(default)]
    pub prefix: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PermissionUser {
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// The whole permissions file (`permissions.toml`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Permissions {
    #[serde(default)]
    pub groups: BTreeMap<String, PermissionGroup>,
    /// Keyed by player UUID (string form).
    #[serde(default)]
    pub users: BTreeMap<String, PermissionUser>,
}

/// How a single node list answers for a permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
    Unset,
}

/// Checks one list of nodes. The most specific matching node wins; on a tie
/// a deny beats an allow.
pub fn decide(nodes: &[String], permission: &str) -> Decision {
    let mut best: Option<(usize, bool)> = None; // (specificity, allowed)
    for node in nodes {
        let (allowed, pattern) = match node.strip_prefix('-') {
            Some(rest) => (false, rest),
            None => (true, node.as_str()),
        };
        let Some(specificity) = match_specificity(pattern, permission) else {
            continue;
        };
        match best {
            Some((s, a)) if s > specificity || (s == specificity && !a) => {}
            _ => best = Some((specificity, allowed)),
        }
    }
    match best {
        Some((_, true)) => Decision::Allow,
        Some((_, false)) => Decision::Deny,
        None => Decision::Unset,
    }
}

/// `None` if the pattern does not match, otherwise how many literal segments
/// matched (higher = more specific). `*` alone matches everything.
fn match_specificity(pattern: &str, permission: &str) -> Option<usize> {
    if pattern == "*" {
        return Some(0);
    }
    let mut count = 0;
    let mut pat = pattern.split('.').peekable();
    let mut perm = permission.split('.');
    loop {
        match (pat.next(), perm.next()) {
            (Some("*"), _) if pat.peek().is_none() => return Some(count),
            (Some(p), Some(q)) if p == q => count += 1,
            (None, None) => return Some(count),
            _ => return None,
        }
    }
}

impl Permissions {
    /// Full resolution for a user. `is_op` short-circuits to allow unless a
    /// node explicitly denies.
    pub fn check(&self, uuid: &str, is_op: bool, permission: &str) -> bool {
        let user = self.users.get(uuid);
        if let Some(user) = user {
            match decide(&user.permissions, permission) {
                Decision::Allow => return true,
                Decision::Deny => return false,
                Decision::Unset => {}
            }
        }
        let mut groups: Vec<&str> = user.map(|u| u.groups.iter().map(String::as_str).collect()).unwrap_or_default();
        if groups.is_empty() {
            groups.push("default");
        }
        let mut visited = Vec::new();
        for group in groups {
            match self.check_group(group, permission, &mut visited) {
                Decision::Allow => return true,
                Decision::Deny => return false,
                Decision::Unset => {}
            }
        }
        is_op
    }

    fn check_group(&self, name: &str, permission: &str, visited: &mut Vec<String>) -> Decision {
        if visited.iter().any(|v| v == name) {
            return Decision::Unset;
        }
        visited.push(name.to_owned());
        let Some(group) = self.groups.get(name) else {
            return Decision::Unset;
        };
        match decide(&group.permissions, permission) {
            Decision::Unset => {}
            decided => return decided,
        }
        for parent in &group.inherits {
            match self.check_group(parent, permission, visited) {
                Decision::Unset => {}
                decided => return decided,
            }
        }
        Decision::Unset
    }

    /// The prefix of the user's first group with one set.
    pub fn prefix(&self, uuid: &str) -> Option<&str> {
        let groups = self.users.get(uuid).map(|u| u.groups.as_slice()).unwrap_or(&[]);
        groups
            .iter()
            .chain(std::iter::once(&"default".to_owned()))
            .filter_map(|g| self.groups.get(g))
            .map(|g| g.prefix.as_str())
            .find(|p| !p.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_and_deny() {
        let nodes = vec!["garnet.command.*".to_owned(), "-garnet.command.stop".to_owned()];
        assert_eq!(decide(&nodes, "garnet.command.kick"), Decision::Allow);
        assert_eq!(decide(&nodes, "garnet.command.stop"), Decision::Deny);
        assert_eq!(decide(&nodes, "garnet.admin"), Decision::Unset);
    }

    #[test]
    fn groups_and_inheritance() {
        let mut perms = Permissions::default();
        perms.groups.insert(
            "default".into(),
            PermissionGroup {
                permissions: vec!["garnet.command.help".into()],
                ..Default::default()
            },
        );
        perms.groups.insert(
            "mod".into(),
            PermissionGroup {
                permissions: vec!["garnet.command.kick".into()],
                inherits: vec!["default".into()],
                ..Default::default()
            },
        );
        perms.users.insert(
            "u1".into(),
            PermissionUser {
                groups: vec!["mod".into()],
                permissions: vec![],
            },
        );
        assert!(perms.check("u1", false, "garnet.command.kick"));
        assert!(perms.check("u1", false, "garnet.command.help"));
        assert!(!perms.check("u1", false, "garnet.command.stop"));
        assert!(perms.check("u1", true, "garnet.command.stop"));
        assert!(perms.check("nobody", false, "garnet.command.help"));
    }
}
