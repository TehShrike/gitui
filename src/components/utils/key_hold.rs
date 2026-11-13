use std::time::{Duration, Instant};

/// Longest wait between a press and the first auto-repeat that follows
/// it. Must exceed the OS initial key-repeat delay, and stay below
/// `HOLD_ACCELERATE_AFTER` so that two separate presses never speed up.
const HOLD_START_GAP: Duration = Duration::from_millis(500);
/// Longest gap between presses once auto-repeat is running. Must exceed
/// the OS key-repeat interval. A press after a longer gap is a new
/// press, so this is also how soon a released hold is forgotten.
const HOLD_REPEAT_GAP: Duration = Duration::from_millis(150);
/// How long a key has to be held before movement speeds up.
const HOLD_ACCELERATE_AFTER: Duration = Duration::from_millis(800);
/// Lines moved per press once the key has been held long enough.
const HOLD_STEP: usize = 2;

const _: () = assert!(
	HOLD_START_GAP.as_millis() < HOLD_ACCELERATE_AFTER.as_millis()
);

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
		self.press_at(key, Instant::now())
	}

	fn press_at(&mut self, key: T, now: Instant) -> usize {
		let gap = self
			.last_press
			.filter(|_| self.key == Some(key))
			.map(|t| now.duration_since(t));

		match gap {
			// auto-repeat is running, the hold goes on
			Some(gap) if gap <= HOLD_REPEAT_GAP => {}
			// could be the wait before the first auto-repeat, in which
			// case the hold began with the previous press
			Some(gap) if gap <= HOLD_START_GAP => {
				self.hold_start = self.last_press;
			}
			_ => {
				self.key = Some(key);
				self.hold_start = Some(now);
			}
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

#[cfg(test)]
mod tests {
	use super::*;
	use pretty_assertions::assert_eq;

	/// macOS at its fastest settings: 225ms until the first repeat,
	/// then one every 30ms.
	const INITIAL_DELAY: u64 = 225;
	const REPEAT: u64 = 30;

	struct Keyboard {
		hold: KeyHold<char>,
		start: Instant,
		now: u64,
	}

	impl Keyboard {
		fn new() -> Self {
			Self {
				hold: KeyHold::new(),
				start: Instant::now(),
				now: 0,
			}
		}

		/// Press `key` once, `wait` ms after the previous press.
		fn tap(&mut self, key: char, wait: u64) -> usize {
			self.now += wait;
			self.hold.press_at(
				key,
				self.start + Duration::from_millis(self.now),
			)
		}

		/// Press `key` `wait` ms after the previous press and keep it
		/// down for `duration` ms. Returns the step of every press as
		/// `(ms since the key went down, step)`.
		fn hold(
			&mut self,
			key: char,
			wait: u64,
			duration: u64,
		) -> Vec<(u64, usize)> {
			let mut steps = vec![(0, self.tap(key, wait))];
			let mut held = INITIAL_DELAY;
			let mut wait = INITIAL_DELAY;
			while held <= duration {
				steps.push((held, self.tap(key, wait)));
				wait = REPEAT;
				held += REPEAT;
			}
			steps
		}
	}

	fn expected_while_held(
		steps: &[(u64, usize)],
	) -> Vec<(u64, usize)> {
		steps
			.iter()
			.map(|&(held, _)| (held, if held >= 800 { 2 } else { 1 }))
			.collect()
	}

	#[test]
	fn test_hold_speeds_up_after_800ms() {
		let mut keyboard = Keyboard::new();
		let steps = keyboard.hold('j', 0, 2000);

		assert_eq!(steps, expected_while_held(&steps));
		assert_eq!(steps.last().map(|&(_, step)| step), Some(2));
	}

	#[test]
	fn test_single_presses_after_a_hold_are_slow() {
		let mut keyboard = Keyboard::new();
		keyboard.hold('j', 0, 2000);

		for wait in [200, 200, 300, 450, 160, 250] {
			assert_eq!(keyboard.tap('j', wait), 1);
		}
	}

	#[test]
	fn test_steady_tapping_never_speeds_up() {
		for wait in [160, 200, 300, 400, 500, 600] {
			let mut keyboard = Keyboard::new();
			for _ in 0..40 {
				assert_eq!(keyboard.tap('j', wait), 1);
			}
		}
	}

	#[test]
	fn test_hold_right_after_a_tap_speeds_up_on_time() {
		let mut keyboard = Keyboard::new();
		keyboard.tap('j', 0);
		let steps = keyboard.hold('j', 300, 2000);

		assert_eq!(steps, expected_while_held(&steps));
	}

	#[test]
	fn test_other_key_starts_over() {
		let mut keyboard = Keyboard::new();
		keyboard.hold('j', 0, 2000);

		assert_eq!(keyboard.tap('k', REPEAT), 1);
	}
}
