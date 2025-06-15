use std::cell::Cell;

pub struct TriggerCell<T> {
    value: Cell<T>,
    triggered: Cell<bool>,
}
impl<T> TriggerCell<T> {
    pub fn new(initial: T) -> Self {
        Self {
            value: Cell::new(initial),
            triggered: Cell::new(false),
        }
    }

    pub fn get_if_triggered(&self) -> Option<T>
    where
        T: Copy,
    {
        if self.triggered.replace(false) {
            Some(self.value.get())
        } else {
            None
        }
    }

    pub fn set(&self, value: T)
    where
        T: Copy + PartialEq,
    {
        let old_value = self.value.get();
        self.value.set(value);
        self.triggered.set(old_value != value);
    }
}
