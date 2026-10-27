//! Refuses a force push when the remote branch has moved since it was last
//! fetched.
//!
//! This stands in for `git push --force-with-lease`, which libgit2 does not
//! support (<https://github.com/libgit2/libgit2/issues/3947>). Git performs
//! the check on the server, atomically with the update. Here the check runs
//! against the refs the server advertises when the push connection opens,
//! and the push then reuses that same connection, so only a tiny window
//! remains between the comparison and the update.
//!
//! Everything about this check lives in this module plus one call in
//! `push::push_raw`. Once libgit2 ships the real feature, delete this module
//! and that call.

use crate::error::{Error, Result};
use git2::{
	Direction, Oid, ProxyOptions, Remote, RemoteCallbacks,
	RemoteConnection, Repository,
};

/// Opens a push connection to `remote` and checks that `remote_ref`
/// (a full name such as `refs/heads/main`) still points where the local
/// remote-tracking ref says it does.
///
/// The connection is returned open so the push that follows reuses it.
/// A branch the remote does not have yet passes the check, since there is
/// nothing to overwrite.
pub(super) fn connect_checked<'repo, 'connection, 'cb>(
	repo: &Repository,
	remote: &'connection mut Remote<'repo>,
	remote_name: &str,
	remote_ref: &str,
	callbacks: RemoteCallbacks<'cb>,
	proxy: ProxyOptions<'cb>,
) -> Result<RemoteConnection<'repo, 'connection, 'cb>> {
	let short_name =
		remote_ref.strip_prefix("refs/heads/").unwrap_or(remote_ref);
	let tracking_ref =
		format!("refs/remotes/{remote_name}/{short_name}");
	let expected = repo
		.find_reference(&tracking_ref)
		.ok()
		.and_then(|r| r.target());

	let connection = remote.connect_auth(
		Direction::Push,
		Some(callbacks),
		Some(proxy),
	)?;

	let actual = connection
		.list()?
		.iter()
		.find(|head| head.name() == remote_ref)
		.map(git2::RemoteHead::oid);

	match (expected, actual) {
		(_, None) => Ok(connection),
		(Some(expected), Some(actual)) if expected == actual => {
			Ok(connection)
		}
		(Some(expected), Some(actual)) => Err(Error::Generic(format!(
			"force push refused: '{remote_name}/{short_name}' moved since it was last fetched (expected {}, remote has {}). Fetch first and check what changed.",
			short(expected),
			short(actual),
		))),
		(None, Some(actual)) => Err(Error::Generic(format!(
			"force push refused: '{remote_name}/{short_name}' has never been fetched but exists on the remote at {}. Fetch first.",
			short(actual),
		))),
	}
}

fn short(oid: Oid) -> String {
	oid.to_string().chars().take(7).collect()
}
