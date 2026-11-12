use crate::{
	app::Environment,
	components::{
		visibility_blocking, CommandBlocking, CommandInfo, Component,
		DrawableComponent, EventState,
	},
	input::{
		disable_keyboard_enhancement, enable_keyboard_enhancement,
	},
	keys::SharedKeyConfig,
	strings,
	ui::{self, style::SharedTheme},
};
use anyhow::{anyhow, bail, Result};
use asyncgit::sync::{
	get_config_string, utils::repo_work_dir, RepoPath,
};
use crossterm::{
	event::Event,
	terminal::{EnterAlternateScreen, LeaveAlternateScreen},
	ExecutableCommand,
};
use ratatui::{
	layout::Rect,
	text::{Line, Span},
	widgets::{Block, BorderType, Borders, Clear, Paragraph},
	Frame,
};
use scopeguard::defer;
use std::ffi::{OsStr, OsString};
use std::{env, io, path::Path, process::Command};

/// editor for working tree files, see `open_worktree_file`
const ENV_FILE_EDITOR: &str = "GITUI_FILE_EDITOR";

///
pub struct ExternalEditorPopup {
	visible: bool,
	theme: SharedTheme,
	key_config: SharedKeyConfig,
}

impl ExternalEditorPopup {
	///
	pub fn new(env: &Environment) -> Self {
		Self {
			visible: false,
			theme: env.theme.clone(),
			key_config: env.key_config.clone(),
		}
	}

	/// opens file at given `path` in an available editor
	pub fn open_file_in_editor(
		repo: &RepoPath,
		path: &Path,
	) -> Result<()> {
		Self::open(repo, path, None, false)
	}

	/// opens a working tree file, at `line` if the editor is one whose
	/// syntax for that is known.
	///
	/// `GITUI_FILE_EDITOR` takes precedence over the usual editor
	/// settings here: those are often set up to block until the file is
	/// closed, which suits commit messages but not jumping to a file.
	pub fn open_worktree_file(
		repo: &RepoPath,
		path: &Path,
		line: Option<u32>,
	) -> Result<()> {
		Self::open(repo, path, line, true)
	}

	fn open(
		repo: &RepoPath,
		path: &Path,
		line: Option<u32>,
		is_worktree_file: bool,
	) -> Result<()> {
		let work_dir = repo_work_dir(repo)?;

		let path = if path.is_relative() {
			Path::new(&work_dir).join(path)
		} else {
			path.into()
		};

		if !path.exists() {
			bail!("file not found: {path:?}");
		}

		disable_keyboard_enhancement();
		io::stdout().execute(LeaveAlternateScreen)?;
		defer! {
			io::stdout().execute(EnterAlternateScreen).expect("reset terminal");
			enable_keyboard_enhancement();
		}

		let environment_options = ["GIT_EDITOR", "VISUAL", "EDITOR"];

		let file_editor = is_worktree_file
			.then(|| env::var(ENV_FILE_EDITOR).ok())
			.flatten()
			.filter(|editor| !editor.trim().is_empty());

		let editor = file_editor
			.or_else(|| env::var(environment_options[0]).ok())
			.or_else(|| {
				get_config_string(repo, "core.editor").ok()?
			})
			.or_else(|| env::var(environment_options[1]).ok())
			.or_else(|| env::var(environment_options[2]).ok())
			.unwrap_or_else(|| String::from("vi"));

		// TODO: proper handling arguments containing whitespaces
		// This does not do the right thing if the input is `editor --something "with spaces"`

		// deal with "editor name with spaces" p1 p2 p3
		// and with "editor_no_spaces" p1 p2 p3
		// does not address spaces in pn
		let mut echars = editor.chars().peekable();

		let first_char = *echars.peek().ok_or_else(|| {
			anyhow!(
				"editor env variable found empty: {}",
				environment_options.join(" or ")
			)
		})?;
		let command: String = if first_char == '\"' {
			echars
				.by_ref()
				.skip(1)
				.take_while(|c| *c != '\"')
				.collect()
		} else {
			echars.by_ref().take_while(|c| *c != ' ').collect()
		};

		let remainder_str = echars.collect::<String>();
		let remainder = remainder_str.split_whitespace();

		let mut args: Vec<OsString> =
			remainder.map(OsString::from).collect();

		args.extend(file_args(&command, &path, line));

		Command::new(command.clone())
			.current_dir(work_dir)
			.args(args)
			.status()
			.map_err(|e| anyhow!("\"{command}\": {e}"))?;

		Ok(())
	}
}

