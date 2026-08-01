# Generate with: vhs assets/demo.tape
# (run from the repo root, after `cargo build --release`)

Output assets/demo.gif

Set Shell "bash"
Set FontSize 20
Set Width 1200
Set Height 800
Set Padding 20
Set WindowBar Rings
Set BorderRadius 8
Set TypingSpeed 60ms

Hide
Type "./target/release/terminal-spinner"
Enter
Sleep 800ms
Show

# resting
Sleep 1s

# spin up toward top speed (crosses the rainbow threshold)
Right@150ms 9
Sleep 1200ms
Right@300ms 3
Sleep 1500ms

# let it coast and wind down on its own (friction)
Sleep 1800ms

# tap the opposite direction: decelerates through zero, then reverses
Left@150ms 12
Sleep 1200ms

# hard stop
Space
Sleep 900ms

# help popup
Type "?"
Sleep 2200ms
Type "?"
Sleep 500ms

# quit
Type "q"
Sleep 250ms
