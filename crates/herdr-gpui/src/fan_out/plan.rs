//! What a fan-out asks for: the agents picked, the branch and agent name each
//! lane gets, and how a lane's changes are read and summarized. Pure, so it
//! is tested without a host.

use crate::teleport::AgentKind;
use herdr_client::shell_quote;

/// The most lanes one fan-out starts: each is a checkout and a running agent.
pub(crate) const MAX_LANES: usize = 6;

/// Branches of one fan-out share this namespace and a generated stem.
const PREFIX: &str = "fan-out";

/// How many lanes of each agent kind are picked, in the order first picked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Picks(Vec<(AgentKind, usize)>);

impl Picks {
    pub(crate) fn count(&self, kind: AgentKind) -> usize {
        self.0
            .iter()
            .find(|(picked, _)| *picked == kind)
            .map_or(0, |(_, count)| *count)
    }

    pub(crate) fn total(&self) -> usize {
        self.0.iter().map(|(_, count)| count).sum()
    }

    /// Add a lane of `kind`, unless the fan-out is already full.
    pub(crate) fn add(&mut self, kind: AgentKind) -> bool {
        if self.total() >= MAX_LANES {
            return false;
        }
        match self.0.iter_mut().find(|(picked, _)| *picked == kind) {
            Some((_, count)) => *count += 1,
            None => self.0.push((kind, 1)),
        }
        true
    }

    /// Drop one lane of `kind`, if any is picked.
    pub(crate) fn remove(&mut self, kind: AgentKind) -> bool {
        let Some(index) = self.0.iter().position(|(picked, _)| *picked == kind) else {
            return false;
        };
        self.0[index].1 -= 1;
        if self.0[index].1 == 0 {
            self.0.remove(index);
        }
        true
    }
}

/// One agent's share of a fan-out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Lane {
    pub(crate) kind: AgentKind,
    pub(crate) branch: String,
    /// The Herdr agent name, unique to this lane.
    pub(crate) agent: String,
}

/// One lane per pick, all named after `seed` so a fan-out's branches sort
/// together: `fan-out/calm-river-1a2b-claude`, then `...-claude-2` for a
/// second lane of the same agent.
pub(crate) fn lanes(picks: &Picks, seed: u64) -> Vec<Lane> {
    let generated = crate::worktree::generated_branch_slug(seed);
    let stem = generated
        .split_once('/')
        .map_or(generated.as_str(), |(_, stem)| stem);
    let mut lanes = Vec::with_capacity(picks.total());
    for (kind, count) in &picks.0 {
        for number in 1..=*count {
            let agent = match number {
                1 => format!("{stem}-{}", kind.name()),
                _ => format!("{stem}-{}-{number}", kind.name()),
            };
            lanes.push(Lane {
                kind: *kind,
                branch: format!("{PREFIX}/{agent}"),
                agent,
            });
        }
    }
    lanes
}

/// What one lane changed relative to the fan-out's base commit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DiffStat {
    /// Tracked files differing from the base, committed or not.
    pub(crate) files: u64,
    pub(crate) additions: u64,
    pub(crate) deletions: u64,
    pub(crate) untracked: u64,
    /// Commits on the lane's branch since the base.
    pub(crate) commits: u64,
}

impl DiffStat {
    pub(crate) fn summary(&self) -> String {
        if *self == Self::default() {
            return "No changes yet".to_owned();
        }
        let plural = |count: u64, one: &str, many: &str| {
            format!("{count} {}", if count == 1 { one } else { many })
        };
        let mut parts = vec![format!("+{} −{}", self.additions, self.deletions)];
        parts.push(plural(self.files, "file", "files"));
        if self.untracked > 0 {
            parts.push(format!("{} untracked", self.untracked));
        }
        if self.commits > 0 {
            parts.push(plural(self.commits, "commit", "commits"));
        }
        parts.join(" · ")
    }
}

/// Separates one lane's record in the stats script's output.
const RECORD: char = '\x1e';
/// Separates the sections of one record.
const SECTION: char = '\x1f';

/// A script reporting, for each checkout still on disk, its tracked changes
/// since `base` (working tree included), its untracked files, and its commits
/// since `base`. A checkout that is gone prints nothing.
pub(crate) fn stats_script<S: AsRef<str>>(checkouts: &[S], base: &str) -> String {
    let base = shell_quote(base);
    let mut script = String::new();
    for (index, checkout) in checkouts.iter().enumerate() {
        let path = shell_quote(checkout.as_ref());
        script.push_str(&format!(
            r#"if git -C {path} rev-parse --git-dir >/dev/null 2>&1; then
printf '\036%s\n' {index}
git -C {path} diff --numstat {base} -- 2>/dev/null || :
printf '\037\n'
git -C {path} ls-files --others --exclude-standard 2>/dev/null | wc -l
printf '\037\n'
git -C {path} rev-list --count {base}..HEAD 2>/dev/null || printf '0\n'
fi
"#
        ));
    }
    script
}

/// Read [`stats_script`]'s output for `lanes` checkouts. A lane without a
/// well-formed record has no stats.
pub(crate) fn parse_stats(output: &str, lanes: usize) -> Vec<Option<DiffStat>> {
    let mut stats = vec![None; lanes];
    for record in output.split(RECORD).skip(1) {
        let Some((index, rest)) = record.split_once('\n') else {
            continue;
        };
        let Some(slot) = index
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|index| stats.get_mut(index))
        else {
            continue;
        };
        let mut sections = rest.split(SECTION);
        let (Some(numstat), Some(untracked), Some(commits), None) = (
            sections.next(),
            sections.next(),
            sections.next(),
            sections.next(),
        ) else {
            continue;
        };
        let (Ok(untracked), Ok(commits)) = (
            untracked.trim().parse::<u64>(),
            commits.trim().parse::<u64>(),
        ) else {
            continue;
        };
        let lines = crate::git::parse_numstat(numstat);
        *slot = Some(DiffStat {
            files: numstat
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count() as u64,
            additions: lines.additions,
            deletions: lines.deletions,
            untracked,
            commits,
        });
    }
    stats
}

