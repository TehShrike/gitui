//! undoing a staging operation by applying its reverse as a patch to the index

use super::{repository::repo, RepoPath};
use crate::error::{Error, Result};
use git2::{
	ApplyLocation, Diff, DiffFormat, DiffOptions, Oid, Patch,
	Repository,
};
use scopetime::scope_time;

/// state of the index before a staging operation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexSnapshot {
	tree: Oid,
	head: Option<Oid>,
}

/// the reverse of one staging operation
#[derive(Debug, Clone)]
pub struct StagingUndo {
	patch: Vec<u8>,
	head: Option<Oid>,
	/// first file touched
	pub path: String,
	/// a line in `path`, numbered as in the index after undoing,
	/// close to the first change
	pub index_line: u32,
}

/// fails while the index can't be written as a tree, e.g. during a merge conflict
pub fn snapshot_index(repo_path: &RepoPath) -> Result<IndexSnapshot> {
	scope_time!("snapshot_index");

	let repo = repo(repo_path)?;
	snapshot(&repo)
}

fn snapshot(repo: &Repository) -> Result<IndexSnapshot> {
	Ok(IndexSnapshot {
		tree: repo.index()?.write_tree()?,
		head: repo.head().ok().and_then(|h| h.target()),
	})
}

/// how to get the index from its current state back to `before`,
/// or `None` if the index has not changed since
pub fn staging_undo(
	repo_path: &RepoPath,
	before: &IndexSnapshot,
) -> Result<Option<StagingUndo>> {
	scope_time!("staging_undo");

	let repo = repo(repo_path)?;
	let after = snapshot(&repo)?;
	if after.tree == before.tree {
		return Ok(None);
	}

	let mut opt = DiffOptions::new();
	opt.show_binary(true);
	let diff = repo.diff_tree_to_tree(
		Some(&repo.find_tree(after.tree)?),
		Some(&repo.find_tree(before.tree)?),
		Some(&mut opt),
	)?;

	let Some(delta) = diff.deltas().next() else {
		return Ok(None);
	};
	let path = delta
		.new_file()
		.path()
		.or_else(|| delta.old_file().path())
		.and_then(|p| p.to_str())
		.unwrap_or_default()
		.to_string();
	let index_line = Patch::from_diff(&diff, 0)?
		.and_then(|p| p.hunk(0).ok().map(|(h, _)| h.new_start()))
		.unwrap_or(1);

	let mut patch_bytes = Vec::new();
	diff.print(DiffFormat::Patch, |_, _, line| {
		if matches!(line.origin(), '+' | '-' | ' ') {
			patch_bytes.push(line.origin() as u8);
		}
		patch_bytes.extend_from_slice(line.content());
		true
	})?;

	Ok(Some(StagingUndo {
		patch: patch_bytes,
		head: after.head,
		path,
		index_line,
	}))
}

