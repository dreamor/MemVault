# Render from the repository root:
#   vhs plugins/memvault/assets/demo.tape
# Uses an isolated temporary MemVault home and disables embeddings so the
# recording never touches the maintainer's real memory database.
Output demo.gif
Set Shell "bash"
Set FontSize 18
Set Width 1400
Set Height 620
Set Padding 28
Set Margin 24
Set Theme "Dracula"
Set TypingSpeed 50ms
Set WindowBar Colorful

Hide
Type "export MEMVAULT_HOME=$(mktemp -d /tmp/memvault-demo-XXXXXX)"
Enter
Type "export MEMVAULT_EMBEDDING_PROVIDER=off"
Enter
Type "clear"
Enter
Show

Type "# 1. Save a preference once"
Enter
Sleep 500ms
Type "memvault-cli save --content 'I prefer pnpm over npm' --type preference --priority MUST"
Enter
Sleep 2s

Type "# 2. Any agent session can recall it"
Enter
Sleep 500ms
Type "memvault-cli search --query 'pnpm'"
Enter
Sleep 3s

Type "# 3. Verify every integration surface"
Enter
Sleep 500ms
Type "memvault-cli --version"
Enter
Sleep 2s
