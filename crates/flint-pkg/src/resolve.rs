//! Dependency resolution: repositories, install and upgrade plans, removal and autoremove.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::control::{parse_index, Control, IndexEntry, THIS_ARCH};
use crate::db::Installed;
use crate::version::{compare, Constraint};

pub struct Repo {
    pub dir: PathBuf,
    pub entries: Vec<IndexEntry>,
}

pub fn load_repos(dirs: &[PathBuf]) -> Result<Vec<Repo>, String> {
    let mut repos = Vec::new();
    for dir in dirs {
        let index = dir.join("INDEX");
        let text = fs::read_to_string(&index).map_err(|e| format!("repository {}: cannot read INDEX: {e} (create it with: pkg index {})", dir.display(), dir.display()))?;
        let entries = parse_index(&text).map_err(|e| format!("repository {}: {e}", dir.display()))?;
        repos.push(Repo { dir: dir.clone(), entries });
    }
    Ok(repos)
}

#[derive(Debug, Clone)]
pub enum Source {
    Repo { path: PathBuf, size: u64, sha256: String },
    File(PathBuf),
}

#[derive(Debug, Clone)]
pub struct Step {
    pub control: Control,
    pub source: Source,
    pub explicit: bool,
    /// The version being replaced, for an upgrade.
    pub from: Option<String>,
}

pub enum Request {
    Name(Constraint),
    File(PathBuf, Control),
}

#[derive(Default)]
pub struct Plan {
    pub steps: Vec<Step>,
    /// Things worth telling the user (already installed, ...).
    pub notes: Vec<String>,
}

struct Planner<'a> {
    installed: &'a [Installed],
    repos: &'a [Repo],
    plan: Plan,
    visiting: HashSet<String>,
}

impl<'a> Planner<'a> {
    fn installed_pkg(&self, name: &str) -> Option<&'a Installed> {
        self.installed.iter().find(|i| i.control.name == name)
    }

    fn planned(&self, name: &str) -> Option<&Step> {
        self.plan.steps.iter().find(|s| s.control.name == name)
    }

    fn candidates(&self, name: &str) -> Vec<(&'a IndexEntry, &'a Repo)> {
        let mut v = Vec::new();
        for r in self.repos {
            for e in &r.entries {
                if e.control.name == name && (e.control.arch == "any" || e.control.arch == THIS_ARCH) {
                    v.push((e, r));
                }
            }
        }
        v
    }

    fn best(&self, c: &Constraint) -> Result<(&'a IndexEntry, &'a Repo), String> {
        let all = self.candidates(&c.name);
        let mut best: Option<(&IndexEntry, &Repo)> = None;
        for (e, r) in &all {
            if c.satisfied_by(&e.control.version) && best.map_or(true, |(b, _)| compare(&e.control.version, &b.control.version).is_gt()) {
                best = Some((e, r));
            }
        }
        best.ok_or_else(|| {
            if all.is_empty() {
                format!("package '{}' not found in any repository", c.name)
            } else {
                let mut vs: Vec<&str> = all.iter().map(|(e, _)| e.control.version.as_str()).collect();
                vs.sort_by(|a, b| compare(a, b));
                format!("no version of '{}' satisfies '{c}' (available: {})", c.name, vs.join(", "))
            }
        })
    }

    fn visit(&mut self, c: &Constraint, explicit: bool) -> Result<(), String> {
        if let Some(i) = self.plan.steps.iter().position(|s| s.control.name == c.name) {
            let v = self.plan.steps[i].control.version.clone();
            if !c.satisfied_by(&v) {
                return Err(format!("'{c}' is needed, but {} {v} is already planned", c.name));
            }
            if explicit {
                self.plan.steps[i].explicit = true;
            }
            return Ok(());
        }
        if let Some(inst) = self.installed_pkg(&c.name) {
            if c.satisfied_by(&inst.control.version) {
                if explicit {
                    self.plan.notes.push(format!("{} {} is already installed", c.name, inst.control.version));
                }
                return Ok(());
            }
        }
        let (entry, repo) = self.best(c)?;
        let source = Source::Repo { path: repo.dir.join(&entry.file), size: entry.size, sha256: entry.sha256.clone() };
        self.visit_candidate(entry.control.clone(), source, explicit)
    }

    fn visit_candidate(&mut self, control: Control, source: Source, explicit: bool) -> Result<(), String> {
        if self.visiting.contains(&control.name) {
            return Ok(()); // a dependency cycle: the other side is already being planned
        }
        self.visiting.insert(control.name.clone());
        for d in &control.depends {
            self.visit(d, false).map_err(|e| format!("{} {} needs {d}: {e}", control.name, control.version))?;
        }
        for c in &control.conflicts {
            let clash = self.installed_pkg(c).is_some() || self.planned(c).is_some();
            if clash {
                return Err(format!("{} conflicts with {c}", control.name));
            }
        }
        for i in self.installed {
            if i.control.name != control.name && i.control.conflicts.contains(&control.name) {
                return Err(format!("installed package {} conflicts with {}", i.control.name, control.name));
            }
        }
        let from = self.installed_pkg(&control.name).map(|i| i.control.version.clone());
        self.visiting.remove(&control.name);
        self.plan.steps.push(Step { control, source, explicit, from });
        Ok(())
    }
}

