//! Undo history, including the generation numbers
//! [`crate::document::EditHistory`] keys its snapshots by.
use super::Selection;
use crate::document::Position;

/// Typed changes with the same `+origin` merge into one undo step within this delay.
pub const EVENT_DELAY_MS: f64 = 1250.0;

/// A change as history replays it: replace `from..to` with `text`.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub from: Position,
    pub to: Position,
    pub text: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Event {
    /// Applied last to first.
    pub steps: Vec<Step>,
    /// The generation before the event.
    pub generation: u64,
    /// The selection to restore.
    pub selection: Selection,
}

#[derive(Clone, Debug)]
pub struct History {
    pub(super) done: Vec<Event>,
    pub(super) undone: Vec<Event>,
    generation: u64,
    max: u64,
    last_time: f64,
    last_operation: Option<u64>,
    last_origin: Option<String>,
}

impl Default for History {
    fn default() -> Self {
        Self {
            done: Vec::new(),
            undone: Vec::new(),
            generation: 1,
            max: 1,
            last_time: 0.0,
            last_operation: None,
            last_origin: None,
        }
    }
}

impl History {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The next change starts a new undo step; returns the generation.
    pub fn close(&mut self) -> u64 {
        self.last_operation = None;
        self.last_origin = None;
        self.generation
    }

    pub fn generations(&self) -> std::collections::BTreeSet<u64> {
        std::iter::once(self.generation)
            .chain(self.done.iter().map(|event| event.generation))
            .chain(self.undone.iter().map(|event| event.generation))
            .collect()
    }

    pub fn size(&self) -> (usize, usize) {
        (self.done.len(), self.undone.len())
    }

    pub fn clear(&mut self) {
        self.done.clear();
        self.undone.clear();
        self.close();
    }

    /// Records a change. `change_to` is the end of the replaced range before the change;
    /// `step.to` is the end of the inserted text.
    pub fn add(
        &mut self,
        step: Step,
        change_to: Position,
        origin: &str,
        operation: u64,
        time: f64,
        selection: Selection,
    ) {
        self.undone.clear();
        let merge = self.last_operation == Some(operation)
            || (!origin.is_empty()
                && self.last_origin.as_deref() == Some(origin)
                && ((origin.starts_with('+') && self.last_time > time - EVENT_DELAY_MS)
                    || origin.starts_with('*')));
        match self.done.last_mut().filter(|_| merge) {
            Some(event) => {
                let last = event.steps.last_mut().expect("history events have steps");
                if step.from == change_to && step.from == last.to {
                    // Typing extends the previous insertion instead of adding a step.
                    last.to = step.to;
                } else {
                    event.steps.push(step);
                }
            }
            None => self.done.push(Event {
                steps: vec![step],
                generation: self.generation,
                selection,
            }),
        }
        self.max += 1;
        self.generation = self.max;
        self.last_time = time;
        self.last_operation = Some(operation);
        self.last_origin = Some(origin.to_owned());
    }

    /// Takes the event to undo (or redo); [`History::finish`] records its inverse.
    pub fn take(&mut self, undo: bool) -> Option<Event> {
        let event = if undo {
            self.done.pop()
        } else {
            self.undone.pop()
        }?;
        self.last_origin = None;
        Some(event)
    }

    pub fn finish(&mut self, undo: bool, event: &Event, inverse: Vec<Step>, selection: Selection) {
        let inverse = Event {
            steps: inverse,
            generation: self.generation,
            selection,
        };
        if undo {
            self.undone.push(inverse);
        } else {
            self.done.push(inverse);
        }
        self.generation = if event.generation > 0 {
            event.generation
        } else {
            self.max += 1;
            self.max
        };
    }
}
