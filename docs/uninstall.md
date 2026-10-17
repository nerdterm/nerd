# Uninstall

From a terminal outside nerd:

```sh
nerd --stop
nerd --uninstall-hook
nerd --uninstall-keys
nerd --uninstall
rm ~/.local/bin/nerd
rm -rf ~/Library/Application\ Support/nerd
```

If you set up another account for isolation, also remove its copy of nerd, `sudo rm -rf "/Library/Application Support/nerd"`, and, if you added it, the sudoers file the Accounts window showed, `sudo rm /etc/sudoers.d/nerd-<account>`.
