use slint::winit_030::winit::keyboard::{Key, ModifiersState, NamedKey};
use texti_settings::CommandPaletteSettings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandDef {
    pub id: &'static str,
    pub title: &'static str,
    pub group: &'static str,
    pub default_shortcut: &'static str,
    pub default_visible: bool,
}

macro_rules! command {
    ($id:literal, $title:literal, $group:literal, $shortcut:literal) => {
        CommandDef {
            id: $id,
            title: $title,
            group: $group,
            default_shortcut: $shortcut,
            default_visible: true,
        }
    };
}

pub const COMMANDS: &[CommandDef] = &[
    command!("file.new", "New File", "File", "Ctrl+N"),
    command!("file.open", "Open File", "File", "Ctrl+O"),
    command!("file.open-folder", "Open Folder", "File", "Ctrl+Shift+O"),
    command!("file.save", "Save", "File", "Ctrl+S"),
    command!("file.save-as", "Save As", "File", "Ctrl+Shift+S"),
    command!("file.reload", "Reload from Disk", "File", "Ctrl+R"),
    command!("file.close", "Close File", "File", "Ctrl+W"),
    command!("tab.next", "Next Tab", "Navigation", "Ctrl+Tab"),
    command!(
        "tab.previous",
        "Previous Tab",
        "Navigation",
        "Ctrl+Shift+Tab"
    ),
    command!("edit.undo", "Undo", "Edit", "Ctrl+Z"),
    command!("edit.redo", "Redo", "Edit", "Ctrl+Shift+Z"),
    command!("edit.cut", "Cut", "Edit", "Ctrl+X"),
    command!("edit.copy", "Copy", "Edit", "Ctrl+C"),
    command!("edit.paste", "Paste", "Edit", "Ctrl+V"),
    command!("edit.select-all", "Select All", "Edit", "Ctrl+A"),
    command!("search.find", "Find", "Search", "Ctrl+F"),
    command!("search.replace", "Replace", "Search", "Ctrl+H"),
    command!("search.goto", "Go to Line", "Search", "Ctrl+G"),
    command!(
        "workspace.search",
        "Search Workspace Files",
        "Workspace",
        ""
    ),
    command!("workspace.recent", "Recent Files", "Workspace", ""),
    command!(
        "workspace.new-file",
        "New File in Workspace",
        "Workspace",
        ""
    ),
    command!(
        "workspace.new-folder",
        "New Folder in Workspace",
        "Workspace",
        ""
    ),
    command!("workspace.rename", "Rename Selected Item", "Workspace", ""),
    command!(
        "workspace.trash",
        "Move Selected Item to Trash",
        "Workspace",
        ""
    ),
    command!("view.wrap", "Toggle Word Wrap", "View", ""),
    command!("view.line-numbers", "Toggle Line Numbers", "View", ""),
    command!("view.minimap", "Toggle Minimap", "View", ""),
    command!("view.whitespace", "Toggle Whitespace", "View", ""),
    command!("view.focus", "Toggle Focus Mode", "View", "F11"),
    command!(
        "palette.open",
        "Open Command Palette",
        "Application",
        "Ctrl+Shift+P"
    ),
    command!("settings.open", "Open Settings", "Application", "Ctrl+,"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_key: bool,
    pub key: String,
}

impl Shortcut {
    pub fn parse(value: &str) -> Result<Self, String> {
        let parts = value
            .split('+')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        let Some((key, modifiers)) = parts.split_last() else {
            return Err("Press a shortcut or clear the current one.".to_string());
        };
        let mut shortcut = Self {
            ctrl: false,
            alt: false,
            shift: false,
            super_key: false,
            key: normalize_key_label(key)?,
        };
        for modifier in modifiers {
            let slot = if modifier.eq_ignore_ascii_case("ctrl")
                || modifier.eq_ignore_ascii_case("control")
            {
                &mut shortcut.ctrl
            } else if modifier.eq_ignore_ascii_case("alt") {
                &mut shortcut.alt
            } else if modifier.eq_ignore_ascii_case("shift") {
                &mut shortcut.shift
            } else if modifier.eq_ignore_ascii_case("super")
                || modifier.eq_ignore_ascii_case("meta")
                || modifier.eq_ignore_ascii_case("cmd")
            {
                &mut shortcut.super_key
            } else {
                return Err(format!("Unknown shortcut modifier: {modifier}"));
            };
            if *slot {
                return Err(format!("Duplicate shortcut modifier: {modifier}"));
            }
            *slot = true;
        }
        shortcut.validate()?;
        Ok(shortcut)
    }

    pub fn from_key(key: &Key, modifiers: ModifiersState) -> Result<Option<Self>, String> {
        if is_modifier_key(key) {
            return Ok(None);
        }
        let key = match key {
            Key::Character(value) => normalize_character(value)?,
            Key::Named(named) => named_key_label(*named)
                .map(str::to_string)
                .ok_or_else(|| "That key cannot be used as a Texti shortcut.".to_string())?,
            _ => return Err("That key cannot be used as a Texti shortcut.".to_string()),
        };
        let shortcut = Self {
            ctrl: modifiers.control_key(),
            alt: modifiers.alt_key(),
            shift: modifiers.shift_key(),
            super_key: modifiers.super_key(),
            key,
        };
        shortcut.validate()?;
        Ok(Some(shortcut))
    }

    pub fn label(&self) -> String {
        let mut parts = Vec::with_capacity(5);
        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.super_key {
            parts.push("Super".to_string());
        }
        parts.push(self.key.clone());
        parts.join("+")
    }

    fn validate(&self) -> Result<(), String> {
        if self.key == "Escape" {
            return Err("Escape is reserved for closing menus and dialogs.".to_string());
        }
        if self.key == "F1" {
            return Err("F1 is reserved as the permanent Command Palette shortcut.".to_string());
        }
        let function_key =
            function_key_number(&self.key).is_some_and(|number| (2..=12).contains(&number));
        if !function_key && !(self.ctrl || self.alt || self.super_key) {
            return Err(
                "Use Ctrl, Alt, or Super with a key, or choose F2 through F12.".to_string(),
            );
        }
        Ok(())
    }
}

pub fn command(id: &str) -> Option<&'static CommandDef> {
    COMMANDS.iter().find(|command| command.id == id)
}

