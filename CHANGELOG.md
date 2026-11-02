# Changelog

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
