# Manual tests

Run these on one Windows machine and one Mac on the same LAN, after both have been paired (the 6-digit code matches on both screens).

## R1 Hop modes

1. Set hop mode to Relative. Leave a monitor at about 30% across. The cursor arrives at 30% of the next monitor.
2. Set Memory, visit the next monitor, move the cursor, hop back, hop forward again. The cursor returns to the remembered spot.
3. Set Center. A hotkey hop lands in the middle of the target.
4. Number hops follow reading order: top row left to right, then the next row.

## R2 DPI crossing

1. Put a short monitor beside a tall one.
2. Leave the tall monitor three quarters of the way down. The cursor arrives three quarters of the way down the short one.
3. Leave through a corner with more vertical than horizontal overshoot. The vertical neighbor wins.

## R3 Stuck keys

1. Hold a key, then cross to the other computer and release it there.
2. Pull the network cable. Within about 300 ms the held key is released on the computer that was being controlled.

## R4 Latency

1. On a wired LAN, cross and move the cursor for a few seconds.
2. The log p95 for that session is at or below 10 ms. A file copy running at the same time does not change that.

## R5 Heartbeat

1. Cross, then stop the other DevHop process.
2. Keys and buttons release, the local cursor comes back, and a drag is cancelled.

## R6 Panic

1. Cross and hold several keys.
2. Press Ctrl+Alt+Esc. Everything releases and the cursor is local again, even if the other computer is gone.

## R7 Pairing

1. A newly discovered computer shows the same 6-digit code on both screens.
2. Input does nothing until both sides confirm.
3. Reject keeps the connection closed to input.
4. Forget removes the pin. The next connection asks again.

## R8 Clipboard

1. Copy text, HTML, RTF, and a PNG each way.
2. A payload over 32 MB is not pasted.
3. An image copied on Windows pastes as a picture on macOS, and the other way around.

## R9 Modifiers and scroll

1. With the swaps on, Cmd+C on the Mac copies on Windows, and Ctrl+C on Windows copies on the Mac.
2. Natural scrolling flips the wheel. Speed 1.5 scales a 120-unit notch to 180.

## R10 macOS permissions

1. On a fresh Mac, DevHop shows the permission screen and does not capture input.
2. After Accessibility and Input Monitoring are granted, capture starts.

## R11 Display changes

1. Unplug a monitor. Within a second the layout drops it and hops no longer target it.

## R12 File clipboard

1. Copy files on the Mac and paste into Explorer.
2. Copy files on Windows and paste into Finder.
3. Names that differ only by NFC/NFD arrive as the same file.

## R13 Drag and drop

1. Drag files off the edge of one screen and drop them on the desktop of the other.
2. Drop into Explorer or Finder, and into a browser upload field.
3. Press Esc during the drag. The drag cancels and the cursor stays on the remote computer.
4. Cross back home during the drag. The drag cancels and the cursor is local.

## R14 Transfers

1. Send a folder of about 100 files, including an empty file and a nested directory.
2. Cancel halfway. The destination and the staging directory have no partial files.
3. A hash mismatch does not publish the file.

## R15 Priority

1. Start a multi-gigabyte copy, then move the cursor across. Motion stays smooth.

## R16 Fallback

1. Drop onto a target that refuses the drag. The files appear in Downloads/DevHop.
