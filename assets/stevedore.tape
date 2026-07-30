# Output path
Output assets/stevedore.gif

# Settings
Set Shell "bash"
Set FontSize 16
Set Width 1200
Set Height 720
Set Padding 10

# Setup the environment silently
Hide
Type "export RUSTFLAGS='-A warnings'"
Enter
Type "clear"
Enter
Sleep 500ms
Show

# Start the application
Type "cargo run"
Enter
Sleep 3s

# Navigate down the container list to show live stats updating
Down 2
Sleep 1.5s
Down 1
Sleep 1.5s
Down 3
Sleep 1.5s

# Switch to the logs view of the selected container
Enter
Sleep 4s

# Scroll down the logs to show scrollback working
PageDown
Sleep 1.5s
PageDown
Sleep 1.5s

# Scroll back up the logs
PageUp 2
Sleep 2s

# Switch back to the details pane
Escape
Sleep 2s

# Navigate down a bit more
Down 2
Sleep 1.5s

# Quit the application gracefully in the background
Hide
Type "q"
Sleep 1s

