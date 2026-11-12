use easy_cast::Cast;
use ratatui::{
	buffer::Buffer,
	layout::{Alignment, Position, Rect},
	style::Style,
	text::{StyledGrapheme, Text},
	widgets::{Block, StatefulWidget, Widget, Wrap},
};
use std::{cell::Cell, iter};
use unicode_width::UnicodeWidthStr;

use super::reflow::{LineComposer, LineTruncator, WordWrapper};

const fn get_line_offset(
	line_width: u16,
	text_area_width: u16,
	alignment: Alignment,
) -> u16 {
	match alignment {
		Alignment::Center => {
			(text_area_width / 2).saturating_sub(line_width / 2)
		}
		Alignment::Right => {
			text_area_width.saturating_sub(line_width)
		}
		Alignment::Left => 0,
	}
}

#[derive(Debug, Clone)]
pub struct StatefulParagraph<'a> {
	/// A block to wrap the widget in
	block: Option<Block<'a>>,
	/// Widget style
	style: Style,
	/// How to wrap the text
	wrap: Option<Wrap>,
	/// The text to display
	text: Text<'a>,
	/// Alignment of the text
	alignment: Alignment,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ScrollPos {
	pub x: u16,
	pub y: u16,
}

#[derive(Debug, Copy, Clone, Default)]
pub struct ParagraphState {
	/// Scroll
	scroll: ScrollPos,
	/// after all wrapping this is the amount of lines
	lines: u16,
	/// last visible height
	height: u16,
	/// index of the line of text that the top visible row belongs to,
	/// which wrapping makes different from the scroll position
	top_line: u32,
}

impl ParagraphState {
	pub const fn lines(self) -> u16 {
		self.lines
	}

	pub const fn height(self) -> u16 {
		self.height
	}

	pub const fn scroll(self) -> ScrollPos {
		self.scroll
	}

	pub const fn top_line(self) -> u32 {
		self.top_line
	}

	pub const fn set_scroll(&mut self, scroll: ScrollPos) {
		self.scroll = scroll;
	}
}

impl<'a> StatefulParagraph<'a> {
	pub fn new<T>(text: T) -> Self
	where
		T: Into<Text<'a>>,
	{
		Self {
			block: None,
			style: Style::default(),
			wrap: None,
			text: text.into(),
			alignment: Alignment::Left,
		}
	}

	pub fn block(mut self, block: Block<'a>) -> Self {
		self.block = Some(block);
		self
	}

	pub const fn wrap(mut self, wrap: Wrap) -> Self {
		self.wrap = Some(wrap);
		self
	}
}

impl StatefulWidget for StatefulParagraph<'_> {
	type State = ParagraphState;

	fn render(
		mut self,
		area: Rect,
		buf: &mut Buffer,
		state: &mut Self::State,
	) {
		buf.set_style(area, self.style);
		let text_area = self.block.take().map_or(area, |b| {
			let inner_area = b.inner(area);
			b.render(area, buf);
			inner_area
		});

		if text_area.height < 1 {
			return;
		}

		let style = self.style;
		// lines of text the composer has consumed in full, which makes it
		// the index of the line that the next row it composes belongs to
		let lines_consumed = Cell::new(0_u32);
		let mut styled = self.text.lines.iter().flat_map(|line| {
			line.spans
				.iter()
				.flat_map(|span| span.styled_graphemes(style))
				// Required given the way composers work but might be refactored out if we change
				// composers to operate on lines instead of a stream of graphemes.
				.chain(iter::once_with(|| {
					lines_consumed
						.set(lines_consumed.get().saturating_add(1));
					StyledGrapheme {
						symbol: "\n",
						style,
					}
				}))
		});

		let mut line_composer: Box<dyn LineComposer> =
			if let Some(Wrap { trim }) = self.wrap {
				Box::new(WordWrapper::new(
					&mut styled,
					text_area.width,
					trim,
				))
			} else {
				let mut line_composer = Box::new(LineTruncator::new(
					&mut styled,
					text_area.width,
				));
				if self.alignment == Alignment::Left {
					line_composer
						.set_horizontal_offset(state.scroll.x);
				}
				line_composer
			};
		let mut y = 0;
		let mut end_reached = false;
		loop {
			let text_line = lines_consumed.get();
			let Some((current_line, current_line_width)) =
				line_composer.next_line()
			else {
				break;
			};
			if y == state.scroll.y {
				state.top_line = text_line;
			}
			if !end_reached && y >= state.scroll.y {
				let mut x = get_line_offset(
					current_line_width,
					text_area.width,
					self.alignment,
				);
				for StyledGrapheme { symbol, style } in current_line {
					buf.cell_mut(Position::new(
						text_area.left() + x,
						text_area.top() + y - state.scroll.y,
					))
					.map(|cell| {
						cell.set_symbol(if symbol.is_empty() {
							// If the symbol is empty, the last char which rendered last time will
							// leave on the line. It's a quick fix.
							" "
						} else {
							symbol
						})
						.set_style(*style)
					});
					x += Cast::<u16>::cast(symbol.width());
				}
			}
			y += 1;
			if y >= text_area.height + state.scroll.y {
				end_reached = true;
			}
		}

		state.lines = y;
		state.height = area.height;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// renders four lines, the second of which wraps into four rows,
	/// and returns the top line along with the text of the top row
	fn top_line_scrolled_to(y: u16) -> (u32, String) {
		let area = Rect::new(0, 0, 10, 2);
		let mut buf = Buffer::empty(area);
		let mut state = ParagraphState::default();
		state.set_scroll(ScrollPos { x: 0, y });

		StatefulParagraph::new(
			"one\ntwo is long enough to wrap\nthree\nfour",
		)
		.wrap(Wrap { trim: false })
		.render(area, &mut buf, &mut state);

		let top_row = (0..area.width)
			.map(|x| buf[(x, 0)].symbol())
			.collect::<String>();

		(state.top_line(), top_row.trim_end().to_string())
	}

	#[test]
	fn top_line_is_the_line_that_the_top_row_was_wrapped_from() {
		assert_eq!(top_line_scrolled_to(0), (0, "one".into()));
		assert_eq!(top_line_scrolled_to(1), (1, "two is".into()));
		assert_eq!(top_line_scrolled_to(4), (1, "wrap".into()));
		assert_eq!(top_line_scrolled_to(5), (2, "three".into()));
		assert_eq!(top_line_scrolled_to(6), (3, "four".into()));
	}
}
