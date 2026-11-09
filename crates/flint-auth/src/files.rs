//! /etc/passwd, /etc/shadow and /etc/group: parsing and editing.
//! Lines starting with `#` and blank lines are ignored when reading and kept when editing.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub name: String,
    /// The passwd password field: normally "x" (see /etc/shadow).
    pub passwd: String,
    pub uid: u32,
    pub gid: u32,
    pub gecos: String,
    pub home: String,
    pub shell: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub gid: u32,
    pub members: Vec<String>,
}

fn data_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(str::trim_end).filter(|l| !l.is_empty() && !l.starts_with('#'))
}

pub fn parse_passwd(text: &str) -> Vec<User> {
    data_lines(text)
        .filter_map(|line| {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() != 7 || f[0].is_empty() {
                return None;
            }
            Some(User {
                name: f[0].to_string(),
                passwd: f[1].to_string(),
                uid: f[2].parse().ok()?,
                gid: f[3].parse().ok()?,
                gecos: f[4].to_string(),
                home: f[5].to_string(),
                shell: f[6].to_string(),
            })
        })
        .collect()
}

pub fn find_user<'a>(users: &'a [User], name: &str) -> Option<&'a User> {
    users.iter().find(|u| u.name == name)
}

pub fn find_user_by_uid(users: &[User], uid: u32) -> Option<&User> {
    users.iter().find(|u| u.uid == uid)
}

pub fn parse_group(text: &str) -> Vec<Group> {
    data_lines(text)
        .filter_map(|line| {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() != 4 || f[0].is_empty() {
                return None;
            }
            Some(Group {
                name: f[0].to_string(),
                gid: f[2].parse().ok()?,
                members: f[3].split(',').filter(|m| !m.is_empty()).map(String::from).collect(),
            })
        })
        .collect()
}

/// All group ids for `user`: the primary one first, then every group listing them as a member.
pub fn groups_of(groups: &[Group], user: &User) -> Vec<u32> {
    let mut ids = vec![user.gid];
    for g in groups {
        if g.members.iter().any(|m| m == &user.name) && !ids.contains(&g.gid) {
            ids.push(g.gid);
        }
    }
    ids
}

/// The password hash field of `name` in /etc/shadow text.
pub fn shadow_hash<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    data_lines(text).find_map(|line| {
        let mut f = line.split(':');
        (f.next()? == name).then(|| f.next()).flatten()
    })
}

/// Return `text` with `name`'s hash replaced (or a new entry appended). Other lines,
/// comments and the remaining shadow fields are preserved.
pub fn set_shadow_hash(text: &str, name: &str, new_hash: &str) -> String {
    let mut out = String::new();
    let mut found = false;
    for line in text.lines() {
        let mut f: Vec<&str> = line.split(':').collect();
        if !line.starts_with('#') && f.len() >= 2 && f[0] == name {
            f[1] = new_hash;
            out.push_str(&f.join(":"));
            found = true;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !found {
        out.push_str(&format!("{name}:{new_hash}:::::::\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "# accounts\nroot:x:0:0:root:/root:/bin/sh\n\nflint:x:1000:1000:Flint User:/home/flint:/bin/sh\nbroken:line\nbad:x:abc:0::/:/bin/sh\n";

    #[test]
    fn parses_passwd_and_skips_bad_lines() {
        let u = parse_passwd(PASSWD);
        assert_eq!(u.len(), 2);
        assert_eq!(u[1].name, "flint");
        assert_eq!(u[1].uid, 1000);
        assert_eq!(u[1].home, "/home/flint");
        assert_eq!(find_user(&u, "root").unwrap().uid, 0);
        assert!(find_user(&u, "nobody").is_none());
        assert_eq!(find_user_by_uid(&u, 1000).unwrap().name, "flint");
    }

    #[test]
    fn groups() {
        let g = parse_group("root:x:0:\ntty:x:5:flint,root\nusers:x:100:flint\n# c\n");
        assert_eq!(g.len(), 3);
        assert_eq!(g[1].members, vec!["flint", "root"]);
        let u = parse_passwd(PASSWD);
        assert_eq!(groups_of(&g, &u[1]), vec![1000, 5, 100]);
        assert_eq!(groups_of(&g, &u[0]), vec![0, 5]);
    }

    #[test]
    fn shadow_lookup_and_edit() {
        let shadow = "# s\nroot:$flint1$old:19000:0:99999:7:::\nflint:!:19000::::::\n";
        assert_eq!(shadow_hash(shadow, "root"), Some("$flint1$old"));
        assert_eq!(shadow_hash(shadow, "flint"), Some("!"));
        assert_eq!(shadow_hash(shadow, "nobody"), None);

        let edited = set_shadow_hash(shadow, "root", "$flint1$new");
        assert_eq!(edited, "# s\nroot:$flint1$new:19000:0:99999:7:::\nflint:!:19000::::::\n");

        let added = set_shadow_hash(shadow, "bob", "$flint1$bob");
        assert!(added.ends_with("bob:$flint1$bob:::::::\n"));
        assert!(added.starts_with(shadow));
    }
}
