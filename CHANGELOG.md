# Changelog

## 0.2.6 - Wayland windows that open reliably

- Builds on windowing-gos 0.1.11, whose windows under libdecor no longer hang while they wait for their first configure.
- A run box grows for its suggestions on a driver that cannot change a window's resizability, such as SDL's offscreen driver, instead of failing.

## 0.2.5 - FreeBSD desktops, leaner memory

- Builds on windowing-gos 0.1.10, whose Wayland windows present through shared memory instead of loading the GPU driver.

- gui-engine builds and runs on FreeBSD, taking the paths it takes on Linux: AT-SPI accessibility over D-Bus, GStreamer media, desktop-entry application lists, XDG settings and cache directories, and system fonts under `/usr/local/share/fonts`. It did not compile there.
- A terminal on FreeBSD knows the program running in it and its working directory, read through `sysctl` where Linux reads `/proc`.
- A terminal repaint that copies rows unchanged since the frame before keeps their text laid out, so the next scroll or change lays out only new text; such a repaint emptied the terminal's layout cache.
- An installed font file that no program of the user's can change is mapped instead of read into memory, so a fallback font such as a color emoji or CJK collection adds file pages the system shares and reclaims, not heap: a terminal showing Claude Code's symbols uses about 30 MB less. The 32 MB limit now applies only to fonts read into memory, so larger collections such as Apple Color Emoji load.
- A hidden window lets its surface, raster tiles and recorded scene go, as a minimized one does, and paints whole when shown again; a hidden quick terminal held its frame.
- Raster tiles and cached subtrees a window has not reused for five to ten seconds are let go, so a window that went quiet keeps none.
- The installed-font catalog keeps one copy of each distinct coverage list, which a family's styles share: about 1.3 MB less with a typical set of fonts.
- The background worker that loads fonts and images hands its freed memory back before it waits, and a bounded file read allocates once at the file's size.
- A window shown in answer to a later launch of the program takes focus as that launch's own, as a window opened for one does.

## 0.2.4 - Terminal cursor focus and blinking

- Builds on windowing-gos 0.1.9, whose macOS windows keep only the buffer they show in memory.
- A window that is hidden or covered keeps its keyboard focus when it comes back: on macOS a hidden and shown window lost it in the engine's view until it was clicked, so a focused terminal drew no cursor.
- A terminal whose window lacks keyboard focus draws its cursor hollow; it drew none.
- A terminal's cursor blinks when its options ask for that, as soon as they do; it stayed solid.

## 0.2.3 - Faster terminal output

- Terminal output that keeps arriving while it takes longer to parse than to read, such as colored or non-Latin text, is parsed on a thread of its own while the next output is read, so it streams faster; plain text and a program's reply to typing are still parsed as they are read.
- Text in non-Latin scripts, and text that mixes scripts, parses faster, and a combining mark at the start of output joins the character before it only when the two form one grapheme: builds on wezterm-lite 0.1.6.

## 0.2.2 - Faster terminal repaints

- A terminal repaint copies each row that looks as it did in the frame before instead of drawing it again, so a keystroke, or a program redrawing a few lines of a full screen, costs about a tenth of a whole repaint.
- Terminal output paints once per refresh of the display a window is on, 8 ms apart at 120 Hz, instead of at most 60 times a second; a display that reports no refresh rate keeps 60.
- Screens of colored text paint faster: the legibility check reads the sRGB curve from a table.
- `Retained::recording` takes `&mut self`, and a recording can copy commands the recording before it made: `Canvas::recorded_mark`, `recorded_span` and `replay_span`.

## 0.2.1 - Typing after full screen TUI apps

- The first character typed after a full screen TUI app that used kitty's keyboard protocol exits on Ctrl+C or Ctrl+D reaches the shell; it used to be dropped.

## 0.2.0 - Event and command names