pub fn plan_install(reqs: &[Request], installed: &[Installed], repos: &[Repo]) -> Result<Plan, String> {
    let mut p = Planner { installed, repos, plan: Plan::default(), visiting: HashSet::new() };
    for r in reqs {
        match r {
            Request::Name(c) => p.visit(c, true)?,
            Request::File(path, control) => {
                if p.planned(&control.name).is_none() {
                    p.visit_candidate(control.clone(), Source::File(path.clone()), true)?;
                }
            }
        }
    }
    Ok(p.plan)
}

/// Upgrade the named packages (or all installed ones) to the newest repository version.
pub fn plan_upgrade(names: &[String], installed: &[Installed], repos: &[Repo]) -> Result<Plan, String> {
    for n in names {
        if !installed.iter().any(|i| &i.control.name == n) {
            return Err(format!("{n} is not installed"));
        }
    }
    let mut p = Planner { installed, repos, plan: Plan::default(), visiting: HashSet::new() };
    for inst in installed {
        let name = &inst.control.name;
        if !names.is_empty() && !names.contains(name) {
            continue;
        }
        if p.planned(name).is_some() {
            continue;
        }
        let Ok((entry, repo)) = p.best(&Constraint { name: name.clone(), req: None }) else { continue };
        if compare(&entry.control.version, &inst.control.version).is_gt() {
            let source = Source::Repo { path: repo.dir.join(&entry.file), size: entry.size, sha256: entry.sha256.clone() };
            p.visit_candidate(entry.control.clone(), source, inst.explicit)?;
        }
    }
    Ok(p.plan)
}

/// The order to remove `names` in (dependents first); fails if something else needs them.
pub fn plan_remove(names: &[String], installed: &[Installed], force: bool) -> Result<Vec<String>, String> {
    for n in names {
        if !installed.iter().any(|i| &i.control.name == n) {
            return Err(format!("{n} is not installed"));
        }
    }
    if !force {
        for i in installed {
            if names.contains(&i.control.name) {
                continue;
            }
            for d in &i.control.depends {
                if names.contains(&d.name) {
                    return Err(format!("{} is required by {} (remove that first, or use --force)", d.name, i.control.name));
                }
            }
        }
    }
    let mut left: Vec<String> = Vec::new();
    for n in names {
        if !left.contains(n) {
            left.push(n.clone());
        }
    }
    let mut order = Vec::new();
    while !left.is_empty() {
        let pick = left.iter().position(|n| {
            !left.iter().any(|other| {
                other != n && installed.iter().find(|i| &i.control.name == other).is_some_and(|i| i.control.depends.iter().any(|d| &d.name == n))
            })
        });
        // a cycle: just take the first
        order.push(left.remove(pick.unwrap_or(0)));
    }
    Ok(order)
}

