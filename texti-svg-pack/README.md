# Texti SVG Pack

Custom SVG icon set for the no-chrome Texti redesign.

Design anchor:
- smoked graphite / dark glass UI
- violet-blue accent system
- decagon Texti mark
- right-click command layer
- focused editor canvas with minimal visible chrome

Implementation notes:
- Most UI icons are `24x24`, stroke-only, and use `currentColor`.
- Brand icons are intentionally colored.
- Recommended Slint usage: tint menu icons via foreground/text color, not per-file SVG edits.
- Keep visible UI sparse; use icons mainly in context menus, command overlays, settings, and file picker overlays.