/// arguments that make the editor `command` open `path`, at `line` if
/// the editor is one whose syntax for that is known
fn file_args(
	command: &str,
	path: &Path,
	line: Option<u32>,
) -> Vec<OsString> {
	let Some(line) = line else {
		return vec![path.into()];
	};

	let path_at_line = || {
		let mut arg = path.as_os_str().to_owned();
		arg.push(format!(":{line}"));
		arg
	};

	let name = Path::new(command)
		.file_stem()
		.and_then(OsStr::to_str)
		.unwrap_or_default()
		.to_lowercase();

	// only the first word of the name counts, so that a wrapper script
	// named for its editor, like `subl-repo-window`, gets its syntax
	let editor = name.split(['-', '_']).next().unwrap_or_default();

	match editor {
		"vi" | "vim" | "nvim" | "gvim" | "mvim" | "nano"
		| "emacs" | "emacsclient" | "kak" | "micro" => {
			vec![format!("+{line}").into(), path.into()]
		}
		"subl" | "sublime" | "zed" | "hx" | "helix" => {
			vec![path_at_line()]
		}
		"code" | "codium" | "cursor" | "windsurf" => {
			vec!["--goto".into(), path_at_line()]
		}
		_ => vec![path.into()],
	}
}

impl DrawableComponent for ExternalEditorPopup {
	fn draw(&self, f: &mut Frame, _rect: Rect) -> Result<()> {
		if self.visible {
			let txt = Line::from(
				strings::msg_opening_editor(&self.key_config)
					.split('\n')
					.map(|string| {
						Span::raw::<String>(string.to_string())
					})
					.collect::<Vec<Span>>(),
			);

			let area = ui::centered_rect_absolute(25, 3, f.area());
			f.render_widget(Clear, area);
			f.render_widget(
				Paragraph::new(txt)
					.block(
						Block::default()
							.borders(Borders::ALL)
							.border_type(BorderType::Thick)
							.border_style(self.theme.block(true)),
					)
					.style(self.theme.block(true)),
				area,
			);
		}

		Ok(())
	}
}

impl Component for ExternalEditorPopup {
	fn commands(
		&self,
		out: &mut Vec<CommandInfo>,
		force_all: bool,
	) -> CommandBlocking {
		if self.visible && !force_all {
			out.clear();
		}

		visibility_blocking(self)
	}

	fn event(&mut self, _ev: &Event) -> Result<EventState> {
		if self.visible {
			return Ok(EventState::Consumed);
		}

		Ok(EventState::NotConsumed)
	}

	fn is_visible(&self) -> bool {
		self.visible
	}

	fn hide(&mut self) {
		self.visible = false;
	}

	fn show(&mut self) -> Result<()> {
		self.visible = true;

		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn args(command: &str, line: Option<u32>) -> Vec<String> {
		file_args(command, Path::new("/repo/src/main.rs"), line)
			.into_iter()
			.map(|arg| arg.to_string_lossy().into_owned())
			.collect()
	}

	#[test]
	fn file_args_use_each_editors_line_syntax() {
		assert_eq!(args("subl", Some(12)), ["/repo/src/main.rs:12"]);
		assert_eq!(
			args("vim", Some(12)),
			["+12", "/repo/src/main.rs"]
		);
		assert_eq!(
			args("code", Some(12)),
			["--goto", "/repo/src/main.rs:12"]
		);
	}

	#[test]
	fn file_args_recognise_an_editor_given_by_path() {
		assert_eq!(
			args("/usr/local/bin/subl", Some(3)),
			["/repo/src/main.rs:3"]
		);
	}

	#[test]
	fn file_args_recognise_a_wrapper_named_for_its_editor() {
		assert_eq!(
			args("/home/me/scripts/subl-repo-window", Some(3)),
			["/repo/src/main.rs:3"]
		);
		assert_eq!(
			args("code-insiders", Some(3)),
			["--goto", "/repo/src/main.rs:3"]
		);
		assert_eq!(
			args("sublime_text", Some(3)),
			["/repo/src/main.rs:3"]
		);
		// a name that merely starts with an editor's name is not one
		assert_eq!(args("vimdiff", Some(3)), ["/repo/src/main.rs"]);
	}

	#[test]
	fn file_args_are_just_the_path_without_a_line_or_known_editor() {
		assert_eq!(args("subl", None), ["/repo/src/main.rs"]);
		assert_eq!(args("ed", Some(12)), ["/repo/src/main.rs"]);
	}
}