/// fails without changing anything if HEAD moved since the operation
/// or the staged changes it touched have changed since
pub fn undo_staging(
	repo_path: &RepoPath,
	undo: &StagingUndo,
) -> Result<()> {
	scope_time!("undo_staging");

	let repo = repo(repo_path)?;
	if repo.head().ok().and_then(|h| h.target()) != undo.head {
		return Err(Error::Generic(
			"HEAD has moved since this was staged".to_string(),
		));
	}

	let diff = Diff::from_buffer(&undo.patch)?;
	repo.apply(&diff, ApplyLocation::Index, None)?;

	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::sync::{
		commit, reset_stage, stage_add_file, stage_hunk,
		status::{get_status, StatusType},
		tests::repo_init,
		utils::repo_write_file,
	};
	use std::path::Path;

	fn staged_paths(repo_path: &RepoPath) -> Vec<String> {
		get_status(repo_path, StatusType::Stage, None)
			.unwrap()
			.into_iter()
			.map(|s| s.path)
			.collect()
	}

	fn setup() -> (tempfile::TempDir, Repository, RepoPath) {
		let (td, repo) = repo_init().unwrap();
		let root = repo.path().parent().unwrap();
		let repo_path: RepoPath =
			root.as_os_str().to_str().unwrap().into();

		repo_write_file(
			&repo,
			"a.txt",
			"1\n2\n3\n4\n5\n6\n7\n8\n9\n",
		)
		.unwrap();
		stage_add_file(&repo_path, Path::new("a.txt")).unwrap();
		commit(&repo_path, "add a").unwrap();
		(td, repo, repo_path)
	}

	#[test]
	fn test_undo_stage_hunk() {
		let (_td, repo, repo_path) = setup();
		repo_write_file(
			&repo,
			"a.txt",
			"1\n2\n3\n4\n5\n6\n7\n8\nnine\n",
		)
		.unwrap();

		let before = snapshot_index(&repo_path).unwrap();
		let diff = crate::sync::diff::get_diff(
			&repo_path, "a.txt", false, None,
		)
		.unwrap();
		stage_hunk(
			&repo_path,
			"a.txt",
			diff.hunks[0].header_hash,
			None,
		)
		.unwrap();
		assert_eq!(staged_paths(&repo_path), vec!["a.txt"]);

		let undo =
			staging_undo(&repo_path, &before).unwrap().unwrap();
		assert_eq!(undo.path, "a.txt");
		assert_eq!(undo.index_line, 6);

		undo_staging(&repo_path, &undo).unwrap();
		assert!(staged_paths(&repo_path).is_empty());
		assert_eq!(snapshot_index(&repo_path).unwrap(), before);
	}

	#[test]
	fn test_undo_unstage_new_file() {
		let (_td, repo, repo_path) = setup();
		repo_write_file(&repo, "b.txt", "new\n").unwrap();
		stage_add_file(&repo_path, Path::new("b.txt")).unwrap();

		let before = snapshot_index(&repo_path).unwrap();
		reset_stage(&repo_path, "b.txt").unwrap();
		assert!(staged_paths(&repo_path).is_empty());

		let undo =
			staging_undo(&repo_path, &before).unwrap().unwrap();
		undo_staging(&repo_path, &undo).unwrap();
		assert_eq!(staged_paths(&repo_path), vec!["b.txt"]);
	}

	#[test]
	fn test_nothing_to_undo() {
		let (_td, _repo, repo_path) = setup();
		let before = snapshot_index(&repo_path).unwrap();
		assert!(staging_undo(&repo_path, &before).unwrap().is_none());
	}

	#[test]
	fn test_undo_refused_after_commit() {
		let (_td, repo, repo_path) = setup();
		repo_write_file(&repo, "b.txt", "new\n").unwrap();

		let before = snapshot_index(&repo_path).unwrap();
		stage_add_file(&repo_path, Path::new("b.txt")).unwrap();
		let undo =
			staging_undo(&repo_path, &before).unwrap().unwrap();
		commit(&repo_path, "add b").unwrap();

		assert!(undo_staging(&repo_path, &undo).is_err());
		assert!(staged_paths(&repo_path).is_empty());
	}

	#[test]
	fn test_undo_refused_when_staged_lines_changed() {
		let (_td, repo, repo_path) = setup();
		repo_write_file(
			&repo,
			"a.txt",
			"1\n2\n3\n4\n5\n6\n7\n8\nnine\n",
		)
		.unwrap();

		let before = snapshot_index(&repo_path).unwrap();
		stage_add_file(&repo_path, Path::new("a.txt")).unwrap();
		let undo =
			staging_undo(&repo_path, &before).unwrap().unwrap();

		repo_write_file(
			&repo,
			"a.txt",
			"1\n2\n3\n4\n5\n6\n7\n8\nNINE\n",
		)
		.unwrap();
		stage_add_file(&repo_path, Path::new("a.txt")).unwrap();
		let staged = snapshot_index(&repo_path).unwrap();

		assert!(undo_staging(&repo_path, &undo).is_err());
		assert_eq!(snapshot_index(&repo_path).unwrap(), staged);
	}
}
