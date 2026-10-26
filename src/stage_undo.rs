use asyncgit::sync::{self, RepoPath, StagingUndo};
use std::{cell::RefCell, rc::Rc};

const MAX_ENTRIES: usize = 10;

pub type SharedStageUndo = Rc<RefCell<StageUndo>>;

/// the reverse of one staging keypress
pub struct StageUndoEntry {
	pub undo: StagingUndo,
	/// whether the keypress moved changes into the index (rather than out of it)
	pub staged: bool,
}

/// the most recent staging keypresses, newest last
#[derive(Default)]
pub struct StageUndo {
	entries: Vec<StageUndoEntry>,
}

impl StageUndo {
	/// run a staging operation and remember how to undo it.
	/// failing to remember it never fails the operation itself
	pub fn record<T, E>(
		&mut self,
		repo: &RepoPath,
		staged: bool,
		op: impl FnOnce() -> Result<T, E>,
	) -> Result<T, E> {
		let before = sync::snapshot_index(repo).ok();
		let res = op()?;

		if let Some(undo) = before
			.and_then(|before| sync::staging_undo(repo, &before).ok())
			.flatten()
		{
			if self.entries.len() == MAX_ENTRIES {
				self.entries.remove(0);
			}
			self.entries.push(StageUndoEntry { undo, staged });
		}

		Ok(res)
	}

	pub fn pop(&mut self) -> Option<StageUndoEntry> {
		self.entries.pop()
	}

	pub fn clear(&mut self) {
		self.entries.clear();
	}

	pub const fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}
}
