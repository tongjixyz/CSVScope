use crate::model::{EditState, ViewState};
use std::collections::VecDeque;
use std::sync::Arc;

pub struct History {
    pub present: Arc<ViewState>,
    past: VecDeque<Arc<ViewState>>,
    future: Vec<Arc<ViewState>>,
}

impl Default for History {
    fn default() -> Self {
        Self { present: Arc::new(ViewState::default()), past: VecDeque::new(), future: vec![] }
    }
}

impl History {
    pub fn load_colors(&mut self, colors: &EditState) {
        for state in self.past.iter_mut().chain(self.future.iter_mut()).chain(std::iter::once(&mut self.present)) {
            Arc::make_mut(state).edits.colors = colors.colors.clone();
        }
    }
    pub fn commit(&mut self, state: ViewState) -> bool {
        if *self.present == state { return false; }
        self.past.push_back(self.present.clone());
        self.present = Arc::new(state);
        self.future.clear();
        while self.past.len() > 100 || self.bytes() > 8 * 1024 * 1024 {
            if self.past.pop_front().is_none() { break; }
        }
        true
    }
    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.past.pop_back() else { return false };
        self.future.push(self.present.clone());
        self.present = previous;
        true
    }
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.future.pop() else { return false };
        self.past.push_back(self.present.clone());
        self.present = next;
        true
    }
    pub fn can_undo(&self) -> bool { !self.past.is_empty() }
    pub fn can_redo(&self) -> bool { !self.future.is_empty() }
    pub fn trim_oldest(&mut self) -> bool {
        self.past.pop_front().is_some() || self.future.pop().is_some()
    }
    pub fn bytes(&self) -> usize {
        self.past.iter().chain(self.future.iter()).chain(std::iter::once(&self.present))
            .map(|s| serde_json::to_vec(s).map(|v| v.len()).unwrap_or(0)).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SortKey;
    #[test]
    fn history_branches_and_ignores_noop() {
        let mut h = History::default();
        let mut state = ViewState::default();
        state.sort.push(SortKey { column: 0, descending: false, interpretation: "text".into() });
        assert!(h.commit(state.clone()));
        assert!(!h.commit(state));
        assert!(h.undo());
        assert!(h.can_redo());
        let mut other = ViewState::default();
        other.sort.push(SortKey { column: 1, descending: true, interpretation: "text".into() });
        h.commit(other);
        assert!(!h.can_redo());
        assert!(h.undo());
        assert!(h.present.sort.is_empty());
    }
}
