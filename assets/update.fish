ansi2 cmd -c "neofetch" -c "mho install" -c "mho config update" -c "mho start" -c "curl google.com" --shell fish --prompt -f ans > mho.ans
ansi2 cmd -c "neofetch" -c "mho install" -c "mho config update" -c "mho start" -c "curl google.com" --shell fish --prompt -f svg > mho.svg
ansi2 mho.ans > mho.svg