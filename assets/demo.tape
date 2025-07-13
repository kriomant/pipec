Require cargo
Require jq
Require ctp
Require astrp

Set Shell "zsh"
Set FontSize 20
Set Theme "Catppuccin Mocha"
Set WindowBar Colorful
Set BorderRadius 10
Set Margin 20
Set MarginFill "#0D1116"
Set Width 1024
Set Height 600
Set Padding 10

# Request some public API
Type "curl -s 'https://restcountries.com/v3.1/all?fields=cca3,capital,population,area'"
Enter
Sleep 2

# Re-run with formatting
Up
Type " | jq . "
Enter
Sleep 2

# Show intention
Type 'clear'
Sleep 1
Enter
Type '# Return to last command and Press "Ctrl-X P"'
Sleep 2

# Show previous command
Up 2
Sleep 1

# Emulate calling zsh widget
Hide
Ctrl+U # clear command line
Type `cargo run -- --print-on-exit=pipeline --parse-commands "curl -s 'https://restcountries.com/v3.1/all?fields=cca3,capital,population,area' | jq ."`
Enter
Sleep 5
Wait+Screen@10s /Welcome/
Show
Sleep 2

# Execute pipeline
Enter
Sleep 2

# Add new stage
Ctrl+N
Type `jq '[.[] | {code: .cca3, capital, population}]'`
Enter
Sleep 2

# Add table stage
Ctrl+N
Type `ctp -s`
Enter
Sleep 2

# Filter ANSI
Ctrl+N
Type `astrp`
Enter
Sleep 2

# View output in pager
Ctrl+L
Sleep 1
Ctrl+D
Sleep 1
Ctrl+D
Sleep 1
Type 'q'
Sleep 2

# Edit command in editor
Up@1s 2
Ctrl+V
Type@0.2s '$hhi, area'
Escape
Sleep 2
Type `:wq`
Sleep 1
Enter

# Go to last stage and execute
Down@1s 2
Enter

Sleep 0.5
Screenshot assets/screenshot.png

# Disable unneeded commands
Ctrl+X
Up@1s 3
Ctrl+X
Sleep 1

Ctrl+Q
Sleep 1

Hide
Type "y"
Sleep 0.5
Type "clear"
Enter
Type `curl -s 'https://restcountries.com/v3.1/all?fields=cca3,capital,population,area' | jq '[.[] | {code: .cca3, capital, population, area}]' | ctp -s`
Show

Sleep 10

