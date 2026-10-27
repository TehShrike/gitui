use crate::{
	error::{Error, Result},
	progress::ProgressPercent,
	sync::{
		branch::branch_set_upstream_after_push,
		config::{
			push_default_strategy_config_repo,
			PushDefaultStrategyConfig,
		},
		cred::BasicAuthCredential,
		get_branch_upstream_merge,
		remotes::{force_push_guard, proxy_auto, Callbacks},
		repository::repo,
		CommitId, RepoPath,
	},
};
use crossbeam_channel::Sender;
use git2::{PackBuilderStage, PushOptions};
use scopetime::scope_time;
use std::fmt::Write as _;

///
pub trait AsyncProgress: Clone + Send + Sync {
	///
	fn is_done(&self) -> bool;
	///
	fn progress(&self) -> ProgressPercent;
}

///
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressNotification {
	///
	UpdateTips {
		///
		name: String,
		///
		a: CommitId,
		///
		b: CommitId,
	},
	///
	Transfer {
		///
		objects: usize,
		///
		total_objects: usize,
	},
	///
	PushTransfer {
		///
		current: usize,
		///
		total: usize,
		///
		bytes: usize,
	},
	///
	Packing {
		///
		stage: PackBuilderStage,
		///
		total: usize,
		///
		current: usize,
	},
	///
	Done,
}

impl AsyncProgress for ProgressNotification {
	fn is_done(&self) -> bool {
		*self == Self::Done
	}
	fn progress(&self) -> ProgressPercent {
		match *self {
			Self::Packing {
				stage,
				current,
				total,
			} => match stage {
				PackBuilderStage::AddingObjects
				| PackBuilderStage::Deltafication => {
					ProgressPercent::new(current, total)
				}
			},
			Self::PushTransfer { current, total, .. } => {
				ProgressPercent::new(current, total)
			}
			Self::Transfer {
				objects,
				total_objects,
				..
			} => ProgressPercent::new(objects, total_objects),
			_ => ProgressPercent::full(),
		}
	}
}

///
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum PushType {
	///
	#[default]
	Branch,
	///
	Tag,
}

#[cfg(test)]
pub fn push_branch(
	repo_path: &RepoPath,
	remote: &str,
	branch: &str,
	force: bool,
	delete: bool,
	basic_credential: Option<BasicAuthCredential>,
	progress_sender: Option<Sender<ProgressNotification>>,
) -> Result<()> {
	push_raw(
		repo_path,
		remote,
		branch,
		PushType::Branch,
		force,
		delete,
		basic_credential,
		progress_sender,
	)
}