pub fn is_palette_visible(command: &CommandDef, preferences: &CommandPaletteSettings) -> bool {
    command.default_visible && !preferences.hidden_commands.contains(command.id)
}

pub fn resolved_shortcut(
    command: &CommandDef,
    preferences: &CommandPaletteSettings,
) -> Option<Shortcut> {
    match preferences.shortcut_overrides.get(command.id) {
        Some(None) => None,
        Some(Some(value)) => Shortcut::parse(value)
            .ok()
            .or_else(|| parse_default_shortcut(command)),
        None => parse_default_shortcut(command),
    }
}

pub fn resolved_shortcut_label(
    command: &CommandDef,
    preferences: &CommandPaletteSettings,
) -> String {
    resolved_shortcut(command, preferences)
        .map(|shortcut| shortcut.label())
        .unwrap_or_default()
}

pub fn shortcut_conflict(
    target_id: &str,
    candidate: &Shortcut,
    preferences: &CommandPaletteSettings,
) -> Option<&'static CommandDef> {
    COMMANDS.iter().find(|command| {
        command.id != target_id
            && resolved_shortcut(command, preferences).as_ref() == Some(candidate)
    })
}

fn parse_default_shortcut(command: &CommandDef) -> Option<Shortcut> {
    if command.default_shortcut.is_empty() {
        None
    } else {
        Shortcut::parse(command.default_shortcut).ok()
    }
}

fn normalize_character(value: &str) -> Result<String, String> {
    let mut characters = value.chars();
    let Some(character) = characters.next() else {
        return Err("That key cannot be used as a Texti shortcut.".to_string());
    };
    if characters.next().is_some() {
        return Err("That key cannot be used as a Texti shortcut.".to_string());
    }
    if character == ' ' {
        Ok("Space".to_string())
    } else if character.is_control() {
        Err("That key cannot be used as a Texti shortcut.".to_string())
    } else {
        Ok(character.to_uppercase().collect())
    }
}

