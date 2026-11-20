use crate::notify_mutex::NotifiableMutex;
use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver, Sender};
use crossterm::{
	event::{
		self, Event, Event::Key, KeyCode, KeyEvent, KeyEventKind,
		KeyModifiers, KeyboardEnhancementFlags,
		PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
	},
	terminal::supports_keyboard_enhancement,
	ExecutableCommand,
};
use std::{
	io,
	sync::{
		atomic::{AtomicBool, Ordering},
		Arc, OnceLock,
	},
	thread,
	time::{Duration, Instant},
};

static FAST_POLL_DURATION: Duration = Duration::from_millis(100);
static SLOW_POLL_DURATION: Duration = Duration::from_secs(10);

/// Whether the terminal answered yes to the kitty keyboard protocol query.
///
/// The query reads the reply from stdin, so it must only run before the
/// input thread starts polling. The answer is cached for later terminal
/// setups, such as returning from an external editor.
static KEYBOARD_ENHANCEMENT_SUPPORTED: OnceLock<bool> =
	OnceLock::new();

/// Asks the terminal to report modified keys that the legacy encoding
/// cannot express, such as ctrl+enter, which legacy terminals send as a
/// plain enter, and to tell the auto-repeats of a held key apart from
/// presses. Neither flag changes how unmodified enter, tab and backspace
/// or shifted letters are sent, so existing bindings are unaffected.
///
/// Call before the input thread exists, or while it is paused.
pub fn enable_keyboard_enhancement() {
	let supported =
		*KEYBOARD_ENHANCEMENT_SUPPORTED.get_or_init(|| {
			supports_keyboard_enhancement().unwrap_or_else(|e| {
				log::warn!("keyboard enhancement query failed: {e}");
				false
			})
		});

	if !supported {
		log::info!("terminal does not support the kitty keyboard protocol, modified keys like ctrl+enter arrive unmodified");
		return;
	}

	if let Err(e) =
		io::stdout().execute(PushKeyboardEnhancementFlags(
			KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
				| KeyboardEnhancementFlags::REPORT_EVENT_TYPES,
		)) {
		log::error!("push keyboard enhancement flags failed: {e}");
	}
}

/// Undoes [`enable_keyboard_enhancement`] so whatever runs in the terminal
/// next sees legacy key encoding.
pub fn disable_keyboard_enhancement() {
	if KEYBOARD_ENHANCEMENT_SUPPORTED
		.get()
		.copied()
		.unwrap_or(false)
	{
		if let Err(e) =
			io::stdout().execute(PopKeyboardEnhancementFlags)
		{
			log::error!("pop keyboard enhancement flags failed: {e}");
		}
	}
}

/// Longest gap between two presses of one key for the second to count as
/// an auto-repeat, on a terminal that does not say which it is. Must
/// exceed the OS initial key-repeat delay, or the first repeat of a hold
/// looks like a press. Taps closer together than this look like a hold.
const REPEAT_GUESS_GAP: Duration = Duration::from_millis(300);

/// Marks the auto-repeats of a held key as [`KeyEventKind::Repeat`].
///
/// A terminal speaking the kitty keyboard protocol reports repeats itself,
/// so its presses are taken at their word however fast they come. Any
/// other sends repeats as presses, and those are guessed from how soon a
/// press follows the previous one of the same key.
#[derive(Default)]
struct RepeatMarker {
	terminal_reports_repeats: bool,
	last_press: Option<(KeyCode, KeyModifiers, Instant)>,
}

impl RepeatMarker {
	/// Stop guessing, the terminal tells repeats from presses.
	const fn trust_terminal(&mut self) {
		self.terminal_reports_repeats = true;
	}

