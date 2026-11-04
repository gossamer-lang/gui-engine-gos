# Changelog

## 0.1.7 - windowing-gos 0.1.5

- Builds on windowing-gos 0.1.5.

## 0.1.6 - Global shortcuts, fetching and window visibility

- Requires Gossamer 0.76.0 or later.
- Builds on windowing-gos 0.1.4 and wezterm-lite 0.1.4.
- A terminal's cursor at the start of its line keeps its place when the terminal changes width, so a shell's prompt returns where it was after a full-screen program exits.
- Delete reaches a program that turned on the kitty keyboard protocol as Delete, not as Backspace.
- `Effect::RegisterHotkey` delivers a shortcut as `UiMsg::Hotkey(id)` while any application has focus, and answers a refusal as `UiMsg::Task(id, Failed(why))`; `Effect::UnregisterHotkey` gives it back.
- `Effect::SetWindowVisible` hides a window without closing it and shows it again focused, `Effect::SetAlwaysOnTop` keeps it above other windows, and `Effect::DockTop` sizes it to fractions of its display's usable area along the top edge.
- A window docked with `DockTop` that the user resizes reports its new share of the display as `UiMsg::DockResized(window, width, height)`, and a docked window no longer saves its size and place over the one remembered for its key.
- A launch forwarded to a single-instance application shows its main window when it was hidden.
- A terminal's scrollbar set to "always" shows whether or not there is history, and `ViewOptions::scrollbar_side` puts it on the left or the right.
- A terminal reports `Event::Focused` when its view takes keyboard focus.
- `Operation::Fetch` GETs a URL over HTTP or HTTPS on the session's worker, with Rustls and the Mozilla root certificates, and answers `Outcome::Fetched` with the status and body.
- Every dialog button has a keyboard accelerator, its first free letter, drawn underlined: the letter alone presses it, or Alt and the letter while a field has focus. Left and Right move between a dialog's buttons, and Enter presses the focused one.
- A tab dropped beside its window, as onto a window next to it, leaves the strip (`TabMove` or `TabDetach`) instead of reordering.
- A context menu opened by a right press stays open when that press is released where it was made, even where the menu opens under the pointer; releasing after moving onto an item still chooses it.
- `ThemeOverride` takes `surface` and `field` colors for its subtree, which a joined tab strip and menu bar draw with.
- `TextInput::family` sets a field's text in an installed font family, and `TextInput::font_points` sets its size in points; a line field's height follows that size.

## 0.1.5 - Faster terminals, bounded history, keyboard modes and held spin box buttons

- Requires Gossamer 0.75.17 or later and builds on windowing-gos 0.1.3 and wezterm-lite 0.1.2.
- A terminal shows a typed character as soon as the program echoes it, instead of up to a frame later.
- A typed key reaches the program from the session's thread when no input waits before it, and a keystroke repaints only when it changes the view, so an echoed character shows sooner.
- A terminal parses a program's output on its reader thread while the session paints, and hands output with no OSC, DCS or APC string to the parser without copying it: plain text shows about twice as fast as in 0.1.4, colored text about 1.8 times and mixed Unicode about 2.4 times.
- While a window waits for its next frame, more terminal output does not wake the session again, so a flood of output wakes it about 230 times a second instead of 2,000, and its frames arrive more evenly.
- A terminal's history is bounded by memory as well as by rows, so a long history no longer refuses the terminal. Once the windows show all of a terminal's output, its history well above the screen is compressed on a thread of its own, so a flood's rows are not compressed only to be dropped.
- A paste of up to 16 MiB goes to the program as fast as it reads it, as one paste: input typed meanwhile follows it, and Escape or Ctrl+C stops it. A paste from the view that fails is reported as `Refused`.
- A program that asks for kitty's keyboard protocol gets keys encoded under it, key releases included when it asks; others keep classic xterm keys.
- Terminal copy mode (`terminal::copy_mode`) moves a cursor of its own over the screen and the history to select and copy; link hints (`terminal::hints`) label the links on the screen for a key to open or copy; `terminal::leave_mode` ends either.
- `terminal::save_text` writes the history or the selection to a file, each wrapped row joined into its line.
- `Session::set_remember_size(false)` opens windows at the size the application asks for instead of the size they last closed at; they still open where they were.
- Holding a spin box's - or + button keeps stepping, as in GTK: after half a second, then 20 steps a second until the button is released or the value reaches its bound.

## 0.1.4 - F11, accent overrides, activation tokens and SVG icons

- In a terminal, F11 reaches the program unless a window command binds it, so programs receive it on macOS.
- A subtree theme override can set the accent color, which also colors selected rows.
- Activating a window can pass the Wayland activation token that came with the user's request.
- An application icon can be drawn from an SVG document's text.

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