//TODO: cleanup
#[allow(clippy::too_many_arguments)]
pub fn push_raw(
	repo_path: &RepoPath,
	remote: &str,
	branch: &str,
	ref_type: PushType,
	force: bool,
	delete: bool,
	basic_credential: Option<BasicAuthCredential>,
	progress_sender: Option<Sender<ProgressNotification>>,
) -> Result<()> {
	scope_time!("push");

	let repo = repo(repo_path)?;
	let remote_name = remote;
	let mut remote = repo.find_remote(remote_name)?;

	let push_default_strategy =
		push_default_strategy_config_repo(&repo)?;

	let mut options = PushOptions::new();
	options.proxy_options(proxy_auto());

	let callbacks = Callbacks::new(progress_sender, basic_credential);
	options.remote_callbacks(callbacks.callbacks());
	options.packbuilder_parallelism(0);

	let branch_modifier = match (force, delete) {
		(true, true) => "+:",
		(false, true) => ":",
		(true, false) => "+",
		(false, false) => "",
	};
	let git_ref_type = match ref_type {
		PushType::Branch => "heads",
		PushType::Tag => "tags",
	};

	let mut remote_ref = format!("refs/{git_ref_type}/{branch}");
	let mut push_ref = format!("{branch_modifier}{remote_ref}");

	if !delete
		&& ref_type == PushType::Branch
		&& push_default_strategy
			== PushDefaultStrategyConfig::Upstream
	{
		if let Ok(Some(branch_upstream_merge)) =
			get_branch_upstream_merge(repo_path, branch)
		{
			let _ = write!(push_ref, ":{branch_upstream_merge}");
			remote_ref = branch_upstream_merge;
		}
	}

	log::debug!("push to: {push_ref}");
	if force && !delete && ref_type == PushType::Branch {
		let mut connection = force_push_guard::connect_checked(
			&repo,
			&mut remote,
			remote_name,
			&remote_ref,
			callbacks.callbacks(),
			proxy_auto(),
		)?;
		connection.remote().push(&[push_ref], Some(&mut options))?;
	} else {
		remote.push(&[push_ref], Some(&mut options))?;
	}

	if let Some((reference, msg)) =
		callbacks.get_stats()?.push_rejected_msg
	{
		return Err(Error::Generic(format!(
			"push to '{reference}' rejected: {msg}"
		)));
	}

	if !delete {
		branch_set_upstream_after_push(&repo, branch)?;
	}

	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::sync::{
		self,
		remotes::fetch_all,
		tests::{
			get_commit_ids, repo_clone, repo_init, repo_init_bare,
			write_commit_file,
		},
	};
	use git2::Repository;
	use std::{fs::File, io::Write, path::Path};

	#[test]
	fn test_force_push() {
		// This test mimics the scenario of 2 people having 2
		// local branches and both modifying the same file then
		// both pushing, sequentially
		let (tmp_repo_dir, repo) = repo_init().unwrap();
		let (tmp_other_repo_dir, other_repo) = repo_init().unwrap();
		let (tmp_upstream_dir, _) = repo_init_bare().unwrap();

		repo.remote(
			"origin",
			tmp_upstream_dir.path().to_str().unwrap(),
		)
		.unwrap();

		other_repo
			.remote(
				"origin",
				tmp_upstream_dir.path().to_str().unwrap(),
			)
			.unwrap();

		let tmp_repo_file_path =
			tmp_repo_dir.path().join("temp_file.txt");
		let mut tmp_repo_file =
			File::create(tmp_repo_file_path).unwrap();
		writeln!(tmp_repo_file, "TempSomething").unwrap();

		sync::commit(
			&tmp_repo_dir.path().to_str().unwrap().into(),
			"repo_1_commit",
		)
		.unwrap();

		push_branch(
			&tmp_repo_dir.path().to_str().unwrap().into(),
			"origin",
			"master",
			false,
			false,
			None,
			None,
		)
		.unwrap();

		let tmp_other_repo_file_path =
			tmp_other_repo_dir.path().join("temp_file.txt");
		let mut tmp_other_repo_file =
			File::create(tmp_other_repo_file_path).unwrap();
		writeln!(tmp_other_repo_file, "TempElse").unwrap();

		sync::commit(
			&tmp_other_repo_dir.path().to_str().unwrap().into(),
			"repo_2_commit",
		)
		.unwrap();

		// Attempt a normal push,
		// should fail as branches diverged
		assert!(push_branch(
			&tmp_other_repo_dir.path().to_str().unwrap().into(),
			"origin",
			"master",
			false,
			false,
			None,
			None,
		)
		.is_err());

		// Attempt force push without ever having fetched,
		// should be refused since the remote branch is unknown locally
		let err = push_branch(
			&tmp_other_repo_dir.path().to_str().unwrap().into(),
			"origin",
			"master",
			true,
			false,
			None,
			None,
		)
		.unwrap_err();
		assert!(err.to_string().contains("never been fetched"));

		fetch_all(
			&tmp_other_repo_dir.path().to_str().unwrap().into(),
			&None,
			&None,
		)
		.unwrap();

		// Attempt force push after fetching,
		// should work as it forces the push through
		assert!(push_branch(
			&tmp_other_repo_dir.path().to_str().unwrap().into(),
			"origin",
			"master",
			true,
			false,
			None,
			None,
		)
		.is_ok());
	}

	#[test]
	fn test_force_push_refused_when_remote_moved() {
		let (tmp_repo_dir, repo) = repo_init().unwrap();
		let (tmp_other_repo_dir, other_repo) = repo_init().unwrap();
		let (tmp_upstream_dir, upstream) = repo_init_bare().unwrap();

		let repo_path: RepoPath =
			tmp_repo_dir.path().to_str().unwrap().into();
		let other_path: RepoPath =
			tmp_other_repo_dir.path().to_str().unwrap().into();

		repo.remote(
			"origin",
			tmp_upstream_dir.path().to_str().unwrap(),
		)
		.unwrap();
		other_repo
			.remote(
				"origin",
				tmp_upstream_dir.path().to_str().unwrap(),
			)
			.unwrap();

		write_commit_file(&repo, "a.txt", "a", "commit a");
		push_branch(
			&repo_path, "origin", "master", false, false, None, None,
		)
		.unwrap();

		// the other clone learns about commit a
		write_commit_file(&other_repo, "c.txt", "c", "commit c");
		fetch_all(&other_path, &None, &None).unwrap();

		// meanwhile the remote moves on to commit b
		let commit_b =
			write_commit_file(&repo, "b.txt", "b", "commit b");
		push_branch(
			&repo_path, "origin", "master", false, false, None, None,
		)
		.unwrap();

		// a force push from the stale clone must not wipe out commit b
		let err = push_branch(
			&other_path,
			"origin",
			"master",
			true,
			false,
			None,
			None,
		)
		.unwrap_err();
		assert!(err.to_string().contains("moved since"));
		assert!(get_commit_ids(&upstream, 1).contains(&commit_b));

		// once the clone has seen commit b the force push goes through
		fetch_all(&other_path, &None, &None).unwrap();
		push_branch(
			&other_path,
			"origin",
			"master",
			true,
			false,
			None,
			None,
		)
		.unwrap();
		assert!(!get_commit_ids(&upstream, 1).contains(&commit_b));
	}

	#[test]
	fn test_force_push_of_new_branch_is_allowed() {
		let (tmp_repo_dir, repo) = repo_init().unwrap();
		let (tmp_upstream_dir, _) = repo_init_bare().unwrap();
		let repo_path: RepoPath =
			tmp_repo_dir.path().to_str().unwrap().into();

		repo.remote(
			"origin",
			tmp_upstream_dir.path().to_str().unwrap(),
		)
		.unwrap();
		write_commit_file(&repo, "a.txt", "a", "commit a");

		// nothing on the remote yet, so there is nothing to protect
		push_branch(
			&repo_path, "origin", "master", true, false, None, None,
		)
		.unwrap();
	}

	#[test]
	fn test_force_push_rewrites_history() {
		// This test mimics the scenario of 2 people having 2
		// local branches and both modifying the same file then
		// both pushing, sequentially

		let (tmp_repo_dir, repo) = repo_init().unwrap();
		let (tmp_other_repo_dir, other_repo) = repo_init().unwrap();
		let (tmp_upstream_dir, upstream) = repo_init_bare().unwrap();

		repo.remote(
			"origin",
			tmp_upstream_dir.path().to_str().unwrap(),
		)
		.unwrap();

		other_repo
			.remote(
				"origin",
				tmp_upstream_dir.path().to_str().unwrap(),
			)
			.unwrap();

		let tmp_repo_file_path =
			tmp_repo_dir.path().join("temp_file.txt");
		let mut tmp_repo_file =
			File::create(tmp_repo_file_path).unwrap();
		writeln!(tmp_repo_file, "TempSomething").unwrap();

		sync::stage_add_file(
			&tmp_repo_dir.path().to_str().unwrap().into(),
			Path::new("temp_file.txt"),
		)
		.unwrap();

		let repo_1_commit = sync::commit(
			&tmp_repo_dir.path().to_str().unwrap().into(),
			"repo_1_commit",
		)
		.unwrap();

		//NOTE: make sure the commit actually contains that file
		assert_eq!(
			sync::get_commit_files(
				&tmp_repo_dir.path().to_str().unwrap().into(),
				repo_1_commit,
				None
			)
			.unwrap()[0]
				.path,
			String::from("temp_file.txt")
		);

		let commits = get_commit_ids(&repo, 1);
		assert!(commits.contains(&repo_1_commit));

		push_branch(
			&tmp_repo_dir.path().to_str().unwrap().into(),
			"origin",
			"master",
			false,
			false,
			None,
			None,
		)
		.unwrap();

		let tmp_other_repo_file_path =
			tmp_other_repo_dir.path().join("temp_file.txt");
		let mut tmp_other_repo_file =
			File::create(tmp_other_repo_file_path).unwrap();
		writeln!(tmp_other_repo_file, "TempElse").unwrap();

		sync::stage_add_file(
			&tmp_other_repo_dir.path().to_str().unwrap().into(),
			Path::new("temp_file.txt"),
		)
		.unwrap();

		let repo_2_commit = sync::commit(
			&tmp_other_repo_dir.path().to_str().unwrap().into(),
			"repo_2_commit",
		)
		.unwrap();

		let repo_2_parent = other_repo
			.find_commit(repo_2_commit.into())
			.unwrap()
			.parents()
			.next()
			.unwrap()
			.id();

		let commits = get_commit_ids(&other_repo, 1);
		assert!(commits.contains(&repo_2_commit));

		// Attempt a normal push,
		// should fail as branches diverged
		assert!(push_branch(
			&tmp_other_repo_dir.path().to_str().unwrap().into(),
			"origin",
			"master",
			false,
			false,
			None,
			None,
		)
		.is_err());

		// Check that the other commit is not in upstream,
		// a normal push would not rewrite history
		let commits = get_commit_ids(&upstream, 1);
		assert!(!commits.contains(&repo_2_commit));

		// Attempt force push after fetching,
		// should work as it forces the push through
		fetch_all(
			&tmp_other_repo_dir.path().to_str().unwrap().into(),
			&None,
			&None,
		)
		.unwrap();

		push_branch(
			&tmp_other_repo_dir.path().to_str().unwrap().into(),
			"origin",
			"master",
			true,
			false,
			None,
			None,
		)
		.unwrap();

		let commits = get_commit_ids(&upstream, 1);
		assert!(commits.contains(&repo_2_commit));

		let new_upstream_parent =
			Repository::init_bare(tmp_upstream_dir.path())
				.unwrap()
				.find_commit(repo_2_commit.into())
				.unwrap()
				.parents()
				.next()
				.unwrap()
				.id();
		assert_eq!(new_upstream_parent, repo_2_parent,);
	}

	#[test]
	fn test_delete_remote_branch() {
		// This test mimics the scenario of a user creating a branch, push it, and then remove it on the remote

		let (upstream_dir, upstream_repo) = repo_init_bare().unwrap();

		let (tmp_repo_dir, repo) =
			repo_clone(upstream_dir.path().to_str().unwrap())
				.unwrap();

		// You need a commit before being able to branch !
		let commit_1 = write_commit_file(
			&repo,
			"temp_file.txt",
			"SomeContent",
			"Initial commit",
		);

		let commits = get_commit_ids(&repo, 1);
		assert!(commits.contains(&commit_1));

		push_branch(
			&tmp_repo_dir.path().to_str().unwrap().into(),
			"origin",
			"master",
			false,
			false,
			None,
			None,
		)
		.unwrap();

		// Create the local branch
		sync::create_branch(
			&tmp_repo_dir.path().to_str().unwrap().into(),
			"test_branch",
		)
		.unwrap();

		// Push the local branch
		push_branch(
			&tmp_repo_dir.path().to_str().unwrap().into(),
			"origin",
			"test_branch",
			false,
			false,
			None,
			None,
		)
		.unwrap();

		// Test if the branch exits on the remote
		assert!(upstream_repo
			.branches(None)
			.unwrap()
			.map(std::result::Result::unwrap)
			.map(|(i, _)| i.name().unwrap().unwrap().to_string())
			.any(|i| &i == "test_branch"));

		// Delete the remote branch
		assert!(push_branch(
			&tmp_repo_dir.path().to_str().unwrap().into(),
			"origin",
			"test_branch",
			false,
			true,
			None,
			None,
		)
		.is_ok());

		// Test that the branch has be remove from the remote
		assert!(!upstream_repo
			.branches(None)
			.unwrap()
			.map(std::result::Result::unwrap)
			.map(|(i, _)| i.name().unwrap().unwrap().to_string())
			.any(|i| &i == "test_branch"));
	}
}