	fn mark(&mut self, key: &mut KeyEvent, now: Instant) {
		if key.kind == KeyEventKind::Repeat {
			self.trust_terminal();
		}
		if self.terminal_reports_repeats {
			return;
		}

		let follows_closely =
			self.last_press.is_some_and(|(code, modifiers, at)| {
				code == key.code
					&& modifiers == key.modifiers
					&& now.duration_since(at) < REPEAT_GUESS_GAP
			});
		if follows_closely {
			key.kind = KeyEventKind::Repeat;
		}

		self.last_press = Some((key.code, key.modifiers, now));
	}
}

///
#[derive(Clone, Copy, Debug)]
pub enum InputState {
	Paused,
	Polling,
}

///
#[derive(Clone, Debug)]
pub enum InputEvent {
	Input(Event),
	State(InputState),
}

///
#[derive(Clone)]
pub struct Input {
	desired_state: Arc<NotifiableMutex<bool>>,
	current_state: Arc<AtomicBool>,
	receiver: Receiver<InputEvent>,
	aborted: Arc<AtomicBool>,
}

impl Input {
	///
	pub fn new() -> Self {
		let (tx, rx) = unbounded();

		let desired_state = Arc::new(NotifiableMutex::new(true));
		let current_state = Arc::new(AtomicBool::new(true));
		let aborted = Arc::new(AtomicBool::new(false));

		let arc_desired = Arc::clone(&desired_state);
		let arc_current = Arc::clone(&current_state);
		let arc_aborted = Arc::clone(&aborted);

		thread::spawn(move || {
			if let Err(e) =
				Self::input_loop(&arc_desired, &arc_current, &tx)
			{
				log::error!("input thread error: {e}");
				arc_aborted.store(true, Ordering::SeqCst);
			}
		});

		Self {
			receiver: rx,
			desired_state,
			current_state,
			aborted,
		}
	}

	///
	pub fn receiver(&self) -> Receiver<InputEvent> {
		self.receiver.clone()
	}

	///
	pub fn set_polling(&self, enabled: bool) {
		self.desired_state.set_and_notify(enabled);
	}

	fn shall_poll(&self) -> bool {
		self.desired_state.get()
	}

	///
	pub fn is_state_changing(&self) -> bool {
		self.shall_poll()
			!= self.current_state.load(Ordering::Relaxed)
	}

	pub fn is_aborted(&self) -> bool {
		self.aborted.load(Ordering::SeqCst)
	}

	fn poll(dur: Duration) -> anyhow::Result<Option<Event>> {
		if event::poll(dur)? {
			Ok(Some(event::read()?))
		} else {
			Ok(None)
		}
	}

	fn input_loop(
		arc_desired: &Arc<NotifiableMutex<bool>>,
		arc_current: &Arc<AtomicBool>,
		tx: &Sender<InputEvent>,
	) -> Result<()> {
		let mut poll_duration = SLOW_POLL_DURATION;
		let mut repeat_marker = RepeatMarker::default();
		loop {
			if arc_desired.get() {
				if !arc_current.load(Ordering::Relaxed) {
					log::info!("input polling resumed");

					tx.send(InputEvent::State(InputState::Polling))?;
				}
				arc_current.store(true, Ordering::Relaxed);

				if let Some(mut e) = Self::poll(poll_duration)? {
					// windows and kitty protocol terminals send key
					// release too, only process presses and repeats
					if let Key(key) = &mut e {
						if key.kind == KeyEventKind::Release {
							continue;
						}
						if KEYBOARD_ENHANCEMENT_SUPPORTED
							.get()
							.copied()
							.unwrap_or(false)
						{
							repeat_marker.trust_terminal();
						}
						repeat_marker.mark(key, Instant::now());
					}

					tx.send(InputEvent::Input(e))?;
					//Note: right after an input event we might have a reason to stop
					// polling (external editor opening) so lets do a quick poll until the next input
					// this fixes https://github.com/gitui-org/gitui/issues/1506
					poll_duration = FAST_POLL_DURATION;
				} else {
					poll_duration = SLOW_POLL_DURATION;
				}
			} else {
				if arc_current.load(Ordering::Relaxed) {
					log::info!("input polling suspended");

					tx.send(InputEvent::State(InputState::Paused))?;
				}

				arc_current.store(false, Ordering::Relaxed);

				arc_desired.wait(true);
			}
		}
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
		marker: RepeatMarker,
		start: Instant,
		now: u64,
	}

