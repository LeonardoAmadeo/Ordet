use super::*;

impl OrdetApp {
    pub(super) fn handle_prompt_key(&mut self, key: KeyEvent) -> bool {
        let Some(mut prompt) = self.state.ui.prompt.take() else {
            return false;
        };

        let plain_key = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);

        if plain_key && key.code == KeyCode::Tab {
            if let PromptKind::FindReplaceValue { query } = prompt.kind.clone() {
                self.run_replace_all(&query, prompt.value);
                return true;
            }
        }

        // Support pasting via Ctrl-V or Alt-V into prompts (e.g. pasting API keys or paths)
        if (key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('v') | KeyCode::Char('V')))
            || (key.modifiers.contains(KeyModifiers::ALT)
                && matches!(key.code, KeyCode::Char('v') | KeyCode::Char('V')))
        {
            if let Some((text, _)) = self.state.clipboard.paste() {
                prompt
                    .value
                    .push_str(text.trim_end_matches(&['\n', '\r'][..]));
                self.state.ui.prompt = Some(prompt);
                return true;
            }
        }

        // Search flags flip live while a find/replace prompt is open (Alt-R/W/C or Mac Option characters).
        let is_search_prompt = matches!(
            prompt.kind,
            PromptKind::FindText
                | PromptKind::FindReplaceQuery
                | PromptKind::FindReplaceValue { .. }
        );
        if is_search_prompt {
            if matches!(prompt.kind, PromptKind::FindText)
                && key.modifiers == KeyModifiers::NONE
                && matches!(
                    key.code,
                    KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
                )
                && !prompt.value.trim().is_empty()
            {
                let query = prompt.value.clone();
                self.run_find_direction(&query, matches!(key.code, KeyCode::Right | KeyCode::Down));
                self.state.ui.prompt = Some(prompt);
                return true;
            }
            let toggled = if key.modifiers.contains(KeyModifiers::ALT) {
                if let KeyCode::Char(ch) = key.code {
                    match ch.to_ascii_lowercase() {
                        'r' => {
                            self.state.ui.search_options.regex ^= true;
                            true
                        }
                        'w' => {
                            self.state.ui.search_options.whole_word ^= true;
                            true
                        }
                        'c' => {
                            self.state.ui.search_options.case_sensitive ^= true;
                            true
                        }
                        _ => false,
                    }
                } else {
                    false
                }
            } else {
                match key.code {
                    KeyCode::Char('®') => {
                        self.state.ui.search_options.regex ^= true;
                        true
                    }
                    KeyCode::Char('∑') => {
                        self.state.ui.search_options.whole_word ^= true;
                        true
                    }
                    KeyCode::Char('ç') => {
                        self.state.ui.search_options.case_sensitive ^= true;
                        true
                    }
                    _ => false,
                }
            };
            if toggled {
                prompt.hint =
                    crate::prompt::search_hint(&prompt.kind, self.state.ui.search_options);
                self.state.ui.prompt = Some(prompt);
                return true;
            }
        }

        match key.code {
            KeyCode::Esc => {
                if matches!(prompt.kind, PromptKind::SavePath) {
                    self.state.ui.quit_after_save = false;
                    self.pending_after_save = CloseIntent::None;
                }
                self.restore_focus_status();
            }
            KeyCode::Enter => {
                self.submit_prompt(prompt);
            }
            KeyCode::Backspace => {
                prompt.backspace();
                self.state.ui.prompt = Some(prompt);
            }
            KeyCode::Char(ch) if plain_key => {
                prompt.push(ch);
                self.state.ui.prompt = Some(prompt);
            }
            _ => {
                self.state.ui.prompt = Some(prompt);
            }
        }

        true
    }

    pub(super) fn submit_prompt(&mut self, prompt: PromptState) {
        let value = prompt.value.trim().to_string();
        match prompt.kind {
            PromptKind::Theme => match ThemeSelection::from_query(&value) {
                Some(theme) => {
                    self.apply_theme(theme, true);
                }
                None => {
                    let mut retry = PromptState::theme();
                    retry.value = prompt.value;
                    retry.hint = crate::theme::ThemeSelection::picker_hint().to_string();
                    self.state.ui.prompt = Some(retry);
                    self.state.set_status("Unknown theme name.");
                }
            },
            PromptKind::CustomColor => match self
                .state
                .ui
                .theme
                .with_custom_accent(&value, self.state.theme_context())
            {
                Ok(theme) => self.apply_theme(theme, true),
                Err(error) => {
                    let mut retry = PromptState::custom_color(Some(prompt.value.as_str()));
                    retry.hint = error;
                    self.state.ui.prompt = Some(retry);
                }
            },
            PromptKind::WordTarget => match parse_word_target(&value) {
                Ok(target) => self.apply_word_target(target),
                Err(error) => {
                    let mut retry = PromptState::word_target(self.state.ui.word_target);
                    retry.value = prompt.value;
                    retry.hint = error;
                    self.state.ui.prompt = Some(retry);
                }
            },
            PromptKind::ParagraphAlignment => match parse_alignment_input(&value) {
                Ok(alignment) => self.apply_paragraph_alignment(alignment),
                Err(error) => {
                    let mut retry = PromptState::paragraph_alignment(
                        self.current_text_buffer().current_alignment(),
                    );
                    retry.value = prompt.value;
                    retry.hint = error;
                    self.state.ui.prompt = Some(retry);
                }
            },
            PromptKind::SavePath => {
                match parse_save_path_input(&value, &self.state.editor().title()) {
                    Ok(path) => self.spawn_save_to(path),
                    Err(error) => {
                        let mut retry = PromptState::save_path(Some(prompt.value.as_str()));
                        retry.hint = error;
                        self.state.ui.prompt = Some(retry);
                    }
                }
            }
            PromptKind::OpenPath => match parse_open_path_input(&value) {
                Ok(path) => self.open_or_focus_path(path),
                Err(error) => {
                    let mut retry = PromptState::open_path(Some(prompt.value.as_str()));
                    retry.hint = error;
                    self.state.ui.prompt = Some(retry);
                }
            },
            PromptKind::WorkspacePath => match parse_workspace_path_input(&value) {
                Ok(path) => self.add_workspace(path),
                Err(error) => {
                    let mut retry = PromptState::workspace_path(Some(prompt.value.as_str()));
                    retry.hint = error;
                    self.state.ui.prompt = Some(retry);
                }
            },
            PromptKind::DocumentStatus => self.apply_document_status(value),
            PromptKind::BackupDirectory => self.apply_backup_location(value),
            PromptKind::RenameBrowserEntry { source, is_dir } => {
                match parse_browser_target_input(&value, &source) {
                    Ok(target) => {
                        if let Err(error) =
                            self.rename_browser_entry(source.clone(), target, is_dir)
                        {
                            let mut retry = PromptState::rename_browser_entry(&source, is_dir);
                            retry.value = prompt.value;
                            retry.hint = error;
                            self.state.ui.prompt = Some(retry);
                        }
                    }
                    Err(error) => {
                        let mut retry = PromptState::rename_browser_entry(&source, is_dir);
                        retry.value = prompt.value;
                        retry.hint = error;
                        self.state.ui.prompt = Some(retry);
                    }
                }
            }
            PromptKind::CopyBrowserEntry { source, is_dir } => {
                match parse_browser_target_input(&value, &source) {
                    Ok(target) => {
                        if let Err(error) = self.copy_browser_entry(source.clone(), target, is_dir)
                        {
                            let mut retry = PromptState::copy_browser_entry(
                                &source,
                                is_dir,
                                prompt.value.as_str(),
                            );
                            retry.hint = error;
                            self.state.ui.prompt = Some(retry);
                        }
                    }
                    Err(error) => {
                        let mut retry =
                            PromptState::copy_browser_entry(&source, is_dir, prompt.value.as_str());
                        retry.hint = error;
                        self.state.ui.prompt = Some(retry);
                    }
                }
            }
            PromptKind::FindText => {
                if value.is_empty() {
                    self.state.ui.prompt = Some(retry_prompt(
                        PromptState::find(None),
                        prompt.value,
                        "Enter text to find",
                    ));
                    return;
                }
                self.run_find(value.clone());
                // Keep the find bar open so Left/Right can walk matches
                // without another command chord.
                self.state.ui.prompt = Some(PromptState::find(Some(&value)));
            }
            PromptKind::FindReplaceQuery => {
                if value.is_empty() {
                    self.state.ui.prompt = Some(retry_prompt(
                        PromptState::find_replace_query(None),
                        prompt.value,
                        "Enter the text you want to replace",
                    ));
                    return;
                }
                self.state.ui.last_search = Some(value.clone());
                self.state
                    .ui
                    .prompt
                    .replace(PromptState::find_replace_value(&value, None));
            }
            PromptKind::FindReplaceValue { query } => self.run_replace(&query, value),
            PromptKind::GoToLine => match parse_line_number_input(&value) {
                Ok(line) => self.run_go_to_line(line),
                Err(error) => {
                    self.state.ui.prompt =
                        Some(retry_prompt(PromptState::go_to_line(), prompt.value, error));
                }
            },
            PromptKind::WriteBlock => match parse_block_target_input(&value) {
                Ok(path) => self.run_write_block(path),
                Err(error) => {
                    self.state.ui.prompt = Some(retry_prompt(
                        PromptState::write_block(None),
                        prompt.value,
                        error,
                    ));
                }
            },
            PromptKind::ReadFile => match parse_block_target_input(&value) {
                Ok(path) => self.run_read_file(path),
                Err(error) => {
                    self.state.ui.prompt = Some(retry_prompt(
                        PromptState::read_file(None),
                        prompt.value,
                        error,
                    ));
                }
            },
        }
    }
}