fn normalize_key_label(value: &str) -> Result<String, String> {
    if value.chars().count() == 1 {
        return normalize_character(value);
    }
    let normalized = match value.to_ascii_lowercase().as_str() {
        "escape" | "esc" => "Escape",
        "space" => "Space",
        "tab" => "Tab",
        "enter" | "return" => "Enter",
        "backspace" => "Backspace",
        "delete" | "del" => "Delete",
        "insert" | "ins" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" | "page-up" => "PageUp",
        "pagedown" | "page-down" => "PageDown",
        "arrowup" | "up" => "ArrowUp",
        "arrowdown" | "down" => "ArrowDown",
        "arrowleft" | "left" => "ArrowLeft",
        "arrowright" | "right" => "ArrowRight",
        other if function_key_number(other).is_some() => {
            return Ok(other.to_ascii_uppercase());
        }
        _ => return Err(format!("Unknown shortcut key: {value}")),
    };
    Ok(normalized.to_string())
}

fn function_key_number(value: &str) -> Option<u8> {
    let number = value.strip_prefix(['F', 'f'])?.parse().ok()?;
    (1..=12).contains(&number).then_some(number)
}

fn is_modifier_key(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(
            NamedKey::Alt
                | NamedKey::AltGraph
                | NamedKey::Control
                | NamedKey::Shift
                | NamedKey::Meta
                | NamedKey::Hyper
                | NamedKey::Super
        )
    )
}

fn named_key_label(key: NamedKey) -> Option<&'static str> {
    Some(match key {
        NamedKey::Escape => "Escape",
        NamedKey::Space => "Space",
        NamedKey::Tab => "Tab",
        NamedKey::Enter => "Enter",
        NamedKey::Backspace => "Backspace",
        NamedKey::Delete => "Delete",
        NamedKey::Insert => "Insert",
        NamedKey::Home => "Home",
        NamedKey::End => "End",
        NamedKey::PageUp => "PageUp",
        NamedKey::PageDown => "PageDown",
        NamedKey::ArrowUp => "ArrowUp",
        NamedKey::ArrowDown => "ArrowDown",
        NamedKey::ArrowLeft => "ArrowLeft",
        NamedKey::ArrowRight => "ArrowRight",
        NamedKey::F1 => "F1",
        NamedKey::F2 => "F2",
        NamedKey::F3 => "F3",
        NamedKey::F4 => "F4",
        NamedKey::F5 => "F5",
        NamedKey::F6 => "F6",
        NamedKey::F7 => "F7",
        NamedKey::F8 => "F8",
        NamedKey::F9 => "F9",
        NamedKey::F10 => "F10",
        NamedKey::F11 => "F11",
        NamedKey::F12 => "F12",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_labels_are_normalized() {
        assert_eq!(
            Shortcut::parse("shift+ctrl+p").unwrap().label(),
            "Ctrl+Shift+P"
        );
        assert_eq!(Shortcut::parse("ctrl+,").unwrap().label(), "Ctrl+,");
    }

    #[test]
    fn plain_keys_and_reserved_keys_are_rejected() {
        assert!(Shortcut::parse("A").is_err());
        assert!(Shortcut::parse("Shift+A").is_err());
        assert!(Shortcut::parse("Escape").is_err());
        assert!(Shortcut::parse("Ctrl+F1").is_err());
        assert!(Shortcut::parse("F2").is_ok());
    }

    #[test]
    fn overrides_clear_and_invalid_values_fall_back_safely() {
        let command = command("file.save").unwrap();
        let mut preferences = CommandPaletteSettings::default();
        preferences
            .shortcut_overrides
            .insert(command.id.to_string(), None);
        assert_eq!(resolved_shortcut(command, &preferences), None);

        preferences
            .shortcut_overrides
            .insert(command.id.to_string(), Some("plain-key".to_string()));
        assert_eq!(resolved_shortcut_label(command, &preferences), "Ctrl+S");
    }

    #[test]
    fn conflicts_use_resolved_shortcuts() {
        let preferences = CommandPaletteSettings::default();
        let candidate = Shortcut::parse("Ctrl+S").unwrap();
        assert_eq!(
            shortcut_conflict("file.open", &candidate, &preferences).map(|item| item.id),
            Some("file.save")
        );
    }
}
