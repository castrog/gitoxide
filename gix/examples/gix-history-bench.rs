//! Benchmark a full-history walk that enumerates the paths changed by every commit, and its parts.
//!
//! Run from the workspace root against any repository, for example one created by
//! `etc/scripts/make-wide-tree-history.sh`:
//!
//! ```sh
//! cargo run --release -p gix --example gix-history-bench -- /path/to/repository diff
//! ```
//!
//! Modes separate the cost of the history walk into its parts:
//!
//! * `walk`: traverse all commits reachable from `HEAD` and read the root tree ID of each commit and its parents.
//! * `decode`: like `diff`, but also record the sequence of trees the diff looks up, then look up that sequence
//!   again with a fresh object database handle and print the time of only that second pass as `decode=`.
//!   This is the object decoding cost of `diff`; the process wall time includes the `diff` pass as well.
//! * `diff`: `walk`, then diff every commit against each of its parents (or the empty tree for root commits),
//!   with rename tracking disabled, and count the changed non-tree paths.
//! * `print`: like `diff`, but print `<commit> <parent>` (or `<commit> -` for root commits) followed by each
//!   changed non-tree path, one per line. Compare it with `git diff-tree -r --no-renames --name-only`.
//!
//! Options must follow the mode:
//!
//! * `--pack-cache <spec>` selects the pack delta cache through configuration overrides: `none`
//!   (`core.deltaBaseCacheLimit=0`), `static:<bytes>` (the fixed 64-entry cache selected by
//!   `gitoxide.core.defaultPackCacheMemoryLimit`), or `dynamic:<bytes>` (the memory-capped hash map selected by
//!   `core.deltaBaseCacheLimit`). Byte counts accept `k`, `m`, and `g` suffixes.
//! * `--object-cache <bytes>` enables the cache of fully decoded objects with `gitoxide.objects.cacheLimit`.
//! * `--reuse-recorder` reuses one `Recorder` for all diffs instead of creating one per diff. As the `Recorder`
//!   doesn't reset its current path between diffs, this produces wrong paths and is only useful to measure that cost.
//! * `--threads <n>` splits the commits into `n` contiguous chunks and diffs them in parallel in `diff` mode,
//!   each thread with its own repository handle and thus its own object caches.
//!
//! Statistics go to stderr, so the timing of `print` output can be compared with `git log`.
//! `etc/scripts/bench-wide-tree-history.sh` compares all modes with Git using `hyperfine`, which also reports peak memory.
//! Peak resident memory includes the memory-mapped packs; on macOS, `/usr/bin/time -l` also reports the peak memory
//! footprint, which excludes them.

