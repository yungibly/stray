Simple on-demand tray program. Uses your existing menu (wmenu by default, but could use fuzzel, wofi, etc) to display tray programs and select their options.
This way, if you rarely interact with programs in the tray, you can still do so when you want to, but without having a constant listener or adding complexity to your setup.

To install, just run `make install` in the root with the Makefile. If you want it to go into .local/bin instead of /usr/, run `make install PREFIX=$HOME/.local SUDO=`
