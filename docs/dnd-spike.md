# Drag-and-drop spike

Decision: **go**, with the Downloads/DevHop fallback required.

Windows can start a drag with `DoDragDrop` from an `IDataObject` that advertises `CF_HDROP`, and an edge strip can implement `IDropTarget` to read the source paths while the drag is still in progress. macOS can read file URLs from the drag pasteboard and offer local paths back. Both of those APIs exist and the code in `hop-platform` calls them.

What is not done, and why the fallback stays:

- Windows virtual `FILECONTENTS` / `FILEGROUPDESCRIPTOR` streaming-on-drop is not the path `GetData` returns today. Incoming files are real paths, staged first.
- macOS `NSFilePromiseProvider` / `NSDraggingSession` is not started. The target puts file URLs on the pasteboard and copies into `~/Downloads/DevHop` when a drop is refused.

Interactive confirmation (Explorer, Finder, a browser file input, Mail) is a manual test, not something this note can sign off. See `docs/manual-tests.md` R13 and R16.