- `UiMsg` events are named for what happened: `Clicked`, `Toggled`, `Checked`, `Chosen`, `Submitted`, `Selected`, `SelectedMany`, `Expanded`, `Activated`, `Reordered`, `SortRequested`, `OpenRequested`, `OpenUriRequested`, `TextChanged`, `CaretMoved`, `ValueChanged`, `RangeChanged`, `ColorChanged`, `DateChanged`, `TabSelected`, `SplitMoved`, `CellEdited`, `ColumnResized`, `TabMoved`, `TabDropped`, `TabDetached`, `TabMovedToWindow`, `TabDocked`, `WorkFinished`, `PrintProgress` and `WindowFocusChanged` replace `Click`, `Toggle`, `Check`, `Choose`, `Submit`, `Select`, `SelectMany`, `Expand`, `Activate`, `Reorder`, `Sort`, `Open`, `OpenUri`, `Text`, `Caret`, `Value`, `Range`, `Color`, `Date`, `Tab`, `Split`, `CellEdit`, `ColumnResize`, `TabMove`, `TabDrop`, `TabDetach`, `TabToWindow`, `Dock`, `Worked`, `Print` and `WindowFocus`.
- `Effect` commands say what they do: `StartTimer`, `StartWork`, `ShowToast`, `SetCommands`, `AnswerCompletion`, `AnswerDialogCheck` and `FocusInWindow` replace `Timer`, `Work`, `Toast`, `Commands`, `Complete`, `DialogChecked` and `FocusIn`.
- State reads as nouns: `ChoiceState.highlighted` and `highlighted_index()`, `EditState.undo_stack` and `redo_stack`, `CompletionItem.insert_text`, `TextInput.validator` and the input dialog's `validator`, `Scroll.fits_content`, `Collection.drag_offer`, `Modifiers.drag_source`, `ScrollState.drag_offset`, and `CollectionState.dragged_column`, `editing_row`, `editing_column`, `dragged_row`, `dragged_row_index` and `dragged_row_y`.
- Methods say what they do: `StateStore::edit_state`, `choice_state` and `drag_state`, `ChoiceState::type_ahead`, `FindBar::set_matches`, `ModelLog::record_removal` and `record_move`, `App::initial_focus`, `CommandTable::is_chord_pending`, and `Terminal::set_highlights`.
- A check control draws as a box or a switch by `kind: CheckKind` instead of `switch: bool`.

## 0.1.9 - Close requests over a dialog

- A window's close button, or any request from the desktop to close a window, leaves the modal dialog it shows open and focuses the window, as a dialog in a window of its own already did. A second close request no longer cancels a question such as a confirmation to close.
- Builds on wezterm-lite 0.1.5: a terminal narrowed while a full-screen program runs over a full scrollback returns its shell prompt to its own line, at column zero, when the program exits.
- New text in a field with completions highlights its best match, instead of keeping the row chosen for the earlier text.

## 0.1.8 - Docking, tab drops and forwarded command lines

- Builds on windowing-gos 0.1.8: on X11 every press of a global shortcut arrives, and a window it brings forward takes focus.
- `Effect::Dock(window, edge, length, depth)` docks a window along the top, bottom, left or right of its display's usable area, spanning `length` of that edge and reaching `depth` across; `DockResized` reports a docked window's new length and depth. A docked window's edge facing the rest of the display resizes it when dragged.
- `Effect::SetOnAllDesktops` shows a window on every virtual desktop where the desktop lets an application ask (X11, macOS).
- A tab dropped onto another window of the session away from any strip there arrives as `UiMsg::TabToWindow(strip, key, window)`, so a window that shows one page without a strip still takes tabs.
- `App::single_instance_command_line(arguments)` hands a later launch's command line and working directory to the running instance, which receives them as `UiMsg::CommandLine(directory, arguments)`; a window it opens in answer takes focus as the launch's own. `App::instance(name)` claims the instance under a name of its own, so one application can keep more than one kind.

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
