# Changelog

## 0.1.4 - F11, accent overrides and activation tokens

- In a terminal, F11 reaches the program unless a window command binds it, so programs receive it on macOS.
- A subtree theme override can set the accent color, which also colors selected rows.
- Activating a window can pass the Wayland activation token that came with the user's request.

## 0.1.3 - Terminal options and settings windows

- Text and controls scale with the desktop's interface font.
- Terminal views take options for the font weight, ligatures, the cursor, bold as bright, readability, padding, the scrollbar, scrolling on output and typing, word characters, Option as Meta, copy on select, confirming risky pastes, a flashing bell and background opacity.
- In a terminal, a double click selects a word, dropped files paste as quoted paths, and a window's bound Ctrl and Alt chords reach it unless they hold a letter.
- Terminal snapshots name the foreground job.
- Preferences with several pages show them as tabs, every field can be reset alone, action fields report button clicks, color fields show a swatch, and a dialog sent again updates in place.
- A dialog opened over a dialog window gets a window of its own, cascaded from it.
- Windows can be see-through, switching while open, and be resized by the application.
- Text styles can turn ligatures off.
- A family draws regular and bold text with its faces nearest those weights, and a face whose style names it italic is italic.

## 0.1.2 - Dialog windows

- Preferences and wizards open in a window of their own, centered over the window that asked, which can be moved and resized; the window frame carries their title and close button.
- A popup list too long for its window shows the rows that fit and scrolls through the rest.
- Help beneath a preference wraps.

## 0.1.1 - Startup code release

- Once the first frame reaches a window, the pages of code a link order lays out between `gui_engine_startup_code_begin` and `gui_engine_startup_code_end` are released on Linux; without such an order nothing is released.
- `gui_engine_startup_complete` marks where startup ends, for profiles that split startup from the rest of a session.
- Terminals use wezterm-lite 0.1.1, which stores scrollback lines at their exact size.
- Accessibility tools read the engine's own version as the toolkit version.

## 0.1.0

- Initial version.
