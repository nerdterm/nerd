# Uninstall

From a terminal outside nerd:

```sh
nerd --stop
nerd --uninstall-hook
nerd --uninstall
rm ~/.local/bin/nerd
rm -rf ~/Library/Application\ Support/nerd
```

`nerd --uninstall` also undoes every terminal preset, before the folder that holds the preset files goes.

If you set up another account for isolation, also remove its copy of nerd, `sudo rm -rf "/Library/Application Support/nerd"`, and, if you added it, the sudoers file the Accounts window showed, `sudo rm /etc/sudoers.d/nerd-<account>`.
