use gix_testtools::TestResult;

/// Return `<commit> <parent> <path>` for each non-tree path that changed between each commit reachable from `main`
/// and each of its parents, using `-` as parent of root commits, sorted like the baseline.
fn changed_paths(repo: &gix::Repository) -> gix::Result<Vec<String>> {
    let options = gix::diff::Options::default().with_rewrites(None);
    let shallow_commits = repo.shallow_commits()?;
    let mut out = Vec::new();
    for info in repo.rev_walk([repo.find_reference("main")?.id()]).all()? {
        let info = info?;
        let tree = info.object()?.tree()?;
        // Like Git, treat shallow boundary commits as root commits, as their parents aren't available.
        let is_shallow = shallow_commits
            .as_ref()
            .is_some_and(|commits| commits.contains(&info.id));
        let parent_ids: Vec<_> = info.parent_ids().filter(|_| !is_shallow).collect();
        let parents = if parent_ids.is_empty() {
            vec![None]
        } else {
            parent_ids.into_iter().map(Some).collect()
        };
        for parent_id in parents {
            let parent_tree = parent_id.map(|id| id.object()?.into_commit().tree()).transpose()?;
            let parent = parent_id.map_or_else(|| "-".to_owned(), |id| id.to_string());
            for change in repo.diff_tree_to_tree(parent_tree.as_ref(), &tree, options)? {
                // Like `git diff-tree -r`, list changed leaves, but not the trees containing them.
                if !change.entry_mode().is_tree() {
                    out.push(format!("{} {parent} {}", info.id, change.location()));
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

#[test]
fn full_history_matches_git_with_every_pack_cache() -> TestResult {
    let fixture = gix_testtools::scripted_fixture_read_only("make_history_changed_paths.sh")?;
    for name in ["repo", "replaced", "shallow.git"] {
        let path = fixture.join(name);
        let expected: Vec<String> = std::fs::read_to_string(path.join("changed-paths.baseline"))?
            .lines()
            .map(str::to_owned)
            .collect();
        assert!(
            expected.len() > 150,
            "{name}: the baseline lists the changes of many commits"
        );
        for overrides in [
            &[][..],
            &["core.deltaBaseCacheLimit=0"],
            &["core.deltaBaseCacheLimit=4k"],
            &["gitoxide.core.defaultPackCacheMemoryLimit=4k"],
        ] {
            let repo = gix::open_opts(
                &path,
                gix::open::Options::isolated().config_overrides(overrides.iter().copied()),
            )?;
            assert_eq!(
                changed_paths(&repo)?,
                expected,
                "{name}: the default cache and the caches selected by {overrides:?} produce the same changed paths as Git"
            );
        }
    }
    Ok(())
}