/// The commit `git rev-parse` printed, when it is one.
pub(crate) fn parse_commit(output: &str) -> Option<String> {
    let commit = output.trim();
    (matches!(commit.len(), 40 | 64) && commit.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| commit.to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn picks_are_bounded_and_drop_empty_kinds() {
        let mut picks = Picks::default();
        assert!(picks.add(AgentKind::Claude));
        assert!(picks.add(AgentKind::Codex));
        assert!(picks.add(AgentKind::Claude));
        assert_eq!(picks.count(AgentKind::Claude), 2);
        assert_eq!(picks.total(), 3);
        for _ in 3..MAX_LANES {
            assert!(picks.add(AgentKind::Pi));
        }
        assert!(!picks.add(AgentKind::Pi), "a full fan-out takes no more");
        assert_eq!(picks.total(), MAX_LANES);
        assert!(picks.remove(AgentKind::Codex));
        assert!(!picks.remove(AgentKind::Codex));
        assert_eq!(picks.count(AgentKind::Codex), 0);
        assert!(!picks.0.iter().any(|(kind, _)| *kind == AgentKind::Codex));
    }

    #[test]
    fn lanes_share_a_stem_and_number_repeated_agents() {
        let mut picks = Picks::default();
        picks.add(AgentKind::Claude);
        picks.add(AgentKind::Codex);
        picks.add(AgentKind::Claude);
        let lanes = lanes(&picks, 0);
        let stem = crate::worktree::generated_branch_slug(0)
            .strip_prefix("worktree/")
            .unwrap()
            .to_owned();
        let names: Vec<_> = lanes.iter().map(|lane| lane.branch.as_str()).collect();
        assert_eq!(
            names,
            [
                format!("fan-out/{stem}-claude"),
                format!("fan-out/{stem}-claude-2"),
                format!("fan-out/{stem}-codex"),
            ]
        );
        for lane in &lanes {
            assert!(crate::worktree::validate_branch(&lane.branch).is_ok());
            assert_eq!(lane.branch, format!("fan-out/{}", lane.agent));
        }
        assert_eq!(lanes[2].kind, AgentKind::Codex);
    }

    #[test]
    fn stats_parse_each_lane_and_skip_gone_or_garbled_records() {
        let output = "\x1e0\n3\t1\tsrc/a.rs\n-\t-\tlogo.png\n\x1f\n       2\n\x1f\n1\n\
                      \x1e2\n\x1f\n0\n\x1f\n0\n\
                      \x1e1\nnot a record\n\
                      \x1e9\n\x1f\n0\n\x1f\n0\n";
        assert_eq!(
            parse_stats(output, 3),
            [
                Some(DiffStat {
                    files: 2,
                    additions: 3,
                    deletions: 1,
                    untracked: 2,
                    commits: 1,
                }),
                None,
                Some(DiffStat::default()),
            ]
        );
        assert_eq!(parse_stats("", 2), [None, None]);
    }

    #[test]
    fn summaries_read_naturally() {
        assert_eq!(DiffStat::default().summary(), "No changes yet");
        let one = DiffStat {
            files: 1,
            additions: 4,
            deletions: 0,
            untracked: 0,
            commits: 1,
        };
        assert_eq!(one.summary(), "+4 −0 · 1 file · 1 commit");
        let many = DiffStat {
            files: 3,
            additions: 10,
            deletions: 2,
            untracked: 5,
            commits: 0,
        };
        assert_eq!(many.summary(), "+10 −2 · 3 files · 5 untracked");
    }

    #[test]
    fn only_full_object_names_are_commits() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(parse_commit(&format!("{sha}\n")).as_deref(), Some(sha));
        assert_eq!(parse_commit("HEAD"), None);
        assert_eq!(parse_commit("0123456"), None);
        assert_eq!(parse_commit(&format!("{sha}; rm -rf /")), None);
    }

    #[test]
    fn stats_script_quotes_paths_and_base() {
        let script = stats_script(&["/tmp/it's here"], "abc");
        assert!(script.contains("git -C '/tmp/it'\\''s here' diff --numstat 'abc' --"));
        assert!(script.contains("rev-list --count 'abc'..HEAD"));
    }

    /// The script against a real checkout: a commit, an uncommitted edit,
    /// and an untracked file all show up; a missing checkout prints nothing.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn stats_script_reads_a_real_checkout() {
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .output()
                .unwrap();
            assert!(status.status.success(), "{args:?}: {status:?}");
            String::from_utf8(status.stdout).unwrap()
        };
        git(&["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        let base = parse_commit(&git(&["rev-parse", "HEAD"])).unwrap();
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        git(&["commit", "-qam", "lane"]);
        std::fs::write(repo.join("a.txt"), "uno\ntwo\nthree\n").unwrap();
        std::fs::write(repo.join("new.txt"), "x\n").unwrap();

        let missing = dir.path().join("gone");
        let paths = [
            repo.to_str().unwrap().to_owned(),
            missing.to_str().unwrap().to_owned(),
        ];
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("set -eu\n{}", stats_script(&paths, &base)))
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stats = parse_stats(&String::from_utf8(output.stdout).unwrap(), 2);
        assert_eq!(
            stats,
            [
                Some(DiffStat {
                    files: 1,
                    additions: 3,
                    deletions: 1,
                    untracked: 1,
                    commits: 1,
                }),
                None,
            ]
        );
    }
}