use std::{
    cell::RefCell,
    collections::HashMap,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

use gix::{
    diff::tree::{Recorder, recorder::Change},
    hash::ObjectId,
    objs::{Data, Find, FindExt},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Walk,
    Decode,
    Diff,
    Print,
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let usage = "Usage: gix-history-bench <repository> <walk|decode|diff|print> [--pack-cache none|static:<bytes>|dynamic:<bytes>] [--object-cache <bytes>] [--reuse-recorder] [--threads <n>]";
    let repository = PathBuf::from(args.next().ok_or(usage)?);
    let mode = match args.next().as_deref() {
        Some("walk") => Mode::Walk,
        Some("decode") => Mode::Decode,
        Some("diff") => Mode::Diff,
        Some("print") => Mode::Print,
        _ => return Err(usage.into()),
    };
    let mut config = Vec::new();
    let mut reuse_recorder = false;
    let mut threads = 1;
    while let Some(arg) = args.next() {
        if arg == "--reuse-recorder" {
            reuse_recorder = true;
            continue;
        }
        let value = args.next().ok_or(usage)?;
        match arg.as_str() {
            "--pack-cache" => config.push(pack_cache_config(&value)?),
            "--object-cache" => config.push(format!("gitoxide.objects.cacheLimit={value}")),
            "--threads" => threads = value.parse()?,
            _ => return Err(usage.into()),
        }
    }
    if threads == 0 || (threads > 1 && mode != Mode::Diff) {
        return Err("--threads must be positive, and more than one thread is only supported in `diff` mode".into());
    }

    let start = Instant::now();
    let open = || {
        gix::open_opts(
            &repository,
            gix::open::Options::default().config_overrides(config.clone()),
        )
    };
    let repo = open()?;
    let commits = walk(&repo)?;
    let walk_time = start.elapsed();
    if mode == Mode::Walk {
        eprintln!("commits={} walk={}", commits.len(), ms(walk_time));
        return Ok(());
    }

    let looked_up = RefCell::new(Vec::new());
    let diff_start = Instant::now();
    let (pairs, changed_paths) = if threads > 1 {
        // Each thread gets its own repository handle, and with it its own object caches.
        let chunk_size = commits.len().div_ceil(threads);
        std::thread::scope(|scope| {
            let handles: Vec<_> = commits
                .chunks(chunk_size)
                .map(|chunk| {
                    let repo = repo.clone();
                    scope.spawn(move || diff_commits(&repo, chunk, mode, reuse_recorder, None, &mut std::io::sink()))
                })
                .collect();
            handles.into_iter().try_fold((0, 0), |(pairs, changed_paths), handle| {
                let (p, c) = handle.join().expect("no panic")?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>((pairs + p, changed_paths + c))
            })
        })?
    } else {
        let stdout = std::io::stdout();
        let mut out = std::io::BufWriter::new(stdout.lock());
        let trees = (mode == Mode::Decode).then_some(&looked_up);
        let counts = diff_commits(&repo, &commits, mode, reuse_recorder, trees, &mut out)?;
        out.flush()?;
        counts
    };
    let diff_time = diff_start.elapsed();

    if mode == Mode::Decode {
        let trees = looked_up.into_inner();
        // A fresh handle starts with empty caches, just like the diff pass did.
        let repo = open()?;
        let mut buf = Vec::new();
        let decode_start = Instant::now();
        let mut bytes = 0usize;
        for tree_id in &trees {
            bytes += repo.objects.find(tree_id, &mut buf)?.data.len();
        }
        let decode_time = decode_start.elapsed();
        eprintln!(
            "commits={} tree_lookups={} tree_bytes={} decode={}",
            commits.len(),
            trees.len(),
            bytes,
            ms(decode_time)
        );
        return Ok(());
    }
    eprintln!(
        "commits={} pairs={pairs} changed_paths={changed_paths} walk={} diff={} total={}",
        commits.len(),
        ms(walk_time),
        ms(diff_time),
        ms(start.elapsed())
    );
    Ok(())
}

/// Return the configuration that selects the cache described by `--pack-cache <spec>`.
///
/// Configuration, unlike caches set on a repository handle, also applies to clones of the repository.
fn pack_cache_config(spec: &str) -> Result<String> {
    Ok(match spec.split_once(':') {
        _ if spec == "none" => "core.deltaBaseCacheLimit=0".into(),
        Some(("static", bytes)) => format!("gitoxide.core.defaultPackCacheMemoryLimit={bytes}"),
        Some(("dynamic", bytes)) => format!("core.deltaBaseCacheLimit={bytes}"),
        _ => return Err(format!("invalid pack cache '{spec}'").into()),
    })
}

/// Diff each of `commits` with each of its parents, writing them and their changed paths to `out` in `print` mode,
/// and recording looked-up trees in `trees`. Return the amount of diffed pairs and changed paths.
fn diff_commits(
    repo: &gix::Repository,
    commits: &[CommitWithTrees],
    mode: Mode,
    reuse_recorder: bool,
    trees: Option<&RefCell<Vec<ObjectId>>>,
    out: &mut dyn Write,
) -> Result<(usize, usize)> {
    let objects = Recording {
        inner: &repo.objects,
        trees,
    };
    let mut state = gix::diff::tree::State::default();
    let (mut lhs_buf, mut rhs_buf) = (Vec::new(), Vec::new());
    let mut recorder = Recorder::default();
    let mut pairs = 0usize;
    let mut changed_paths = 0usize;
    for (commit_id, tree_id, parent_tree_ids) in commits {
        let parents: Vec<_> = if parent_tree_ids.is_empty() {
            vec![None]
        } else {
            parent_tree_ids.iter().map(Some).collect()
        };
        for parent in parents {
            pairs += 1;
            if reuse_recorder {
                recorder.records.clear();
            } else {
                recorder = Recorder::default();
            }
            // Root commits are compared with the empty tree, which doesn't have to exist in the object database.
            let lhs = match parent {
                Some((_, parent_tree_id)) => objects.find_tree_iter(parent_tree_id, &mut lhs_buf)?,
                None => gix::objs::TreeRefIter::from_bytes(&[], repo.object_hash()),
            };
            let rhs = objects.find_tree_iter(tree_id, &mut rhs_buf)?;
            gix::diff::tree(lhs, rhs, &mut state, &objects, &mut recorder)?;
            if mode == Mode::Print {
                match parent {
                    Some((parent_id, _)) => writeln!(out, "{commit_id} {parent_id}")?,
                    None => writeln!(out, "{commit_id} -")?,
                }
            }
            for change in &recorder.records {
                // Like `git diff-tree -r`, report the leaves of changed subtrees but not the subtrees themselves.
                // Type changes are reported as a deletion and an addition, so `entry_mode` decides for all changes.
                let (Change::Addition { entry_mode, path, .. }
                | Change::Deletion { entry_mode, path, .. }
                | Change::Modification { entry_mode, path, .. }) = change;
                if entry_mode.is_tree() {
                    continue;
                }
                changed_paths += 1;
                if mode == Mode::Print {
                    writeln!(out, "{path}")?;
                }
            }
        }
    }
    Ok((pairs, changed_paths))
}

/// A commit ID, its root tree ID, and the IDs and root tree IDs of its parents.
type CommitWithTrees = (ObjectId, ObjectId, Vec<(ObjectId, ObjectId)>);

/// Return each commit reachable from `HEAD` with its root tree ID and the IDs and root trees of its parents.
fn walk(repo: &gix::Repository) -> Result<Vec<CommitWithTrees>> {
    let mut tree_ids = HashMap::<ObjectId, ObjectId>::new();
    let mut tree_of = |id: ObjectId| -> Result<ObjectId> {
        if let Some(tree_id) = tree_ids.get(&id) {
            return Ok(*tree_id);
        }
        let tree_id = repo.find_commit(id)?.tree_id()?.detach();
        tree_ids.insert(id, tree_id);
        Ok(tree_id)
    };
    let mut commits = Vec::new();
    for info in repo.head_id()?.ancestors().all()? {
        let info = info?;
        let tree_id = tree_of(info.id)?;
        let parents = info
            .parent_ids
            .iter()
            .map(|parent_id| Ok((*parent_id, tree_of(*parent_id)?)))
            .collect::<Result<Vec<_>>>()?;
        commits.push((info.id, tree_id, parents));
    }
    Ok(commits)
}

/// Forward lookups to `inner`, recording the IDs of trees if `trees` is set.
struct Recording<'a> {
    inner: &'a gix::OdbHandle,
    trees: Option<&'a RefCell<Vec<ObjectId>>>,
}

impl Find for Recording<'_> {
    fn try_find<'a>(&self, id: &gix::hash::oid, buffer: &'a mut Vec<u8>) -> gix::Result<Option<Data<'a>>> {
        if let Some(trees) = self.trees {
            trees.borrow_mut().push(id.to_owned());
        }
        self.inner.try_find(id, buffer)
    }
}

fn ms(duration: Duration) -> String {
    format!("{:.1}ms", duration.as_secs_f64() * 1000.0)
}
