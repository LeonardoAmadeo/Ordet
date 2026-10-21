use super::*;

impl OrdetApp {
    /// Opens the option picker for one settings row: every choice listed at
    /// once, preselected on the current value.
    pub(super) fn open_settings_picker(&mut self, kind: SettingsPickerKind) {
        let (title, options, selected) = match kind {
            SettingsPickerKind::Theme => {
                let presets = ThemePreset::ALL;
                let current = self.state.theme_context();
                let selected = presets
                    .iter()
                    .position(|preset| {
                        *preset == self.state.ui.theme.base_preset(current).canonical()
                    })
                    .unwrap_or(0);
                (
                    "Theme".to_string(),
                    presets
                        .iter()
                        .map(|preset| preset.name().to_string())
                        .collect(),
                    selected,
                )
            }
            SettingsPickerKind::Language => {
                let languages = crate::i18n::UiLanguage::ALL;
                let selected = languages
                    .iter()
                    .position(|language| *language == self.state.ui.language)
                    .unwrap_or(0);
                (
                    "Language".to_string(),
                    languages
                        .iter()
                        .map(|language| language.display_name().to_string())
                        .collect(),
                    selected,
                )
            }
            SettingsPickerKind::IndentWidth => {
                let widths = [2, 4, 8];
                let selected = widths
                    .iter()
                    .position(|width| *width == self.state.ui.indent_width)
                    .unwrap_or(1);
                (
                    "Indent width".to_string(),
                    widths.iter().map(|width| width.to_string()).collect(),
                    selected,
                )
            }
        };

        let mut picker = SettingsPickerState {
            kind,
            title: title.clone(),
            options,
            selected,
            scroll: 0,
            active: selected,
            preview_from: if kind == SettingsPickerKind::Theme {
                Some(self.state.ui.theme.clone())
            } else {
                None
            },
        };
        let visible =
            crate::state::settings_picker_list_rect(self.full_area(), picker.options.len()).height
                as usize;
        picker.clamp_to(visible.max(1));
        self.state.panels.settings_picker = Some(picker);
        self.state.set_status(format!(
            "Select {title}. Up/Down to navigate, Enter to choose, Esc to cancel."
        ));
    }

    /// Shows the highlighted theme without saving it, so browsing the list
    /// previews live. Silent by design: no status per step, no persist.
    /// Enter persists; Esc restores whatever was in force.
    pub(super) fn preview_settings_picker_theme(&mut self) {
        let Some(picker) = self.state.panels.settings_picker.as_ref() else {
            return;
        };
        if picker.kind != SettingsPickerKind::Theme {
            return;
        }
        // ALL holds canonical presets, so no sanitize step is needed.
        if let Some(preset) = ThemePreset::ALL.get(picker.selected) {
            self.state.ui.theme = ThemeSelection::Preset { preset: *preset };
        }
    }

    /// Closes the picker, restoring the pre-browse theme when a theme
    /// preview is showing. Used by every dismiss path so none can strand a
    /// preview nobody chose.
    pub(super) fn cancel_settings_picker(&mut self) {
        let Some(picker) = self.state.panels.settings_picker.take() else {
            return;
        };
        if picker.kind == SettingsPickerKind::Theme {
            if let Some(original) = picker.preview_from {
                self.state.ui.theme = original;
            }
        }
        self.state
            .set_status_quiet("Setting selection closed.".to_string());
    }

    /// Applies the highlighted choice and closes the picker. Shared by the
    /// Enter key and mouse clicks so both confirm the same row.
    pub(super) fn confirm_settings_picker(&mut self) {
        let Some(picker) = self.state.panels.settings_picker.take() else {
            return;
        };
        let choice = picker.options.get(picker.selected).cloned();
        let Some(choice) = choice else {
            return;
        };
        match picker.kind {
            SettingsPickerKind::Theme => {
                if let Some(preset) = ThemePreset::ALL.get(picker.selected) {
                    self.apply_theme(ThemeSelection::Preset { preset: *preset }, true);
                }
            }
            SettingsPickerKind::Language => {
                if let Some(language) = crate::i18n::UiLanguage::ALL.get(picker.selected).copied() {
                    self.apply_language(language);
                }
            }
            SettingsPickerKind::IndentWidth => {
                if let Ok(width) = choice.parse::<usize>() {
                    self.apply_indent_width(width);
                }
            }
        }
    }
}