/// Packages installed only as dependencies that nothing explicit needs any more.
pub fn autoremovable(installed: &[Installed]) -> Vec<String> {
    let mut needed: HashSet<&str> = HashSet::new();
    let mut stack: Vec<&Installed> = installed.iter().filter(|i| i.explicit).collect();
    while let Some(i) = stack.pop() {
        if !needed.insert(i.control.name.as_str()) {
            continue;
        }
        for d in &i.control.depends {
            if let Some(dep) = installed.iter().find(|x| x.control.name == d.name) {
                stack.push(dep);
            }
        }
    }
    installed.iter().filter(|i| !needed.contains(i.control.name.as_str())).map(|i| i.control.name.clone()).collect()
}

pub fn repo_dirs_from_conf(text: &str) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some(loc) = line.strip_prefix("repo").map(str::trim) else {
            return Err(format!("pkg.conf: unknown line '{line}' (expected: repo PATH)"));
        };
        let path = loc.strip_prefix("file://").unwrap_or(loc);
        if path.contains("://") {
            return Err(format!("pkg.conf: '{loc}': only local directory repositories are supported so far"));
        }
        if path.is_empty() {
            return Err("pkg.conf: 'repo' needs a path".into());
        }
        out.push(Path::new(path).to_path_buf());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(text: &str) -> IndexEntry {
        IndexEntry { control: Control::parse(text).unwrap(), file: "x.fpk".into(), size: 1, sha256: "00".repeat(32) }
    }

    fn repo(texts: &[&str]) -> Repo {
        Repo { dir: PathBuf::from("/repo"), entries: texts.iter().map(|t| entry(t)).collect() }
    }

    fn inst(text: &str, explicit: bool) -> Installed {
        Installed { control: Control::parse(text).unwrap(), files: vec![], installed_at: 0, explicit }
    }

    fn names(p: &Plan) -> Vec<String> {
        p.steps.iter().map(|s| format!("{}-{}", s.control.name, s.control.version)).collect()
    }

    fn req(s: &str) -> Request {
        Request::Name(Constraint::parse(s).unwrap())
    }

    #[test]
    fn dependencies_come_first_and_the_best_version_wins() {
        let r = repo(&["name: app\nversion: 1\ndepends: lib>=1.1, tool\n", "name: lib\nversion: 1.0\n", "name: lib\nversion: 1.2\n", "name: lib\nversion: 1.10\n", "name: tool\nversion: 3\ndepends: lib\n"]);
        let p = plan_install(&[req("app")], &[], &[r]).unwrap();
        assert_eq!(names(&p), ["lib-1.10", "tool-3", "app-1"]);
        assert!(p.steps[2].explicit && !p.steps[0].explicit && !p.steps[1].explicit);
    }

    #[test]
    fn installed_dependencies_are_reused_and_errors_are_clear() {
        let r = repo(&["name: app\nversion: 1\ndepends: lib>=1\n", "name: lib\nversion: 2\n"]);
        let have = [inst("name: lib\nversion: 1.5\n", false)];
        assert_eq!(names(&plan_install(&[req("app")], &have, &[r]).unwrap()), ["app-1"]);

        let r = repo(&["name: app\nversion: 1\ndepends: lib>=3\n", "name: lib\nversion: 2\n"]);
        let e = plan_install(&[req("app")], &[], &[r]).map(|_| ()).unwrap_err();
        assert!(e.contains("app 1 needs lib>=3") && e.contains("available: 2"), "{e}");
        let e = plan_install(&[req("nothing")], &[], &[repo(&[])]).map(|_| ()).unwrap_err();
        assert!(e.contains("not found"), "{e}");
    }

    #[test]
    fn already_installed_is_a_note_not_a_step() {
        let r = repo(&["name: lib\nversion: 2\n"]);
        let p = plan_install(&[req("lib")], &[inst("name: lib\nversion: 2\n", true)], &[r]).unwrap();
        assert!(p.steps.is_empty());
        assert!(p.notes[0].contains("already installed"));
    }

    #[test]
    fn cycles_terminate() {
        let r = repo(&["name: a\nversion: 1\ndepends: b\n", "name: b\nversion: 1\ndepends: a\n"]);
        assert_eq!(plan_install(&[req("a")], &[], &[r]).unwrap().steps.len(), 2);
    }

    #[test]
    fn conflicts_are_detected_both_ways() {
        let r = repo(&["name: a\nversion: 1\nconflicts: b\n", "name: b\nversion: 1\n"]);
        assert!(plan_install(&[req("a")], &[inst("name: b\nversion: 1\n", true)], &[r]).is_err());
        let r = repo(&["name: b\nversion: 1\n"]);
        assert!(plan_install(&[req("b")], &[inst("name: a\nversion: 1\nconflicts: b\n", true)], &[r]).is_err());
    }

    #[test]
    fn upgrades() {
        let r = repo(&["name: lib\nversion: 2\n", "name: app\nversion: 2\ndepends: lib>=2\n", "name: same\nversion: 1\n"]);
        let have = [inst("name: lib\nversion: 1\n", false), inst("name: app\nversion: 1\ndepends: lib\n", true), inst("name: same\nversion: 1\n", true)];
        let p = plan_upgrade(&[], &have, &[r]).unwrap();
        assert_eq!(names(&p), ["lib-2", "app-2"]);
        assert_eq!(p.steps[0].from.as_deref(), Some("1"));
        assert!(p.steps[1].explicit && !p.steps[0].explicit, "the original reason is kept");
        let r = repo(&["name: lib\nversion: 2\n"]);
        assert!(plan_upgrade(&["nope".to_string()], &have, &[r]).is_err());
    }

    #[test]
    fn upgrade_pulls_in_new_dependencies() {
        let r = repo(&["name: app\nversion: 2\ndepends: newlib\n", "name: newlib\nversion: 1\n"]);
        let have = [inst("name: app\nversion: 1\n", true)];
        assert_eq!(names(&plan_upgrade(&[], &have, &[r]).unwrap()), ["newlib-1", "app-2"]);
    }

    #[test]
    fn removal_rules() {
        let have = [inst("name: app\nversion: 1\ndepends: lib\n", true), inst("name: lib\nversion: 1\n", false), inst("name: other\nversion: 1\n", true)];
        let e = plan_remove(&["lib".to_string()], &have, false).unwrap_err();
        assert!(e.contains("required by app"), "{e}");
        assert_eq!(plan_remove(&["lib".to_string()], &have, true).unwrap(), ["lib"]);
        assert_eq!(plan_remove(&["lib".to_string(), "app".to_string()], &have, false).unwrap(), ["app", "lib"], "dependents first");
        assert_eq!(plan_remove(&["app".to_string(), "lib".to_string(), "app".to_string()], &have, false).unwrap(), ["app", "lib"], "duplicates are dropped");
        assert!(plan_remove(&["ghost".to_string()], &have, false).is_err());
    }

    #[test]
    fn autoremove_finds_orphans() {
        let have = [inst("name: app\nversion: 1\ndepends: lib\n", true), inst("name: lib\nversion: 1\ndepends: deep\n", false), inst("name: deep\nversion: 1\n", false), inst("name: orphan\nversion: 1\n", false)];
        assert_eq!(autoremovable(&have), ["orphan"]);
        let have = [inst("name: lib\nversion: 1\n", false)];
        assert_eq!(autoremovable(&have), ["lib"]);
    }

    #[test]
    fn repo_config() {
        assert_eq!(repo_dirs_from_conf("# c\nrepo /var/repo\nrepo file:///mnt/usb # x\n").unwrap(), [PathBuf::from("/var/repo"), PathBuf::from("/mnt/usb")]);
        assert!(repo_dirs_from_conf("repo https://example.org/r\n").is_err());
        assert!(repo_dirs_from_conf("mirror /x\n").is_err());
        assert!(repo_dirs_from_conf("repo\n").is_err());
        assert!(repo_dirs_from_conf("").unwrap().is_empty());
    }
}
