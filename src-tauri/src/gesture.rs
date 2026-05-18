use crate::types::WindowContext;
use rdev::{listen, Button, EventType, Key};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
        Arc,
    },
    thread,
    time::Instant,
};

/// Closure called synchronously from the rdev hook callback at the moment of a button press,
/// before the click is forwarded to the target application. This is the only window in which
/// we can query UIA selection state — once the callback returns, the click is delivered to the
/// target window which usually clears the selection.
pub type ContextCapture = Arc<dyn Fn(i32, i32) -> Option<WindowContext> + Send + Sync>;

#[derive(Debug, Clone)]
pub enum GestureEvent {
    Down {
        button: TriggerButton,
        at: Instant,
        x: f64,
        y: f64,
        context: Option<WindowContext>,
    },
    LeftMove {
        at: Instant,
        x: f64,
        y: f64,
    },
    Up {
        button: TriggerButton,
        at: Instant,
        x: f64,
        y: f64,
    },
    Escape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerButton {
    Left,
    Right,
}

pub struct GestureHook {
    enabled: Arc<AtomicBool>,
    started: AtomicBool,
}

impl GestureHook {
    pub fn new() -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(false)),
            started: AtomicBool::new(false),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn start(&self, tx: Sender<GestureEvent>, capture: ContextCapture) -> anyhow::Result<()> {
        self.enabled.store(true, Ordering::Relaxed);
        if self.started.swap(true, Ordering::Relaxed) {
            return Ok(());
        }

        let enabled = self.enabled.clone();
        thread::Builder::new()
            .name("voice-keyboard-gesture-hook".to_string())
            .spawn(move || {
                let mut left_is_down = false;
                let mut right_is_down = false;
                let mut last_x: f64 = 0.0;
                let mut last_y: f64 = 0.0;
                let callback = move |event: rdev::Event| {
                    if !enabled.load(Ordering::Relaxed) {
                        return;
                    }
                    let now = Instant::now();
                    match event.event_type {
                        EventType::ButtonPress(Button::Left) => {
                            left_is_down = true;
                            // Capture context synchronously here, BEFORE the click is delivered
                            // to the target window. This is the only point where the UIA
                            // selection state still reflects what the user had selected — once
                            // this callback returns, the mousedown clears most apps' selection.
                            let ctx = capture(last_x.round() as i32, last_y.round() as i32);
                            let _ = tx.send(GestureEvent::Down {
                                button: TriggerButton::Left,
                                at: now,
                                x: last_x,
                                y: last_y,
                                context: ctx,
                            });
                        }
                        EventType::ButtonPress(Button::Right) => {
                            right_is_down = true;
                            let ctx = capture(last_x.round() as i32, last_y.round() as i32);
                            let _ = tx.send(GestureEvent::Down {
                                button: TriggerButton::Right,
                                at: now,
                                x: last_x,
                                y: last_y,
                                context: ctx,
                            });
                        }
                        EventType::MouseMove { x, y } => {
                            last_x = x;
                            last_y = y;
                            if left_is_down || right_is_down {
                                let _ = tx.send(GestureEvent::LeftMove { at: now, x, y });
                            }
                        }
                        EventType::ButtonRelease(Button::Left) => {
                            left_is_down = false;
                            let _ = tx.send(GestureEvent::Up {
                                button: TriggerButton::Left,
                                at: now,
                                x: last_x,
                                y: last_y,
                            });
                        }
                        EventType::ButtonRelease(Button::Right) => {
                            right_is_down = false;
                            let _ = tx.send(GestureEvent::Up {
                                button: TriggerButton::Right,
                                at: now,
                                x: last_x,
                                y: last_y,
                            });
                        }
                        EventType::KeyPress(Key::Escape) => {
                            let _ = tx.send(GestureEvent::Escape);
                        }
                        _ => {}
                    }
                };
                if let Err(err) = listen(callback) {
                    eprintln!("global input hook failed: {err:?}");
                }
            })?;
        Ok(())
    }

    pub fn stop(&self) {
        self.enabled.store(false, Ordering::Relaxed);
    }
}