	impl Keyboard {
		fn new() -> Self {
			Self {
				marker: RepeatMarker::default(),
				start: Instant::now(),
				now: 0,
			}
		}

		/// Send `code` as `kind`, `wait` ms after the previous key, and
		/// return the kind it is marked as.
		fn send(
			&mut self,
			code: KeyCode,
			kind: KeyEventKind,
			wait: u64,
		) -> KeyEventKind {
			self.now += wait;
			let mut key = KeyEvent::new_with_kind(
				code,
				KeyModifiers::empty(),
				kind,
			);
			self.marker.mark(
				&mut key,
				self.start + Duration::from_millis(self.now),
			);
			key.kind
		}

		/// A press on a terminal that sends repeats as presses too.
		fn press(
			&mut self,
			code: KeyCode,
			wait: u64,
		) -> KeyEventKind {
			self.send(code, KeyEventKind::Press, wait)
		}
	}

	#[test]
	fn test_held_key_is_a_press_then_repeats() {
		let mut keyboard = Keyboard::new();

		let mut kinds = vec![keyboard.press(KeyCode::Left, 0)];
		kinds.push(keyboard.press(KeyCode::Left, INITIAL_DELAY));
		for _ in 0..20 {
			kinds.push(keyboard.press(KeyCode::Left, REPEAT));
		}

		assert_eq!(kinds[0], KeyEventKind::Press);
		assert!(kinds[1..]
			.iter()
			.all(|kind| *kind == KeyEventKind::Repeat));
	}

	#[test]
	fn test_press_after_a_pause_is_a_press() {
		let mut keyboard = Keyboard::new();
		keyboard.press(KeyCode::Left, 0);
		keyboard.press(KeyCode::Left, INITIAL_DELAY);
		keyboard.press(KeyCode::Left, REPEAT);

		assert_eq!(
			keyboard.press(KeyCode::Left, 300),
			KeyEventKind::Press
		);
	}

	#[test]
	fn test_fast_taps_are_guessed_to_be_repeats() {
		let mut keyboard = Keyboard::new();
		keyboard.press(KeyCode::Left, 0);

		assert_eq!(
			keyboard.press(KeyCode::Left, 150),
			KeyEventKind::Repeat
		);
	}

	#[test]
	fn test_other_key_is_a_press() {
		let mut keyboard = Keyboard::new();
		keyboard.press(KeyCode::Left, 0);

		assert_eq!(
			keyboard.press(KeyCode::Right, REPEAT),
			KeyEventKind::Press
		);
	}

	#[test]
	fn test_fast_taps_are_presses_on_a_trusted_terminal() {
		let mut keyboard = Keyboard::new();
		keyboard.marker.trust_terminal();

		for wait in [0, REPEAT, 100, 150, 250] {
			assert_eq!(
				keyboard.press(KeyCode::Left, wait),
				KeyEventKind::Press
			);
		}
	}

	#[test]
	fn test_fast_taps_are_presses_once_terminal_reports_repeats() {
		let mut keyboard = Keyboard::new();
		keyboard.press(KeyCode::Down, 0);
		assert_eq!(
			keyboard.send(
				KeyCode::Down,
				KeyEventKind::Repeat,
				INITIAL_DELAY
			),
			KeyEventKind::Repeat
		);

		keyboard.press(KeyCode::Left, 500);
		for wait in [REPEAT, 100, 150, 250] {
			assert_eq!(
				keyboard.press(KeyCode::Left, wait),
				KeyEventKind::Press
			);
		}
	}
}
