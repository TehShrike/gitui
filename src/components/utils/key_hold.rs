use std::time::{Duration, Instant};

/// Longest gap between two repeated presses that still counts as one
/// continuous hold. Must exceed the OS initial key-repeat delay.
const HOLD_GAP: Duration = Duration::from_millis(500);
/// How long a key has to be held before movement speeds up.
const HOLD_ACCELERATE_AFTER: Duration = Duration::from_millis(400);
/// Lines moved per press once the key has been held long enough.
const HOLD_STEP: usize = 2;

/// Detects a key being held down from the stream of auto-repeat presses
/// the terminal delivers, since terminals do not report press vs release.
#[derive(Default)]
pub struct KeyHold<T: PartialEq + Copy> {
	key: Option<T>,
	hold_start: Option<Instant>,
	last_press: Option<Instant>,
}

impl<T: PartialEq + Copy> KeyHold<T> {
	pub const fn new() -> Self {
		Self {
			key: None,
			hold_start: None,
			last_press: None,
		}
	}

	/// Record a press of `key` and return how many lines it should move:
	/// 1 normally, more once the same key has been held for a while.
	pub fn press(&mut self, key: T) -> usize {
		let now = Instant::now();
		let continuing = self.key == Some(key)
			&& self
				.last_press
				.is_some_and(|t| now.duration_since(t) <= HOLD_GAP);

		if !continuing {
			self.key = Some(key);
			self.hold_start = Some(now);
		}
		self.last_press = Some(now);

		let held = self
			.hold_start
			.map_or(Duration::ZERO, |t| now.duration_since(t));
		if held >= HOLD_ACCELERATE_AFTER {
			HOLD_STEP
		} else {
			1
		}
	}

	/// Forget the current hold, e.g. when an unrelated key is pressed.
	pub const fn reset(&mut self) {
		self.key = None;
		self.hold_start = None;
		self.last_press = None;
	}
}
