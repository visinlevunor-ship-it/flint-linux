//! The job table for interactive job control.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Running,
    Stopped,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: usize,
    pub pgid: i32,
    /// Processes of the job that have not exited yet.
    pub pids: Vec<i32>,
    pub last_pid: i32,
    /// Exit status of `last_pid` once it has exited.
    pub status: i32,
    pub cmd: String,
    pub state: JobState,
}

#[derive(Default)]
pub struct Jobs {
    pub list: Vec<Job>,
}

impl Jobs {
    pub fn add(&mut self, pgid: i32, pids: Vec<i32>, cmd: String, state: JobState) -> usize {
        let id = self.list.iter().map(|j| j.id).max().unwrap_or(0) + 1;
        let last_pid = pids.last().copied().unwrap_or(pgid);
        self.list.push(Job { id, pgid, pids, last_pid, status: 0, cmd, state });
        id
    }

    pub fn get_mut(&mut self, id: usize) -> Option<&mut Job> {
        self.list.iter_mut().find(|j| j.id == id)
    }

    pub fn remove(&mut self, id: usize) {
        self.list.retain(|j| j.id != id);
    }

    /// `%n`, `%%`, `%+`, `%-`, a bare `n`, or nothing (the current job).
    pub fn resolve(&self, spec: Option<&str>) -> Option<usize> {
        let spec = spec.map(|s| s.strip_prefix('%').unwrap_or(s));
        match spec {
            None | Some("") | Some("%") | Some("+") => self.list.last().map(|j| j.id),
            Some("-") => self.list.len().checked_sub(2).and_then(|i| self.list.get(i)).map(|j| j.id),
            Some(n) => n.parse::<usize>().ok().filter(|id| self.list.iter().any(|j| j.id == *id)),
        }
    }

    pub fn job_of_pid(&mut self, pid: i32) -> Option<&mut Job> {
        self.list.iter_mut().find(|j| j.pids.contains(&pid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_specs() {
        let mut j = Jobs::default();
        let a = j.add(10, vec![10], "a".into(), JobState::Running);
        let b = j.add(20, vec![20, 21], "b".into(), JobState::Stopped);
        assert_eq!((a, b), (1, 2));
        assert_eq!(j.resolve(None), Some(2));
        assert_eq!(j.resolve(Some("%%")), Some(2));
        assert_eq!(j.resolve(Some("%-")), Some(1));
        assert_eq!(j.resolve(Some("%1")), Some(1));
        assert_eq!(j.resolve(Some("2")), Some(2));
        assert_eq!(j.resolve(Some("%9")), None);
        assert_eq!(j.get_mut(2).unwrap().last_pid, 21);
        assert_eq!(j.job_of_pid(21).unwrap().id, 2);
        j.remove(2);
        assert_eq!(j.resolve(None), Some(1));
        assert_eq!(j.add(30, vec![30], "c".into(), JobState::Running), 2, "ids are reused");
    }
}
