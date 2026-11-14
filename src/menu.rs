//! Menu de reedline habillé pour shsh : même comportement que le menu en
//! colonnes, mais le message « aucun résultat » est en français et discret.

use reedline::{ColumnarMenu, Completer, Editor, Menu, MenuEvent, MenuSettings, Painter, Suggestion};

/// Le second champ borne la hauteur du menu en lignes (0 : pas de limite).
pub struct MenuFr(pub ColumnarMenu, pub u16);

impl Menu for MenuFr {
    fn settings(&self) -> &MenuSettings {
        self.0.settings()
    }
    fn name(&self) -> &str {
        self.0.name()
    }
    fn indicator(&self) -> &str {
        self.0.indicator()
    }
    fn is_active(&self) -> bool {
        self.0.is_active()
    }
    fn set_active(&mut self, active: bool) {
        self.0.set_active(active)
    }
    fn clear_input(&mut self) {
        self.0.clear_input()
    }
    fn on_activate(&mut self) {
        self.0.on_activate()
    }
    fn on_deactivate(&mut self) {
        self.0.on_deactivate()
    }
    fn handle_menu_event(&mut self, event: &MenuEvent) {
        self.0.handle_menu_event(event)
    }
    fn menu_event(&mut self, event: MenuEvent) {
        self.0.menu_event(event)
    }
    fn can_quick_complete(&self) -> bool {
        self.0.can_quick_complete()
    }
    fn can_partially_complete(&mut self, values_updated: bool, editor: &mut Editor, completer: &mut dyn Completer) -> bool {
        self.0.can_partially_complete(values_updated, editor, completer)
    }
    fn update_values(&mut self, editor: &mut Editor, completer: &mut dyn Completer) {
        self.0.update_values(editor, completer)
    }
    fn reset_position(&mut self) {
        self.0.reset_position()
    }
    fn reload(&mut self, updated: bool, editor: &mut Editor, completer: &mut dyn Completer) {
        self.0.reload(updated, editor, completer)
    }
    fn update_working_details(&mut self, editor: &mut Editor, completer: &mut dyn Completer, painter: &Painter) {
        self.0.update_working_details(editor, completer, painter)
    }
    fn replace_in_buffer(&self, editor: &mut Editor) {
        self.0.replace_in_buffer(editor)
    }
    fn menu_required_lines(&self, terminal_columns: u16) -> u16 {
        let n = self.0.menu_required_lines(terminal_columns);
        if self.1 > 0 { n.min(self.1) } else { n }
    }
    fn menu_string(&self, available_lines: u16, use_ansi_coloring: bool) -> String {
        let lignes = if self.1 > 0 { available_lines.min(self.1) } else { available_lines };
        let s = self.0.menu_string(lignes, use_ansi_coloring);
        if s.contains("NO RECORDS FOUND") {
            // gris brume : une information, pas une alerte
            return "\x1b[38;2;98;104;140maucun résultat\x1b[0m".to_string();
        }
        s
    }
    fn min_rows(&self) -> u16 {
        self.0.min_rows()
    }
    fn get_values(&self) -> &[Suggestion] {
        self.0.get_values()
    }
    fn results_are_provisional(&self) -> bool {
        self.0.results_are_provisional()
    }
    fn is_awaiting_first_answer(&self) -> bool {
        self.0.is_awaiting_first_answer()
    }
    fn is_visible(&self) -> bool {
        self.0.is_visible()
    }
    fn set_cursor_pos(&mut self, pos: (u16, u16)) {
        self.0.set_cursor_pos(pos)
    }
    fn selection_at(&self, column: u16, menu_row: u16, terminal_columns: u16, available_lines: u16) -> Option<usize> {
        self.0.selection_at(column, menu_row, terminal_columns, available_lines)
    }
    fn set_selection(&mut self, index: usize) {
        self.0.set_selection(index)
    }
}
