use crate::notify_mutex::NotifiableMutex;
use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver, Sender};
use crossterm::{
	event::{
		self, Event, Event::Key, KeyEventKind,
		KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
		PushKeyboardEnhancementFlags,
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
	time::Duration,
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
/// plain enter. Only the disambiguation flag is requested, so unmodified
/// enter, tab and backspace and shifted letters keep their legacy form and
/// existing bindings are unaffected.
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
			KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
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
		loop {
			if arc_desired.get() {
				if !arc_current.load(Ordering::Relaxed) {
					log::info!("input polling resumed");

					tx.send(InputEvent::State(InputState::Polling))?;
				}
				arc_current.store(true, Ordering::Relaxed);

				if let Some(e) = Self::poll(poll_duration)? {
					// windows send key release too, only process key press
					if let Key(key) = e {
						if key.kind != KeyEventKind::Press {
							continue;
						}
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
